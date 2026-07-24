use crate::{Error, Result, error::io, manifest::Profile};
use serde::{Deserialize, Serialize, de::Error as _};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserState {
    pub schema: u32,
    pub generation: u64,
    pub components: BTreeMap<String, String>,
    pub active_generation: u64,
    pub active_components: BTreeMap<String, String>,
}
impl Default for UserState {
    fn default() -> Self {
        Self {
            schema: 1,
            generation: 0,
            components: BTreeMap::new(),
            active_generation: 0,
            active_components: BTreeMap::new(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUserState {
    #[serde(default)]
    schema: Option<u32>,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    components: BTreeMap<String, String>,
    #[serde(default)]
    active_generation: u64,
    #[serde(default)]
    active_components: BTreeMap<String, String>,
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
pub fn managed_links_path(state_path: &Path) -> Result<PathBuf> {
    let parent = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    Ok(parent.join("managed-links.toml"))
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
    };
    parse_state_text(&io(path, fs::read_to_string(path))?).map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}
pub fn parse_state_text(text: &str) -> std::result::Result<UserState, toml::de::Error> {
    let raw: RawUserState = toml::from_str(text)?;
    match raw.schema {
        None => Ok(UserState {
            schema: 1,
            generation: raw.generation,
            components: raw.components.clone(),
            active_generation: raw.generation,
            active_components: raw.components,
        }),
        Some(1) => Ok(UserState {
            schema: 1,
            generation: raw.generation,
            components: raw.components,
            active_generation: raw.active_generation,
            active_components: raw.active_components,
        }),
        Some(schema) => Err(toml::de::Error::custom(format!(
            "unsupported state schema {schema}"
        ))),
    }
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
    if state.schema != 1 || state.active_generation > state.generation {
        return Err(Error::Message("invalid state generation".into()));
    }
    validate_components(&state.components, profiles)?;
    validate_components(&state.active_components, profiles)
}

fn validate_components(
    components: &BTreeMap<String, String>,
    profiles: &BTreeMap<String, Profile>,
) -> Result<()> {
    for (role, reference) in components {
        if !valid_state_id(role) {
            return Err(Error::Message(format!("invalid saved role {role}")));
        }
        let mut parts = reference.split('/');
        let (Some(profile_id), Some(component_id), None) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Message(format!(
                "invalid saved component reference {reference}"
            )));
        };
        if !valid_state_id(profile_id) || !valid_state_id(component_id) {
            return Err(Error::Message(format!(
                "invalid saved component reference {reference}"
            )));
        }
        let profile = profiles.get(profile_id).ok_or_else(|| {
            Error::Message(format!("profile {profile_id} is no longer installed"))
        })?;
        let component = profile.components.get(component_id).ok_or_else(|| {
            Error::Message(format!("component {reference} is no longer installed"))
        })?;
        if component.role != *role {
            return Err(Error::Message(format!(
                "{reference} has role {}, not {role}",
                component.role
            )));
        }
    }
    Ok(())
}

fn valid_state_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
pub fn read_user_records(directory: &Path) -> Result<Vec<crate::UserRecord>> {
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut records = Vec::new();
    for entry in io(directory, fs::read_dir(directory))? {
        let path = io(directory, entry)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }
        let text = io(&path, fs::read_to_string(&path))?;
        records.push(toml::from_str(&text).map_err(|source| Error::Toml {
            path: path.display().to_string(),
            source,
        })?);
    }
    Ok(records)
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
pub fn write_system_packages(path: &Path, state: &crate::SystemPackageState) -> Result<()> {
    atomic_write(
        path,
        &toml::to_string_pretty(state).map_err(|error| Error::Message(error.to_string()))?,
    )
}
pub fn select_profile(profile: &Profile) -> Result<UserState> {
    let components = profile
        .defaults
        .iter()
        .map(|(role, id)| (role.clone(), format!("{}/{}", profile.id, id)))
        .collect();
    Ok(UserState {
        schema: 1,
        generation: 1,
        components,
        active_generation: 0,
        active_components: BTreeMap::new(),
    })
}
pub fn select_component(
    state: &mut UserState,
    profiles: &BTreeMap<String, Profile>,
    role: &str,
    reference: &str,
) -> Result<()> {
    let (profile_id, component_id) = reference
        .split_once('/')
        .ok_or_else(|| Error::Message("component must be profile/component".into()))?;
    let component = profiles
        .get(profile_id)
        .and_then(|profile| profile.components.get(component_id))
        .ok_or_else(|| Error::Message(format!("unknown component {reference}")))?;
    if component.role != role {
        return Err(Error::Message(format!(
            "{reference} has role {}, not {role}",
            component.role
        )));
    }
    state.components.insert(role.into(), reference.into());
    state.generation += 1;
    Ok(())
}
