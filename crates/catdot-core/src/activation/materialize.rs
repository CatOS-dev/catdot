use crate::{
    ActivationJournal, ComponentDef, Error, Lifecycle, OverwriteMode, Profile, Result, UserState,
    atomic_write,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedTarget {
    pub owner: String,
    pub lifecycle: String,
    pub source: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedRegistry {
    pub entries: BTreeMap<String, ManagedTarget>,
    #[serde(default)]
    pub user_initialized: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum Materialization {
    Generate {
        contents: Vec<u8>,
    },
    Symlink {
        source: PathBuf,
    },
    File {
        source: PathBuf,
    },
    User {
        seed: Option<PathBuf>,
        release_managed: bool,
        initialized: bool,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedTarget {
    pub target: PathBuf,
    pub owner: String,
    pub materialization: Materialization,
}

#[derive(Debug, Clone)]
pub struct ActivationPlan {
    pub entries: Vec<PlannedTarget>,
    pub removals: Vec<PathBuf>,
    registry: ManagedRegistry,
}

pub fn managed_targets_path(state_path: &Path) -> Result<PathBuf> {
    let parent = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    Ok(parent.join("managed.toml"))
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
    home: &Path,
    registry_path: &Path,
) -> Result<ActivationPlan> {
    build_activation_plan_with_sources(profiles, state, home, registry_path, true)
}

pub fn build_activation_preview(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    home: &Path,
    registry_path: &Path,
) -> Result<ActivationPlan> {
    build_activation_plan_with_sources(profiles, state, home, registry_path, false)
}

fn build_activation_plan_with_sources(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    home: &Path,
    registry_path: &Path,
    verify_sources: bool,
) -> Result<ActivationPlan> {
    let previous = read_managed_registry(registry_path)?;
    let mut entries = Vec::new();
    for (role, reference) in &state.components {
        let (profile_id, component_id) = reference
            .split_once('/')
            .ok_or_else(|| Error::Message(format!("invalid component reference {reference}")))?;
        let profile = profiles
            .get(profile_id)
            .ok_or_else(|| Error::Message(format!("profile {profile_id} is not installed")))?;
        let component = profile
            .components
            .get(component_id)
            .ok_or_else(|| Error::Message(format!("component {reference} is not installed")))?;
        if component.role != *role {
            return Err(Error::Message(format!(
                "{reference} does not provide role {role}"
            )));
        }
        add_component_entries(
            &mut entries,
            profile,
            component,
            reference,
            home,
            verify_sources,
        )?;
    }
    validate_conflicts(&entries, home)?;
    for entry in &mut entries {
        if let Materialization::User {
            release_managed,
            initialized,
            ..
        } = &mut entry.materialization
        {
            let target = entry.target.display().to_string();
            *release_managed = previous.entries.contains_key(&target);
            *initialized = previous.user_initialized.contains_key(&target);
        }
    }
    if let Some(target) = previous
        .entries
        .keys()
        .map(Path::new)
        .find(|target| !target.starts_with(home))
    {
        return Err(Error::Message(format!(
            "managed registry target escapes home: {}",
            target.display()
        )));
    }
    let current: BTreeSet<_> = entries
        .iter()
        .map(|entry| entry.target.display().to_string())
        .collect();
    let removals = previous
        .entries
        .keys()
        .filter(|target| !current.contains(*target))
        .map(PathBuf::from)
        .collect();
    let mut registry = ManagedRegistry {
        entries: BTreeMap::new(),
        user_initialized: previous.user_initialized.clone(),
    };
    for entry in &entries {
        if let Materialization::User { .. } = entry.materialization {
            registry
                .user_initialized
                .insert(entry.target.display().to_string(), entry.owner.clone());
            continue;
        }
        let (lifecycle, source) = match &entry.materialization {
            Materialization::Generate { .. } => ("generate".into(), None),
            Materialization::Symlink { source } => {
                ("overwrite/symlink".into(), Some(source.clone()))
            }
            Materialization::File { source } => ("overwrite/file".into(), Some(source.clone())),
            Materialization::User { .. } => unreachable!(),
        };
        registry.entries.insert(
            entry.target.display().to_string(),
            ManagedTarget {
                owner: entry.owner.clone(),
                lifecycle,
                source,
            },
        );
    }
    Ok(ActivationPlan {
        entries,
        removals,
        registry,
    })
}

fn add_component_entries(
    entries: &mut Vec<PlannedTarget>,
    profile: &Profile,
    component: &ComponentDef,
    owner: &str,
    home: &Path,
    verify_sources: bool,
) -> Result<()> {
    for configuration in &component.configuration {
        let target = home.join(&configuration.target);
        let materialization = match &configuration.lifecycle {
            Lifecycle::Generate => Materialization::Generate {
                contents: configuration
                    .template
                    .as_ref()
                    .expect("validated template")
                    .as_bytes()
                    .to_vec(),
            },
            Lifecycle::Overwrite(OverwriteMode::Symlink) => Materialization::Symlink {
                source: source(
                    profile,
                    configuration.source.as_ref().expect("validated source"),
                    verify_sources,
                )?,
            },
            Lifecycle::Overwrite(OverwriteMode::File) => Materialization::File {
                source: source(
                    profile,
                    configuration.source.as_ref().expect("validated source"),
                    verify_sources,
                )?,
            },
            Lifecycle::User => Materialization::User {
                seed: configuration
                    .seed
                    .as_ref()
                    .map(|seed| source(profile, seed, verify_sources))
                    .transpose()?,
                release_managed: false,
                initialized: false,
            },
        };
        entries.push(PlannedTarget {
            target,
            owner: owner.into(),
            materialization,
        });
    }
    Ok(())
}

fn source(profile: &Profile, relative: &Path, verify_sources: bool) -> Result<PathBuf> {
    let path = profile.source_root.join(relative);
    if verify_sources && fs::symlink_metadata(&path).is_err() {
        return Err(Error::Message(format!(
            "configuration source {} does not exist",
            path.display()
        )));
    }
    Ok(path)
}

fn validate_conflicts(entries: &[PlannedTarget], home: &Path) -> Result<()> {
    let mut seen = BTreeMap::<&Path, &PlannedTarget>::new();
    for entry in entries {
        if !entry.target.starts_with(home) || entry.target == home {
            return Err(Error::Message(format!(
                "target escapes home: {}",
                entry.target.display()
            )));
        }
        if let Some(previous) = seen.insert(&entry.target, entry) {
            return Err(Error::Message(format!(
                "configuration target conflict: {} ({}, {})",
                entry.target.display(),
                previous.owner,
                entry.owner
            )));
        }
    }
    for (target, entry) in &seen {
        for ancestor in target.ancestors().skip(1).take_while(|path| *path != home) {
            if let Some(parent) = seen.get(ancestor) {
                return Err(Error::Message(format!(
                    "configuration path conflict: {} owned by {} conflicts with descendant {} owned by {}",
                    ancestor.display(),
                    parent.owner,
                    target.display(),
                    entry.owner
                )));
            }
            if fs::symlink_metadata(ancestor)
                .ok()
                .is_some_and(|m| m.file_type().is_symlink())
            {
                return Err(Error::Message(format!(
                    "configuration target {} has symlink parent {}",
                    target.display(),
                    ancestor.display()
                )));
            }
        }
    }
    Ok(())
}

pub fn forget_user_initialization(
    registry_path: &Path,
    targets: impl IntoIterator<Item = PathBuf>,
) -> Result<()> {
    let mut registry = read_managed_registry(registry_path)?;
    for target in targets {
        registry
            .user_initialized
            .remove(&target.display().to_string());
    }
    atomic_write(
        registry_path,
        &toml::to_string_pretty(&registry).map_err(|error| Error::Message(error.to_string()))?,
    )
}

pub fn activate_configuration(
    plan: &ActivationPlan,
    registry_path: &Path,
    journal: &mut ActivationJournal,
) -> Result<()> {
    for entry in &plan.entries {
        journal.track_path(&entry.target)?;
    }
    for target in &plan.removals {
        journal.track_path(target)?;
    }
    journal.track_path(registry_path)?;
    for entry in &plan.entries {
        apply_entry(entry)?;
        journal.mark_applied()?;
    }
    for target in &plan.removals {
        remove_path(target)?;
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
        Materialization::Generate { contents } => replace_file(&entry.target, contents, 0o644),
        Materialization::Symlink { source } => {
            if fs::symlink_metadata(&entry.target)
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_symlink())
                && fs::read_link(&entry.target).ok().as_ref() == Some(source)
            {
                return Ok(());
            }
            parent(&entry.target)?;
            let temporary = sibling(&entry.target, "link");
            symlink(source, &temporary).map_err(|source| Error::Io {
                path: temporary.display().to_string(),
                source,
            })?;
            rename_staged(&temporary, &entry.target)
        }
        Materialization::File { source } => replace_from(source, &entry.target),
        Materialization::User {
            seed,
            release_managed,
            initialized,
        } => match fs::symlink_metadata(&entry.target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if *initialized && !*release_managed {
                    return Ok(());
                }
                if let Some(seed) = seed {
                    copy_tree(seed, &entry.target)
                } else {
                    fs::create_dir_all(&entry.target).map_err(|source| Error::Io {
                        path: entry.target.display().to_string(),
                        source,
                    })
                }
            }
            Ok(metadata) if *release_managed && metadata.file_type().is_symlink() => {
                let resolved = fs::canonicalize(&entry.target).map_err(|source| Error::Io {
                    path: entry.target.display().to_string(),
                    source,
                })?;
                replace_from(&resolved, &entry.target)
            }
            Ok(_) => Ok(()),
            Err(source) => Err(Error::Io {
                path: entry.target.display().to_string(),
                source,
            }),
        },
    }
}

fn replace_file(target: &Path, contents: &[u8], mode: u32) -> Result<()> {
    parent(target)?;
    let temporary = sibling(target, "new");
    fs::write(&temporary, contents).map_err(|source| Error::Io {
        path: temporary.display().to_string(),
        source,
    })?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).map_err(|source| {
        Error::Io {
            path: temporary.display().to_string(),
            source,
        }
    })?;
    rename_staged(&temporary, target)
}

fn replace_from(source: &Path, target: &Path) -> Result<()> {
    let temporary = sibling(target, "new");
    copy_tree(source, &temporary)?;
    parent(target)?;
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
        parent(target)?;
        return symlink(
            fs::read_link(source).map_err(|source_error| Error::Io {
                path: source.display().to_string(),
                source: source_error,
            })?,
            target,
        )
        .map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        });
    }
    if metadata.is_file() {
        parent(target)?;
        fs::copy(source, target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
    } else if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        for child in fs::read_dir(source).map_err(|source_error| Error::Io {
            path: source.display().to_string(),
            source: source_error,
        })? {
            let child = child.map_err(|source_error| Error::Io {
                path: source.display().to_string(),
                source: source_error,
            })?;
            copy_tree(&child.path(), &target.join(child.file_name()))?;
        }
    } else {
        return Err(Error::Message(format!(
            "unsupported configuration source {}",
            source.display()
        )));
    }
    fs::set_permissions(
        target,
        fs::Permissions::from_mode(metadata.permissions().mode()),
    )
    .map_err(|source_error| Error::Io {
        path: target.display().to_string(),
        source: source_error,
    })
}

fn parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    Ok(())
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
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".{}.{}-{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            suffix,
            std::process::id()
        ))
}
