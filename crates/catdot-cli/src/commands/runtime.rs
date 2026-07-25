use anyhow::{Context, Result, bail};
use catdot_core::*;
use std::{
    collections::BTreeSet,
    env, fs,
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
        .args(["-T", name])
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
    journal.mark_applied()?;
    Ok(())
}

/// Remove only the explicit user areas owned by the active profile.  The
/// caller's activation journal snapshots each path first, so reset retains the
/// same binary-safe rollback and backup semantics as a normal activation.
pub(super) fn clear_profile_custom(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    profile_id: &str,
    journal: &mut ActivationJournal,
) -> Result<()> {
    let home = home()?;
    let active_components: Vec<_> = state
        .active_components
        .values()
        .filter(|reference| reference.starts_with(&format!("{profile_id}/")))
        .collect();
    if active_components.is_empty() {
        bail!("refusing to reset {profile_id}: it is not the current active profile");
    }
    for reference in active_components {
        let (_, component_id) = reference
            .split_once('/')
            .context("invalid active component reference")?;
        let profile = profiles
            .get(profile_id)
            .context("active profile is no longer installed")?;
        let component = profile
            .components
            .get(component_id)
            .with_context(|| format!("active component {reference} is no longer installed"))?;
        for entry in &component.configuration {
            if !matches!(entry.lifecycle, Lifecycle::User) {
                continue;
            }
            let target = home.join(&entry.target);
            journal.track_path(&target)?;
            match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
                    fs::remove_file(&target)?
                }
                Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&target)?,
                Ok(_) => bail!("unsupported user target {}", target.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
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
    use super::package_present;
    use std::{env, fs, os::unix::fs::PermissionsExt, sync::Mutex};
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // Dependency checks must understand versions and virtual providers rather
    // than treating the manifest expression as a literal installed package name.
    #[test]
    fn package_presence_uses_dependency_satisfaction() {
        let _lock = ENV_LOCK.lock().unwrap();
        let directory = tempdir().unwrap();
        let pacman = directory.path().join("pacman");
        fs::write(
            &pacman,
            "#!/bin/sh\ntest \"$1\" = -T && test \"$2\" = 'virtual-provider>=2'\n",
        )
        .unwrap();
        fs::set_permissions(&pacman, fs::Permissions::from_mode(0o755)).unwrap();
        let old_path = env::var_os("PATH");
        unsafe { env::set_var("PATH", directory.path()) };
        assert!(package_present("virtual-provider>=2"));
        assert!(!package_present("literal-package"));
        match old_path {
            Some(path) => unsafe { env::set_var("PATH", path) },
            None => unsafe { env::remove_var("PATH") },
        }
    }
}
