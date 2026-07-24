use super::package_journal::{
    PackageJournal, PreparedTransaction, commit_records, recover_pending,
};
use crate::{
    auth::{caller_uid, read_trusted_user_state, user_home},
    backend::{install_with_alpm, open_handle, prepared_install_plan, satisfier_name},
    system::{load_records, replace_record, user_record_path, valid_records},
};
use anyhow::{Result, bail};
use catdot_core::*;
use std::{collections::BTreeMap, path::Path};

use super::DB;

struct ResolveContext {
    record: UserRecord,
    records: Vec<UserRecord>,
    plan: PackagePlan,
}

pub(super) fn print_plan(
    uid: u32,
    generation: u64,
    state_path: &Path,
    optional: bool,
) -> Result<()> {
    let _lock = if unsafe { libc::geteuid() } == 0 {
        Some(lock(&Path::new(DB).join("lock"))?)
    } else {
        None
    };
    let context = prepare(uid, generation, state_path, optional)?;
    let preview = PackagePlanPreview {
        plan: context.plan,
        requirements: aggregate_requirements(&context.records),
    };
    print!("{}", toml::to_string(&preview)?);
    Ok(())
}

pub(super) fn apply(
    uid: u32,
    generation: u64,
    state_path: &Path,
    digest: &str,
    optional: bool,
) -> Result<()> {
    caller_uid(uid)?;
    let _lock = lock(&Path::new(DB).join("lock"))?;
    let database = Path::new(DB);
    let mut handle = open_handle()?;
    recover_pending(database, |name| handle.localdb().pkg(name).is_ok())?;
    let context = prepare(uid, generation, state_path, optional)?;
    if context.plan.digest() != digest {
        bail!("plan changed; run catdot resolve again")
    }
    let expected_packages = planned_package_state(database, &context.records, &context.plan)?;
    let transaction_packages = context.plan.install.iter().cloned().collect();
    let previously_present = context
        .plan
        .install
        .iter()
        .filter(|name| handle.localdb().pkg(name.as_str()).is_ok())
        .cloned()
        .collect();
    let mut journal = PackageJournal::prepared(
        database,
        &context.plan,
        PreparedTransaction {
            uid,
            generation,
            direct_requirements: context.record.pending_requirements.clone(),
            transaction_packages,
            previously_present,
            expected_record: context.record.clone(),
            expected_packages,
        },
    )?;
    journal.verify(uid, generation, digest)?;
    install_with_alpm(&mut handle, &context.plan.install)?;
    journal.mark_alpm_committed()?;
    commit_records(
        database,
        journal.expected_packages(),
        journal.expected_record(),
    )?;
    journal.mark_records_committed()?;
    journal.complete()?;
    Ok(())
}

pub(super) fn finalize(uid: u32, generation: u64, state_path: &Path) -> Result<()> {
    caller_uid(uid)?;
    let _lock = lock(&Path::new(DB).join("lock"))?;
    let handle = open_handle()?;
    recover_pending(Path::new(DB), |name| handle.localdb().pkg(name).is_ok())?;
    let state = read_trusted_user_state(uid, state_path)?;
    if state.active_generation != generation || state.active_generation != state.generation {
        bail!("active state is not ready to finalize")
    }
    let database = Path::new(DB);
    let mut records = load_records(database)?;
    let record = records
        .iter_mut()
        .find(|record| record.uid == uid)
        .ok_or_else(|| anyhow::anyhow!("pending user record is missing"))?;
    if record.pending_generation != generation || record.state_path != state_path {
        bail!("pending record generation does not match active state")
    }
    record.active_generation = generation;
    record.active_components = state.active_components;
    record.active_requirements = std::mem::take(&mut record.pending_requirements);
    atomic_write(
        &user_record_path(database, uid),
        &toml::to_string_pretty(record)?,
    )?;
    Ok(())
}

fn prepare(uid: u32, generation: u64, state_path: &Path, optional: bool) -> Result<ResolveContext> {
    let profiles = discover_profile_registry(Path::new(DEFAULT_PROFILE_ROOT))?.valid_profiles;
    let database = Path::new(DB);
    let existing_records = load_records(database)?;
    if let Some(record) = existing_records.iter().find(|record| record.uid == uid)
        && record.state_path != state_path
    {
        bail!("state path does not match the path registered for uid {uid}")
    }
    let state = read_trusted_user_state(uid, state_path)?;
    validate_user_state(&state, &profiles)?;
    if state.generation != generation {
        bail!("state changed; run catdot resolve again")
    }
    let record = UserRecord::from_state(uid, state_path, &state, &profiles, optional)?;
    let mut records = valid_records(existing_records, |record_uid| user_home(record_uid).is_ok());
    replace_record(&mut records, record.clone());
    let mut handle = open_handle()?;
    normalize_requirement_providers(&mut records, &handle)?;
    let record = records
        .iter()
        .find(|current| current.uid == uid)
        .cloned()
        .expect("current user record was inserted");
    let packages = aggregate_packages(&records);
    let plan = prepared_install_plan(&mut handle, &packages)?;
    Ok(ResolveContext {
        record,
        records,
        plan,
    })
}

fn normalize_requirement_providers(records: &mut [UserRecord], handle: &alpm::Alpm) -> Result<()> {
    for record in records {
        for requirements in [
            &mut record.active_requirements,
            &mut record.pending_requirements,
        ] {
            let pending = std::mem::take(requirements);
            for (dependency, components) in pending {
                let package = satisfier_name(handle, &dependency)?;
                requirements.entry(package).or_default().extend(components);
            }
        }
    }
    Ok(())
}

fn planned_package_state(
    database: &Path,
    records: &[UserRecord],
    plan: &PackagePlan,
) -> Result<SystemPackageState> {
    let mut state = read_system_packages(&database.join("packages.toml"))?;
    let requirements = aggregate_requirements(records);
    let previous = std::mem::take(&mut state.packages);
    let mut packages = BTreeMap::new();
    for (name, requirement) in requirements {
        let prior = previous.get(&name);
        packages.insert(
            name.clone(),
            ManagedPackage {
                name: name.clone(),
                catdot_installed: prior.is_some_and(|package| package.catdot_installed)
                    || plan.install.contains(&name),
                was_missing_before_catdot: prior
                    .is_some_and(|package| package.was_missing_before_catdot)
                    || plan.install.contains(&name),
                install_reason: prior
                    .map(|package| package.install_reason.clone())
                    .unwrap_or(InstallReason::Dependency),
                introduced_by_transaction: prior
                    .and_then(|package| package.introduced_by_transaction.clone()),
                references: requirement.references,
            },
        );
    }
    for name in &plan.install {
        packages
            .entry(name.clone())
            .or_insert_with(|| ManagedPackage {
                name: name.clone(),
                catdot_installed: true,
                was_missing_before_catdot: true,
                install_reason: InstallReason::Dependency,
                introduced_by_transaction: None,
                references: vec![],
            });
    }
    for (name, package) in previous {
        if package.catdot_installed && !packages.contains_key(&name) {
            packages.insert(name, package);
        }
    }
    state.packages = packages;
    Ok(state)
}
