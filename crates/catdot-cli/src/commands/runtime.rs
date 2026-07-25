use crate::services::sync_gtk_settings;
use anyhow::{Context, Result, bail};
use catdot_core::*;
use std::{
    collections::BTreeSet,
    env,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
};

pub(super) fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(env::var("HOME").context("HOME is not set")?))
}

pub(super) fn profiles() -> Result<ProfileRegistry> {
    discover_profile_registry(&profile_root()).map_err(Into::into)
}

pub(super) fn state_file() -> Result<PathBuf> {
    Ok(state_path(&home()?))
}

pub(super) fn xdg_config_home(home: &std::path::Path) -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                home.join(path)
            }
        })
        .unwrap_or_else(|| home.join(".config"))
}

pub(super) fn component_for<'a>(
    profiles: &'a std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    role: &str,
) -> Result<(&'a Profile, &'a ComponentDef, String)> {
    let reference = state
        .components
        .get(role)
        .context("role is not selected")?
        .clone();
    let (profile_id, component_id) = reference
        .split_once('/')
        .context("invalid saved component reference")?;
    let profile = profiles
        .get(profile_id)
        .with_context(|| format!("profile {profile_id} is no longer installed"))?;
    let component = profile
        .components
        .get(component_id)
        .with_context(|| format!("component {reference} is no longer installed"))?;
    Ok((profile, component, reference))
}

pub(super) fn package_present(name: &str) -> bool {
    Command::new("pacman")
        .args(["-Q", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub(super) fn unresolved_packages(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
) -> Result<BTreeSet<String>> {
    let mut missing = BTreeSet::new();
    for role in state.components.keys() {
        let (_, component, _) = component_for(profiles, state, role)?;
        missing.extend(
            component
                .packages
                .iter()
                .filter(|package| !package_present(package))
                .cloned(),
        );
    }
    Ok(missing)
}

pub(super) fn apply(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    journal: &mut ActivationJournal,
) -> Result<()> {
    let home = home()?;
    let registry = managed_targets_path(&state_file()?)?;
    let xdg = xdg_config_home(&home);
    let plan = build_activation_plan(profiles, state, &home, &registry)?;
    activate_configuration(&plan, &registry, journal)?;
    let xdg_plan = build_xdg_plan(profiles, state, &xdg)?;
    activate_xdg(&xdg_plan, journal)?;
    for role in state.components.keys() {
        let (_profile, component, _) = component_for(profiles, state, role)?;
        if component
            .packages
            .iter()
            .any(|package| !package_present(package))
        {
            continue;
        }
        if component.backend.is_some() {
            let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
            let plasma = desktop.contains("KDE") || desktop.contains("Plasma");
            {
                for (path, contents) in theme_expected_files(component, &xdg, plasma)? {
                    journal.track_file(&path, &contents)?;
                }
            }
            if let Err(error) = apply_theme(component, &xdg, plasma) {
                return Err(error.into());
            }
        }
    }
    journal.mark_applied()?;
    Ok(())
}

pub(super) fn sync_active_settings(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
) -> Result<()> {
    for role in state.components.keys() {
        let (_, component, _) = component_for(profiles, state, role)?;
        if component
            .packages
            .iter()
            .all(|package| package_present(package))
        {
            sync_gtk_settings(component);
        }
    }
    Ok(())
}

pub(super) fn exec_role(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    role: &str,
    arguments: &[String],
) -> Result<()> {
    if !state.active_components.contains_key(role) {
        if state.components.contains_key(role) {
            bail!("{role} is selected but not activated; run: catdot resolve");
        }
        if profiles.values().any(|profile| {
            profile
                .components
                .values()
                .any(|component| component.role == role)
        }) {
            bail!("role {role} is not selected");
        }
        bail!("unknown role {role}");
    }
    let mut active = state.clone();
    active.components = active.active_components.clone();
    let (profile, component, reference) = component_for(profiles, &active, role)?;
    let missing: Vec<_> = component
        .packages
        .iter()
        .filter(|package| !package_present(package))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            "{role} ({reference}) is unresolved; missing {}; run: catdot resolve",
            missing.join(", ")
        )
    }
    let home = home()?;
    let xdg = xdg_config_home(&home).display().to_string();
    let component_id = reference.split_once('/').expect("validated reference").1;
    let argv = expand_exec(
        profile,
        component_id,
        &home.display().to_string(),
        &xdg,
        arguments,
    )?;
    if !binary_present(&argv[0]) {
        bail!(
            "{role} ({reference}) has an active exec provider, but binary {} is missing",
            argv[0]
        );
    }
    let error = Command::new(&argv[0]).args(&argv[1..]).exec();
    Err(error.into())
}

fn binary_present(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &std::path::Path| {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    if binary.contains('/') {
        return executable(std::path::Path::new(binary));
    }
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|directory| executable(&directory.join(binary)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, sync::Mutex};
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // Protects the activation ordering: if a later theme consumer rejects its
    // input after XDG has been written, journal recovery restores mimeapps.
    #[test]
    fn apply_rolls_back_xdg_when_a_later_theme_step_fails() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().unwrap();
        let config = temp.path().join(".config");
        fs::create_dir_all(&config).unwrap();
        let mimeapps = config.join("mimeapps.list");
        fs::write(
            &mimeapps,
            "[Default Applications]\ntext/html=outside.desktop;\n",
        )
        .unwrap();
        unsafe {
            env::set_var("HOME", temp.path());
            env::set_var("XDG_CONFIG_HOME", &config);
        }
        let component = ComponentDef {
            role: "browser".into(),
            packages: vec![],
            optional_packages: vec![],
            exec: vec![],
            xdg: XdgProvider {
                desktop_entry: Some("browser.desktop".into()),
                mime_types: vec!["text/html".into()],
                uri_schemes: vec![],
            },
            configuration: vec![],
            backend: Some("unsupported".into()),
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
        let state_path = state_path(temp.path());
        let mut journal =
            ActivationJournal::begin(&state_path, UserState::default(), UserState::default())
                .unwrap();
        journal.mark_applying().unwrap();
        assert!(apply(&profiles, &state, &mut journal).is_err());
        recover_activation_journals(&state_path).unwrap();
        assert_eq!(
            fs::read_to_string(mimeapps).unwrap(),
            "[Default Applications]\ntext/html=outside.desktop;\n"
        );
    }
}
