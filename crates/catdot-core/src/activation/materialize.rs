use crate::{ActivationJournal, Error, Profile, Result, UserState, atomic_write};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationMode {
    Select,
    Update,
    Reset,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedTarget {
    pub owner: String,
    pub source: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedRegistry {
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub entries: BTreeMap<String, ManagedTarget>,
}

#[derive(Debug, Clone)]
pub enum Materialization {
    Managed {
        source: PathBuf,
    },
    ManagedCache {
        source_root: PathBuf,
        managed: BTreeSet<PathBuf>,
    },
    Seed {
        source: PathBuf,
        overwrite: bool,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedTarget {
    pub relative: PathBuf,
    pub target: PathBuf,
    pub owner: String,
    pub cache: bool,
    pub materialization: Materialization,
}

#[derive(Debug, Clone)]
pub struct ActivationPlan {
    pub target_profile: String,
    pub mode: ActivationMode,
    pub entries: Vec<PlannedTarget>,
    pub removals: Vec<PathBuf>,
    pub seeded_paths: BTreeSet<PathBuf>,
    registry: ManagedRegistry,
    registry_changed: bool,
}

impl ActivationPlan {
    pub fn has_changes(&self) -> bool {
        self.registry_changed
            || !self.seeded_paths.is_empty()
            || self.removals.iter().any(|target| target.exists())
            || self.entries.iter().any(entry_has_changes)
    }

    pub fn identity_digest(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.target_profile.as_bytes());
        digest.update([self.mode as u8]);
        for removal in &self.removals {
            digest.update(b"remove\0");
            digest.update(removal.as_os_str().as_bytes());
            digest.update([0xff]);
        }
        for entry in &self.entries {
            digest.update(b"entry\0");
            digest.update(entry.relative.as_os_str().as_bytes());
            digest.update([entry.cache as u8]);
            digest.update(entry.owner.as_bytes());
            digest.update([0]);
            match &entry.materialization {
                Materialization::Managed { source } => {
                    digest.update(b"managed\0");
                    hash_source(source, &mut digest);
                }
                Materialization::ManagedCache {
                    source_root,
                    managed,
                } => {
                    digest.update(b"managed-cache\0");
                    for relative in managed {
                        digest.update(relative.as_os_str().as_bytes());
                        digest.update([0]);
                        hash_source(&source_root.join(relative), &mut digest);
                        digest.update([0xff]);
                    }
                }
                Materialization::Seed { source, overwrite } => {
                    digest.update(b"seed\0");
                    digest.update([*overwrite as u8]);
                    hash_source(source, &mut digest);
                }
            }
            digest.update([0xff]);
        }
        for seeded in &self.seeded_paths {
            digest.update(b"seeded\0");
            digest.update(seeded.as_os_str().as_bytes());
            digest.update([0xff]);
        }
        format!("{:x}", digest.finalize())
    }

    pub fn record_applied_state(&self, state: &mut UserState) -> Result<()> {
        let profile = state
            .profiles
            .get_mut(&self.target_profile)
            .ok_or_else(|| {
                Error::Message(format!(
                    "profile {} is not retained in user state",
                    self.target_profile
                ))
            })?;
        profile.seeded.extend(self.seeded_paths.iter().cloned());
        profile.initialized = true;
        Ok(())
    }

    pub fn record_seeded_paths(&self, state: &mut UserState) -> Result<()> {
        self.record_applied_state(state)
    }
}

pub fn managed_targets_path(state_path: &Path) -> Result<PathBuf> {
    let parent = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    Ok(parent.join("managed.toml"))
}

pub fn profile_managed_cache(registry_path: &Path, profile: &str) -> Result<PathBuf> {
    let parent = registry_path
        .parent()
        .ok_or_else(|| Error::Message("managed registry has no parent".into()))?;
    Ok(parent.join("profiles").join(profile).join("managed"))
}

pub fn read_managed_registry(path: &Path) -> Result<ManagedRegistry> {
    if !path.exists() {
        return Ok(ManagedRegistry::default());
    }
    toml::from_str(&fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?)
    .map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}

pub fn build_activation_plan(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    target_profile: &str,
    home: &Path,
    registry_path: &Path,
    mode: ActivationMode,
) -> Result<ActivationPlan> {
    let profile = profiles
        .get(target_profile)
        .ok_or_else(|| Error::Message(format!("unknown profile {target_profile}")))?;
    let profile_state = state
        .profiles
        .get(target_profile)
        .ok_or_else(|| Error::Message(format!("profile {target_profile} is not retained")))?;
    if mode == ActivationMode::Reset && state.active_profile.as_deref() != Some(target_profile) {
        return Err(Error::Message(format!(
            "profile {target_profile} is not active"
        )));
    }
    if profile_state.managed != profile.manage
        && !matches!(mode, ActivationMode::Update | ActivationMode::Reset)
        && !profile_state.initialized
    {
        return Err(Error::Message(format!(
            "profile {target_profile} declaration was not prepared"
        )));
    }

    let current_registry = read_managed_registry(registry_path)?;
    let switching = state.active_profile.as_deref() != Some(target_profile)
        || current_registry.active_profile.as_deref() != Some(target_profile);
    let refresh_cache = !profile_state.initialized
        || matches!(mode, ActivationMode::Update | ActivationMode::Reset);
    let refresh_home = match mode {
        ActivationMode::Select => switching,
        ActivationMode::Update => state.active_profile.as_deref() == Some(target_profile),
        ActivationMode::Reset => true,
    };
    let cache_root = profile_managed_cache(registry_path, target_profile)?;

    let mut registry = current_registry.clone();
    if refresh_home {
        registry = ManagedRegistry {
            active_profile: Some(target_profile.to_owned()),
            entries: BTreeMap::new(),
        };
        for relative in &profile_state.managed {
            registry.entries.insert(
                relative.display().to_string(),
                ManagedTarget {
                    owner: target_profile.to_owned(),
                    source: cache_root.join(relative),
                },
            );
        }
    }

    let mut entries = Vec::new();
    if refresh_cache {
        entries.push(PlannedTarget {
            relative: PathBuf::new(),
            target: cache_root.clone(),
            owner: target_profile.to_owned(),
            cache: true,
            materialization: Materialization::ManagedCache {
                source_root: profile.source_root.clone(),
                managed: profile_state.managed.clone(),
            },
        });
    }
    if refresh_home {
        for relative in &profile_state.managed {
            entries.push(PlannedTarget {
                relative: relative.clone(),
                target: checked_target(home, relative)?,
                owner: target_profile.to_owned(),
                cache: false,
                materialization: Materialization::Managed {
                    source: if refresh_cache {
                        profile.source_root.join(relative)
                    } else {
                        cache_root.join(relative)
                    },
                },
            });
        }
    }

    let mut seeded_paths = BTreeSet::new();
    if !matches!(mode, ActivationMode::Update) {
        for relative in &profile_state.seeds {
            let overwrite = mode == ActivationMode::Reset;
            if overwrite || !profile_state.seeded.contains(relative) {
                entries.push(PlannedTarget {
                    target: checked_target(home, relative)?,
                    relative: relative.clone(),
                    owner: target_profile.to_owned(),
                    cache: false,
                    materialization: Materialization::Seed {
                        source: profile.source_root.join(relative),
                        overwrite,
                    },
                });
                seeded_paths.insert(relative.clone());
            }
        }
    }

    let mut removals = Vec::new();
    if refresh_home && mode != ActivationMode::Update {
        for old in current_registry.entries.keys() {
            let old = PathBuf::from(old);
            let covered_by_new_managed = profile_state
                .managed
                .iter()
                .any(|new| old == *new || old.starts_with(new));
            let becomes_seed = profile_state
                .seeds
                .iter()
                .any(|seed| seed == &old || seed.starts_with(&old) || old.starts_with(seed));
            if !covered_by_new_managed && !becomes_seed {
                removals.push(checked_target(home, &old)?);
            }
        }
    }
    removals.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    entries
        .sort_by(|left, right| (left.cache, &left.relative).cmp(&(right.cache, &right.relative)));
    validate_plan_targets(&entries, &removals, home, &cache_root)?;

    let registry_changed = registry != current_registry;
    Ok(ActivationPlan {
        target_profile: target_profile.to_owned(),
        mode,
        entries,
        removals,
        seeded_paths,
        registry,
        registry_changed,
    })
}

pub fn build_activation_preview(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    target_profile: &str,
    home: &Path,
    registry_path: &Path,
    mode: ActivationMode,
) -> Result<ActivationPlan> {
    build_activation_plan(profiles, state, target_profile, home, registry_path, mode)
}

fn checked_target(home: &Path, relative: &Path) -> Result<PathBuf> {
    let target = home.join(relative);
    let mut current = home.to_owned();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            current.push(component.as_os_str());
            if fs::symlink_metadata(&current)
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(Error::Message(format!(
                    "configuration parent {} is a symbolic link",
                    current.display()
                )));
            }
        }
    }
    Ok(target)
}

