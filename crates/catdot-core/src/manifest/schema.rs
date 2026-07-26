use crate::{Error, Result, error::io};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

pub const DEFAULT_PROFILE_ROOT: &str = "/usr/share/catdot/profiles";
pub const PROFILE_SCHEMA: u32 = 4;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema: u32,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    manage: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source_root: PathBuf,
    pub packages: Vec<String>,
    pub manage: BTreeSet<PathBuf>,
}

impl Profile {
    pub fn is_managed(&self, relative: &Path) -> bool {
        self.manage
            .iter()
            .any(|managed| relative == managed || relative.starts_with(managed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileDiagnosticKind {
    Toml,
    Io,
    Validation,
}

#[derive(Debug, Clone)]
pub struct ProfileDiagnostic {
    pub profile_directory: PathBuf,
    pub manifest_path: PathBuf,
    pub kind: ProfileDiagnosticKind,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileRegistry {
    pub valid_profiles: BTreeMap<String, Profile>,
    pub diagnostics: Vec<ProfileDiagnostic>,
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

fn valid_package(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.is_ascii()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'_' | b'+' | b'.' | b'-')
        })
}

pub fn profile_root() -> PathBuf {
    std::env::var_os("CATDOT_PROFILE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILE_ROOT))
}

pub fn discover_profiles(root: &Path) -> Result<BTreeMap<String, Profile>> {
    let registry = discover_profile_registry(root)?;
    if let Some(diagnostic) = registry.diagnostics.first() {
        return Err(Error::Message(format!(
            "{}: {}",
            diagnostic.manifest_path.display(),
            diagnostic.message
        )));
    }
    Ok(registry.valid_profiles)
}

pub fn discover_profile_registry(root: &Path) -> Result<ProfileRegistry> {
    if !root.exists() {
        return Ok(ProfileRegistry::default());
    }
    let mut directories = Vec::new();
    for entry in io(root, fs::read_dir(root))? {
        let entry = io(root, entry)?;
        if io(&entry.path(), entry.file_type())?.is_dir() {
            directories.push(entry.path());
        }
    }
    directories.sort();

    let mut registry = ProfileRegistry::default();
    for directory in directories {
        let manifest_path = directory.join("profile.toml");
        if !manifest_path.exists() {
            continue;
        }
        match load_profile(root, &directory, &manifest_path) {
            Ok(profile) => {
                registry.valid_profiles.insert(profile.id.clone(), profile);
            }
            Err(error) => {
                let kind = match &error {
                    Error::Toml { .. } => ProfileDiagnosticKind::Toml,
                    Error::Io { .. } => ProfileDiagnosticKind::Io,
                    Error::Message(_) => ProfileDiagnosticKind::Validation,
                };
                registry.diagnostics.push(ProfileDiagnostic {
                    profile_directory: directory,
                    manifest_path,
                    kind,
                    message: error.to_string(),
                });
            }
        }
    }
    Ok(registry)
}

fn load_profile(root: &Path, directory: &Path, manifest_path: &Path) -> Result<Profile> {
    let id = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Message("profile directory name is not valid UTF-8".into()))?
        .to_owned();
    if !valid_id(&id) {
        return Err(Error::Message(format!("invalid profile id {id}")));
    }

    let raw: RawManifest = toml::from_str(&io(manifest_path, fs::read_to_string(manifest_path))?)
        .map_err(|source| Error::Toml {
        path: manifest_path.display().to_string(),
        source,
    })?;
    if raw.schema != PROFILE_SCHEMA {
        return Err(Error::Message(format!(
            "unsupported profile schema {}; expected {PROFILE_SCHEMA}",
            raw.schema
        )));
    }
    if raw.name.trim().is_empty() {
        return Err(Error::Message("profile name cannot be empty".into()));
    }

    let share_root = root.parent().and_then(Path::parent).ok_or_else(|| {
        Error::Message("profile root must be below <share>/catdot/profiles".into())
    })?;
    let source_root = share_root.join(&id);
    let source_metadata = fs::symlink_metadata(&source_root).map_err(|source| Error::Io {
        path: source_root.display().to_string(),
        source,
    })?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(Error::Message(format!(
            "profile content root {} must be a real directory",
            source_root.display()
        )));
    }
    validate_content_tree(&source_root)?;

    let mut packages = Vec::new();
    let mut seen_packages = BTreeSet::new();
    for package in raw.packages {
        if !valid_package(&package) {
            return Err(Error::Message(format!("invalid package name {package}")));
        }
        if !seen_packages.insert(package.clone()) {
            return Err(Error::Message(format!("duplicate package name {package}")));
        }
        packages.push(package);
    }

    let mut manage = BTreeSet::new();
    for path in raw.manage {
        let path = PathBuf::from(path);
        if !safe_relative(&path) {
            return Err(Error::Message(format!(
                "invalid managed path {}",
                path.display()
            )));
        }
        if path == Path::new(".local/state/catdot")
            || path.starts_with(".local/state/catdot")
            || Path::new(".local/state/catdot").starts_with(&path)
        {
            return Err(Error::Message(format!(
                "managed path {} overlaps Catdot state",
                path.display()
            )));
        }
        if !source_root.join(&path).exists() {
            return Err(Error::Message(format!(
                "managed path {} does not exist in {}",
                path.display(),
                source_root.display()
            )));
        }
        if manage
            .iter()
            .any(|existing: &PathBuf| path.starts_with(existing) || existing.starts_with(&path))
        {
            return Err(Error::Message(format!(
                "managed path {} overlaps another managed path",
                path.display()
            )));
        }
        manage.insert(path);
    }

    Ok(Profile {
        id,
        name: raw.name,
        description: raw.description,
        source_root,
        packages,
        manage,
    })
}

fn validate_content_tree(path: &Path) -> Result<()> {
    for entry in io(path, fs::read_dir(path))? {
        let entry = io(path, entry)?;
        let metadata = io(&entry.path(), fs::symlink_metadata(entry.path()))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::Message(format!(
                "profile content may not contain symbolic link {}",
                entry.path().display()
            )));
        }
        if metadata.is_dir() {
            validate_content_tree(&entry.path())?;
        } else if !metadata.is_file() {
            return Err(Error::Message(format!(
                "unsupported profile content {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}
