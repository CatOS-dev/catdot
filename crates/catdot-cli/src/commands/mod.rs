use anyhow::{Context, Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::process::Command;

use crate::services::print_system_record_diagnostics;

mod runtime;

use runtime::{
    apply, component_for, deactivate, exec_role, package_present, profiles, state_file,
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
    let mut state = read_state(&path)?;
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
                println!("{r} = {c}")
            }
        }
        Cmd::Select { first, reference } => {
            let selected;
            if let Some(reference) = reference {
                select_component(&mut state, ps, &first, &reference)?;
                selected = format!("Selected {first}: {reference}");
            } else {
                let p = ps.get(&first).context("unknown profile")?;
                state = select_profile(p)?;
                state.generation = state.generation.max(read_state(&path)?.generation + 1);
                selected = format!("Selected profile {first}");
            }
            apply(ps, &state, None)?;
            write_state(&path, &state)?;
            println!("{selected}");
            print_missing_packages(ps, &state)?;
        }
        Cmd::Disable { role } => {
            if state.components.contains_key(&role)
                && let Err(error) = deactivate(ps, &state, &role)
            {
                eprintln!("warning: could not remove managed links for disabled {role}: {error}");
            }
            state.components.remove(&role);
            state.generation += 1;
            write_state(&path, &state)?;
        }
        Cmd::Apply => {
            apply(ps, &state, None)?;
            print_missing_packages(ps, &state)?;
        }
        Cmd::Adopt { role } => apply(ps, &state, Some(&role))?,
        Cmd::Exec { role, arguments } => return exec_role(ps, &state, &role, &arguments),
        Cmd::Resolve {
            dry_run,
            yes,
            with_optional,
        } => helper("resolve", &state, &path, with_optional, dry_run, yes)?,
        Cmd::Prune { dry_run, yes } => helper("prune", &state, &path, false, dry_run, yes)?,
        Cmd::Doctor => {
            for diagnostic in &registry.diagnostics {
                println!(
                    "invalid profile: {} ({:?}): {}",
                    diagnostic.manifest_path.display(),
                    diagnostic.kind,
                    diagnostic.message
                );
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
