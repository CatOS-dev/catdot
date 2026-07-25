use crate::{ActivationJournal, ComponentDef, Error, Result, UserState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The desktop-integration state Catdot owns for the selected components.
/// MIME defaults and the dedicated environment.d file are planned together so
/// a provider switch is committed by the same activation journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XdgPlan {
    pub path: PathBuf,
    pub defaults: BTreeMap<String, String>,
    pub restore: BTreeMap<String, Option<String>>,
    pub environment_path: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub environment_restore: bool,
    pub environment_remove: bool,
    pub warnings: Vec<String>,
    registry_path: PathBuf,
    registry: XdgRegistry,
    rendered: String,
    environment_output: Option<String>,
    mime_changed: bool,
    environment_changed: bool,
    registry_changed: bool,
    changed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct XdgRegistry {
    previous: BTreeMap<String, String>,
    #[serde(default)]
    absent: BTreeSet<String>,
    #[serde(default)]
    environment_previous: Option<String>,
    #[serde(default)]
    environment_absent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    role: String,
    reference: String,
    value: String,
}

pub fn build_xdg_plan(
    profiles: &BTreeMap<String, crate::Profile>,
    state: &UserState,
    config_home: &Path,
) -> Result<XdgPlan> {
    let mut default_candidates = BTreeMap::<String, Vec<Candidate>>::new();
    let mut environment_candidates = BTreeMap::<String, Vec<Candidate>>::new();
    for (role, reference) in &state.components {
        let (profile_id, component_id) = reference
            .split_once('/')
            .ok_or_else(|| Error::Message(format!("invalid component reference {reference}")))?;
        let component = profiles
            .get(profile_id)
            .and_then(|profile| profile.components.get(component_id))
            .ok_or_else(|| Error::Message(format!("active component {reference} is missing")))?;
        if component.role != *role {
            return Err(Error::Message(format!(
                "{reference} does not provide role {role}"
            )));
        }
        collect_component(
            &mut default_candidates,
            &mut environment_candidates,
            role,
            reference,
            component,
        );
    }
    let (defaults, mut warnings) = resolve_candidates(default_candidates, false);
    let (environment, environment_warnings) = resolve_candidates(environment_candidates, true);
    warnings.extend(environment_warnings);

    let path = config_home.join("mimeapps.list");
    let registry_path = config_home.join("catdot/xdg.toml");
    let existing_text = read_text(&path)?;
    let existing = read_mimeapps_text(&existing_text);
    let old = read_registry(&registry_path)?;
    let previous_registry = old.clone();
    let mut registry = XdgRegistry::default();
    for association in defaults.keys() {
        let previous = old
            .previous
            .get(association)
            .cloned()
            .map(Some)
            .or_else(|| old.absent.contains(association).then_some(None))
            .unwrap_or_else(|| existing.get(association).cloned());
        match previous {
            Some(desktop) => {
                registry.previous.insert(association.clone(), desktop);
            }
            None => {
                registry.absent.insert(association.clone());
            }
        }
    }
    let restore = old
        .previous
        .iter()
        .map(|(association, desktop)| (association.clone(), Some(desktop.clone())))
        .chain(
            old.absent
                .iter()
                .map(|association| (association.clone(), None)),
        )
        .filter(|(association, _)| !defaults.contains_key(association))
        .collect();
    let rendered = render_mimeapps(&existing_text, &defaults, &restore);
    let mime_changed = (!defaults.is_empty() || !restore.is_empty()) && rendered != existing_text;

    let environment_path = config_home.join("environment.d/90-catdot.conf");
    let environment_exists = environment_path.exists();
    let existing_environment = read_text(&environment_path)?;
    let old_environment_owned = old.environment_previous.is_some() || old.environment_absent;
    let (environment_output, environment_changed) = if environment.is_empty() {
        if old_environment_owned {
            let output = old.environment_previous.clone();
            let changed = match &output {
                Some(previous) => !environment_exists || existing_environment != *previous,
                None => environment_exists,
            };
            (output, changed)
        } else {
            (None, false)
        }
    } else {
        if old_environment_owned {
            registry.environment_previous = old.environment_previous.clone();
            registry.environment_absent = old.environment_absent;
        } else if environment_exists {
            registry.environment_previous = Some(existing_environment.clone());
        } else {
            registry.environment_absent = true;
        }
        let output = render_environment(&environment);
        let changed = !environment_exists || output != existing_environment;
        (Some(output), changed)
    };
    let environment_restore = environment.is_empty() && old.environment_previous.is_some();
    let environment_remove = environment.is_empty() && old.environment_absent;
    let registry_changed = registry != previous_registry;
    let changed = mime_changed || environment_changed || registry_changed;
    Ok(XdgPlan {
        path,
        defaults,
        restore,
        environment_path,
        environment,
        environment_restore,
        environment_remove,
        warnings,
        registry_path,
        registry,
        rendered,
        environment_output,
        mime_changed,
        environment_changed,
        registry_changed,
        changed,
    })
}

impl XdgPlan {
    pub fn has_changes(&self) -> bool {
        self.changed
    }

    pub fn identity_digest(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.path.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(self.registry_path.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(self.environment_path.as_os_str().as_encoded_bytes());
        digest.update([0]);
        for (association, desktop) in &self.defaults {
            digest.update(b"default\0");
            digest.update(association.as_bytes());
            digest.update([0]);
            digest.update(desktop.as_bytes());
            digest.update([0xff]);
        }
        for (association, desktop) in &self.restore {
            digest.update(b"restore\0");
            digest.update(association.as_bytes());
            digest.update([0]);
            if let Some(desktop) = desktop {
                digest.update(desktop.as_bytes());
            }
            digest.update([0xff]);
        }
        for (key, value) in &self.environment {
            digest.update(b"environment\0");
            digest.update(key.as_bytes());
            digest.update([0]);
            digest.update(value.as_bytes());
            digest.update([0xff]);
        }
        digest.update(self.rendered.as_bytes());
        digest.update([0]);
        match &self.environment_output {
            Some(output) => {
                digest.update(b"environment-output\0");
                digest.update(output.as_bytes());
            }
            None => digest.update(b"environment-remove\0"),
        }
        digest.update([0]);
        digest.update(
            toml::to_string(&self.registry)
                .unwrap_or_default()
                .as_bytes(),
        );
        format!("{:x}", digest.finalize())
    }
}

fn collect_component(
    defaults: &mut BTreeMap<String, Vec<Candidate>>,
    environment: &mut BTreeMap<String, Vec<Candidate>>,
    role: &str,
    reference: &str,
    component: &ComponentDef,
) {
    if let Some(desktop) = &component.xdg.desktop_entry {
        for association in component.xdg.mime_types.iter().cloned().chain(
            component
                .xdg
                .uri_schemes
                .iter()
                .map(|scheme| format!("x-scheme-handler/{scheme}")),
        ) {
            defaults.entry(association).or_default().push(Candidate {
                role: role.into(),
                reference: reference.into(),
                value: desktop.clone(),
            });
        }
    }
    if let Some(command) = &component.xdg.command {
        for key in &component.xdg.environment {
            environment.entry(key.clone()).or_default().push(Candidate {
                role: role.into(),
                reference: reference.into(),
                value: command.clone(),
            });
        }
    }
}

fn resolve_candidates(
    candidates: BTreeMap<String, Vec<Candidate>>,
    semantic_environment_role: bool,
) -> (BTreeMap<String, String>, Vec<String>) {
    let mut selected = BTreeMap::new();
    let mut warnings = Vec::new();
    for (key, mut values) in candidates {
        let expected = semantic_environment_role
            .then(|| expected_environment_role(&key))
            .flatten();
        values.sort_by(|left, right| {
            let left_priority = usize::from(expected.is_some_and(|role| left.role != role));
            let right_priority = usize::from(expected.is_some_and(|role| right.role != role));
            (left_priority, &left.role, &left.reference, &left.value).cmp(&(
                right_priority,
                &right.role,
                &right.reference,
                &right.value,
            ))
        });
        let distinct = values
            .iter()
            .map(|candidate| candidate.value.as_str())
            .collect::<BTreeSet<_>>();
        let chosen = values.first().expect("candidate group is non-empty");
        if distinct.len() > 1 {
            warnings.push(format!(
                "xdg conflict for {key}: {}; using {} from {}",
                values
                    .iter()
                    .map(|candidate| format!("{} from {}", candidate.value, candidate.reference))
                    .collect::<Vec<_>>()
                    .join(", "),
                chosen.value,
                chosen.reference
            ));
        }
        selected.insert(key, chosen.value.clone());
    }
    (selected, warnings)
}

fn expected_environment_role(key: &str) -> Option<&'static str> {
    match key {
        "TERMINAL" => Some("terminal"),
        "EDITOR" | "VISUAL" => Some("editor"),
        "BROWSER" => Some("browser"),
        _ => None,
    }
}

fn render_environment(environment: &BTreeMap<String, String>) -> String {
    environment
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}

pub fn activate_xdg(plan: &XdgPlan, journal: &mut ActivationJournal) -> Result<()> {
    if !plan.changed {
        return Ok(());
    }
    if plan.mime_changed {
        journal.track_path(&plan.path)?;
        crate::atomic_write(&plan.path, &plan.rendered)?;
    }
    if plan.environment_changed {
        journal.track_path(&plan.environment_path)?;
        if let Some(output) = &plan.environment_output {
            crate::atomic_write(&plan.environment_path, output)?;
        } else {
            match fs::remove_file(&plan.environment_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::Io {
                        path: plan.environment_path.display().to_string(),
                        source,
                    });
                }
            }
        }
    }
    if plan.registry_changed {
        journal.track_path(&plan.registry_path)?;
        crate::atomic_write(
            &plan.registry_path,
            &toml::to_string_pretty(&plan.registry)
                .map_err(|error| Error::Message(error.to_string()))?,
        )?;
    }
    journal.mark_applied()
}

