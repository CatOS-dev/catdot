use crate::{ActivationJournal, ComponentDef, Error, Result, UserState};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// The XDG defaults Catdot owns for the selected active components.  Keeping
/// this as a plan makes the whole mimeapps.list replacement journalled along
/// with configuration materialization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XdgPlan {
    pub path: PathBuf,
    pub defaults: BTreeMap<String, String>,
    pub restore: BTreeMap<String, Option<String>>,
    registry_path: PathBuf,
    registry: XdgRegistry,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct XdgRegistry {
    previous: BTreeMap<String, Option<String>>,
}

pub fn build_xdg_plan(
    profiles: &BTreeMap<String, crate::Profile>,
    state: &UserState,
    config_home: &Path,
) -> Result<XdgPlan> {
    let mut defaults = BTreeMap::new();
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
        add_component(&mut defaults, component)?;
    }
    let path = config_home.join("mimeapps.list");
    let registry_path = config_home.join("catdot/xdg.toml");
    let existing = read_mimeapps(&path)?;
    let old = read_registry(&registry_path)?;
    let mut registry = XdgRegistry::default();
    for association in defaults.keys() {
        registry.previous.insert(
            association.clone(),
            old.previous
                .get(association)
                .cloned()
                .unwrap_or_else(|| existing.get(association).cloned()),
        );
    }
    let restore = old
        .previous
        .into_iter()
        .filter(|(association, _)| !defaults.contains_key(association))
        .collect();
    Ok(XdgPlan {
        path,
        defaults,
        restore,
        registry_path,
        registry,
    })
}

fn add_component(defaults: &mut BTreeMap<String, String>, component: &ComponentDef) -> Result<()> {
    let Some(desktop) = &component.xdg.desktop_entry else {
        return Ok(());
    };
    for association in component.xdg.mime_types.iter().cloned().chain(
        component
            .xdg
            .uri_schemes
            .iter()
            .map(|scheme| format!("x-scheme-handler/{scheme}")),
    ) {
        match defaults.insert(association.clone(), desktop.clone()) {
            Some(previous) if previous != *desktop => {
                return Err(Error::Message(format!(
                    "xdg default conflict for {association}: {previous} and {desktop}"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn activate_xdg(plan: &XdgPlan, journal: &mut ActivationJournal) -> Result<()> {
    if plan.defaults.is_empty() && plan.restore.is_empty() {
        return Ok(());
    }
    journal.track_path(&plan.path)?;
    journal.track_path(&plan.registry_path)?;
    let existing = read_text(&plan.path)?;
    let rendered = render_mimeapps(&existing, &plan.defaults, &plan.restore);
    crate::atomic_write(&plan.path, &rendered)?;
    crate::atomic_write(
        &plan.registry_path,
        &toml::to_string_pretty(&plan.registry)
            .map_err(|error| Error::Message(error.to_string()))?,
    )?;
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

fn read_mimeapps(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    let mut in_defaults = false;
    for line in read_text(path)?.lines() {
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
    Ok(values)
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
            optional_packages: vec![],
            exec: vec![],
            xdg: crate::XdgProvider {
                desktop_entry: Some("browser.desktop".into()),
                mime_types: vec!["text/html".into()],
                uri_schemes: vec!["http".into()],
            },
            configuration: vec![],
            backend: None,
            settings: BTreeMap::new(),
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
                    optional_packages: vec![],
                    exec: vec![],
                    xdg: crate::XdgProvider {
                        desktop_entry: Some(desktop.into()),
                        mime_types: vec!["text/html".into()],
                        uri_schemes: vec![],
                    },
                    configuration: vec![],
                    backend: None,
                    settings: BTreeMap::new(),
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
