use crate::{Error, Result, error::io};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

pub const DEFAULT_PROFILE_ROOT: &str = "/usr/share/catdot/profiles";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema: u32,
    profile: RawProfile,
    #[serde(default)]
    defaults: BTreeMap<String, String>,
    #[serde(default)]
    component_files: Vec<String>,
    #[serde(default)]
    components: Vec<RawComponent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    id: String,
    name: String,
    description: String,
    source_root: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponentFile {
    component: RawComponent,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponent {
    id: String,
    role: String,
    #[serde(default)]
    packages: Vec<String>,
    exec: Option<RawExec>,
    xdg: Option<RawXdg>,
    wm: Option<RawWm>,
    #[serde(default)]
    configuration: Vec<RawConfiguration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExec {
    argv: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawXdg {
    command: Option<String>,
    #[serde(default)]
    environment: Vec<String>,
    desktop_entry: Option<String>,
    #[serde(default)]
    mime_types: Vec<String>,
    #[serde(default)]
    uri_schemes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWm {
    autostart_target: String,
    autostart_template: String,
    #[serde(default)]
    autostart: Vec<RawWmAutostart>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWmAutostart {
    role: String,
    #[serde(default)]
    before: Vec<String>,
    #[serde(default)]
    after: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfiguration {
    target: String,
    lifecycle: String,
    mode: Option<String>,
    source: Option<String>,
    template: Option<String>,
    seed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lifecycle {
    Generate,
    Symlink,
    Merge,
    User,
}

#[derive(Debug, Clone)]
pub struct ConfigurationEntry {
    pub target: PathBuf,
    pub lifecycle: Lifecycle,
    pub source: Option<PathBuf>,
    pub template: Option<String>,
    pub seed: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct XdgProvider {
    pub command: Option<String>,
    pub environment: Vec<String>,
    pub desktop_entry: Option<String>,
    pub mime_types: Vec<String>,
    pub uri_schemes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WmConfig {
    pub autostart_target: PathBuf,
    pub autostart_template: String,
    pub autostart: Vec<WmAutostart>,
}

#[derive(Debug, Clone)]
pub struct WmAutostart {
    pub role: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The installed skel-style content tree. This is deliberately distinct
    /// from the directory that contains profile metadata.
    pub source_root: PathBuf,
    pub defaults: BTreeMap<String, String>,
    pub components: BTreeMap<String, ComponentDef>,
}

#[derive(Debug, Clone)]
pub struct ComponentDef {
    pub role: String,
    pub packages: Vec<String>,
    pub exec: Vec<String>,
    pub xdg: XdgProvider,
    pub wm: Option<WmConfig>,
    pub configuration: Vec<ConfigurationEntry>,
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

fn safe_home_target(value: &str) -> bool {
    Path::new(value).is_relative() && safe_relative(value)
}

fn valid_package(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 255
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0 || byte == b'/')
    {
        return false;
    }
    let operator = value
        .char_indices()
        .find(|(_, character)| matches!(character, '<' | '>' | '='));
    let (name, constraint) = match operator {
        Some((index, _)) => (&value[..index], Some(&value[index..])),
        None => (value, None),
    };
    let mut name_bytes = name.bytes();
    if !name_bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !name_bytes.all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'+' | b'-')
        })
    {
        return false;
    }
    let Some(constraint) = constraint else {
        return true;
    };
    let version = [">=", "<=", "=", ">", "<"]
        .into_iter()
        .find_map(|operator| constraint.strip_prefix(operator));
    version.is_some_and(|version| {
        !version.is_empty()
            && version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'@' | b'.' | b'_' | b'+' | b'~' | b':' | b'-')
            })
    })
}

fn valid_exec(argv: &[String]) -> bool {
    !argv.is_empty()
        && argv.iter().all(|arg| {
            !arg.is_empty()
                && !arg.contains('\0')
                && !matches!(
                    arg.as_str(),
                    "sh" | "bash"
                        | "dash"
                        | "zsh"
                        | "/bin/sh"
                        | "/bin/bash"
                        | "/bin/dash"
                        | "/bin/zsh"
                )
        })
}

fn valid_xdg_command(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

fn valid_environment_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
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
        }
        match load_profile(&canonical_root, &entry, &manifest_path) {
            Ok(profile) => {
                if registry.valid_profiles.contains_key(&profile.id) {
                    registry.diagnostics.push(ProfileDiagnostic {
                        profile_directory: entry.path(),
                        manifest_path,
                        kind: ProfileDiagnosticKind::Validation,
                        message: format!("duplicate profile id {}", profile.id),
                    });
                } else {
                    registry.valid_profiles.insert(profile.id.clone(), profile);
                }
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

fn read_toml<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T> {
    toml::from_str(&io(path, fs::read_to_string(path))?).map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}

fn parse_wm(
    manifest: &Path,
    schema: u32,
    component: &str,
    raw: Option<RawWm>,
) -> Result<Option<WmConfig>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if schema != 3
        || !safe_home_target(&raw.autostart_target)
        || raw.autostart_template.matches("{role}").count() != 1
        || raw.autostart_template.contains(['\0', '\n', '\r'])
    {
        return Err(Error::Message(format!(
            "{}: invalid wm declaration for {component}",
            manifest.display()
        )));
    }
    let mut roles = BTreeSet::new();
    for entry in &raw.autostart {
        if !valid_id(&entry.role)
            || !roles.insert(entry.role.clone())
            || entry
                .before
                .iter()
                .chain(&entry.after)
                .any(|role| !valid_id(role))
            || entry.before.iter().any(|role| role == &entry.role)
            || entry.after.iter().any(|role| role == &entry.role)
        {
            return Err(Error::Message(format!(
                "{}: invalid wm autostart declaration for {component}",
                manifest.display()
            )));
        }
    }
    for entry in &raw.autostart {
        if entry
            .before
            .iter()
            .chain(&entry.after)
            .any(|role| !roles.contains(role))
        {
            return Err(Error::Message(format!(
                "{}: wm autostart references an undeclared role for {component}",
                manifest.display()
            )));
        }
    }
    validate_wm_autostart_graph(manifest, component, &raw.autostart)?;
    Ok(Some(WmConfig {
        autostart_target: PathBuf::from(raw.autostart_target),
        autostart_template: raw.autostart_template,
        autostart: raw
            .autostart
            .into_iter()
            .map(|entry| WmAutostart {
                role: entry.role,
                before: entry.before,
                after: entry.after,
            })
            .collect(),
    }))
}

fn validate_wm_autostart_graph(
    manifest: &Path,
    component: &str,
    entries: &[RawWmAutostart],
) -> Result<()> {
    let mut outgoing = entries
        .iter()
        .map(|entry| (entry.role.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut indegree = entries
        .iter()
        .map(|entry| (entry.role.clone(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    for entry in entries {
        for before in &entry.before {
            if outgoing
                .get_mut(&entry.role)
                .expect("declared role")
                .insert(before.clone())
            {
                *indegree.get_mut(before).expect("declared role") += 1;
            }
        }
        for after in &entry.after {
            if outgoing
                .get_mut(after)
                .expect("declared role")
                .insert(entry.role.clone())
            {
                *indegree.get_mut(&entry.role).expect("declared role") += 1;
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(role, _)| role.clone())
        .collect::<BTreeSet<_>>();
    let mut visited = 0;
    while let Some(role) = ready.pop_first() {
        visited += 1;
        for next in outgoing.get(&role).expect("declared role") {
            let degree = indegree.get_mut(next).expect("declared role");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(next.clone());
            }
        }
    }
    if visited != entries.len() {
        return Err(Error::Message(format!(
            "{}: wm autostart cycle for {component}",
            manifest.display()
        )));
    }
    Ok(())
}

fn validate_default_closure(
    manifest: &Path,
    defaults: &BTreeMap<String, String>,
    components: &BTreeMap<String, ComponentDef>,
) -> Result<()> {
    let selected = defaults
        .iter()
        .map(|(role, id)| {
            (
                role.as_str(),
                id.as_str(),
                components.get(id).expect("validated default"),
            )
        })
        .collect::<Vec<_>>();

    let wm_owners = selected
        .iter()
        .filter(|(_, _, component)| component.wm.is_some())
        .map(|(role, id, _)| format!("{role}={id}"))
        .collect::<Vec<_>>();
    if wm_owners.len() > 1 {
        return Err(Error::Message(format!(
            "{}: multiple default wm owners: {}",
            manifest.display(),
            wm_owners.join(", ")
        )));
    }

    let mut xdg = BTreeMap::<String, (String, String)>::new();
    for (role, id, component) in &selected {
        let reference = format!("{role}={id}");
        if let Some(command) = &component.xdg.command {
            for key in &component.xdg.environment {
                insert_default_xdg(manifest, &mut xdg, key, command, &reference)?;
            }
        }
        if let Some(desktop) = &component.xdg.desktop_entry {
            for association in component.xdg.mime_types.iter().cloned().chain(
                component
                    .xdg
                    .uri_schemes
                    .iter()
                    .map(|scheme| format!("x-scheme-handler/{scheme}")),
            ) {
                insert_default_xdg(manifest, &mut xdg, &association, desktop, &reference)?;
            }
        }
    }

    let mut targets = Vec::<(PathBuf, String)>::new();
    for (role, id, component) in &selected {
        let owner = format!("{role}={id}");
        for configuration in &component.configuration {
            targets.push((configuration.target.clone(), owner.clone()));
        }
        if let Some(wm) = &component.wm {
            targets.push((wm.autostart_target.clone(), owner.clone()));
            for autostart in &wm.autostart {
                if let Some(provider_id) = defaults.get(&autostart.role) {
                    let provider = components.get(provider_id).expect("validated default");
                    if provider.exec.is_empty() {
                        return Err(Error::Message(format!(
                            "{}: default autostart role {} uses {}, which has no exec provider",
                            manifest.display(),
                            autostart.role,
                            provider_id
                        )));
                    }
                }
            }
        }
    }
    for (index, (target, owner)) in targets.iter().enumerate() {
        for (other, other_owner) in targets.iter().skip(index + 1) {
            if target == other || target.starts_with(other) || other.starts_with(target) {
                return Err(Error::Message(format!(
                    "{}: default configuration target conflict: {} ({owner}) and {} ({other_owner})",
                    manifest.display(),
                    target.display(),
                    other.display()
                )));
            }
        }
    }
    Ok(())
}

fn insert_default_xdg(
    manifest: &Path,
    values: &mut BTreeMap<String, (String, String)>,
    key: &str,
    value: &str,
    owner: &str,
) -> Result<()> {
    if let Some((previous, previous_owner)) = values.get(key)
        && previous != value
    {
        return Err(Error::Message(format!(
            "{}: default xdg conflict for {key}: {previous} from {previous_owner}, {value} from {owner}",
            manifest.display()
        )));
    }
    values
        .entry(key.into())
        .or_insert_with(|| (value.into(), owner.into()));
    Ok(())
}

fn load_profile(
    canonical_root: &Path,
    entry: &fs::DirEntry,
    manifest_path: &Path,
) -> Result<Profile> {
    let raw: RawManifest = read_toml(manifest_path)?;
    if !matches!(raw.schema, 2 | 3) {
        return Err(Error::Message(format!(
            "{}: unsupported schema {}",
            manifest_path.display(),
            raw.schema
        )));
    }
    if !valid_id(&raw.profile.id) || entry.file_name().to_string_lossy() != raw.profile.id {
        return Err(Error::Message(format!(
            "{}: invalid profile id",
            manifest_path.display()
        )));
    }
    let metadata_root = io(&entry.path(), entry.path().canonicalize())?;
    if !metadata_root.starts_with(canonical_root) {
        return Err(Error::Message(format!(
            "{}: profile directory escapes profile root",
            manifest_path.display()
        )));
    }
    let source_root = PathBuf::from(&raw.profile.source_root);
    if !source_root.is_absolute() {
        return Err(Error::Message(format!(
            "{}: source root must be absolute",
            manifest_path.display()
        )));
    }
    let source_for_overlap_check = if source_root.exists() {
        io(&source_root, source_root.canonicalize())?
    } else {
        source_root.clone()
    };
    if source_for_overlap_check.starts_with(&metadata_root)
        || metadata_root.starts_with(&source_for_overlap_check)
    {
        return Err(Error::Message(format!(
            "{}: source root must be separate from profile metadata",
            manifest_path.display()
        )));
    }

    let mut raw_components = raw.components;
    let mut files = BTreeSet::new();
    for file in raw.component_files {
        if !safe_relative(&file) || !file.ends_with(".toml") || !files.insert(file.clone()) {
            return Err(Error::Message(format!(
                "{}: invalid component file {file}",
                manifest_path.display()
            )));
        }
        let path = metadata_root.join(&file);
        if !path.is_file() {
            return Err(Error::Message(format!(
                "{}: component file {file} is missing",
                manifest_path.display()
            )));
        }
        raw_components.push(read_toml::<RawComponentFile>(&path)?.component);
    }
    let mut components = BTreeMap::new();
    for component in raw_components {
        if !valid_id(&component.id)
            || !valid_id(&component.role)
            || component
                .packages
                .iter()
                .any(|package| !valid_package(package))
        {
            return Err(Error::Message(format!(
                "{}: invalid component {}",
                manifest_path.display(),
                component.id
            )));
        }
        if components.contains_key(&component.id) {
            return Err(Error::Message(format!(
                "{}: duplicate component {}",
                manifest_path.display(),
                component.id
            )));
        }
        let exec = component
            .exec
            .map(|provider| provider.argv)
            .unwrap_or_default();
        if !exec.is_empty() && !valid_exec(&exec) {
            return Err(Error::Message(format!(
                "{}: invalid exec argv for component {}",
                manifest_path.display(),
                component.id
            )));
        }
        let configuration = component
            .configuration
            .into_iter()
            .map(|entry| parse_configuration(manifest_path, raw.schema, &component.id, entry))
            .collect::<Result<Vec<_>>>()?;
        let wm = parse_wm(manifest_path, raw.schema, &component.id, component.wm)?;
        let xdg = component.xdg.unwrap_or_default();
        if xdg.desktop_entry.as_deref().is_some_and(|value| {
            value.is_empty() || value.contains('/') || !value.ends_with(".desktop")
        }) || xdg
            .command
            .as_deref()
            .is_some_and(|value| !valid_xdg_command(value))
            || (!xdg.environment.is_empty() && xdg.command.is_none())
            || xdg
                .environment
                .iter()
                .any(|value| !valid_environment_key(value))
        {
            return Err(Error::Message(format!(
                "{}: invalid xdg provider for component {}",
                manifest_path.display(),
                component.id
            )));
        }
        components.insert(
            component.id,
            ComponentDef {
                role: component.role,
                packages: component.packages,
                exec,
                xdg: XdgProvider {
                    command: xdg.command,
                    environment: xdg.environment,
                    desktop_entry: xdg.desktop_entry,
                    mime_types: xdg.mime_types,
                    uri_schemes: xdg.uri_schemes,
                },
                wm,
                configuration,
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
    validate_default_closure(manifest_path, &raw.defaults, &components)?;
    Ok(Profile {
        id: raw.profile.id,
        name: raw.profile.name,
        description: raw.profile.description,
        source_root,
        defaults: raw.defaults,
        components,
    })
}

fn parse_configuration(
    manifest: &Path,
    schema: u32,
    component: &str,
    raw: RawConfiguration,
) -> Result<ConfigurationEntry> {
    if !safe_home_target(&raw.target) {
        return Err(Error::Message(format!(
            "{}: unsafe configuration target for {component}",
            manifest.display()
        )));
    }
    if matches!(
        raw.target.as_str(),
        ".config/environment.d/90-catdot.conf"
            | ".config/mimeapps.list"
            | ".config/catdot/xdg.toml"
    ) {
        return Err(Error::Message(format!(
            "{}: reserved Catdot target {} for {component}",
            manifest.display(),
            raw.target
        )));
    }
    let source = raw.source.map(PathBuf::from);
    let seed = raw.seed.map(PathBuf::from);
    if source
        .as_ref()
        .is_some_and(|path| !safe_relative(&path.to_string_lossy()))
        || seed
            .as_ref()
            .is_some_and(|path| !safe_relative(&path.to_string_lossy()))
    {
        return Err(Error::Message(format!(
            "{}: unsafe configuration source for {component}",
            manifest.display()
        )));
    }
    let lifecycle = match schema {
        2 => match (raw.lifecycle.as_str(), raw.mode.as_deref()) {
            ("generate", None) if raw.template.is_some() && source.is_none() && seed.is_none() => {
                Lifecycle::Generate
            }
            ("overwrite", Some("symlink"))
                if source.is_some() && raw.template.is_none() && seed.is_none() =>
            {
                Lifecycle::Symlink
            }
            ("overwrite", Some("file"))
                if source.is_some() && raw.template.is_none() && seed.is_none() =>
            {
                // Schema 2 compatibility: copied files become generated inputs.
                Lifecycle::Generate
            }
            ("user", None) if raw.template.is_none() && source.is_none() => Lifecycle::User,
            _ => {
                return Err(Error::Message(format!(
                    "{}: invalid configuration lifecycle for {component}",
                    manifest.display()
                )));
            }
        },
        3 => match (raw.lifecycle.as_str(), raw.mode.as_deref()) {
            ("generate", None) if raw.template.is_some() ^ source.is_some() && seed.is_none() => {
                Lifecycle::Generate
            }
            ("symlink", None) if source.is_some() && raw.template.is_none() && seed.is_none() => {
                Lifecycle::Symlink
            }
            ("merge", None) if source.is_some() && raw.template.is_none() && seed.is_none() => {
                Lifecycle::Merge
            }
            ("user", None) if raw.template.is_none() && source.is_none() => Lifecycle::User,
            _ => {
                return Err(Error::Message(format!(
                    "{}: invalid schema 3 configuration lifecycle for {component}",
                    manifest.display()
                )));
            }
        },
        _ => unreachable!("validated schema"),
    };
    Ok(ConfigurationEntry {
        target: PathBuf::from(raw.target),
        lifecycle,
        source,
        template: raw.template,
        seed,
    })
}
