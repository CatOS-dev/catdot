use crate::{Error, Profile, ProfileState, Result, UserState};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const BACKUP_RETENTION: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationMode {
    Select,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWrite {
    pub relative: PathBuf,
    pub source: PathBuf,
    pub target: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationPlan {
    pub removals: Vec<PathBuf>,
    pub writes: Vec<PlannedWrite>,
}

#[derive(Debug, Clone, Copy)]
pub struct ActivationSources<'a> {
    pub managed: &'a Path,
    pub seeds: Option<&'a Path>,
}

pub fn profile_cache_path(state_path: &Path, profile: &str) -> Result<PathBuf> {
    let parent = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    Ok(parent.join("profiles").join(profile).join("managed"))
}

pub fn cache_profile_content(profile: &Profile, cache: &Path) -> Result<()> {
    remove_path(cache)?;
    fs::create_dir_all(cache).map_err(|source| Error::Io {
        path: cache.display().to_string(),
        source,
    })?;
    for relative in &profile.manage {
        copy_profile_tree(&profile.source_root.join(relative), &cache.join(relative))?;
    }
    Ok(())
}

pub fn build_activation_plan(
    state: &UserState,
    target_profile: &str,
    target_state: &ProfileState,
    sources: ActivationSources<'_>,
    home: &Path,
    state_path: &Path,
    mode: ActivationMode,
) -> Result<ActivationPlan> {
    if !sources.managed.is_dir() {
        return Err(Error::Message(format!(
            "profile source {} does not exist",
            sources.managed.display()
        )));
    }

    let active_target = state.active_profile.as_deref() == Some(target_profile);
    let mut removals = Vec::new();
    match mode {
        ActivationMode::Select => {
            if let Some(active) = state
                .active_profile
                .as_ref()
                .and_then(|active| state.profiles.get(active))
            {
                for relative in &active.manage {
                    removals.push(checked_target(home, state_path, relative)?);
                }
            }
        }
        ActivationMode::Update if active_target => {
            if let Some(current) = state.profiles.get(target_profile) {
                for relative in &current.manage {
                    removals.push(checked_target(home, state_path, relative)?);
                }
            }
        }
        ActivationMode::Update => {}
    }

    let write_home = mode == ActivationMode::Select || active_target;
    let mut writes = Vec::new();
    for relative in &target_state.manage {
        let source = sources.managed.join(relative);
        validate_profile_source(&source)?;
        if write_home {
            let target = checked_target(home, state_path, relative)?;
            writes.push(PlannedWrite {
                relative: relative.clone(),
                source,
                target,
            });
        }
    }

    if let Some(seed_source_root) = sources.seeds {
        if !seed_source_root.is_dir() {
            return Err(Error::Message(format!(
                "profile seed source {} does not exist",
                seed_source_root.display()
            )));
        }
        for relative in seed_files(seed_source_root, &target_state.manage)? {
            writes.push(PlannedWrite {
                source: seed_source_root.join(&relative),
                target: checked_target(home, state_path, &relative)?,
                relative,
            });
        }
    }

    removals.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    removals.dedup();
    writes.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(ActivationPlan { removals, writes })
}

pub fn apply_activation_plan(
    plan: &ActivationPlan,
    home: &Path,
    state_path: &Path,
) -> Result<Option<PathBuf>> {
    let backup_targets = backup_targets(plan, home)?;
    let backup = if backup_targets.is_empty() {
        None
    } else {
        let root = new_backup_root(state_path)?;
        for target in &backup_targets {
            let relative = target.strip_prefix(home).map_err(|_| {
                Error::Message(format!("backup target {} escapes HOME", target.display()))
            })?;
            copy_existing(target, &root.join("home").join(relative))?;
        }
        Some(root)
    };

    for target in &plan.removals {
        remove_path(target)?;
    }
    for write in &plan.writes {
        remove_path(&write.target)?;
        copy_profile_tree(&write.source, &write.target)?;
    }
    if backup.is_some() {
        prune_backups(state_path)?;
    }
    Ok(backup)
}

fn validate_profile_source(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(Error::Message(format!(
            "unsupported profile content {}",
            path.display()
        )));
    }
    Ok(())
}

