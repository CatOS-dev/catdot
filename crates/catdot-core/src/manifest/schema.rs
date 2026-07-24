use crate::{Error, Result, error::io};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

pub const DEFAULT_PROFILE_ROOT: &str = "/usr/share/catdot/profiles";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema: u32,
    profile: RawProfile,
    defaults: BTreeMap<String, String>,
    components: BTreeMap<String, RawComponent>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    id: String,
    name: String,
    description: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponent {
    role: String,
    path: String,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    optional_packages: Vec<String>,
    #[serde(default)]
    exec: Vec<String>,
    #[serde(default)]
    links: Vec<Link>,
    backend: Option<String>,
    settings: Option<BTreeMap<String, String>>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub source: String,
    pub target: String,
}
#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub root: PathBuf,
    pub defaults: BTreeMap<String, String>,
    pub components: BTreeMap<String, ComponentDef>,
}
#[derive(Debug, Clone)]
pub struct ComponentDef {
    pub role: String,
    pub path: PathBuf,
    pub packages: Vec<String>,
    pub optional_packages: Vec<String>,
    pub exec: Vec<String>,
    pub links: Vec<Link>,
    pub backend: Option<String>,
    pub settings: BTreeMap<String, String>,
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
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn validate_template(value: &str) -> bool {
    let stripped = value
        .replace("{profile}", "")
        .replace("{component}", "")
        .replace("{home}", "")
        .replace("{xdg_config_home}", "");
    !stripped.contains('{') && !stripped.contains('}')
}
fn shell_argument(value: &str) -> bool {
    matches!(
        value,
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "/bin/sh"
            | "/bin/bash"
            | "/bin/dash"
            | "/bin/zsh"
            | "/usr/bin/sh"
            | "/usr/bin/bash"
    )
}
fn validate_theme_settings(
    backend: Option<&str>,
    settings: &Option<BTreeMap<String, String>>,
) -> bool {
    let settings = settings.as_ref();
    let expected: &[&str] = match backend {
        None => return settings.is_none_or(BTreeMap::is_empty),
        Some("gtk") => &[
            "theme",
            "icon_theme",
            "cursor_theme",
            "font",
            "color_scheme",
        ],
        Some("qtct-kvantum") => &["qt5_style", "qt6_style", "kvantum_theme", "icon_theme"],
        Some(_) => return false,
    };
    let Some(settings) = settings else {
        return false;
    };
    settings.len() == expected.len()
        && expected
            .iter()
            .all(|key| settings.contains_key(*key) && !settings[*key].is_empty())
}
pub fn profile_root() -> PathBuf {
    std::env::var_os("CATDOT_PROFILE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILE_ROOT))
}
pub fn discover_profiles(root: &Path) -> Result<BTreeMap<String, Profile>> {
    let registry = discover_profile_registry(root)?;
    if let Some(diagnostic) = registry.diagnostics.into_iter().next() {
        return Err(Error::Message(format!(
            "{}: {}",
            diagnostic.manifest_path.display(),
            diagnostic.message
        )));
    }
    Ok(registry.valid_profiles)
}

pub fn discover_profile_registry(root: &Path) -> Result<ProfileRegistry> {
    let canonical_root = io(root, root.canonicalize())?;
    let mut registry = ProfileRegistry::default();
    for entry in io(root, fs::read_dir(root))? {
        let entry = io(root, entry)?;
        let manifest_path = entry.path().join("profile.toml");
        if !manifest_path.is_file() {
            continue;
        };
        match load_profile(&canonical_root, &entry, &manifest_path) {
            Ok(profile) => {
                registry.valid_profiles.insert(profile.id.clone(), profile);
            }
            Err(error) => registry.diagnostics.push(ProfileDiagnostic {
                profile_directory: entry.path(),
                manifest_path,
                kind: match error {
                    Error::Toml { .. } => ProfileDiagnosticKind::Toml,
                    Error::Io { .. } => ProfileDiagnosticKind::Io,
                    Error::Message(_) => ProfileDiagnosticKind::Validation,
                },
                message: error.to_string(),
            }),
        }
    }
    Ok(registry)
}

fn load_profile(
    canonical_root: &Path,
    entry: &fs::DirEntry,
    manifest_path: &Path,
) -> Result<Profile> {
    let raw: RawManifest = toml::from_str(&io(manifest_path, fs::read_to_string(manifest_path))?)
        .map_err(|source| Error::Toml {
        path: manifest_path.display().to_string(),
        source,
    })?;
    if raw.schema != 1 {
        return Err(Error::Message(format!(
            "{}: unsupported schema {}",
            manifest_path.display(),
            raw.schema
        )));
    };
    if !valid_id(&raw.profile.id) || entry.file_name().to_string_lossy() != raw.profile.id {
        return Err(Error::Message(format!(
            "{}: invalid profile id",
            manifest_path.display()
        )));
    };
    let root = io(&entry.path(), entry.path().canonicalize())?;
    if !root.starts_with(canonical_root) {
        return Err(Error::Message(format!(
            "{}: profile directory escapes profile root",
            manifest_path.display()
        )));
    }
    let mut components = BTreeMap::new();
    for (id, raw_component) in raw.components {
        if !valid_id(&id)
            || !valid_id(&raw_component.role)
            || !safe_relative(&raw_component.path)
            || raw_component
                .packages
                .iter()
                .chain(&raw_component.optional_packages)
                .any(|package| !valid_id(package))
        {
            return Err(Error::Message(format!(
                "{}: invalid component {id}",
                manifest_path.display()
            )));
        };
        let path = root.join(&raw_component.path);
        if !path.exists() {
            return Err(Error::Message(format!(
                "{}: component {id} path does not exist",
                manifest_path.display()
            )));
        }
        let actual = io(&path, path.canonicalize())?;
        if !actual.starts_with(&root) {
            return Err(Error::Message(format!(
                "{}: component {id} escapes profile",
                manifest_path.display()
            )));
        }
        for link in &raw_component.links {
            let target_suffix = link
                .target
                .strip_prefix("{home}/")
                .or_else(|| link.target.strip_prefix("{xdg_config_home}/"));
            let source = actual.join(&link.source);
            if !safe_relative(&link.source)
                || !target_suffix.is_some_and(safe_relative)
                || !source.is_file()
                || !io(&source, source.canonicalize())?.starts_with(&root)
            {
                return Err(Error::Message(format!(
                    "{}: unsafe link in {id}",
                    manifest_path.display()
                )));
            }
        }
        if raw_component.exec.iter().any(|argument| {
            argument.contains('\0') || !validate_template(argument) || shell_argument(argument)
        }) {
            return Err(Error::Message(format!(
                "{}: unsafe exec argument",
                manifest_path.display()
            )));
        };
        if !validate_theme_settings(raw_component.backend.as_deref(), &raw_component.settings) {
            return Err(Error::Message(format!(
                "{}: invalid backend settings for component {id}",
                manifest_path.display()
            )));
        }
        components.insert(
            id,
            ComponentDef {
                role: raw_component.role,
                path,
                packages: raw_component.packages,
                optional_packages: raw_component.optional_packages,
                exec: raw_component.exec,
                links: raw_component.links,
                backend: raw_component.backend,
                settings: raw_component.settings.unwrap_or_default(),
            },
        );
    }
    for (role, id) in &raw.defaults {
        if !valid_id(role)
            || components
                .get(id)
                .is_none_or(|component| component.role != *role)
        {
            return Err(Error::Message(format!(
                "{}: invalid default {role}",
                manifest_path.display()
            )));
        }
    }
    Ok(Profile {
        id: raw.profile.id,
        name: raw.profile.name,
        description: raw.profile.description,
        root,
        defaults: raw.defaults,
        components,
    })
}