fn validate_plan_targets(
    entries: &[PlannedTarget],
    removals: &[PathBuf],
    home: &Path,
    cache_root: &Path,
) -> Result<()> {
    let mut home_targets = Vec::new();
    let mut cache_targets = Vec::new();
    for entry in entries {
        let (root, targets) = if entry.cache {
            (cache_root, &mut cache_targets)
        } else {
            (home, &mut home_targets)
        };
        if !entry.target.starts_with(root) {
            return Err(Error::Message(format!(
                "configuration target {} escapes {}",
                entry.target.display(),
                root.display()
            )));
        }
        if targets.iter().any(|other: &PathBuf| {
            entry.target == *other
                || entry.target.starts_with(other)
                || other.starts_with(&entry.target)
        }) {
            return Err(Error::Message(format!(
                "configuration target {} overlaps another target",
                entry.target.display()
            )));
        }
        targets.push(entry.target.clone());
    }
    for removal in removals {
        if !removal.starts_with(home) {
            return Err(Error::Message(format!(
                "configuration removal {} escapes HOME",
                removal.display()
            )));
        }
    }
    Ok(())
}

fn entry_has_changes(entry: &PlannedTarget) -> bool {
    match &entry.materialization {
        Materialization::Managed { source } => !paths_equal(source, &entry.target),
        Materialization::ManagedCache { .. } => true,
        Materialization::Seed { source, overwrite } => {
            if *overwrite {
                !paths_equal(source, &entry.target)
            } else {
                !entry.target.exists()
            }
        }
    }
}