fn read_registry(path: &Path) -> Result<XdgRegistry> {
    if !path.exists() {
        return Ok(XdgRegistry::default());
    }
    toml::from_str(&read_text(path)?).map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}

fn read_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(Error::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn read_mimeapps_text(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut in_defaults = false;
    for line in text.lines() {
        if line.trim() == "[Default Applications]" {
            in_defaults = true;
            continue;
        }
        if line.starts_with('[') {
            in_defaults = false;
        }
        if in_defaults && let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().into(), value.trim().trim_end_matches(';').into());
        }
    }
    values
}

fn render_mimeapps(
    existing: &str,
    defaults: &BTreeMap<String, String>,
    restore: &BTreeMap<String, Option<String>>,
) -> String {
    let mut changes = defaults
        .iter()
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect::<BTreeMap<_, _>>();
    changes.extend(restore.clone());
    let mut output = Vec::new();
    let mut in_defaults = false;
    let mut saw_defaults = false;
    for line in existing.lines() {
        if line.trim() == "[Default Applications]" {
            if saw_defaults {
                continue;
            }
            saw_defaults = true;
            in_defaults = true;
            output.push(line.to_owned());
            for (kind, desktop) in &changes {
                if let Some(desktop) = desktop {
                    output.push(format!("{kind}={desktop};"));
                }
            }
            continue;
        }
        if line.starts_with('[') {
            in_defaults = false;
        }
        if in_defaults
            && line
                .split_once('=')
                .is_some_and(|(key, _)| changes.contains_key(key.trim()))
        {
            continue;
        }
        output.push(line.to_owned());
    }
    if !saw_defaults {
        if !output.is_empty() && !output.last().is_some_and(String::is_empty) {
            output.push(String::new());
        }
        output.push("[Default Applications]".into());
        for (kind, desktop) in &changes {
            if let Some(desktop) = desktop {
                output.push(format!("{kind}={desktop};"));
            }
        }
    }
    format!("{}\n", output.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActivationJournal, Profile, UserState};
    use std::{collections::BTreeMap, fs};
    use tempfile::tempdir;

    // Protects a user's browser and URI defaults: profile activation must
    // update both through the journal, instead of invoking irreversible xdg
    // commands after configuration work has committed.
    #[test]
    fn xdg_defaults_are_written_and_rollback_restores_the_original_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(".config/mimeapps.list");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[Default Applications]\ntext/plain=old.desktop;\n").unwrap();
        let component = crate::ComponentDef {
            role: "browser".into(),
            packages: vec![],
            exec: vec![],
            xdg: crate::XdgProvider {
                command: None,
                environment: vec![],
                desktop_entry: Some("browser.desktop".into()),
                mime_types: vec!["text/html".into()],
                uri_schemes: vec!["http".into()],
            },
            wm: None,
            configuration: vec![],
        };
        let mut profile = Profile {
            id: "demo".into(),
            name: "Demo".into(),
            description: "test".into(),
            source_root: temp.path().join("share"),
            defaults: BTreeMap::new(),
            components: BTreeMap::new(),
        };
        profile.components.insert("browser".into(), component);
        let mut profiles = BTreeMap::new();
        profiles.insert("demo".into(), profile);
        let mut state = UserState::default();
        state
            .components
            .insert("browser".into(), "demo/browser".into());
        let plan = build_xdg_plan(&profiles, &state, path.parent().unwrap()).unwrap();
        let state_path = temp.path().join(".local/state/catdot/state.toml");
        let mut journal =
            ActivationJournal::begin(&state_path, UserState::default(), UserState::default())
                .unwrap();
        journal.mark_applying().unwrap();
        activate_xdg(&plan, &mut journal).unwrap();
        let changed = fs::read_to_string(&path).unwrap();
        assert!(changed.contains("text/html=browser.desktop;"));
        assert!(changed.contains("x-scheme-handler/http=browser.desktop;"));
        journal.rollback().unwrap();
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "[Default Applications]\ntext/plain=old.desktop;\n"
        );
    }

    // Protects component switches and disable: Catdot restores the association
    // that existed before it became owner instead of leaving a stale browser.
    #[test]
    fn xdg_switches_provider_then_restores_the_original_default() {
        let temp = tempdir().unwrap();
        let config = temp.path().join(".config");
        fs::create_dir_all(&config).unwrap();
        let path = config.join("mimeapps.list");
        fs::write(
            &path,
            "[Default Applications]\ntext/html=outside.desktop;\n",
        )
        .unwrap();
        let mut profile = Profile {
            id: "demo".into(),
            name: "Demo".into(),
            description: "test".into(),
            source_root: temp.path().join("share"),
            defaults: BTreeMap::new(),
            components: BTreeMap::new(),
        };
        for (id, desktop) in [("one", "one.desktop"), ("two", "two.desktop")] {
            profile.components.insert(
                id.into(),
                crate::ComponentDef {
                    role: "browser".into(),
                    packages: vec![],
                    exec: vec![],
                    xdg: crate::XdgProvider {
                        command: None,
                        environment: vec![],
                        desktop_entry: Some(desktop.into()),
                        mime_types: vec!["text/html".into()],
                        uri_schemes: vec![],
                    },
                    wm: None,
                    configuration: vec![],
                },
            );
        }
        let mut profiles = BTreeMap::new();
        profiles.insert("demo".into(), profile);
        let state_path = temp.path().join(".local/state/catdot/state.toml");
        for reference in [Some("demo/one"), Some("demo/two"), None] {
            let mut state = UserState::default();
            if let Some(reference) = reference {
                state.components.insert("browser".into(), reference.into());
            }
            let plan = build_xdg_plan(&profiles, &state, &config).unwrap();
            let mut journal =
                ActivationJournal::begin(&state_path, UserState::default(), UserState::default())
                    .unwrap();
            journal.mark_applying().unwrap();
            activate_xdg(&plan, &mut journal).unwrap();
            journal.complete().unwrap();
        }
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "[Default Applications]\ntext/html=outside.desktop;\n"
        );
    }
}
