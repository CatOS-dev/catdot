use crate::auth::{caller_uid, user_home};
use crate::backend::{open_handle, prepared_removal_plan, removable_with_alpm, remove_with_alpm};
use crate::system::{load_records, user_record_path, valid_records};
use anyhow::{Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::{fs, path::PathBuf};

mod resolve;

pub(super) const DB: &str = "/var/lib/catdot";
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}
#[derive(Subcommand)]
enum Cmd {
    Resolve {
        #[arg(long)]
        uid: u32,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        state_path: PathBuf,
        #[arg(long, conflicts_with = "without_optional")]
        with_optional: bool,
        #[arg(long, conflicts_with = "with_optional")]
        without_optional: bool,
    },
    ResolvePlan {
        #[arg(long)]
        uid: u32,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        state_path: PathBuf,
        #[arg(long, conflicts_with = "without_optional")]
        with_optional: bool,
        #[arg(long, conflicts_with = "with_optional")]
        without_optional: bool,
    },
    Prune {
        #[arg(long)]
        uid: u32,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        without_optional: bool,
    },
    PrunePlan {
        #[arg(long)]
        uid: u32,
    },
    UsersPrune,
    UsersPrunePlan,
    UsersList {
        #[arg(long)]
        uid: u32,
    },
}
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::ResolvePlan {
            uid,
            generation,
            state_path,
            with_optional,
            ..
        } => {
            authorize_plan_caller(uid)?;
            resolve::print_plan(uid, generation, &state_path, with_optional)
        }
        Cmd::PrunePlan { uid } => {
            authorize_plan_caller(uid)?;
            print_prune_plan(uid)
        }
        Cmd::UsersPrune => {
            require_root()?;
            users_prune(caller_uid_from_pkexec()?)
        }
        Cmd::UsersPrunePlan => {
            require_root()?;
            users_prune_plan(caller_uid_from_pkexec()?)
        }
        Cmd::UsersList { uid } => {
            require_root()?;
            users_list(caller_uid(uid)?)
        }
        Cmd::Resolve {
            uid,
            generation,
            digest,
            state_path,
            with_optional,
            ..
        } => {
            require_root()?;
            resolve::apply(uid, generation, &state_path, &digest, with_optional)
        }
        Cmd::Prune {
            uid,
            generation,
            digest,
            ..
        } => {
            require_root()?;
            prune(uid, generation, &digest)
        }
    }
}

fn require_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("catdot-helper must run as root for this operation")
    }
    Ok(())
}

fn authorize_plan_caller(uid: u32) -> Result<()> {
    let effective_uid = unsafe { libc::geteuid() };
    if effective_uid == 0 {
        caller_uid(uid)?;
    } else {
        direct_plan_uid(effective_uid, uid)?;
    }
    Ok(())
}

fn direct_plan_uid(effective_uid: u32, uid: u32) -> Result<()> {
    if effective_uid != uid {
        bail!("read-only plan uid does not match the calling user")
    }
    Ok(())
}

fn prune(uid: u32, _generation: u64, digest: &str) -> Result<()> {
    caller_uid(uid)?;
    let _lock = lock(&PathBuf::from(DB).join("lock"))?;
    let database = PathBuf::from(DB);
    let mut state = read_system_packages(&database.join("packages.toml"))?;
    let mut handle = open_handle()?;
    let plan = canonical_prune_plan(&mut handle, &state)?;
    if plan.digest() != digest {
        bail!("plan changed; run catdot prune again")
    };
    remove_with_alpm(&mut handle, &plan.remove)?;
    for name in &plan.remove {
        state.packages.remove(name);
    }
    write_system_packages(&database.join("packages.toml"), &state)?;
    Ok(())
}

fn print_prune_plan(_uid: u32) -> Result<()> {
    let _lock = if unsafe { libc::geteuid() } == 0 {
        Some(lock(&PathBuf::from(DB).join("lock"))?)
    } else {
        None
    };
    let database = PathBuf::from(DB);
    let state = read_system_packages(&database.join("packages.toml"))?;
    let mut handle = open_handle()?;
    let plan = canonical_prune_plan(&mut handle, &state)?;
    print!("{}", toml::to_string(&plan)?);
    Ok(())
}

fn canonical_prune_plan(
    handle: &mut alpm::Alpm,
    state: &SystemPackageState,
) -> Result<PackagePlan> {
    let candidates: Vec<_> = state
        .packages
        .values()
        .filter(|package| prunable(package, false) && removable_with_alpm(handle, &package.name))
        .map(|package| package.name.clone())
        .collect();
    Ok(PackagePlan {
        install: vec![],
        remove: prepared_removal_plan(handle, &candidates)?,
        satisfied: vec![],
    })
}
fn caller_uid_from_pkexec() -> Result<u32> {
    let value = std::env::var("PKEXEC_UID")
        .map_err(|_| anyhow::anyhow!("missing PKEXEC_UID; helper must be launched by pkexec"))?;
    caller_uid(value.parse()?)
}
fn users_prune(_caller: u32) -> Result<()> {
    let _lock = lock(&PathBuf::from(DB).join("lock"))?;
    let database = PathBuf::from(DB);
    for uid in stale_user_record_uids()? {
        println!("removing stale record for uid {uid}");
        fs::remove_file(user_record_path(&database, uid))?;
    }
    refresh_package_references(&database)?;
    let state = read_system_packages(&database.join("packages.toml"))?;
    let mut handle = open_handle()?;
    let plan = canonical_prune_plan(&mut handle, &state)?;
    if !plan.remove.is_empty() {
        println!("Packages now eligible for catdot prune:");
        for package in plan.remove {
            println!("  {package}");
        }
    }
    Ok(())
}

fn users_prune_plan(_caller: u32) -> Result<()> {
    let _lock = lock(&PathBuf::from(DB).join("lock"))?;
    let stale = stale_user_record_uids()?;
    if stale.is_empty() {
        println!("No stale user records.");
    } else {
        println!("Remove stale user records:");
        for uid in stale {
            println!("  {uid}");
        }
    }
    Ok(())
}

fn stale_user_record_uids() -> Result<Vec<u32>> {
    let mut stale: Vec<_> = load_records(std::path::Path::new(DB))?
        .into_iter()
        .filter(|record| user_home(record.uid).is_err())
        .map(|record| record.uid)
        .collect();
    stale.sort();
    Ok(stale)
}

fn refresh_package_references(database: &std::path::Path) -> Result<()> {
    let records = valid_records(load_records(database)?, |uid| user_home(uid).is_ok());
    let requirements = aggregate_requirements(&records);
    let mut state = read_system_packages(&database.join("packages.toml"))?;
    for package in state.packages.values_mut() {
        package.references = requirements
            .get(&package.name)
            .map_or_else(Vec::new, |requirement| requirement.references.clone());
    }
    write_system_packages(&database.join("packages.toml"), &state)?;
    Ok(())
}
fn users_list(_caller: u32) -> Result<()> {
    for record in load_records(std::path::Path::new(DB))? {
        let status = if user_home(record.uid).is_ok() {
            "valid"
        } else {
            "missing"
        };
        println!("uid {}: {}", record.uid, status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::direct_plan_uid;

    #[test]
    fn nonprivileged_plan_requires_the_callers_own_uid() {
        assert!(direct_plan_uid(1000, 1000).is_ok());
        assert!(direct_plan_uid(1000, 1001).is_err());
    }
}
