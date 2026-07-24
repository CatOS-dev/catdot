use crate::{Error, Result, UserState, atomic_write, read_state, write_state};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum Snapshot {
    Missing,
    File { contents: Vec<u8> },
    Symlink { target: PathBuf },
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
    state_path: PathBuf,
    old_active_state: UserState,
    new_active_state: UserState,
    files: Vec<JournalFile>,
    stage: JournalStage,
    #[serde(skip)]
    path: PathBuf,
}

pub fn activation_transactions_path(state_path: &Path) -> Result<PathBuf> {
    let parent = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    Ok(parent.join("transactions"))
}

impl ActivationJournal {
    pub fn begin(
        state_path: &Path,
        old_active_state: UserState,
        new_active_state: UserState,
    ) -> Result<Self> {
        let directory = activation_transactions_path(state_path)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::Message(error.to_string()))?
            .as_nanos();
        let id = format!("{}-{stamp}", std::process::id());
        let path = directory.join(format!("{id}.toml"));
        let journal = Self {
            id,
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

    pub fn track_file(&mut self, path: &Path, expected_contents: &[u8]) -> Result<()> {
        self.track(
            path,
            Snapshot::File {
                contents: expected_contents.to_vec(),
            },
        )
    }

    pub fn track_symlink(&mut self, path: &Path, expected_target: &Path) -> Result<()> {
        self.track(
            path,
            Snapshot::Symlink {
                target: expected_target.to_owned(),
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

    pub fn complete(self) -> Result<()> {
        fs::remove_file(&self.path).map_err(|source| Error::Io {
            path: self.path.display().to_string(),
            source,
        })?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Error::Message("activation journal has no parent".into()))?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| Error::Io {
                path: parent.display().to_string(),
                source,
            })
    }

    fn track(&mut self, path: &Path, expected: Snapshot) -> Result<()> {
        if self.files.iter().any(|file| file.path == path) {
            return Ok(());
        }
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
            &toml::to_string_pretty(self).map_err(|error| Error::Message(error.to_string()))?,
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
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }
        let contents = fs::read_to_string(&path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        let mut journal: ActivationJournal =
            toml::from_str(&contents).map_err(|source| Error::Toml {
                path: path.display().to_string(),
                source,
            })?;
        if journal.state_path != state_path {
            return Err(Error::Message(format!(
                "activation journal {} belongs to another state file",
                path.display()
            )));
        }
        journal.path = path;
        recover(&journal)?;
    }
    Ok(())
}

fn recover(journal: &ActivationJournal) -> Result<()> {
    let current = read_state(&journal.state_path)?;
    if current != journal.old_active_state && current != journal.new_active_state {
        return Err(Error::Message(
            "activation state was changed outside the journal".into(),
        ));
    }
    if journal.stage == JournalStage::StateWritten && current == journal.new_active_state {
        return journal.clone().complete();
    }
    for file in &journal.files {
        let current = snapshot(&file.path)?;
        if current != file.previous && current != file.expected {
            return Err(Error::Message(format!(
                "refusing to recover manually changed activation target {}",
                file.path.display()
            )));
        }
    }
    for file in journal.files.iter().rev() {
        restore(&file.path, &file.previous)?;
    }
    write_state(&journal.state_path, &journal.old_active_state)?;
    journal.clone().complete()
}

fn snapshot(path: &Path) -> Result<Snapshot> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(Snapshot::Symlink {
            target: fs::read_link(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?,
        }),
        Ok(metadata) if metadata.is_file() => Ok(Snapshot::File {
            contents: fs::read(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?,
        }),
        Ok(_) => Err(Error::Message(format!(
            "unsupported activation target {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Snapshot::Missing),
        Err(source) => Err(Error::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn restore(path: &Path, snapshot: &Snapshot) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        fs::remove_file(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
    }
    match snapshot {
        Snapshot::Missing => Ok(()),
        Snapshot::File { contents } => atomic_write(path, &String::from_utf8_lossy(contents)),
        Snapshot::Symlink { target } => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| Error::Io {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
            std::os::unix::fs::symlink(target, path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })
        }
    }
}
