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
    transaction.stage(source, target, adopt)?;
    transaction.commit()
}

pub fn reconcile_managed_links(
    registry_path: &Path,
    desired: &[(PathBuf, PathBuf)],
    adopted_targets: &[PathBuf],
) -> Result<()> {
    let mut transaction = LinkTransaction::new(registry_path)?;
    transaction.reconcile();
    for (target, source) in desired {
        transaction.stage(source, target, adopted_targets.contains(target))?;
    }
    transaction.commit()
}

pub struct LinkTransaction {
    registry_path: PathBuf,
    registry: LinkRegistry,
    desired: BTreeMap<String, DesiredLink>,
    changes: Vec<LinkChange>,
    reconcile: bool,
}

struct DesiredLink {
    target: PathBuf,
    source: PathBuf,
    adopt: bool,
}

enum LinkAction {
    Create(DesiredLink),
    Replace(DesiredLink),
    Remove { target: PathBuf },
    Keep,
}

struct LinkChange {
    target: PathBuf,
    source: Option<PathBuf>,
    backup: Option<PathBuf>,
}

impl LinkTransaction {
    pub fn new(registry_path: &Path) -> Result<Self> {
        Ok(Self {
            registry_path: registry_path.to_owned(),
            registry: read_link_registry(registry_path)?,
            desired: BTreeMap::new(),
            changes: vec![],
            reconcile: false,
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
        let key = target.display().to_string();
        if self.desired.contains_key(&key) {
            return Err(Error::Message(format!(
                "duplicate Catdot link target: {}",
                target.display()
            )));
        }
        self.desired.insert(
            key,
            DesiredLink {
                target: target.to_owned(),
                source: io(source, source.canonicalize())?,
                adopt,
            },
        );
        Ok(())
    }

    pub fn reconcile(&mut self) {
        self.reconcile = true;
    }

    pub fn commit(&mut self) -> Result<()> {
        let actions = self.plan()?;
        let mut new_registry = self.registry.clone();
        if self.reconcile {
            new_registry.entries.clear();
        }
        new_registry.entries.extend(
            self.desired
                .iter()
                .map(|(target, desired)| (target.clone(), desired.source.display().to_string())),
        );
        for action in actions {
            if let Err(error) = self.apply(action) {
                let _ = self.rollback();
                return Err(error);
            }
        }
        if let Err(error) = atomic_write(
            &self.registry_path,
            &toml::to_string_pretty(&new_registry)
                .map_err(|error| Error::Message(error.to_string()))?,
        ) {
            let _ = self.rollback();
            return Err(error);
        }
        self.registry = new_registry;
        for change in &self.changes {
            if let Some(backup) = &change.backup {
                io(backup, fs::remove_file(backup))?;
            }
        }
        self.changes.clear();
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        for change in self.changes.iter().rev() {
            if let Some(source) = &change.source {
                let metadata = io(&change.target, fs::symlink_metadata(&change.target))?;
                if !metadata.file_type().is_symlink()
                    || fs::read_link(&change.target).ok().as_deref() != Some(source.as_path())
                {
                    return Err(Error::Message(format!(
                        "cannot roll back changed link {}",
                        change.target.display()
                    )));
                }
                io(&change.target, fs::remove_file(&change.target))?;
            } else if change.target.exists() {
                return Err(Error::Message(format!(
                    "cannot roll back changed link {}",
                    change.target.display()
                )));
            }
            if let Some(backup) = &change.backup {
                io(&change.target, fs::rename(backup, &change.target))?;
            }
        }
        self.changes.clear();
        Ok(())
    }

    fn plan(&self) -> Result<Vec<LinkAction>> {
        let mut actions = Vec::new();
        for (key, desired) in &self.desired {
            match self.registry.entries.get(key) {
                Some(saved) if link_matches(&desired.target, saved) => {
                    if saved == &desired.source.display().to_string() {
                        actions.push(LinkAction::Keep);
                    } else {
                        actions.push(LinkAction::Replace(DesiredLink {
                            target: desired.target.clone(),
                            source: desired.source.clone(),
                            adopt: desired.adopt,
                        }));
                    }
                }
                Some(_) => return changed_link_error(&desired.target),
                None if fs::symlink_metadata(&desired.target).is_ok() && !desired.adopt => {
                    return Err(Error::Message(format!(
                        "refusing to replace unmanaged {}: use catdot adopt <role>",
                        desired.target.display()
                    )));
                }
                None if fs::symlink_metadata(&desired.target).is_ok() => {
                    actions.push(LinkAction::Replace(DesiredLink {
                        target: desired.target.clone(),
                        source: desired.source.clone(),
                        adopt: true,
                    }));
                }
                None => actions.push(LinkAction::Create(DesiredLink {
                    target: desired.target.clone(),
                    source: desired.source.clone(),
                    adopt: desired.adopt,
                })),
            }
        }
        for (target, saved) in &self.registry.entries {
            if self.reconcile && !self.desired.contains_key(target) {
                let target = PathBuf::from(target);
                if !link_matches(&target, saved) {
                    return changed_link_error(&target);
                }
                actions.push(LinkAction::Remove { target });
            }
        }
        Ok(actions)
    }

    fn apply(&mut self, action: LinkAction) -> Result<()> {
        match action {
            LinkAction::Keep => Ok(()),
            LinkAction::Create(desired) => self.create(desired, None),
            LinkAction::Replace(desired) => {
                let backup = backup_path(&desired.target)?;
                io(&desired.target, fs::rename(&desired.target, &backup))?;
                self.create(desired, Some(backup))
            }
            LinkAction::Remove { target } => {
                let backup = backup_path(&target)?;
                io(&target, fs::rename(&target, &backup))?;
                self.changes.push(LinkChange {
                    target,
                    source: None,
                    backup: Some(backup),
                });
                Ok(())
            }
        }
    }

    fn create(&mut self, desired: DesiredLink, backup: Option<PathBuf>) -> Result<()> {
        if let Some(parent) = desired.target.parent() {
            io(parent, fs::create_dir_all(parent))?;
        }
        if let Err(source_error) = std::os::unix::fs::symlink(&desired.source, &desired.target) {
            if let Some(backup) = &backup {
                let _ = fs::rename(backup, &desired.target);
            }
            return Err(Error::Io {
                path: desired.target.display().to_string(),
                source: source_error,
            });
        }
        self.changes.push(LinkChange {
            target: desired.target,
            source: Some(desired.source),
            backup,
        });
        Ok(())
    }
}

fn link_matches(target: &Path, expected: &str) -> bool {
    fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink())
        && fs::read_link(target)
            .ok()
            .is_some_and(|actual| actual == Path::new(expected))
}

fn changed_link_error(target: &Path) -> Result<Vec<LinkAction>> {
    Err(Error::Message(format!(
        "refusing to replace changed managed link {}",
        target.display()
    )))
}

pub fn deactivate_managed_link(registry_path: &Path, source: &Path, target: &Path) -> Result<()> {
    let source = io(source, source.canonicalize())?;
    let key = target.display().to_string();
    let registry = read_link_registry(registry_path)?;
    if registry.entries.get(&key) != Some(&source.display().to_string()) {
        return Err(Error::Message(format!(
            "refusing to remove unregistered {}",
            target.display()
        )));
    }
    reconcile_managed_links(registry_path, &[], &[])
}

fn backup_path(target: &Path) -> Result<PathBuf> {
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
