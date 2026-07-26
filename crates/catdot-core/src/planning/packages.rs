use super::{PackageBackend, require_available};
use crate::{Error, Profile, Result, UserState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub fn packages_for_state(
    state: &UserState,
    profiles: &BTreeMap<String, Profile>,
) -> Result<BTreeSet<String>> {
    let mut packages = BTreeSet::new();
    for (id, profile_state) in &state.profiles {
        if !profiles.contains_key(id) {
            return Err(Error::Message(format!(
                "profile {id} is no longer installed"
            )));
        }
        packages.extend(profile_state.packages.iter().cloned());
    }
    Ok(packages)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UserRecord {
    pub uid: u32,
    pub generation: u64,
    pub state_path: PathBuf,
    pub profiles: BTreeSet<String>,
    pub requirements: BTreeMap<String, BTreeSet<String>>,
}

impl UserRecord {
    pub fn from_state(
        uid: u32,
        state_path: &Path,
        state: &UserState,
        profiles: &BTreeMap<String, Profile>,
    ) -> Result<Self> {
        let mut requirements = BTreeMap::<String, BTreeSet<String>>::new();
        for (id, profile_state) in &state.profiles {
            if !profiles.contains_key(id) {
                return Err(Error::Message(format!(
                    "profile {id} is no longer installed"
                )));
            }
            for package in &profile_state.packages {
                requirements
                    .entry(package.clone())
                    .or_default()
                    .insert(id.clone());
            }
        }
        Ok(Self {
            uid,
            generation: state.generation,
            state_path: state_path.to_owned(),
            profiles: state.profiles.keys().cloned().collect(),
            requirements,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageReference {
    pub uid: u32,
    pub profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub uids: Vec<u32>,
    pub references: Vec<PackageReference>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SystemDoctorReport {
    pub lines: Vec<String>,
    pub warnings: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePlanPreview {
    pub plan: PackagePlan,
    pub requirements: BTreeMap<String, Requirement>,
    #[serde(default)]
    pub system_update_required: bool,
}

pub fn aggregate_requirements(records: &[UserRecord]) -> BTreeMap<String, Requirement> {
    let mut requirements = BTreeMap::new();
    for record in records {
        for (package, profiles) in &record.requirements {
            let requirement = requirements.entry(package.clone()).or_insert(Requirement {
                uids: vec![],
                references: vec![],
            });
            if !requirement.uids.contains(&record.uid) {
                requirement.uids.push(record.uid);
            }
            requirement
                .references
                .extend(profiles.iter().cloned().map(|profile| PackageReference {
                    uid: record.uid,
                    profile,
                }));
        }
    }
    for requirement in requirements.values_mut() {
        requirement.uids.sort_unstable();
        requirement.uids.dedup();
        requirement
            .references
            .sort_by(|left, right| (left.uid, &left.profile).cmp(&(right.uid, &right.profile)));
        requirement.references.dedup();
    }
    requirements
}

pub fn aggregate_packages(records: &[UserRecord]) -> BTreeSet<String> {
    records
        .iter()
        .flat_map(|record| record.requirements.keys().cloned())
        .collect()
}

pub fn install_plan<B: PackageBackend>(
    backend: &B,
    packages: &BTreeSet<String>,
) -> Result<PackagePlan> {
    let mut plan = PackagePlan {
        install: vec![],
        remove: vec![],
        replacements: vec![],
        satisfied: vec![],
    };
    for package in packages {
        if require_available(backend.availability(package)?, package)? {
            plan.install.push(package.clone());
        } else {
            plan.satisfied.push(package.clone());
        }
    }
    Ok(plan)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageReplacement {
    pub remove: String,
    pub install: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackagePlan {
    pub install: Vec<String>,
    pub remove: Vec<String>,
    #[serde(default)]
    pub replacements: Vec<PackageReplacement>,
    pub satisfied: Vec<String>,
}

impl PackagePlan {
    pub fn digest(&self) -> String {
        let data = toml::to_string(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(data))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstallReason {
    Dependency,
    Explicit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedPackage {
    pub name: String,
    pub catdot_installed: bool,
    pub was_missing_before_catdot: bool,
    pub install_reason: InstallReason,
    #[serde(default)]
    pub introduced_by_transaction: Option<String>,
    pub references: Vec<PackageReference>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemPackageState {
    #[serde(default)]
    pub packages: BTreeMap<String, ManagedPackage>,
}

pub fn prunable(package: &ManagedPackage, has_other_dependency: bool) -> bool {
    package.catdot_installed
        && package.was_missing_before_catdot
        && package.references.is_empty()
        && package.install_reason == InstallReason::Dependency
        && !has_other_dependency
}
