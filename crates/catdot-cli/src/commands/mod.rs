use anyhow::{Context, Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
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

fn installed_packages() -> Result<BTreeSet<String>> {
    let output = Command::new("pacman")
        .arg("-Qq")
        .output()
        .context("query installed packages with pacman")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "pacman package query failed with {}: {}",
            output.status,
            stderr.trim()
        )
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
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

fn install_packages(
    packages: &[String],
    installed_before: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if packages.is_empty() {
        return Ok(BTreeSet::new());
    }
    run_sudo_pacman(&["-S", "--needed"], packages, "installation")?;
    Ok(packages
        .iter()
        .filter(|package| !installed_before.contains(*package))
        .cloned()
        .collect())
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
    let first_activation = !state.profiles.contains_key(target);

    let (target_state, packages, preflight, profile) = if first_activation {
        let profile = profiles
            .get(target)
            .with_context(|| format!("unknown Profile {target}"))?;
        let target_state = ProfileState::from_profile(profile);
        let plan = build_activation_plan(
            &state,
            target,
            &target_state,
            ActivationSources {
                managed: &profile.source_root,
                seeds: Some(&profile.source_root),
            },
            home,
            state_file,
            ActivationMode::Select,
        )?;
        (target_state, profile.packages.clone(), plan, Some(profile))
    } else {
        let target_state = state.profiles[target].clone();
        let cache = profile_cache_path(state_file, target)?;
        let plan = build_activation_plan(
            &state,
            target,
            &target_state,
            ActivationSources {
                managed: &cache,
                seeds: None,
            },
            home,
            state_file,
            ActivationMode::Select,
        )?;
        (
            target_state.clone(),
            target_state.packages.iter().cloned().collect(),
            plan,
            None,
        )
    };

    let installed_before = if packages.is_empty() {
        BTreeSet::new()
    } else {
        installed_packages()?
    };
    let introduced = install_packages(&packages, &installed_before)?;
    state.introduced_packages.extend(introduced);

    let plan = if let Some(profile) = profile {
        let cache = profile_cache_path(state_file, target)?;
        cache_profile_content(profile, &cache)?;
        build_activation_plan(
            &state,
            target,
            &target_state,
            ActivationSources {
                managed: &cache,
                seeds: Some(&profile.source_root),
            },
            home,
            state_file,
            ActivationMode::Select,
        )?
    } else {
        preflight
    };

    let backup = apply_activation_plan(&plan, home, state_file)?;
    state.profiles.insert(target.to_owned(), target_state);
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
    let refreshed = ProfileState::from_profile(profile);

    build_activation_plan(
        &state,
        target,
        &refreshed,
        ActivationSources {
            managed: &profile.source_root,
            seeds: None,
        },
        home,
        state_file,
        ActivationMode::Update,
    )?;

    let installed_before = if profile.packages.is_empty() {
        BTreeSet::new()
    } else {
        installed_packages()?
    };
    let introduced = install_packages(&profile.packages, &installed_before)?;
    state.introduced_packages.extend(introduced);

    let cache = profile_cache_path(state_file, target)?;
    cache_profile_content(profile, &cache)?;
    let plan = build_activation_plan(
        &state,
        target,
        &refreshed,
        ActivationSources {
            managed: &cache,
            seeds: None,
        },
        home,
        state_file,
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

    let installed_packages = installed_packages()?;
    let installed = candidates
        .iter()
        .filter(|package| installed_packages.contains(*package))
        .cloned()
        .collect::<Vec<_>>();
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

fn print_available_profile(profile: &Profile) {
    println!("{} (available)", profile.id);
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

fn print_retained_profile(
    id: &str,
    profile: &ProfileState,
    active: bool,
    installed: Option<&Profile>,
) {
    let marker = match (active, installed.is_some()) {
        (true, false) => "active, unavailable",
        (false, false) => "retained, unavailable",
        (true, true) => "active",
        (false, true) => "retained",
    };
    println!("{id} ({marker})");
    println!("  {}", profile.name);
    if !profile.description.is_empty() {
        println!("  {}", profile.description);
    }
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
            for (id, retained) in &state.profiles {
                print_retained_profile(
                    id,
                    retained,
                    state.active_profile.as_deref() == Some(id),
                    registry.valid_profiles.get(id),
                );
            }
            for profile in registry.valid_profiles.values() {
                if !state.profiles.contains_key(&profile.id) {
                    print_available_profile(profile);
                }
            }
        }
        Cmd::Show { profile } => {
            let registry = installed_registry()?;
            let state = read_state(&state_file)?;
            validate_user_state(&state)?;
            if let Some(retained) = state.profiles.get(&profile) {
                print_retained_profile(
                    &profile,
                    retained,
                    state.active_profile.as_deref() == Some(&profile),
                    registry.valid_profiles.get(&profile),
                );
            } else if let Some(installed) = registry.valid_profiles.get(&profile) {
                print_available_profile(installed);
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
