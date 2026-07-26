use crate::{Error, Profile, Result, error::io};
use serde::{Deserialize, Serialize, de::Error as _};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const USER_STATE_SCHEMA: u32 = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileState {
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub packages: BTreeSet<String>,
    #[serde(default)]
    pub managed: BTreeSet<PathBuf>,
    #[serde(default)]
    pub seeds: BTreeSet<PathBuf>,
    #[serde(default)]
    pub seeded: BTreeSet<PathBuf>,
}

impl ProfileState {
    fn from_profile(profile: &Profile) -> Result<Self> {
        Ok(Self {
            initialized: false,
            packages: profile.packages.iter().cloned().collect(),
            managed: profile.manage.clone(),
            seeds: profile.seed_files()?.into_iter().collect(),
            seeded: BTreeSet::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserState {
    pub schema: u32,
    pub generation: u64,
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileState>,
}

impl Default for UserState {
    fn default() -> Self {
        Self {
            schema: USER_STATE_SCHEMA,
            generation: 0,
            active_profile: None,
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

pub fn validate_user_state(state: &UserState, profiles: &BTreeMap<String, Profile>) -> Result<()> {
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
    for (id, profile_state) in &state.profiles {
        if !valid_id(id) {
            return Err(Error::Message(format!("invalid saved profile {id}")));
        }
        if !profiles.contains_key(id) {
            return Err(Error::Message(format!(
                "profile {id} is no longer installed"
            )));
        }
        for path in profile_state
            .managed
            .iter()
            .chain(&profile_state.seeds)
            .chain(&profile_state.seeded)
        {
            if !safe_relative(path) {
                return Err(Error::Message(format!(
                    "invalid saved path {} for profile {id}",
                    path.display()
                )));
            }
        }
        if !profile_state.seeded.is_subset(&profile_state.seeds) {
            return Err(Error::Message(format!(
                "profile {id} records seeded paths outside its seed declaration"
            )));
        }
        if profile_state.seeds.iter().any(|seed| {
            profile_state
                .managed
                .iter()
                .any(|managed| seed.starts_with(managed))
        }) {
            return Err(Error::Message(format!(
                "profile {id} records a seed below a managed path"
            )));
        }
    }
    Ok(())
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

pub fn retain_profile(state: &mut UserState, profile: &Profile) -> Result<bool> {
    if state.profiles.contains_key(&profile.id) {
        return Ok(false);
    }
    state
        .profiles
        .insert(profile.id.clone(), ProfileState::from_profile(profile)?);
    state.generation += 1;
    Ok(true)
}

pub fn prepare_profile_state(
    state: &mut UserState,
    profile: &Profile,
    mode: crate::ActivationMode,
) -> Result<()> {
    let current = state
        .profiles
        .get_mut(&profile.id)
        .ok_or_else(|| Error::Message(format!("profile {} is not retained", profile.id)))?;
    if !current.initialized
        || matches!(
            mode,
            crate::ActivationMode::Update | crate::ActivationMode::Reset
        )
    {
        current.packages = profile.packages.iter().cloned().collect();
        current.managed = profile.manage.clone();
        current.seeds.retain(|path| !profile.is_managed(path));
        current.seeded.retain(|path| current.seeds.contains(path));
    }
    if !current.initialized || mode == crate::ActivationMode::Reset {
        current.seeds = profile.seed_files()?.into_iter().collect();
        current.seeded.retain(|path| current.seeds.contains(path));
    }
    Ok(())
}

pub fn remove_profile(state: &mut UserState, profile: &str) -> Result<bool> {
    if state.active_profile.as_deref() == Some(profile) {
        return Err(Error::Message(format!(
            "cannot remove active profile {profile}"
        )));
    }
    let removed = state.profiles.remove(profile).is_some();
    if removed {
        state.generation += 1;
    }
    Ok(removed)
}

pub fn read_system_packages(path: &Path) -> Result<crate::SystemPackageState> {
    if !path.exists() {
        return Ok(crate::SystemPackageState::default());
    }
    toml::from_str(&io(path, fs::read_to_string(path))?).map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}
