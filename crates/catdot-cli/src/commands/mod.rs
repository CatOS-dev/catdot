use anyhow::{Context, Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::{
    collections::BTreeSet,
    io::{self, Write},
    process::Command,
};

mod runtime;

use runtime::{
    apply, component_for, exec_role, package_present, profiles, state_file, sync_active_settings,
    unresolved_packages,
};

const MANAGE_HELPER: &str = "/usr/lib/catdot/catdot-helper";
const QUERY_HELPER: &str = "/usr/lib/catdot/catdot-query-helper";

#[derive(Parser)]
#[command(
    name = "catdot",
    about = "Manage CatOS desktop profiles safely",
    long_about = "Select, activate, diagnose, and remove CatOS desktop profile components while preserving the last working configuration until activation succeeds."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List installed profiles or inspect one profile.
    #[command(about = "List installed profiles")]
    List {
        /// Profile ID to inspect.
        #[arg(value_name = "PROFILE")]
        profile: Option<String>,
    },
    /// Show selected and active components.
    #[command(about = "Show selected and active components")]
    Current {
        /// Include state generation numbers.
        #[arg(long)]
        verbose: bool,
    },
    /// Select a complete profile or replace one component role.
    #[command(
        about = "Select a profile or component",
        after_help = "Examples:
  catdot select <PROFILE>
  catdot select <ROLE> <PROFILE/COMPONENT>"
    )]
    Select {
        /// Profile ID, or the role when selecting one component.
        #[arg(value_name = "PROFILE_OR_ROLE")]
        profile_or_role: String,
        /// Component reference used with the role form.
        #[arg(value_name = "PROFILE/COMPONENT")]
        component: Option<String>,
    },
    /// Stop selecting a component role after the next resolve.
    #[command(about = "Disable a selected component role")]
    Disable {
        /// Component role, such as terminal or bar.
        role: String,
    },
    /// Reapply the current active configuration without changing selection.
    #[command(about = "Reapply the active configuration")]
    Apply,
    /// Execute the active provider for a role.
    #[command(about = "Execute an active component role")]
    Exec {
        /// Active role to execute.
        role: String,
        /// Arguments appended directly to the component argv.
        #[arg(trailing_var_arg = true)]
        arguments: Vec<String>,
    },
    /// Validate dependencies and activate the desired configuration.
    #[command(about = "Resolve dependencies and activate selections")]
    Resolve {
        /// Show the complete plan without changing the system.
        #[arg(long)]
        dry_run: bool,
        /// Accept the displayed plan without an interactive prompt.
        #[arg(long)]
        yes: bool,
        /// Include optional component packages.
        #[arg(long)]
        with_optional: bool,
    },
    /// Remove unreferenced packages originally installed by Catdot.
    #[command(about = "Remove unused Catdot-managed packages")]
    Prune {
        /// Show the safe removal plan without changing the system.
        #[arg(long)]
        dry_run: bool,
        /// Accept the displayed removal plan without a prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Diagnose user configuration and privileged system records.
    #[command(about = "Diagnose user and system state")]
    Doctor,
    /// Inspect or remove Catdot records for system users.
    #[command(about = "Manage multi-user Catdot records")]
    Users {
        #[command(subcommand)]
        command: UsersCmd,
    },
}

#[derive(Subcommand)]
enum UsersCmd {
    /// List users known to Catdot.
    List,
    /// Remove records for users that no longer exist.
    Prune {
        /// Accept removal without an interactive prompt.
        #[arg(long)]
        yes: bool,
    },
}

fn confirm(yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!("refusing non-interactive transaction without --yes")
    }
    eprint!("Proceed? [Y/n] ");
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    if s.trim().is_empty() || s.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        bail!("cancelled")
    }
}

fn query_helper(args: &[&str], context: &str) -> Result<std::process::Output> {
    let output = Command::new("pkexec")
        .arg(QUERY_HELPER)
        .args(args)
        .output()
        .with_context(|| format!("{context} through the Catdot query helper"))?;
    if !output.status.success() {
        bail!(
            "{context}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(output)
}

fn system_doctor_report() -> Result<SystemDoctorReport> {
    match std::fs::symlink_metadata("/var/lib/catdot") {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SystemDoctorReport::default());
        }
        Ok(_) | Err(_) => {}
    }
    let uid = unsafe { libc::geteuid() }.to_string();
    let output = query_helper(
        &["doctor-system", "--uid", &uid],
        "helper could not inspect system Catdot state",
    )?;
    toml::from_str(&String::from_utf8_lossy(&output.stdout))
        .context("parse system Catdot diagnostics")
}

