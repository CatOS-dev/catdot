use crate::{Error, Result, atomic_write, error::io};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkRegistry {
    pub entries: BTreeMap<String, String>,
}

pub fn read_link_registry(path: &Path) -> Result<LinkRegistry> {
    if !path.exists() {
        return Ok(LinkRegistry::default());
    }
    toml::from_str(&io(path, fs::read_to_string(path))?).map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}

pub fn activate_managed_link(
    registry_path: &Path,
    source: &Path,
    target: &Path,
    adopt: bool,
) -> Result<()> {
    let mut transaction = LinkTransaction::new(registry_path)?;
    if let Err(error) = transaction.stage(source, target, adopt) {
        let _ = transaction.rollback();
        return Err(error);
    }
    if let Err(error) = transaction.commit() {
        let _ = transaction.rollback();
        return Err(error);
    }
    Ok(())
}

pub struct LinkTransaction {
    registry_path: PathBuf,
    registry: LinkRegistry,
    changes: Vec<LinkChange>,
}

struct LinkChange {
    target: PathBuf,
    source: PathBuf,
    backup: Option<PathBuf>,
    keep_backup: bool,
}

impl LinkTransaction {
    pub fn new(registry_path: &Path) -> Result<Self> {
        Ok(Self {
            registry_path: registry_path.to_owned(),
            registry: read_link_registry(registry_path)?,
            changes: vec![],
        })
    }

    pub fn stage(&mut self, source: &Path, target: &Path, adopt: bool) -> Result<()> {
        if !source.is_absolute() || !source.is_file() {
            return Err(Error::Message(format!(
                "link source must be an existing absolute file: {}",
                source.display()
            )));
        }
        if !target.is_absolute() {
            return Err(Error::Message(format!(
                "link target must be absolute: {}",
                target.display()
            )));
        }
        if self.changes.iter().any(|change| change.target == target) {
            return Err(Error::Message(format!(
                "duplicate Catdot link target: {}",
                target.display()
            )));
        }
        let source = io(source, source.canonicalize())?;
        let key = target.display().to_string();
        let source_text = source.display().to_string();
        let owned = self
            .registry
            .entries
            .get(&key)
            .is_some_and(|saved| saved == &source_text);
        let mut backup = None;
        let mut keep_backup = false;

        if let Some(parent) = target.parent() {
            io(parent, fs::create_dir_all(parent))?;
        }

        if let Ok(metadata) = fs::symlink_metadata(target) {
            let matches_saved = metadata.file_type().is_symlink()
                && fs::read_link(target).ok().as_deref() == Some(source.as_path());
            if !(adopt || owned && matches_saved) {
                return Err(Error::Message(format!(
                    "refusing to replace unmanaged {}: use catdot adopt <role>",
                    target.display()
                )));
            }
            let path = backup_path(target)?;
            io(target, fs::rename(target, &path))?;
            backup = Some(path);
            keep_backup = adopt;
        }
        if let Err(source_error) = std::os::unix::fs::symlink(&source, target) {
            if let Some(backup) = &backup {
                let _ = fs::rename(backup, target);
            }
            return Err(Error::Io {
                path: target.display().to_string(),
                source: source_error,
            });
        }
        self.registry.entries.insert(key, source_text);
        self.changes.push(LinkChange {
            target: target.to_owned(),
            source,
            backup,
            keep_backup,
        });
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        atomic_write(
            &self.registry_path,
            &toml::to_string_pretty(&self.registry)
                .map_err(|error| Error::Message(error.to_string()))?,
        )?;
        for change in &self.changes {
            if !change.keep_backup
                && let Some(backup) = &change.backup
            {
                let _ = fs::remove_file(backup);
            }
        }
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        for change in self.changes.iter().rev() {
            let metadata = io(&change.target, fs::symlink_metadata(&change.target))?;
            if !metadata.file_type().is_symlink()
                || fs::read_link(&change.target).ok().as_deref() != Some(change.source.as_path())
            {
                return Err(Error::Message(format!(
                    "cannot roll back changed link {}",
                    change.target.display()
                )));
            }
            io(&change.target, fs::remove_file(&change.target))?;
            if let Some(backup) = &change.backup {
                io(&change.target, fs::rename(backup, &change.target))?;
            }
        }
        self.changes.clear();
        Ok(())
    }
}

pub fn deactivate_managed_link(registry_path: &Path, source: &Path, target: &Path) -> Result<()> {
    let source = io(source, source.canonicalize())?;
    let key = target.display().to_string();
    let mut registry = read_link_registry(registry_path)?;
    let source_text = source.display().to_string();
    if registry.entries.get(&key) != Some(&source_text) {
        return Err(Error::Message(format!(
            "refusing to remove unregistered {}",
            target.display()
        )));
    }
    let metadata = io(target, fs::symlink_metadata(target))?;
    if !metadata.file_type().is_symlink()
        || fs::read_link(target).ok().as_deref() != Some(source.as_path())
    {
        return Err(Error::Message(format!(
            "refusing to remove changed managed link {}",
            target.display()
        )));
    }
    io(target, fs::remove_file(target))?;
    registry.entries.remove(&key);
    atomic_write(
        registry_path,
        &toml::to_string_pretty(&registry).map_err(|error| Error::Message(error.to_string()))?,
    )
}

fn backup_path(target: &Path) -> Result<std::path::PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Message(error.to_string()))?
        .as_nanos();
    let name = target
        .file_name()
        .ok_or_else(|| Error::Message(format!("target has no filename: {}", target.display())))?
        .to_string_lossy();
    Ok(target.with_file_name(format!(
        "{name}.catdot-backup-{stamp}-{}",
        std::process::id()
    )))
}
