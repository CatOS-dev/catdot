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

fn xdg_config_home(home: &std::path::Path) -> PathBuf {
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

fn link_target(link: &Link, home: &std::path::Path, xdg: &std::path::Path) -> Result<PathBuf> {
    if let Some(rest) = link.target.strip_prefix("{xdg_config_home}/") {
        Ok(xdg.join(rest))
    } else if let Some(rest) = link.target.strip_prefix("{home}/") {
        Ok(home.join(rest))
    } else {
        bail!("unsafe manifest target")
    }
}

pub(super) fn apply(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    adopt: Option<&str>,
) -> Result<()> {
    let home = home()?;
    let registry = managed_links_path(&state_file()?)?;
    let xdg = xdg_config_home(&home);
    let mut links = LinkTransaction::new(&registry)?;
    let desired = if let Some(role) = adopt {
        let (profile, component, _) = component_for(profiles, state, role)?;
        component
            .links
            .iter()
            .map(|link| {
                Ok((
                    link_target(link, &home, &xdg)?,
                    profile.root.join(&component.path).join(&link.source),
                ))
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        links.reconcile();
        desired_links(profiles, state, &home, &xdg, None)?
    };
    for (target, source) in desired {
        let should_adopt = adopt.is_some();
        if let Err(error) = links.stage(&source, &target, should_adopt) {
            return Err(error.into());
        }
    }
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
            if let Err(error) = apply_theme(component, &xdg, plasma) {
                let _ = links.rollback();
                return Err(error.into());
            }
            sync_gtk_settings(component);
        }
    }
    if let Err(error) = links.commit() {
        let _ = links.rollback();
        return Err(error.into());
    }
    Ok(())
}

pub(super) fn deactivate(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    role: &str,
) -> Result<()> {
    let registry = managed_links_path(&state_file()?)?;
    let home = home()?;
    let xdg = xdg_config_home(&home);
    reconcile_managed_links(
        &registry,
        &desired_links(profiles, state, &home, &xdg, Some(role))?,
        &[],
    )
    .map_err(Into::into)
}

fn desired_links(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    home: &std::path::Path,
    xdg: &std::path::Path,
    excluded_role: Option<&str>,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut desired = Vec::new();
    for (role, reference) in &state.components {
        if excluded_role == Some(role.as_str()) {
            continue;
        }
        let (profile, component, _) = component_for(profiles, state, role)?;
        if component
            .packages
            .iter()
            .any(|package| !package_present(package))
        {
            continue;
        }
        for link in &component.links {
            let source = profile.root.join(&component.path).join(&link.source);
            if !source.is_file() {
                bail!(
                    "{reference}: link source {} does not exist",
                    source.display()
                );
            }
            desired.push((link_target(link, home, xdg)?, source));
        }
    }
    Ok(desired)
}

pub(super) fn exec_role(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    role: &str,
    arguments: &[String],
) -> Result<()> {
    let (profile, component, reference) = component_for(profiles, state, role)?;
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
    let error = Command::new(&argv[0]).args(&argv[1..]).exec();
    Err(error.into())
}
