use crate::{Error, Profile, Result, error::io};
use serde::{Deserialize, Serialize, de::Error as _};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const USER_STATE_SCHEMA: u32 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileState {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub packages: BTreeSet<String>,
    #[serde(default)]
    pub manage: BTreeSet<PathBuf>,
}

impl ProfileState {
    pub fn from_profile(profile: &Profile) -> Self {
        Self {
            name: profile.name.clone(),
            description: profile.description.clone(),
            packages: profile.packages.iter().cloned().collect(),
            manage: profile.manage.clone(),
        }
    }

    pub fn matches_profile(&self, profile: &Profile) -> bool {
        self.name == profile.name
            && self.description == profile.description
            && self.packages == profile.packages.iter().cloned().collect()
            && self.manage == profile.manage
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserState {
    pub schema: u32,
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub introduced_packages: BTreeSet<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileState>,
}

impl Default for UserState {
    fn default() -> Self {
        Self {
            schema: USER_STATE_SCHEMA,
            active_profile: None,
            introduced_packages: BTreeSet::new(),
            profiles: BTreeMap::new(),
        }
    }
}

pub fn state_path(home: &Path) -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                home.join(path)
            }
        })
        .unwrap_or_else(|| home.join(".local/state"))
        .join("catdot/state.toml")
}

pub fn state_lock_path(state_path: &Path) -> Result<PathBuf> {
    let parent = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    Ok(parent.join("lock"))
}

pub fn read_state(path: &Path) -> Result<UserState> {
    if !path.exists() {
        return Ok(UserState::default());
    }
    parse_state_text(&io(path, fs::read_to_string(path))?).map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}

pub fn parse_state_text(text: &str) -> std::result::Result<UserState, toml::de::Error> {
    let state: UserState = toml::from_str(text)?;
    if state.schema != USER_STATE_SCHEMA {
        return Err(toml::de::Error::custom(format!(
            "unsupported state schema {}; expected {USER_STATE_SCHEMA}",
            state.schema
        )));
    }
    Ok(state)
}

pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Message("path has no parent".into()))?;
    io(parent, fs::create_dir_all(parent))?;
    let (temporary, mut file) = create_temporary(parent, path)?;
    io(&temporary, file.write_all(contents.as_bytes()))?;
    io(&temporary, file.sync_all())?;
    io(path, fs::rename(&temporary, path))?;
    io(parent, File::open(parent))?
        .sync_all()
        .map_err(|source| Error::Io {
            path: parent.display().to_string(),
            source,
        })
}

fn create_temporary(parent: &Path, path: &Path) -> Result<(PathBuf, File)> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Message(error.to_string()))?
        .as_nanos();
    for attempt in 0..32 {
        let candidate = parent.join(format!(
            ".{name}.{}-{stamp}-{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(Error::Io {
                    path: candidate.display().to_string(),
                    source: error,
                });
            }
        }
    }
    Err(Error::Message(format!(
        "cannot create unique temporary file for {}",
        path.display()
    )))
}

pub fn write_state(path: &Path, state: &UserState) -> Result<()> {
    atomic_write(
        path,
        &toml::to_string_pretty(state).map_err(|error| Error::Message(error.to_string()))?,
    )
}

pub fn validate_user_state(state: &UserState) -> Result<()> {
    if state.schema != USER_STATE_SCHEMA {
        return Err(Error::Message("invalid user state schema".into()));
    }
    if let Some(active) = &state.active_profile
        && !state.profiles.contains_key(active)
    {
        return Err(Error::Message(format!(
            "active profile {active} is not retained"
        )));
    }
    for package in &state.introduced_packages {
        validate_package(package)?;
    }
    for (id, profile) in &state.profiles {
        if !valid_id(id) {
            return Err(Error::Message(format!("invalid saved profile {id}")));
        }
        if profile.name.trim().is_empty() {
            return Err(Error::Message(format!(
                "saved profile {id} has an empty name"
            )));
        }
        for package in &profile.packages {
            validate_package(package)?;
        }
        for path in &profile.manage {
            if !safe_relative(path) {
                return Err(Error::Message(format!(
                    "invalid managed path {} for profile {id}",
                    path.display()
                )));
            }
        }
        for path in &profile.manage {
            if profile
                .manage
                .iter()
                .any(|other| path != other && (path.starts_with(other) || other.starts_with(path)))
            {
                return Err(Error::Message(format!(
                    "overlapping managed path {} for profile {id}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

pub fn remove_profile(state: &mut UserState, profile: &str) -> Result<bool> {
    if state.active_profile.as_deref() == Some(profile) {
        return Err(Error::Message(format!(
            "cannot remove active profile {profile}"
        )));
    }
    Ok(state.profiles.remove(profile).is_some())
}

pub fn retained_packages(state: &UserState) -> BTreeSet<String> {
    state
        .profiles
        .values()
        .flat_map(|profile| profile.packages.iter().cloned())
        .collect()
}

pub fn prune_candidates(state: &UserState) -> BTreeSet<String> {
    let retained = retained_packages(state);
    state
        .introduced_packages
        .difference(&retained)
        .cloned()
        .collect()
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.is_relative()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn validate_package(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || !value.is_ascii()
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'_' | b'+' | b'.' | b'-')
        })
    {
        return Err(Error::Message(format!("invalid package name {value}")));
    }
    Ok(())
}
