use crate::backend::{
    hold_packages, open_handle, prepared_removal_plan, removable_with_alpm, remove_with_alpm,
};
use crate::system::{load_records, user_record_path, valid_records, write_system_file};
use crate::{
    HelperMode,
    auth::{caller_uid, read_trusted_user_state, user_home},
};
use anyhow::{Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::{fs, path::PathBuf};

mod package_journal;
mod resolve;

use package_journal::{PruneJournal, recover_pending};

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
    },
    ResolvePlan {
        #[arg(long)]
        uid: u32,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        state_path: PathBuf,
    },
    Finalize {
        #[arg(long)]
        uid: u32,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        state_path: PathBuf,
    },
    Prune {
        #[arg(long)]
        uid: u32,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        digest: String,
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
    DoctorSystem {
        #[arg(long)]
        uid: u32,
    },
}
pub fn run(mode: HelperMode) -> Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        unsafe { libc::umask(0o022) };
    }
    let cli = Cli::parse();
    validate_mode(mode, &cli.command)?;
    match cli.command {
        Cmd::ResolvePlan {
            uid,
            generation,
            state_path,
        } => {
            require_root()?;
            caller_uid(uid)?;
            resolve::print_plan(uid, generation, &state_path)
        }
        Cmd::PrunePlan { uid } => {
            require_root()?;
            caller_uid(uid)?;
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
        Cmd::DoctorSystem { uid } => {
            require_root()?;
            system_doctor(caller_uid(uid)?)
        }
        Cmd::Resolve {
            uid,
            generation,
            digest,
            state_path,
        } => {
            require_root()?;
            resolve::apply(uid, generation, &state_path, &digest)
        }
        Cmd::Finalize {
            uid,
            generation,
            state_path,
        } => {
            require_root()?;
            resolve::finalize(uid, generation, &state_path)
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

fn validate_mode(mode: HelperMode, command: &Cmd) -> Result<()> {
    let query = matches!(
        command,
        Cmd::ResolvePlan { .. }
            | Cmd::PrunePlan { .. }
            | Cmd::UsersPrunePlan
            | Cmd::UsersList { .. }
            | Cmd::DoctorSystem { .. }
    );
    match (mode, query) {
        (HelperMode::Query, true) | (HelperMode::Manage, false) => Ok(()),
        (HelperMode::Query, false) => bail!("query helper refuses mutating operations"),
        (HelperMode::Manage, true) => bail!("manage helper refuses read-only operations"),
    }
}

fn require_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("catdot-helper must run as root for this operation")
    }
    Ok(())
}

fn prune(uid: u32, _generation: u64, digest: &str) -> Result<()> {
    caller_uid(uid)?;
    let _lock = lock(&PathBuf::from(DB).join("lock"))?;
    let database = PathBuf::from(DB);
    let mut handle = open_handle()?;
    recover_pending(&database, |name| handle.localdb().pkg(name).is_ok())?;
    let mut state = read_system_packages(&database.join("packages.toml"))?;
    let plan = canonical_prune_plan(&mut handle, &state)?;
    if plan.digest() != digest {
        bail!("plan changed; run catdot prune again")
    };
    for name in &plan.remove {
        state.packages.remove(name);
    }
    let mut journal = PruneJournal::prepared(&database, &plan, state)?;
    journal.verify(digest)?;
    remove_with_alpm(&mut handle, &plan.remove)?;
    journal.mark_alpm_committed()?;
    write_system_file(
        &database.join("packages.toml"),
        &toml::to_string_pretty(journal.expected_packages())?,
    )?;
    journal.mark_records_committed()?;
    journal.complete()?;
    Ok(())
}

fn print_prune_plan(_uid: u32) -> Result<()> {
    let database = PathBuf::from(DB);
    let state = read_system_packages(&database.join("packages.toml"))?;
    let mut handle = open_handle()?;
    let plan = canonical_prune_plan(&mut handle, &state)?;
    let preview = PackagePlanPreview {
        plan,
        requirements: std::collections::BTreeMap::new(),
        system_update_required: has_pending_transactions(&database)?,
    };
    print!("{}", toml::to_string(&preview)?);
    Ok(())
}

fn has_pending_transactions(database: &std::path::Path) -> Result<bool> {
    let directory = database.join("transactions");
    match fs::read_dir(&directory) {
        Ok(entries) => {
            for entry in entries {
                if entry?
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "toml")
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn canonical_prune_plan(
    handle: &mut alpm::Alpm,
    state: &SystemPackageState,
) -> Result<PackagePlan> {
    let held = hold_packages()?;
    let mut candidates = state
        .packages
        .values()
        .filter(|package| {
            prunable(package, false)
                && removable_with_alpm(handle, &package.name)
                && !held.contains(&package.name)
        })
        .map(|package| package.name.clone())
        .collect::<std::collections::BTreeSet<_>>();

    loop {
        let mut externally_required = std::collections::BTreeSet::new();
        for package in handle.localdb().pkgs().iter() {
            if candidates.contains(package.name()) {
                continue;
            }
            for dependency in package.depends().iter() {
                if let Some(provider) = handle
                    .localdb()
                    .pkgs()
                    .find_satisfier(dependency.to_string())
                    && candidates.contains(provider.name())
                {
                    externally_required.insert(provider.name().to_owned());
                }
            }
        }
        if externally_required.is_empty() {
            break;
        }
        for package in externally_required {
            candidates.remove(&package);
        }
    }

    let names = candidates.into_iter().collect::<Vec<_>>();
    let remove = prepared_removal_plan(handle, &names)?;
    Ok(PackagePlan {
        install: vec![],
        remove,
        replacements: vec![],
        satisfied: vec![],
    })
}

fn system_doctor(_caller: u32) -> Result<()> {
    let database = PathBuf::from(DB);
    let handle = open_handle()?;
    let records = load_records(&database)?;
    let mut report = SystemDoctorReport::default();

    for record in records {
        match user_home(record.uid) {
            Ok(_) => {
                report
                    .lines
                    .push(format!("system user record: uid {}: valid", record.uid));
                match read_trusted_user_state(record.uid, &record.state_path) {
                    Ok(state) => {
                        if state.active_generation != record.active_generation
                            || state.generation != record.pending_generation
                        {
                            report.lines.push(format!(
                                "warning: uid {} state generations differ from the system record",
                                record.uid
                            ));
                            report.warnings += 1;
                        }
                        if !record.pending_requirements.is_empty() {
                            report.lines.push(format!(
                                "warning: uid {} has pending package requirements",
                                record.uid
                            ));
                            report.warnings += 1;
                        }
                    }
                    Err(error) => {
                        report.lines.push(format!(
                            "error: uid {} state cannot be verified: {error}",
                            record.uid
                        ));
                        report.errors += 1;
                    }
                }
            }
            Err(_) => {
                report.lines.push(format!(
                    "warning: system user record: uid {}: missing",
                    record.uid
                ));
                report.warnings += 1;
            }
        }
    }

    let packages = read_system_packages(&database.join("packages.toml"))?;
    for managed in packages.packages.values() {
        match handle.localdb().pkg(managed.name.as_str()) {
            Ok(package) => {
                if managed.install_reason == InstallReason::Dependency
                    && package.reason() == alpm::PackageReason::Explicit
                {
                    report.lines.push(format!(
                        "warning: package {} was explicitly adopted by the administrator",
                        managed.name
                    ));
                    report.warnings += 1;
                }
            }
            Err(_) => {
                report.lines.push(format!(
                    "error: Catdot package record {} is not installed",
                    managed.name
                ));
                report.errors += 1;
            }
        }
    }

    let transactions = database.join("transactions");
    if let Ok(entries) = fs::read_dir(&transactions) {
        for entry in entries {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                report.lines.push(format!(
                    "warning: unfinished package transaction: {}",
                    path.display()
                ));
                report.warnings += 1;
            }
        }
    }

    print!("{}", toml::to_string(&report)?);
    Ok(())
}

fn caller_uid_from_pkexec() -> Result<u32> {
    let value = std::env::var("PKEXEC_UID")
        .map_err(|_| anyhow::anyhow!("missing PKEXEC_UID; helper must be launched by pkexec"))?;
    caller_uid(value.parse()?)
}
fn users_prune(_caller: u32) -> Result<()> {
    let _lock = lock(&PathBuf::from(DB).join("lock"))?;
    let database = PathBuf::from(DB);
    let mut handle = open_handle()?;
    recover_pending(&database, |name| handle.localdb().pkg(name).is_ok())?;
    for uid in stale_user_record_uids()? {
        println!("removing stale record for uid {uid}");
        fs::remove_file(user_record_path(&database, uid))?;
    }
    refresh_package_references(&database)?;
    let state = read_system_packages(&database.join("packages.toml"))?;
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
    write_system_file(
        &database.join("packages.toml"),
        &toml::to_string_pretty(&state)?,
    )?;
    Ok(())
}
fn users_list(_caller: u32) -> Result<()> {
    let records = load_records(std::path::Path::new(DB))?;
    if records.is_empty() {
        println!("No Catdot user records.");
        return Ok(());
    }
    for record in records {
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
    use super::{Cmd, validate_mode};
    use crate::HelperMode;
    use std::path::PathBuf;

    #[test]
    fn query_and_manage_helpers_reject_the_other_mode() {
        let query = Cmd::PrunePlan { uid: 1000 };
        let manage = Cmd::Prune {
            uid: 1000,
            generation: 0,
            digest: "digest".into(),
        };
        assert!(validate_mode(HelperMode::Query, &query).is_ok());
        assert!(validate_mode(HelperMode::Manage, &manage).is_ok());
        assert!(validate_mode(HelperMode::Manage, &query).is_err());
        assert!(validate_mode(HelperMode::Query, &manage).is_err());

        let resolve = Cmd::ResolvePlan {
            uid: 1000,
            generation: 1,
            state_path: PathBuf::from("/home/test/state.toml"),
        };
        assert!(validate_mode(HelperMode::Query, &resolve).is_ok());
    }
}
