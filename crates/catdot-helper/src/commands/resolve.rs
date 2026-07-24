use crate::{
    auth::{caller_uid, read_trusted_user_state, user_home},
    backend::{
        install_with_alpm, open_handle, prepared_install_plan, removable_with_alpm, satisfier_name,
    },
    system::{load_records, replace_record, user_record_path, valid_records},
};
use anyhow::{Result, bail};
use catdot_core::*;
use std::{collections::BTreeMap, fs, path::Path};

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
    let context = prepare(uid, generation, state_path, optional)?;
    let database = Path::new(DB);
    let mut handle = open_handle()?;
    if context.plan.digest() != digest {
        bail!("plan changed; run catdot resolve again")
    }
    install_with_alpm(&mut handle, &context.plan.install)?;
    let package_state = updated_package_state(database, &context.records, &context.plan, &handle)?;
    commit_management_records(
        &database.join("packages.toml"),
        &toml::to_string_pretty(&package_state)?,
        &user_record_path(database, uid),
        &toml::to_string_pretty(&context.record)?,
    )?;
    Ok(())
}

pub(super) fn finalize(uid: u32, generation: u64, state_path: &Path) -> Result<()> {
    caller_uid(uid)?;
    let _lock = lock(&Path::new(DB).join("lock"))?;
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
    let state = read_trusted_user_state(uid, state_path)?;
    validate_user_state(&state, &profiles)?;
    if state.generation != generation {
        bail!("state changed; run catdot resolve again")
    }
    let record = UserRecord::from_state(uid, state_path, &state, &profiles, optional)?;
    let database = Path::new(DB);
    let mut records = valid_records(load_records(database)?, |record_uid| {
        user_home(record_uid).is_ok()
    });
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

fn updated_package_state(
    database: &Path,
    records: &[UserRecord],
    plan: &PackagePlan,
    handle: &alpm::Alpm,
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
                install_reason: if removable_with_alpm(handle, &name) {
                    InstallReason::Dependency
                } else {
                    InstallReason::Explicit
                },
                references: requirement.references,
            },
        );
    }
    for (name, package) in previous {
        if package.catdot_installed && !packages.contains_key(&name) {
            packages.insert(name, package);
        }
    }
    state.packages = packages;
    Ok(state)
}

fn commit_management_records(
    package_path: &Path,
    package_contents: &str,
    user_path: &Path,
    user_contents: &str,
) -> Result<()> {
    let previous_packages = fs::read_to_string(package_path).ok();
    atomic_write(package_path, package_contents)?;
    if let Err(error) = atomic_write(user_path, user_contents) {
        if let Err(restore_error) = restore_file(package_path, previous_packages.as_deref()) {
            bail!(
                "could not write user record ({error}) and could not restore package record ({restore_error})"
            )
        }
        return Err(error.into());
    }
    Ok(())
}

fn restore_file(path: &Path, contents: Option<&str>) -> Result<()> {
    match contents {
        Some(contents) => atomic_write(path, contents).map_err(Into::into),
        None if path.exists() => {
            fs::remove_file(path)?;
            Ok(())
        }
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::commit_management_records;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn failed_user_record_write_restores_the_previous_package_record() {
        let directory = tempdir().unwrap();
        let package = directory.path().join("packages.toml");
        let blocked_parent = directory.path().join("blocked");
        fs::write(&package, "previous packages").unwrap();
        fs::write(&blocked_parent, "not a directory").unwrap();

        let result = commit_management_records(
            &package,
            "new packages",
            &blocked_parent.join("1000.toml"),
            "new user",
        );
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&package).unwrap(), "previous packages");
    }
}