fn print_missing_packages(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
) -> Result<()> {
    let missing = unresolved_packages(profiles, state)?;
    if !missing.is_empty() {
        println!("Missing packages:");
        for package in missing {
            println!("  {package}");
        }
    }
    Ok(())
}

fn package_changes(plan: &PackagePlan) -> bool {
    !plan.install.is_empty() || !plan.remove.is_empty() || !plan.replacements.is_empty()
}

fn print_package_plan(preview: &PackagePlanPreview) {
    let plan = &preview.plan;
    if !package_changes(plan) {
        println!("No package changes are required.");
    }
    if !plan.install.is_empty() {
        println!("Install:");
        for package in &plan.install {
            println!("  {package}");
            if let Some(requirement) = preview.requirements.get(package) {
                for reference in &requirement.references {
                    println!(
                        "    required by uid {}: {}",
                        reference.uid, reference.component
                    );
                }
            }
        }
    }
    if !plan.satisfied.is_empty() {
        println!("Already available:");
        for package in &plan.satisfied {
            println!("  {package}");
        }
    }
    if !plan.replacements.is_empty() {
        println!("Replace:");
        for replacement in &plan.replacements {
            println!(
                "  {} -> {} ({})",
                replacement.remove, replacement.install, replacement.reason
            );
        }
    } else if !plan.remove.is_empty() {
        println!("Remove for installation:");
        for package in &plan.remove {
            println!("  {package}");
        }
    }
}

