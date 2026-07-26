use anyhow::{Context, Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

mod runtime;

#[derive(Parser)]
#[command(
    name = "catdot",
    about = "Switch complete CatOS configuration profiles",
    long_about = "Install Profile packages with pacman, back up overwritten files, and switch complete configuration trees."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate Profile declarations under an explicit metadata root.
    Validate {
        #[arg(value_name = "PROFILE_ROOT")]
        profile_root: PathBuf,
    },
    /// List installed and retained Profiles.
    List,
    /// Show one installed or retained Profile.
    Show { profile: String },
    /// Show the active and retained Profiles.
    Current,
    /// Select and activate a complete Profile.
    Select { profile: String },
    /// Refresh a retained Profile from /usr/share.
    Update { profile: Option<String> },
    /// Forget an inactive retained Profile. Packages remain until prune.
    Remove { profile: String },
    /// Remove unreferenced direct packages introduced by Catdot.
    Prune,
}

fn validate_profile_root(root: &Path) -> Result<i32> {
    let registry = discover_profile_registry(root)?;
    for profile in registry.valid_profiles.values() {
        println!("validated profile {}", profile.id);
    }
    for diagnostic in &registry.diagnostics {
        eprintln!(
            "invalid profile {}: {}",
            diagnostic.manifest_path.display(),
            diagnostic.message
        );
    }
    if registry.valid_profiles.is_empty() && registry.diagnostics.is_empty() {
        eprintln!("no profiles found under {}", root.display());
        return Ok(2);
    }
    Ok(if registry.diagnostics.is_empty() {
        0
    } else {
        2
    })
}

fn installed_registry() -> Result<ProfileRegistry> {
    let registry = runtime::profiles()?;
    for diagnostic in &registry.diagnostics {
        eprintln!(
            "invalid profile {}: {}",
            diagnostic.manifest_path.display(),
            diagnostic.message
        );
    }
    Ok(registry)
}

fn package_installed(package: &str) -> Result<bool> {
    let status = Command::new("pacman")
        .args(["-Qq", "--", package])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("query installed package {package} with pacman"))?;
    Ok(status.success())
}

fn run_sudo_pacman(arguments: &[&str], packages: &[String], action: &str) -> Result<()> {
    let status = Command::new("sudo")
        .arg("pacman")
        .args(arguments)
        .arg("--")
        .args(packages)
        .status()
        .with_context(|| format!("run sudo pacman for {action}"))?;
    if !status.success() {
        bail!("pacman {action} failed with {status}")
    }
    Ok(())
}

fn install_packages(packages: &[String]) -> Result<BTreeSet<String>> {
    if packages.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut missing = BTreeSet::new();
    for package in packages {
        if !package_installed(package)? {
            missing.insert(package.clone());
        }
    }
    run_sudo_pacman(&["-S", "--needed"], packages, "installation")?;
    Ok(missing)
}

fn print_backup(backup: Option<PathBuf>) {
    if let Some(backup) = backup {
        println!("Backup: {}", backup.display());
    }
}

fn select_profile(
    profiles: &BTreeMap<String, Profile>,
    target: &str,
    home: &Path,
    state_file: &Path,
) -> Result<()> {
    let _lock = lock(&state_lock_path(state_file)?)?;
    let mut state = read_state(state_file)?;
    validate_user_state(&state)?;

    if !state.profiles.contains_key(target) {
        let profile = profiles
            .get(target)
            .with_context(|| format!("unknown Profile {target}"))?;
        let introduced = install_packages(&profile.packages)?;
        state.introduced_packages.extend(introduced);
        let cache = profile_cache_path(state_file, target)?;
        cache_profile_content(profile, &cache)?;
        state.profiles.insert(
            target.to_owned(),
            ProfileState::from_profile(profile, false)?,
        );
    } else {
        let packages = state.profiles[target]
            .packages
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let introduced = install_packages(&packages)?;
        state.introduced_packages.extend(introduced);
    }

    let cache = profile_cache_path(state_file, target)?;
    let target_state = state.profiles[target].clone();
    let plan = build_activation_plan(
        &state,
        target,
        &target_state,
        &cache,
        home,
        ActivationMode::Select,
    )?;
    let backup = apply_activation_plan(&plan, home, state_file)?;
    state
        .profiles
        .get_mut(target)
        .expect("target Profile was retained")
        .initialized = true;
    state.active_profile = Some(target.to_owned());
    write_state(state_file, &state)?;
    print_backup(backup);
    println!("Profile {target} is active.");
    Ok(())
}

fn update_retained_profile(
    profiles: &BTreeMap<String, Profile>,
    target: &str,
    home: &Path,
    state_file: &Path,
) -> Result<()> {
    let _lock = lock(&state_lock_path(state_file)?)?;
    let mut state = read_state(state_file)?;
    validate_user_state(&state)?;
    if !state.profiles.contains_key(target) {
        bail!("Profile {target} is not retained")
    }
    let profile = profiles
        .get(target)
        .with_context(|| format!("installed Profile {target} is unavailable or invalid"))?;
    let introduced = install_packages(&profile.packages)?;
    state.introduced_packages.extend(introduced);

    let old_state = state.clone();
    let initialized = state.profiles[target].initialized;
    let refreshed = ProfileState::from_profile(profile, initialized)?;
    let cache = profile_cache_path(state_file, target)?;
    cache_profile_content(profile, &cache)?;
    let plan = build_activation_plan(
        &old_state,
        target,
        &refreshed,
        &cache,
        home,
        ActivationMode::Update,
    )?;
    let backup = apply_activation_plan(&plan, home, state_file)?;
    state.profiles.insert(target.to_owned(), refreshed);
    write_state(state_file, &state)?;
    print_backup(backup);
    if state.active_profile.as_deref() == Some(target) {
        println!("Active Profile {target} was updated.");
    } else {
        println!("Retained Profile {target} was updated.");
    }
    Ok(())
}