fn seed_files(root: &Path, managed: &BTreeSet<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.retain(|relative| {
        !managed
            .iter()
            .any(|path| relative == path || relative.starts_with(path))
    });
    files.sort();
    Ok(files)
}

fn collect_files(root: &Path, path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let metadata = fs::symlink_metadata(entry.path()).map_err(|source| Error::Io {
            path: entry.path().display().to_string(),
            source,
        })?;
        if metadata.is_dir() {
            collect_files(root, &entry.path(), files)?;
        } else if metadata.is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| Error::Message("profile content escaped its root".into()))?
                    .to_owned(),
            );
        } else {
            return Err(Error::Message(format!(
                "unsupported profile content {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn checked_target(home: &Path, state_path: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(Error::Message(format!(
            "invalid HOME-relative path {}",
            relative.display()
        )));
    }
    let target = home.join(relative);
    let state_root = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    if target == state_root || target.starts_with(state_root) || state_root.starts_with(&target) {
        return Err(Error::Message(format!(
            "configuration path {} overlaps Catdot state {}",
            target.display(),
            state_root.display()
        )));
    }

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

fn backup_targets(plan: &ActivationPlan, home: &Path) -> Result<Vec<PathBuf>> {
    let mut candidates = plan.removals.clone();
    candidates.extend(plan.writes.iter().map(|write| write.target.clone()));
    candidates.sort_by_key(|path| path.components().count());
    let mut selected = Vec::<PathBuf>::new();
    for candidate in candidates {
        if !candidate.starts_with(home) {
            return Err(Error::Message(format!(
                "backup target {} escapes HOME",
                candidate.display()
            )));
        }
        if fs::symlink_metadata(&candidate).is_err() {
            continue;
        }
        if selected
            .iter()
            .any(|parent| candidate == *parent || candidate.starts_with(parent))
        {
            continue;
        }
        selected.push(candidate);
    }
    Ok(selected)
}

fn new_backup_root(state_path: &Path) -> Result<PathBuf> {
    let catdot = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    create_private_directory(catdot)?;
    let backups = catdot.join("backups");
    create_private_directory(&backups)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Message(error.to_string()))?
        .as_nanos();
    let root = backups.join(format!("{stamp}-{}", std::process::id()));
    create_private_directory(&root)?;
    Ok(root)
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })
}

fn prune_backups(state_path: &Path) -> Result<()> {
    let backups = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?
        .join("backups");
    let mut entries = fs::read_dir(&backups)
        .map_err(|source| Error::Io {
            path: backups.display().to_string(),
            source,
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|source| Error::Io {
            path: backups.display().to_string(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    let remove_count = entries.len().saturating_sub(BACKUP_RETENTION);
    for entry in entries.into_iter().take(remove_count) {
        remove_path(&entry.path())?;
    }
    Ok(())
}

fn copy_profile_tree(source: &Path, target: &Path) -> Result<()> {
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
        create_parent(target)?;
        fs::copy(source, target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        fs::set_permissions(target, metadata.permissions()).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        return Ok(());
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
            copy_profile_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        return Ok(());
    }
    Err(Error::Message(format!(
        "unsupported profile content {}",
        source.display()
    )))
}

fn copy_existing(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|source_error| Error::Io {
        path: source.display().to_string(),
        source: source_error,
    })?;
    if metadata.file_type().is_symlink() {
        create_parent(target)?;
        let link = fs::read_link(source).map_err(|source_error| Error::Io {
            path: source.display().to_string(),
            source: source_error,
        })?;
        symlink(link, target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        return Ok(());
    }
    if metadata.is_file() {
        create_parent(target)?;
        fs::copy(source, target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        fs::set_permissions(target, metadata.permissions()).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|source_error| Error::Io {
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
            copy_existing(&entry.path(), &target.join(entry.file_name()))?;
        }
        fs::set_permissions(target, metadata.permissions()).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        return Ok(());
    }
    Err(Error::Message(format!(
        "unsupported backup target {}",
        source.display()
    )))
}

fn create_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Message("path has no parent".into()))?;
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
