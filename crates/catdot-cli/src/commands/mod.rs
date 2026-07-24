use anyhow::{Context, Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::process::Command;

use crate::services::print_system_record_diagnostics;

mod runtime;

use runtime::{
    apply, component_for, exec_role, package_present, profiles, state_file, sync_active_settings,
    unresolved_packages,
};

#[derive(Parser)]
#[command(name = "catdot", about = "CatOS desktop profile manager")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}
#[derive(Subcommand)]
enum Cmd {
    List {
        profile: Option<String>,
    },
    Current,
    Select {
        first: String,
        reference: Option<String>,
    },
    Disable {
        role: String,
    },
    Apply,
    Adopt {
        role: String,
    },
    Exec {
        role: String,
        #[arg(trailing_var_arg = true)]
        arguments: Vec<String>,
    },
    Resolve {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        with_optional: bool,
    },
    Prune {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    Doctor,
    Users {
        #[command(subcommand)]
        command: UsersCmd,
    },
}
#[derive(Subcommand)]
enum UsersCmd {
    List,
    Prune {
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
        println!("\nRun:\n  catdot resolve");
    }
    Ok(())
}
fn helper(
    action: &str,
    state: &UserState,
    state_path: &std::path::Path,
    optional: bool,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    if action == "prune" {
        return prune_helper(dry_run, yes);
    }
    let uid_value = unsafe { libc::geteuid() };
    let uid = uid_value.to_string();
    let generation = state.generation.to_string();
    let state_path = state_path
        .to_str()
        .context("state path is not valid UTF-8")?;
    let output = Command::new("/usr/lib/catdot/catdot-helper")
        .args([
            "resolve-plan",
            "--uid",
            &uid,
            "--generation",
            &generation,
            "--state-path",
            state_path,
        ])
        .arg(if optional {
            "--with-optional"
        } else {
            "--without-optional"
        })
        .output()
        .context("obtain canonical package plan from catdot helper")?;
    if !output.status.success() {
        bail!(
            "helper could not create package plan: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    let preview: PackagePlanPreview = toml::from_str(&String::from_utf8_lossy(&output.stdout))
        .context("parse canonical package plan from helper")?;
    let plan = preview.plan;
    println!("Install:");
    for x in &plan.install {
        println!("  {x}");
        if let Some(requirement) = preview.requirements.get(x) {
            for reference in &requirement.references {
                println!(
                    "    required by uid {}: {}",
                    reference.uid, reference.component
                );
            }
        }
    }
    println!("Already satisfied:");
    for x in &plan.satisfied {
        println!("  {x}")
    }
    if dry_run {
        return Ok(());
    }
    confirm(yes)?;
    let digest = plan.digest();
    let status = Command::new("pkexec")
        .arg("/usr/lib/catdot/catdot-helper")
        .args([
            action,
            "--uid",
            &uid,
            "--generation",
            &generation,
            "--state-path",
            state_path,
            "--digest",
            &digest,
        ])
        .arg(if optional {
            "--with-optional"
        } else {
            "--without-optional"
        })
        .status()
        .context("start catdot helper through pkexec")?;
    if !status.success() {
        bail!("helper transaction failed")
    };
    Ok(())
}
fn prune_helper(dry_run: bool, yes: bool) -> Result<()> {
    let uid = unsafe { libc::geteuid() }.to_string();
    let output = Command::new("/usr/lib/catdot/catdot-helper")
        .args(["prune-plan", "--uid", &uid])
        .output()
        .context("obtain canonical prune plan from catdot helper")?;
    if !output.status.success() {
        bail!(
            "helper could not create prune plan: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    let plan: PackagePlan = toml::from_str(&String::from_utf8_lossy(&output.stdout))
        .context("parse canonical prune plan from helper")?;
    println!("Remove:");
    for package in &plan.remove {
        println!("  {package}")
    }
    if dry_run {
        return Ok(());
    }
    confirm(yes)?;
    let digest = plan.digest();
    let status = Command::new("pkexec")
        .arg("/usr/lib/catdot/catdot-helper")
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
        .context("start catdot helper through pkexec")?;
    if !status.success() {
        bail!("helper prune transaction failed")
    }
    Ok(())
}
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let registry = profiles()?;
    let ps = &registry.valid_profiles;
    let path = state_file()?;
    let is_doctor = matches!(&cli.command, Cmd::Doctor);
    let mutates_state = matches!(
        &cli.command,
        Cmd::Select { .. }
            | Cmd::Disable { .. }
            | Cmd::Apply
            | Cmd::Adopt { .. }
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
    let mut state = match read_state(&path) {
        Ok(state) => state,
        Err(error) if is_doctor => {
            println!("broken state: {error}");
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
                for (id, c) in &p.components {
                    println!("  {} ({})", id, c.role)
                }
            } else {
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
        Cmd::Current => {
            println!("generation = {}", state.generation);
            for (r, c) in state.components {
                println!("desired {r} = {c}")
            }
            for (r, c) in state.active_components {
                println!("active {r} = {c}")
            }
        }
        Cmd::Select { first, reference } => {
            if let Some(reference) = reference {
                select_component(&mut state, ps, &first, &reference)?;
            } else {
                let p = ps.get(&first).context("unknown profile")?;
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
            println!("Run: catdot resolve");
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
            if let Err(error) = apply(ps, &active, None, Some(&mut journal)) {
                recover_activation_journals(&path)?;
                return Err(error);
            }
            journal.complete()?;
            sync_active_settings(ps, &active)?;
            println!("Reapplied active configuration");
        }
        Cmd::Adopt { role } => {
            let mut journal = ActivationJournal::begin(&path, state.clone(), state.clone())?;
            journal.mark_applying()?;
            if let Err(error) = apply(ps, &state, Some(&role), Some(&mut journal)) {
                recover_activation_journals(&path)?;
                return Err(error);
            }
            journal.complete()?;
        }
        Cmd::Exec { role, arguments } => return exec_role(ps, &state, &role, &arguments),
        Cmd::Resolve {
            dry_run,
            yes,
            with_optional,
        } => {
            helper("resolve", &state, &path, with_optional, dry_run, yes)?;
            if !dry_run {
                let old_state = state.clone();
                let mut new_state = state.clone();
                new_state.active_components = new_state.components.clone();
                new_state.active_generation = new_state.generation;
                let mut journal = ActivationJournal::begin(&path, old_state, new_state.clone())?;
                journal.mark_applying()?;
                if let Err(error) = apply(ps, &state, None, Some(&mut journal)) {
                    recover_activation_journals(&path)?;
                    return Err(error);
                }
                write_state(&path, &new_state)?;
                journal.mark_state_written()?;
                journal.complete()?;
                state = new_state;
                sync_active_settings(ps, &state)?;
                let uid = unsafe { libc::geteuid() }.to_string();
                let generation = state.generation.to_string();
                let state_path = path.to_str().context("state path is not valid UTF-8")?;
                let status = Command::new("pkexec")
                    .arg("/usr/lib/catdot/catdot-helper")
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
                    bail!("helper finalize failed; active and pending requirements were retained")
                }
            }
        }
        Cmd::Prune { dry_run, yes } => helper("prune", &state, &path, false, dry_run, yes)?,
        Cmd::Doctor => {
            let transactions = activation_transactions_path(&path)?;
            if transactions.exists()
                && std::fs::read_dir(&transactions)?.any(|entry| {
                    entry.ok().is_some_and(|entry| {
                        entry.path().extension().is_some_and(|extension| extension == "toml")
                    })
                })
            {
                println!("warning: unfinished activation transaction; run catdot resolve or apply");
            }
            for diagnostic in &registry.diagnostics {
                println!(
                    "invalid profile: {} ({:?}): {}",
                    diagnostic.manifest_path.display(),
                    diagnostic.kind,
                    diagnostic.message
                );
            }
            if let Err(error) = validate_user_state(&state, ps) {
                println!("broken state: {error}");
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
                            println!("unresolved: {role} = {r}; missing {}", m.join(", "))
                        }
                    }
                    Err(e) => println!("broken: {role}: {e}"),
                }
            }
            print_system_record_diagnostics();
        }
        Cmd::Users { command } => match command {
            UsersCmd::List => {
                let uid = unsafe { libc::geteuid() }.to_string();
                let status = Command::new("pkexec")
                    .arg("/usr/lib/catdot/catdot-helper")
                    .args(["users-list", "--uid", &uid])
                    .status()
                    .context("start catdot helper through pkexec")?;
                if !status.success() {
                    bail!("helper users list failed")
                }
            }
            UsersCmd::Prune { yes } => {
                let plan_status = Command::new("pkexec")
                    .arg("/usr/lib/catdot/catdot-helper")
                    .arg("users-prune-plan")
                    .status()
                    .context("obtain stale user-record plan")?;
                if !plan_status.success() {
                    bail!("helper stale user-record plan failed")
                }
                confirm(yes)?;
                let status = Command::new("pkexec")
                    .arg("/usr/lib/catdot/catdot-helper")
                    .arg("users-prune")
                    .status()
                    .context("start catdot helper through pkexec")?;
                if !status.success() {
                    bail!("helper users prune failed")
                }
            }
        },
    }
    Ok(())
}