fn print_activation_plan(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    state_path: &std::path::Path,
) -> Result<bool> {
    let roles = state
        .components
        .keys()
        .chain(state.active_components.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut activate = Vec::new();
    let mut change = Vec::new();
    let mut deactivate = Vec::new();
    for role in roles {
        match (
            state.active_components.get(&role),
            state.components.get(&role),
        ) {
            (None, Some(desired)) => activate.push((role, desired.clone())),
            (Some(active), Some(desired)) if active != desired => {
                change.push((role, active.clone(), desired.clone()))
            }
            (Some(active), None) => deactivate.push((role, active.clone())),
            _ => {}
        }
    }
    if !activate.is_empty() {
        println!("Activate:");
        for (role, reference) in &activate {
            println!("  {role}: {reference}");
        }
    }
    if !change.is_empty() {
        println!("Change:");
        for (role, active, desired) in &change {
            println!("  {role}: {active} -> {desired}");
        }
    }
    if !deactivate.is_empty() {
        println!("Deactivate:");
        for (role, active) in &deactivate {
            println!("  {role}: {active}");
        }
    }
    let home = runtime::home()?;
    let registry_path = managed_targets_path(state_path)?;
    let plan = build_activation_plan(profiles, state, &home, &registry_path)?;
    for entry in &plan.entries {
        match &entry.materialization {
            Materialization::Generate { .. } => {
                println!("  generate: {} ({})", entry.target.display(), entry.owner)
            }
            Materialization::Symlink { source } => println!(
                "  symlink: {} -> {} ({})",
                entry.target.display(),
                source.display(),
                entry.owner
            ),
            Materialization::File { source } => println!(
                "  copy: {} -> {} ({})",
                source.display(),
                entry.target.display(),
                entry.owner
            ),
            Materialization::User { .. } => {
                println!(
                    "  preserve user: {} ({})",
                    entry.target.display(),
                    entry.owner
                )
            }
        }
    }
    for target in &plan.removals {
        println!("  delete managed target: {}", target.display());
    }
    let configuration_changes = plan
        .entries
        .iter()
        .any(|entry| match &entry.materialization {
            Materialization::Generate { contents } => {
                std::fs::read(&entry.target).map_or(true, |existing| existing != *contents)
            }
            Materialization::Symlink { source } => {
                std::fs::read_link(&entry.target).map_or(true, |existing| existing != *source)
            }
            Materialization::File { .. } => !entry.target.exists(),
            Materialization::User { seed } => seed.is_some() && !entry.target.exists(),
        })
        || plan.removals.iter().any(|target| target.exists());
    Ok(
        !(activate.is_empty() && change.is_empty() && deactivate.is_empty())
            || configuration_changes,
    )
}

fn resolve_helper(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    state_path: &std::path::Path,
    optional: bool,
    dry_run: bool,
    yes: bool,
) -> Result<Option<PackagePlanPreview>> {
    let uid = unsafe { libc::geteuid() }.to_string();
    let generation = state.generation.to_string();
    let state_path_text = state_path
        .to_str()
        .context("state path is not valid UTF-8")?;
    println!("Checking package requirements...");
    io::stdout().flush()?;
    let output = query_helper(
        &[
            "resolve-plan",
            "--uid",
            &uid,
            "--generation",
            &generation,
            "--state-path",
            state_path_text,
            if optional {
                "--with-optional"
            } else {
                "--without-optional"
            },
        ],
        "helper could not create package plan",
    )?;
    let preview: PackagePlanPreview = toml::from_str(&String::from_utf8_lossy(&output.stdout))
        .context("parse canonical package plan from helper")?;
    print_package_plan(&preview);
    let activation_changes = print_activation_plan(profiles, state, state_path)?;
    let needs_work =
        package_changes(&preview.plan) || activation_changes || preview.system_update_required;
    if !needs_work {
        println!("Catdot is already up to date.");
        return Ok(None);
    }
    if preview.system_update_required && !package_changes(&preview.plan) && !activation_changes {
        println!("System records need synchronization.");
    }
    if dry_run {
        return Ok(Some(preview));
    }
    if package_changes(&preview.plan) || activation_changes {
        confirm(yes)?;
    }
    if package_changes(&preview.plan) {
        println!("Applying package transaction...");
    } else {
        println!("Registering profile requirements...");
    }
    io::stdout().flush()?;
    let digest = preview.plan.digest();
    let status = Command::new("pkexec")
        .arg(MANAGE_HELPER)
        .args([
            "resolve",
            "--uid",
            &uid,
            "--generation",
            &generation,
            "--state-path",
            state_path_text,
            "--digest",
            &digest,
        ])
        .arg(if optional {
            "--with-optional"
        } else {
            "--without-optional"
        })
        .status()
        .context("start Catdot package transaction")?;
    if !status.success() {
        bail!("helper transaction failed")
    }
    Ok(Some(preview))
}

fn prune_helper(dry_run: bool, yes: bool) -> Result<()> {
    let uid = unsafe { libc::geteuid() }.to_string();
    println!("Checking for unused Catdot packages...");
    io::stdout().flush()?;
    let output = query_helper(
        &["prune-plan", "--uid", &uid],
        "helper could not create prune plan",
    )?;
    let preview: PackagePlanPreview = toml::from_str(&String::from_utf8_lossy(&output.stdout))
        .context("parse canonical prune plan from helper")?;
    let plan = preview.plan;
    if plan.remove.is_empty() {
        if preview.system_update_required {
            println!(
                "No packages are currently removable; an interrupted transaction needs recovery."
            );
            if dry_run {
                return Ok(());
            }
            println!("Recovering package records...");
        } else {
            println!("Nothing to prune.");
            return Ok(());
        }
    } else {
        println!("Remove:");
        for package in &plan.remove {
            println!("  {package}");
        }
        if dry_run {
            return Ok(());
        }
        confirm(yes)?;
        println!("Removing {} package(s)...", plan.remove.len());
    }
    io::stdout().flush()?;
    let digest = plan.digest();
    let status = Command::new("pkexec")
        .arg(MANAGE_HELPER)
        .args([
            "prune",
            "--uid",
            &uid,
            "--generation",
            "0",
            "--digest",
            &digest,
            "--without-optional",
        ])
        .status()
        .context("start Catdot prune transaction")?;
    if !status.success() {
        bail!("helper prune transaction failed")
    }
    if !plan.remove.is_empty() {
        println!("Pruned {} package(s).", plan.remove.len());
    } else {
        println!("Package records recovered.");
    }
    Ok(())
}

fn print_current(state: &UserState, verbose: bool) {
    if state.components.is_empty() && state.active_components.is_empty() {
        println!("No profile components are selected.");
        println!("Run `catdot list` to see installed profiles.");
        return;
    }
    if verbose {
        println!("Desired generation: {}", state.generation);
        println!("Active generation: {}", state.active_generation);
    }
    let pending =
        state.components != state.active_components || state.generation != state.active_generation;
    if !state.active_components.is_empty() {
        println!("Active components:");
        for (role, reference) in &state.active_components {
            println!("  {role}: {reference}");
        }
    }
    if pending {
        println!("Pending selection:");
        for (role, reference) in &state.components {
            match state.active_components.get(role) {
                Some(active) if active == reference => {}
                Some(active) => println!("  {role}: {active} -> {reference}"),
                None => println!("  {role}: activate {reference}"),
            }
        }
        for (role, active) in &state.active_components {
            if !state.components.contains_key(role) {
                println!("  {role}: deactivate {active}");
            }
        }
        println!("Run `catdot resolve` to apply pending changes.");
    }
}

fn print_success_summary(old: &UserState, new: &UserState, plan: &PackagePlan) {
    println!("Profile changes applied successfully.");
    let changed = old.active_components != new.active_components;
    if changed && !new.active_components.is_empty() {
        println!("Active components:");
        for (role, reference) in &new.active_components {
            println!("  {role}: {reference}");
        }
    }
    if !plan.install.is_empty() {
        println!("Installed or upgraded: {} package(s).", plan.install.len());
    }
    if !plan.replacements.is_empty() {
        println!("Replaced: {} package(s).", plan.replacements.len());
    }
}

pub fn run() -> Result<i32> {
    let cli = Cli::parse();
    let registry = profiles()?;
    let ps = &registry.valid_profiles;
    let path = state_file()?;
    let is_doctor = matches!(&cli.command, Cmd::Doctor);
    let dry_run = matches!(&cli.command, Cmd::Resolve { dry_run: true, .. });
    let mutates_state = !dry_run
        && (!path.exists()
            || matches!(
                &cli.command,
                Cmd::Select { .. } | Cmd::Disable { .. } | Cmd::Apply | Cmd::Resolve { .. }
            ));
    let _state_lock = if mutates_state {
        Some(lock(&state_lock_path(&path)?)?)
    } else {
        None
    };
    if mutates_state {
        recover_activation_journals(&path)?;
    }
    let mut state_read_broken = false;
    let mut state = match if dry_run {
        preview_state_from_default(&path, &default_declaration_path(), ps)
    } else {
        initialize_state_from_default(&path, &default_declaration_path(), ps)
    } {
        Ok(state) => state,
        Err(error) if is_doctor => {
            println!("error: broken state: {error}");
            state_read_broken = true;
            UserState::default()
        }
        Err(error) => return Err(error.into()),
    };
    if !is_doctor {
        if let Cmd::Disable { role } = &cli.command {
            let mut remaining = state.clone();
            remaining.components.remove(role);
            validate_user_state(&remaining, ps)?;
        } else {
            validate_user_state(&state, ps)?;
        }
    }
    match cli.command {
        Cmd::List { profile } => {
            if let Some(id) = profile {
                let p = match ps.get(&id) {
                    Some(profile) => profile,
                    None => {
                        if let Some(diagnostic) = registry.diagnostics.iter().find(|diagnostic| {
                            diagnostic
                                .profile_directory
                                .file_name()
                                .is_some_and(|name| name == std::ffi::OsStr::new(&id))
                        }) {
                            bail!("invalid profile {id}: {}", diagnostic.message);
                        }
                        bail!("unknown profile {id}");
                    }
                };
                println!("{} — {}", p.id, p.name);
                println!("{}", p.description);
                println!("Components:");
                for (id, component) in &p.components {
                    let default = p
                        .defaults
                        .get(&component.role)
                        .is_some_and(|selected| selected == id);
                    println!(
                        "  {}: {}{}",
                        component.role,
                        id,
                        if default { " (default)" } else { "" }
                    );
                }
            } else {
                if ps.is_empty() {
                    println!("No Catdot profiles are installed.");
                }
                for p in ps.values() {
                    println!("{} — {}", p.id, p.name)
                }
                if !registry.diagnostics.is_empty() {
                    println!(
                        "Invalid profiles: {} (run: catdot doctor)",
                        registry.diagnostics.len()
                    );
                }
            }
        }
        Cmd::Current { verbose } => print_current(&state, verbose),
        Cmd::Select {
            profile_or_role,
            component,
        } => {
            if let Some(reference) = component {
                select_component(&mut state, ps, &profile_or_role, &reference)?;
            } else {
                let p = ps.get(&profile_or_role).context("unknown profile")?;
                let desired = select_profile(p)?;
                state.components = desired.components;
                state.generation += 1;
            }
            write_state(&path, &state)?;
            for (role, reference) in &state.components {
                println!("Selected desired {role}: {reference}");
                match state.active_components.get(role) {
                    Some(active) => println!("Active {role} remains: {active}"),
                    None => println!("Active {role} remains: none"),
                }
            }
            print_missing_packages(ps, &state)?;
            println!("Next: catdot resolve");
        }
        Cmd::Disable { role } => {
            state.components.remove(&role);
            state.generation += 1;
            write_state(&path, &state)?;
            println!("Disabled desired {role}; run: catdot resolve");
        }
        Cmd::Apply => {
            let mut active = state.clone();
            active.components = active.active_components.clone();
            let mut journal = ActivationJournal::begin(&path, state.clone(), state.clone())?;
            journal.mark_applying()?;
            if let Err(error) = apply(ps, &active, &mut journal) {
                recover_activation_journals(&path)?;
                return Err(error);
            }
            journal.complete()?;
            sync_active_settings(ps, &active)?;
            println!("Reapplied active configuration");
        }
        Cmd::Exec { role, arguments } => {
            exec_role(ps, &state, &role, &arguments)?;
            return Ok(0);
        }
        Cmd::Resolve {
            dry_run,
            yes,
            with_optional,
        } => {
            let Some(preview) = resolve_helper(ps, &state, &path, with_optional, dry_run, yes)?
            else {
                return Ok(0);
            };
            if !dry_run {
                let old_state = state.clone();
                // Packages may have installed the profile's content tree or
                // changed its declarations. Never activate against the
                // pre-transaction registry.
                let refreshed_registry = profiles()?;
                let refreshed_profiles = &refreshed_registry.valid_profiles;
                validate_user_state(&state, refreshed_profiles)?;
                let missing = unresolved_packages(refreshed_profiles, &state)?;
                if !missing.is_empty() {
                    bail!(
                        "installed profile declarations require additional packages: {}; run catdot resolve again",
                        missing.into_iter().collect::<Vec<_>>().join(", ")
                    )
                }
                let home = runtime::home()?;
                let registry_path = managed_targets_path(&path)?;
                // This validates all post-install sources before any HOME
                // target is touched. `apply` builds the same plan again while
                // holding the activation journal.
                build_activation_plan(refreshed_profiles, &state, &home, &registry_path)?;
                let activation_changes = state.active_components != state.components
                    || state.active_generation != state.generation;
                if activation_changes {
                    println!("Applying user configuration...");
                    io::stdout().flush()?;
                    let mut new_state = state.clone();
                    new_state.active_components = new_state.components.clone();
                    new_state.active_generation = new_state.generation;
                    let mut journal =
                        ActivationJournal::begin(&path, old_state.clone(), new_state.clone())?;
                    journal.mark_applying()?;
                    if let Err(error) = apply(refreshed_profiles, &state, &mut journal) {
                        recover_activation_journals(&path)?;
                        return Err(error);
                    }
                    if let Err(error) = write_state(&path, &new_state) {
                        recover_activation_journals(&path)?;
                        return Err(error.into());
                    }
                    journal.mark_state_written()?;
                    state = new_state;
                    println!("Finalizing system records...");
                    io::stdout().flush()?;
                    let uid = unsafe { libc::geteuid() }.to_string();
                    let generation = state.generation.to_string();
                    let state_path = path.to_str().context("state path is not valid UTF-8")?;
                    let status = Command::new("pkexec")
                        .arg(MANAGE_HELPER)
                        .args([
                            "finalize",
                            "--uid",
                            &uid,
                            "--generation",
                            &generation,
                            "--state-path",
                            state_path,
                        ])
                        .status()
                        .context("finalize Catdot activation");
                    let status = match status {
                        Ok(status) => status,
                        Err(error) => {
                            journal.rollback()?;
                            return Err(error);
                        }
                    };
                    if !status.success() {
                        journal.rollback()?;
                        bail!("helper finalize failed; activation was rolled back")
                    }
                    journal.complete()?;
                    sync_active_settings(refreshed_profiles, &state)?;
                } else {
                    println!("Finalizing system records...");
                    io::stdout().flush()?;
                    let uid = unsafe { libc::geteuid() }.to_string();
                    let generation = state.generation.to_string();
                    let state_path = path.to_str().context("state path is not valid UTF-8")?;
                    let status = Command::new("pkexec")
                        .arg(MANAGE_HELPER)
                        .args([
                            "finalize",
                            "--uid",
                            &uid,
                            "--generation",
                            &generation,
                            "--state-path",
                            state_path,
                        ])
                        .status()
                        .context("finalize Catdot activation")?;
                    if !status.success() {
                        bail!("helper finalize failed; pending requirements were retained")
                    }
                }
                print_success_summary(&old_state, &state, &preview.plan);
            }
        }
        Cmd::Prune { dry_run, yes } => prune_helper(dry_run, yes)?,
        Cmd::Doctor => {
            let mut warnings = false;
            let mut errors = state_read_broken;
            let transactions = activation_transactions_path(&path)?;
            if transactions.exists()
                && std::fs::read_dir(&transactions)?.any(|entry| {
                    entry.ok().is_some_and(|entry| {
                        entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "toml")
                    })
                })
            {
                println!("warning: unfinished activation transaction; run catdot resolve or apply");
                warnings = true;
            }
            for diagnostic in &registry.diagnostics {
                println!(
                    "warning: invalid profile {} ({:?}): {}",
                    diagnostic.manifest_path.display(),
                    diagnostic.kind,
                    diagnostic.message
                );
                warnings = true;
            }
            if let Err(error) = validate_user_state(&state, ps) {
                println!("error: broken state: {error}");
                errors = true;
            }
            if state.generation != state.active_generation {
                println!(
                    "warning: pending activation (desired generation {}, active generation {})",
                    state.generation, state.active_generation
                );
                warnings = true;
            }
            for (target, managed) in read_managed_registry(&managed_targets_path(&path)?)?.entries {
                let metadata = std::fs::symlink_metadata(&target).ok();
                let matches = match managed.lifecycle.as_str() {
                    "overwrite/symlink" => {
                        metadata.is_some_and(|metadata| metadata.file_type().is_symlink())
                            && managed.source.as_ref().is_some_and(|source| {
                                std::fs::read_link(&target).ok().as_ref() == Some(source)
                            })
                    }
                    "generate" | "overwrite/file" => {
                        metadata.is_some_and(|metadata| !metadata.file_type().is_symlink())
                    }
                    _ => false,
                };
                if !matches {
                    println!("warning: missing or damaged managed target: {target}");
                    warnings = true;
                }
            }
            for role in state.components.keys() {
                match component_for(ps, &state, role) {
                    Ok((_p, c, r)) => {
                        let m: Vec<_> = c
                            .packages
                            .iter()
                            .filter(|x| !package_present(x))
                            .map(|x| x.as_str())
                            .collect();
                        if m.is_empty() {
                            println!("ok: {role} = {r}")
                        } else {
                            println!("error: unresolved: {role} = {r}; missing {}", m.join(", "));
                            errors = true;
                        }
                    }
                    Err(e) => {
                        println!("error: broken: {role}: {e}");
                        errors = true;
                    }
                }
            }
            let system = system_doctor_report()?;
            let has_system_output = !system.lines.is_empty();
            for line in system.lines {
                println!("{line}");
            }
            warnings |= system.warnings != 0;
            errors |= system.errors != 0;
            if !warnings
                && !errors
                && state.components.is_empty()
                && state.active_components.is_empty()
                && !has_system_output
            {
                println!("ok: Catdot is healthy; no profile components are selected");
            }
            return Ok(if errors {
                2
            } else if warnings {
                1
            } else {
                0
            });
        }
        Cmd::Users { command } => match command {
            UsersCmd::List => {
                let uid = unsafe { libc::geteuid() }.to_string();
                let status = Command::new("pkexec")
                    .arg(QUERY_HELPER)
                    .args(["users-list", "--uid", &uid])
                    .status()
                    .context("start catdot helper through pkexec")?;
                if !status.success() {
                    bail!("helper users list failed")
                }
            }
            UsersCmd::Prune { yes } => {
                let plan = Command::new("pkexec")
                    .arg(QUERY_HELPER)
                    .arg("users-prune-plan")
                    .output()
                    .context("obtain stale user-record plan")?;
                if !plan.status.success() {
                    bail!("helper stale user-record plan failed")
                }
                let plan_text = String::from_utf8_lossy(&plan.stdout);
                print!("{plan_text}");
                if plan_text.contains("No stale user records.") {
                    return Ok(0);
                }
                confirm(yes)?;
                let status = Command::new("pkexec")
                    .arg(MANAGE_HELPER)
                    .arg("users-prune")
                    .status()
                    .context("start catdot helper through pkexec")?;
                if !status.success() {
                    bail!("helper users prune failed")
                }
            }
        },
    }
    Ok(0)
}
