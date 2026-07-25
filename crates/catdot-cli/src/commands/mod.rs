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
    apply, clear_profile_custom, component_for, exec_role, package_present, profiles, state_file,
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
    /// Check a system profile generation and reapply the active profile.
    #[command(about = "Reapply changed active profile inputs without installing packages")]
    Update,
    /// Reset the current active profile's custom areas and managed files.
    #[command(about = "Reset the current active profile")]
    Reset {
        /// Active profile ID to reset.
        profile: String,
    },
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

fn user_update_service_active() -> Option<bool> {
    let unit = std::env::var_os("CATDOT_USER_UPDATE_UNIT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/usr/lib/systemd/user/catdot-update.path".into());
    if !unit.exists() {
        return None;
    }
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", "catdot-update.path"])
        .status()
        .ok()
        .map(|status| status.success())
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivationIdentity {
    configuration: String,
    xdg: String,
}

#[derive(Debug, Clone)]
struct ActivationPreview {
    changes: bool,
    identity: ActivationIdentity,
}

#[derive(Debug, Clone)]
struct ResolvePreview {
    package: PackagePlanPreview,
    activation: ActivationPreview,
}

fn print_activation_plan(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    state_path: &std::path::Path,
) -> Result<ActivationPreview> {
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
    let plan = build_activation_preview(profiles, state, &home, &registry_path)?;
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
    let xdg_plan = build_xdg_plan(profiles, state, &runtime::xdg_config_home(&home))?;
    if xdg_plan.has_changes() {
        for (association, desktop) in &xdg_plan.defaults {
            println!("  xdg default: {association} -> {desktop}");
        }
        for association in xdg_plan.restore.keys() {
            println!("  xdg restore: {association}");
        }
    }
    let configuration_changes = plan.has_changes();
    let selection_changes = !activate.is_empty() || !change.is_empty() || !deactivate.is_empty();
    let xdg_changes = xdg_plan.has_changes();
    Ok(ActivationPreview {
        changes: selection_changes || configuration_changes || xdg_changes,
        identity: ActivationIdentity {
            configuration: plan.identity_digest(),
            xdg: xdg_plan.identity_digest(),
        },
    })
}

fn resolve_helper(
    profiles: &std::collections::BTreeMap<String, Profile>,
    state: &UserState,
    state_path: &std::path::Path,
    dry_run: bool,
    yes: bool,
) -> Result<Option<ResolvePreview>> {
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
        ],
        "helper could not create package plan",
    )?;
    let preview: PackagePlanPreview = toml::from_str(&String::from_utf8_lossy(&output.stdout))
        .context("parse canonical package plan from helper")?;
    print_package_plan(&preview);
    let activation = print_activation_plan(profiles, state, state_path)?;
    let needs_work =
        package_changes(&preview.plan) || activation.changes || preview.system_update_required;
    if !needs_work {
        println!("Catdot is already up to date.");
        return Ok(None);
    }
    if preview.system_update_required && !package_changes(&preview.plan) && !activation.changes {
        println!("System records need synchronization.");
    }
    if dry_run {
        return Ok(Some(ResolvePreview {
            package: preview,
            activation,
        }));
    }
    if package_changes(&preview.plan) || activation.changes {
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
        .status()
        .context("start Catdot package transaction")?;
    if !status.success() {
        bail!("helper transaction failed")
    }
    Ok(Some(ResolvePreview {
        package: preview,
        activation,
    }))
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

fn retain_available_active_components(
    state: &mut UserState,
    profiles: &std::collections::BTreeMap<String, Profile>,
) -> bool {
    let before = state.active_components.clone();
    state.active_components.retain(|role, reference| {
        let Some((profile_id, component_id)) = reference.split_once('/') else {
            return false;
        };
        profiles
            .get(profile_id)
            .and_then(|profile| profile.components.get(component_id))
            .is_some_and(|component| component.role == *role)
    });
    state
        .activation_digests
        .retain(|role, _| state.active_components.contains_key(role));
    state
        .active_package_digests
        .retain(|role, _| state.active_components.contains_key(role));
    before != state.active_components
}

pub fn run() -> Result<i32> {
    let cli = Cli::parse();
    let registry = profiles()?;
    let ps = &registry.valid_profiles;
    let path = state_file()?;
    let is_doctor = matches!(&cli.command, Cmd::Doctor);
    let dry_run = matches!(&cli.command, Cmd::Resolve { dry_run: true, .. });
    let mutates_state = !dry_run
        && matches!(
            &cli.command,
            Cmd::Select { .. }
                | Cmd::Disable { .. }
                | Cmd::Apply
                | Cmd::Update
                | Cmd::Reset { .. }
                | Cmd::Resolve { .. }
        );
    let _state_lock = if mutates_state {
        Some(lock(&state_lock_path(&path)?)?)
    } else {
        None
    };
    if mutates_state {
        recover_activation_journals(&path)?;
    }
    let mut state_read_broken = false;
    let mut state = match if mutates_state {
        initialize_state_from_default(&path, &default_declaration_path(), ps)
    } else {
        preview_state_from_default(&path, &default_declaration_path(), ps)
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
        match &cli.command {
            Cmd::Apply | Cmd::Update | Cmd::Reset { .. } | Cmd::Resolve { .. } => {
                validate_user_state(&state, ps)?;
            }
            Cmd::Exec { role, .. } => {
                if let Err(error) = validate_user_state(&state, ps) {
                    if let Some(reference) = state.active_components.get(role)
                        && let Some((profile_id, _)) = reference.split_once('/')
                        && let Some(diagnostic) = registry.diagnostics.iter().find(|diagnostic| {
                            diagnostic
                                .profile_directory
                                .file_name()
                                .is_some_and(|name| name == std::ffi::OsStr::new(profile_id))
                        })
                    {
                        bail!(
                            "active component {reference} has an invalid provider declaration: {}",
                            diagnostic.message
                        );
                    }
                    return Err(error.into());
                }
            }
            _ => {}
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
            let before = state.clone();
            retain_available_active_components(&mut state, ps);
            if let Some(reference) = component {
                select_component(&mut state, ps, &profile_or_role, &reference)?;
            } else {
                let p = ps.get(&profile_or_role).context("unknown profile")?;
                let desired = select_profile(p)?;
                if state.components != desired.components {
                    state.components = desired.components;
                    state.generation += 1;
                }
            }
            if state != before {
                write_state(&path, &state)?;
            }
            if state.components == before.components {
                println!("Desired selection is unchanged.");
            } else {
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
        }
        Cmd::Disable { role } => {
            let before = state.clone();
            retain_available_active_components(&mut state, ps);
            if state.components.remove(&role).is_some() {
                state.generation += 1;
            }
            if state != before {
                write_state(&path, &state)?;
            }
            if before.components.contains_key(&role) {
                println!("Disabled desired {role}; run: catdot resolve");
            } else {
                println!("Desired role {role} is already disabled.");
            }
        }
        Cmd::Apply | Cmd::Update => {
            let system_generation = read_system_generation()?;
            let mut active = state.clone();
            active.components = active.active_components.clone();
            let package_inputs = package_digests(ps, &active)?;
            let activation_inputs = activation_digests(ps, &active)?;
            if matches!(cli.command, Cmd::Update)
                && system_generation == state.active_system_generation
                && activation_inputs == state.activation_digests
                && package_inputs == state.active_package_digests
                && !state.needs_resolve
            {
                println!("Active profile is current.");
                return Ok(0);
            }
            let declares_packages = active.components.keys().any(|role| {
                component_for(ps, &active, role)
                    .is_ok_and(|(_, component, _)| !component.packages.is_empty())
            });
            let package_declaration_changed = package_inputs != state.active_package_digests
                && (!state.active_package_digests.is_empty() || declares_packages);
            if package_declaration_changed {
                state.needs_resolve = true;
                write_state(&path, &state)?;
                bail!("active profile dependency declarations changed; run: catdot resolve");
            }
            let missing = unresolved_packages(ps, &active)?;
            if !missing.is_empty() {
                state.needs_resolve = true;
                write_state(&path, &state)?;
                bail!(
                    "active profile needs resolve; missing {}",
                    missing.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
            if matches!(cli.command, Cmd::Update)
                && activation_inputs == state.activation_digests
                && package_inputs == state.active_package_digests
                && !state.needs_resolve
            {
                state.active_system_generation = system_generation;
                write_state(&path, &state)?;
                println!("Active profile is current.");
                return Ok(0);
            }
            let mut new_state = state.clone();
            new_state.activation_digests = activation_inputs;
            new_state.active_package_digests = package_inputs;
            new_state.active_system_generation = system_generation;
            new_state.needs_resolve = false;
            let mut journal = ActivationJournal::begin(&path, state.clone(), new_state.clone())?;
            journal.mark_applying()?;
            if let Err(error) = apply(ps, &active, &mut journal) {
                recover_activation_journals(&path)?;
                return Err(error);
            }
            write_state(&path, &new_state)?;
            journal.mark_state_written()?;
            journal.complete()?;
            println!("Reapplied active configuration");
        }
        Cmd::Reset { profile } => {
            let mut active = state.clone();
            active.components = active.active_components.clone();
            let mut journal = ActivationJournal::begin(&path, state.clone(), state.clone())?;
            journal.mark_applying()?;
            let reset =
                clear_profile_custom(ps, &state, &profile, &mut journal).and_then(|targets| {
                    let registry = managed_targets_path(&path)?;
                    journal.track_path(&registry)?;
                    forget_user_initialization(&registry, targets)?;
                    apply(ps, &active, &mut journal)
                });
            if let Err(error) = reset {
                recover_activation_journals(&path)?;
                return Err(error);
            }
            journal.complete()?;
            println!("Reset active profile {profile}");
        }
        Cmd::Exec { role, arguments } => {
            exec_role(ps, &state, &role, &arguments)?;
            return Ok(0);
        }
        Cmd::Resolve { dry_run, yes } => {
            let Some(preview) = resolve_helper(ps, &state, &path, dry_run, yes)? else {
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
                let configuration_plan =
                    build_activation_plan(refreshed_profiles, &state, &home, &registry_path)?;
                let xdg_plan =
                    build_xdg_plan(refreshed_profiles, &state, &runtime::xdg_config_home(&home))?;
                let actual_identity = ActivationIdentity {
                    configuration: configuration_plan.identity_digest(),
                    xdg: xdg_plan.identity_digest(),
                };
                if actual_identity != preview.activation.identity {
                    bail!("activation plan changed after confirmation; run catdot resolve again")
                }

                let selection_changes = state.active_components != state.components
                    || state.active_generation != state.generation;
                let activation_inputs = activation_digests(refreshed_profiles, &state)?;
                let package_inputs = package_digests(refreshed_profiles, &state)?;
                let activation_inputs_changed = activation_inputs != state.activation_digests;
                let package_inputs_changed = package_inputs != state.active_package_digests;
                let state_commit_needed = selection_changes
                    || preview.activation.changes
                    || activation_inputs_changed
                    || package_inputs_changed
                    || state.needs_resolve;

                if state_commit_needed {
                    println!("Applying user configuration...");
                    io::stdout().flush()?;
                    let mut new_state = state.clone();
                    new_state.active_components = new_state.components.clone();
                    new_state.active_generation = new_state.generation;
                    new_state.activation_digests = activation_inputs;
                    new_state.active_package_digests = package_inputs;
                    new_state.active_system_generation = read_system_generation()?;
                    new_state.needs_resolve = false;
                    let mut journal =
                        ActivationJournal::begin(&path, old_state.clone(), new_state.clone())?;
                    journal.mark_applying()?;
                    if preview.activation.changes || activation_inputs_changed {
                        let applied = activate_configuration(
                            &configuration_plan,
                            &registry_path,
                            &mut journal,
                        )
                        .and_then(|_| activate_xdg(&xdg_plan, &mut journal))
                        .and_then(|_| journal.mark_applied());
                        if let Err(error) = applied {
                            recover_activation_journals(&path)?;
                            return Err(error.into());
                        }
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
                print_success_summary(&old_state, &state, &preview.package.plan);
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
            let system_generation = read_system_generation()?;
            if system_generation != state.active_system_generation {
                println!(
                    "warning: system profile generation changed (system {system_generation}, user {})",
                    state.active_system_generation
                );
                warnings = true;
            }
            if state.needs_resolve {
                println!("warning: active profile needs resolve before it can be updated");
                warnings = true;
            }
            match user_update_service_active() {
                Some(true) => println!("ok: user update service is active"),
                Some(false) => {
                    println!("warning: user update service is inactive");
                    warnings = true;
                }
                None => println!("user update service status: not installed"),
            }
            if !state.active_components.is_empty() {
                let mut active = state.clone();
                active.components = active.active_components.clone();
                match activation_digests(ps, &active) {
                    Ok(digests) if digests != state.activation_digests => {
                        println!("warning: active profile inputs changed; run catdot update");
                        warnings = true;
                    }
                    Err(error) => {
                        println!("error: cannot read active profile inputs: {error}");
                        errors = true;
                    }
                    _ => {}
                }
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
            let pending_components = state.components != state.active_components;
            let mut inspected = Vec::new();
            if pending_components {
                let mut active = state.clone();
                active.components = active.active_components.clone();
                inspected.push(("active", active));
                inspected.push(("desired", state.clone()));
            } else {
                inspected.push(("", state.clone()));
            }
            for (label, inspected_state) in inspected {
                for role in inspected_state.components.keys() {
                    match component_for(ps, &inspected_state, role) {
                        Ok((_profile, component, reference)) => {
                            let missing: Vec<_> = component
                                .packages
                                .iter()
                                .filter(|package| !package_present(package))
                                .map(String::as_str)
                                .collect();
                            let prefix = if label.is_empty() {
                                String::new()
                            } else {
                                format!("{label} ")
                            };
                            if missing.is_empty() {
                                println!("ok: {prefix}{role} = {reference}")
                            } else {
                                println!(
                                    "error: unresolved {prefix}{role} = {reference}; missing {}",
                                    missing.join(", ")
                                );
                                errors = true;
                            }
                        }
                        Err(error) => {
                            let prefix = if label.is_empty() {
                                String::new()
                            } else {
                                format!("{label} ")
                            };
                            println!("error: broken {prefix}{role}: {error}");
                            errors = true;
                        }
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