fn paths_equal(source: &Path, target: &Path) -> bool {
    let Ok(source_metadata) = fs::symlink_metadata(source) else {
        return false;
    };
    let Ok(target_metadata) = fs::symlink_metadata(target) else {
        return false;
    };
    if source_metadata.file_type().is_symlink() || target_metadata.file_type().is_symlink() {
        return false;
    }
    if source_metadata.is_file() && target_metadata.is_file() {
        return source_metadata.permissions().mode() == target_metadata.permissions().mode()
            && fs::read(source).ok() == fs::read(target).ok();
    }
    if source_metadata.is_dir() && target_metadata.is_dir() {
        let Ok(source_entries) = directory_entries(source) else {
            return false;
        };
        let Ok(target_entries) = directory_entries(target) else {
            return false;
        };
        return source_entries == target_entries
            && source_entries
                .iter()
                .all(|name| paths_equal(&source.join(name), &target.join(name)));
    }
    false
}

fn directory_entries(path: &Path) -> std::io::Result<BTreeSet<std::ffi::OsString>> {
    fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

pub fn activate_configuration(
    plan: &ActivationPlan,
    registry_path: &Path,
    journal: &mut ActivationJournal,
) -> Result<()> {
    for target in &plan.removals {
        journal.track_path(target)?;
    }
    for entry in &plan.entries {
        journal.track_path(&entry.target)?;
    }
    journal.track_path(registry_path)?;

    for target in &plan.removals {
        remove_path(target)?;
        journal.mark_applied()?;
    }
    for entry in &plan.entries {
        apply_entry(entry)?;
        journal.mark_applied()?;
    }
    atomic_write(
        registry_path,
        &toml::to_string_pretty(&plan.registry)
            .map_err(|error| Error::Message(error.to_string()))?,
    )?;
    journal.mark_applied()
}

fn apply_entry(entry: &PlannedTarget) -> Result<()> {
    match &entry.materialization {
        Materialization::Managed { source } => replace_from(source, &entry.target),
        Materialization::ManagedCache {
            source_root,
            managed,
        } => replace_managed_cache(source_root, managed, &entry.target),
        Materialization::Seed { source, overwrite } => {
            if *overwrite {
                replace_from(source, &entry.target)
            } else if entry.target.exists() {
                Ok(())
            } else {
                replace_from(source, &entry.target)
            }
        }
    }
}

fn replace_managed_cache(
    source_root: &Path,
    managed: &BTreeSet<PathBuf>,
    target: &Path,
) -> Result<()> {
    parent(target)?;
    let temporary = sibling(target, "new");
    remove_path(&temporary)?;
    fs::create_dir_all(&temporary).map_err(|source| Error::Io {
        path: temporary.display().to_string(),
        source,
    })?;
    for relative in managed {
        copy_tree(&source_root.join(relative), &temporary.join(relative))?;
    }
    rename_staged(&temporary, target)
}

fn replace_from(source: &Path, target: &Path) -> Result<()> {
    parent(target)?;
    let temporary = sibling(target, "new");
    remove_path(&temporary)?;
    copy_tree(source, &temporary)?;
    rename_staged(&temporary, target)
}

fn rename_staged(temporary: &Path, target: &Path) -> Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        let temporary_c = std::ffi::CString::new(temporary.as_os_str().as_bytes())
            .map_err(|_| Error::Message("temporary path contains NUL".into()))?;
        let target_c = std::ffi::CString::new(target.as_os_str().as_bytes())
            .map_err(|_| Error::Message("target path contains NUL".into()))?;
        let exchanged = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                temporary_c.as_ptr(),
                libc::AT_FDCWD,
                target_c.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        if exchanged == 0 {
            return remove_path(temporary);
        }
        return Err(Error::Io {
            path: target.display().to_string(),
            source: std::io::Error::last_os_error(),
        });
    }
    fs::rename(temporary, target).map_err(|source| Error::Io {
        path: target.display().to_string(),
        source,
    })
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|source_error| Error::Io {
        path: source.display().to_string(),
        source: source_error,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Message(format!(
            "profile content may not contain symbolic link {}",
            source.display()
        )));
    }
    if metadata.is_file() {
        parent(target)?;
        fs::copy(source, target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        return fs::set_permissions(target, metadata.permissions()).map_err(|source_error| {
            Error::Io {
                path: target.display().to_string(),
                source: source_error,
            }
        });
    }
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        fs::set_permissions(target, metadata.permissions()).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        let mut entries = fs::read_dir(source)
            .map_err(|source_error| Error::Io {
                path: source.display().to_string(),
                source: source_error,
            })?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|source_error| Error::Io {
                path: source.display().to_string(),
                source: source_error,
            })?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        return Ok(());
    }
    Err(Error::Message(format!(
        "unsupported profile content {}",
        source.display()
    )))
}

fn hash_source(path: &Path, digest: &mut Sha256) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        digest.update(b"missing");
        return;
    };
    digest.update(metadata.permissions().mode().to_le_bytes());
    if metadata.is_file() {
        digest.update(b"file\0");
        if let Ok(contents) = fs::read(path) {
            digest.update(contents);
        }
    } else if metadata.is_dir() {
        digest.update(b"dir\0");
        if let Ok(mut entries) =
            fs::read_dir(path).and_then(|entries| entries.collect::<std::io::Result<Vec<_>>>())
        {
            entries.sort_by_key(|entry: &fs::DirEntry| entry.file_name());
            for entry in entries {
                digest.update(entry.file_name().as_bytes());
                digest.update([0]);
                hash_source(&entry.path(), digest);
            }
        }
    } else {
        digest.update(b"unsupported");
    }
}

fn parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Message("configuration target has no parent".into()))?;
    fs::create_dir_all(parent).map_err(|source| Error::Io {
        path: parent.display().to_string(),
        source,
    })
}

fn remove_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })
        }
        Ok(_) => fs::remove_file(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.catdot-{}-{suffix}", std::process::id()))
}