fn remove_retained_profile(target: &str, state_file: &Path) -> Result<()> {
    let _lock = lock(&state_lock_path(state_file)?)?;
    let mut state = read_state(state_file)?;
    validate_user_state(&state)?;
    if !remove_profile(&mut state, target)? {
        bail!("Profile {target} is not retained")
    }
    let profile_cache = profile_cache_path(state_file, target)?;
    match fs::remove_dir_all(
        profile_cache
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Profile cache has no parent"))?,
    ) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    write_state(state_file, &state)?;
    println!("Profile {target} was forgotten. Run catdot prune to remove unused packages.");
    Ok(())
}

fn prune_packages(state_file: &Path) -> Result<()> {
    let _lock = lock(&state_lock_path(state_file)?)?;
    let mut state = read_state(state_file)?;
    validate_user_state(&state)?;
    let candidates = prune_candidates(&state);
    if candidates.is_empty() {
        println!("No packages are eligible for Catdot prune.");
        return Ok(());
    }

    let mut installed = Vec::new();
    for package in &candidates {
        if package_installed(package)? {
            installed.push(package.clone());
        }
    }
    if !installed.is_empty() {
        run_sudo_pacman(&["-Rns"], &installed, "prune")?;
    }
    for package in candidates {
        state.introduced_packages.remove(&package);
    }
    write_state(state_file, &state)?;
    if installed.is_empty() {
        println!("Removed stale Catdot package records; no installed packages required pruning.");
    } else {
        println!("Unused Catdot packages were pruned.");
    }
    Ok(())
}

fn print_profile(profile: &Profile, retained: bool, active: bool) {
    let marker = if active {
        "active"
    } else if retained {
        "retained"
    } else {
        "available"
    };
    println!("{} ({marker})", profile.id);
    println!("  {}", profile.name);
    if !profile.description.is_empty() {
        println!("  {}", profile.description);
    }
    if !profile.packages.is_empty() {
        println!("  packages: {}", profile.packages.join(", "));
    }
    if !profile.manage.is_empty() {
        println!(
            "  managed: {}",
            profile
                .manage
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn print_retained_profile(id: &str, profile: &ProfileState, active: bool) {
    let marker = if active {
        "active, unavailable"
    } else {
        "retained, unavailable"
    };
    println!("{id} ({marker})");
    if !profile.packages.is_empty() {
        println!(
            "  packages: {}",
            profile
                .packages
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

pub fn run() -> Result<i32> {
    let command = Cli::parse().command;
    if let Cmd::Validate { profile_root } = command {
        return validate_profile_root(&profile_root);
    }

    let home = runtime::home()?;
    let state_file = runtime::state_file()?;
    match command {
        Cmd::Validate { .. } => unreachable!(),
        Cmd::Current => {
            let state = read_state(&state_file)?;
            validate_user_state(&state)?;
            println!(
                "Active Profile: {}",
                state.active_profile.as_deref().unwrap_or("none")
            );
            if state.profiles.is_empty() {
                println!("Retained Profiles: none");
            } else {
                println!(
                    "Retained Profiles: {}",
                    state
                        .profiles
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Cmd::Prune => prune_packages(&state_file)?,
        Cmd::Remove { profile } => remove_retained_profile(&profile, &state_file)?,
        Cmd::List => {
            let registry = installed_registry()?;
            let state = read_state(&state_file)?;
            validate_user_state(&state)?;
            for profile in registry.valid_profiles.values() {
                print_profile(
                    profile,
                    state.profiles.contains_key(&profile.id),
                    state.active_profile.as_deref() == Some(&profile.id),
                );
            }
            for (id, profile) in &state.profiles {
                if !registry.valid_profiles.contains_key(id) {
                    print_retained_profile(
                        id,
                        profile,
                        state.active_profile.as_deref() == Some(id),
                    );
                }
            }
            if !registry.diagnostics.is_empty() {
                bail!("one or more installed Profiles are invalid")
            }
        }
        Cmd::Show { profile } => {
            let registry = installed_registry()?;
            let state = read_state(&state_file)?;
            validate_user_state(&state)?;
            if let Some(installed) = registry.valid_profiles.get(&profile) {
                print_profile(
                    installed,
                    state.profiles.contains_key(&profile),
                    state.active_profile.as_deref() == Some(&profile),
                );
            } else if let Some(retained) = state.profiles.get(&profile) {
                print_retained_profile(
                    &profile,
                    retained,
                    state.active_profile.as_deref() == Some(&profile),
                );
            } else {
                bail!("unknown Profile {profile}")
            }
        }
        Cmd::Select { profile } => {
            let registry = installed_registry()?;
            select_profile(&registry.valid_profiles, &profile, &home, &state_file)?;
        }
        Cmd::Update { profile } => {
            let registry = installed_registry()?;
            let target = match profile {
                Some(profile) => profile,
                None => read_state(&state_file)?
                    .active_profile
                    .context("no active Profile")?,
            };
            update_retained_profile(&registry.valid_profiles, &target, &home, &state_file)?;
        }
    }
    Ok(0)
}
