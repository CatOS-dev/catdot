use crate::{Error, Result, UserState, atomic_write, read_state, write_state};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum Snapshot {
    Missing,
    File {
        contents: Vec<u8>,
        mode: u32,
    },
    Symlink {
        target: PathBuf,
    },
    Directory {
        mode: u32,
        children: Vec<(Vec<u8>, Snapshot)>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalFile {
    path: PathBuf,
    previous: Snapshot,
    expected: Snapshot,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum JournalStage {
    Prepared,
    Applying,
    StateWritten,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationJournal {
    id: String,
    created_at: u64,
    state_path: PathBuf,
    old_active_state: UserState,
    new_active_state: UserState,
    files: Vec<JournalFile>,
    stage: JournalStage,
    #[serde(skip)]
    path: PathBuf,
}
pub fn activation_transactions_path(state_path: &Path) -> Result<PathBuf> {
    Ok(state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?
        .join("transactions"))
}
impl ActivationJournal {
    pub fn begin(
        state_path: &Path,
        old_active_state: UserState,
        new_active_state: UserState,
    ) -> Result<Self> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Message(e.to_string()))?
            .as_nanos() as u64;
        let id = format!("{created_at}-{}", std::process::id());
        let path = activation_transactions_path(state_path)?.join(format!("{id}.toml"));
        let journal = Self {
            id,
            created_at,
            state_path: state_path.to_owned(),
            old_active_state,
            new_active_state,
            files: vec![],
            stage: JournalStage::Prepared,
            path,
        };
        journal.persist()?;
        Ok(journal)
    }
    pub fn track_path(&mut self, path: &Path) -> Result<()> {
        self.track(path, snapshot(path)?)
    }
    pub fn track_file(&mut self, path: &Path, expected: &[u8]) -> Result<()> {
        self.track(
            path,
            Snapshot::File {
                contents: expected.to_vec(),
                mode: 0o644,
            },
        )
    }
    pub fn track_symlink(&mut self, path: &Path, expected: &Path) -> Result<()> {
        self.track(
            path,
            Snapshot::Symlink {
                target: expected.to_owned(),
            },
        )
    }
    pub fn track_removal(&mut self, path: &Path) -> Result<()> {
        self.track(path, Snapshot::Missing)
    }
    pub fn mark_applying(&mut self) -> Result<()> {
        self.stage = JournalStage::Applying;
        self.persist()
    }
    pub fn mark_state_written(&mut self) -> Result<()> {
        self.stage = JournalStage::StateWritten;
        self.persist()
    }
    pub fn mark_applied(&mut self) -> Result<()> {
        for file in &mut self.files {
            file.expected = snapshot(&file.path)?;
        }
        self.persist()
    }
    pub fn complete(self) -> Result<()> {
        self.archive_backup()?;
        fs::remove_file(&self.path).map_err(|source| Error::Io {
            path: self.path.display().to_string(),
            source,
        })
    }
    /// Explicitly undo an activation that failed before its state commit.
    /// Recovery on the next invocation treats a written state as committed, so
    /// callers that still own the failed operation must request rollback directly.
    pub fn rollback(self) -> Result<()> {
        let current = read_state(&self.state_path)?;
        if current != self.old_active_state && current != self.new_active_state {
            return Err(Error::Message(
                "activation state was changed outside the journal".into(),
            ));
        }
        for file in &self.files {
            let current = snapshot(&file.path)?;
            if current != file.previous && current != file.expected {
                return Err(Error::Message(format!(
                    "refusing to rollback changed activation target {}",
                    file.path.display()
                )));
            }
        }
        for file in self.files.iter().rev() {
            restore(&file.path, &file.previous)?;
        }
        write_state(&self.state_path, &self.old_active_state)?;
        self.complete()
    }
    fn archive_backup(&self) -> Result<()> {
        let catdot = self
            .state_path
            .parent()
            .ok_or_else(|| Error::Message("state path has no parent".into()))?;
        let home = catdot.ancestors().nth(3).unwrap_or(catdot);
        let backup = catdot.join("backups").join(&self.id);
        let backup_home = backup.join("home");
        for file in &self.files {
            if let Ok(relative) = file.path.strip_prefix(home) {
                restore(&backup_home.join(relative), &file.previous)?;
            }
        }
        atomic_write(
            &backup.join("metadata.toml"),
            &toml::to_string_pretty(self).map_err(|e| Error::Message(e.to_string()))?,
        )?;
        let mut generations: Vec<_> = fs::read_dir(catdot.join("backups"))
            .map_err(|source| Error::Io {
                path: backup.display().to_string(),
                source,
            })?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                let metadata = fs::read_to_string(path.join("metadata.toml")).ok()?;
                let journal: ActivationJournal = toml::from_str(&metadata).ok()?;
                Some((journal.created_at, entry))
            })
            .collect();
        generations.sort_by_key(|(created_at, _)| *created_at);
        for (_, entry) in generations.into_iter().rev().skip(5) {
            fs::remove_dir_all(entry.path()).map_err(|source| Error::Io {
                path: entry.path().display().to_string(),
                source,
            })?;
        }
        Ok(())
    }
    fn track(&mut self, path: &Path, expected: Snapshot) -> Result<()> {
        if self.files.iter().any(|f| f.path == path) {
            return Ok(());
        };
        self.files.push(JournalFile {
            path: path.to_owned(),
            previous: snapshot(path)?,
            expected,
        });
        self.persist()
    }
    fn persist(&self) -> Result<()> {
        atomic_write(
            &self.path,
            &toml::to_string_pretty(self).map_err(|e| Error::Message(e.to_string()))?,
        )
    }
}
pub fn recover_activation_journals(state_path: &Path) -> Result<()> {
    let directory = activation_transactions_path(state_path)?;
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&directory).map_err(|source| Error::Io {
        path: directory.display().to_string(),
        source,
    })? {
        let path = entry
            .map_err(|source| Error::Io {
                path: directory.display().to_string(),
                source,
            })?
            .path();
        if path.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let mut journal: ActivationJournal =
            toml::from_str(&fs::read_to_string(&path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?)
            .map_err(|source| Error::Toml {
                path: path.display().to_string(),
                source,
            })?;
        if journal.state_path != state_path {
            return Err(Error::Message(
                "activation journal belongs to another state file".into(),
            ));
        };
        journal.path = path;
        recover(&journal)?;
    }
    Ok(())
}
fn recover(j: &ActivationJournal) -> Result<()> {
    let current = read_state(&j.state_path)?;
    if j.stage == JournalStage::StateWritten && current == j.new_active_state {
        return j.clone().complete();
    }
    if current != j.old_active_state && current != j.new_active_state {
        return Err(Error::Message(
            "activation state was changed outside the journal".into(),
        ));
    }
    for file in &j.files {
        let current = snapshot(&file.path)?;
        if current != file.previous && current != file.expected {
            return Err(Error::Message(format!(
                "refusing to recover changed activation target {}",
                file.path.display()
            )));
        }
    }
    for file in j.files.iter().rev() {
        restore(&file.path, &file.previous)?
    }
    write_state(&j.state_path, &j.old_active_state)?;
    j.clone().complete()
}
fn snapshot(path: &Path) -> Result<Snapshot> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Ok(Snapshot::Symlink {
            target: fs::read_link(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?,
        }),
        Ok(m) if m.is_file() => Ok(Snapshot::File {
            contents: fs::read(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?,
            mode: m.permissions().mode(),
        }),
        Ok(m) if m.is_dir() => {
            let mut children = vec![];
            for e in fs::read_dir(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })? {
                let e = e.map_err(|source| Error::Io {
                    path: path.display().to_string(),
                    source,
                })?;
                children.push((e.file_name().as_bytes().to_vec(), snapshot(&e.path())?));
            }
            Ok(Snapshot::Directory {
                mode: m.permissions().mode(),
                children,
            })
        }
        Ok(_) => Err(Error::Message(format!(
            "unsupported activation target {}",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Snapshot::Missing),
        Err(source) => Err(Error::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}
fn restore(path: &Path, s: &Snapshot) -> Result<()> {
    remove(path)?;
    match s {
        Snapshot::Missing => Ok(()),
        Snapshot::File { contents, mode } => {
            if let Some(p) = path.parent() {
                fs::create_dir_all(p).map_err(|source| Error::Io {
                    path: p.display().to_string(),
                    source,
                })?
            };
            fs::write(path, contents).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?;
            fs::set_permissions(path, fs::Permissions::from_mode(*mode)).map_err(|source| {
                Error::Io {
                    path: path.display().to_string(),
                    source,
                }
            })
        }
        Snapshot::Symlink { target } => {
            if let Some(p) = path.parent() {
                fs::create_dir_all(p).map_err(|source| Error::Io {
                    path: p.display().to_string(),
                    source,
                })?
            };
            symlink(target, path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })
        }
        Snapshot::Directory { mode, children } => {
            fs::create_dir_all(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?;
            for (name, child) in children {
                restore(&path.join(OsString::from_vec(name.clone())), child)?
            }
            fs::set_permissions(path, fs::Permissions::from_mode(*mode)).map_err(|source| {
                Error::Io {
                    path: path.display().to_string(),
                    source,
                }
            })
        }
    }
}
fn remove(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })
        }
        Ok(_) => fs::remove_file(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}
