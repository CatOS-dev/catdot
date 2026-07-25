use super::{PackageBackend, require_available};
use crate::{Error, Profile, Result, UserState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

fn resolve<'a>(
    profiles: &'a BTreeMap<String, Profile>,
    reference: &str,
) -> Result<(&'a Profile, &'a crate::ComponentDef)> {
    let (profile_id, component_id) = reference
        .split_once('/')
        .ok_or_else(|| Error::Message("invalid saved component reference".into()))?;
    let profile = profiles
        .get(profile_id)
        .ok_or_else(|| Error::Message(format!("profile {profile_id} is no longer installed")))?;
    let component = profile
        .components
        .get(component_id)
        .ok_or_else(|| Error::Message(format!("component {reference} is no longer installed")))?;
    Ok((profile, component))
}
pub fn expand_exec(
    profile: &Profile,
    id: &str,
    home: &str,
    xdg_config_home: &str,
    extra: &[String],
) -> Result<Vec<String>> {
    let component = profile
        .components
        .get(id)
        .ok_or_else(|| Error::Message("unknown component".into()))?;
    if component.exec.is_empty() {
        return Err(Error::Message(format!(
            "component {id} has no exec command"
        )));
    }
    let profile_root = profile.source_root.display().to_string();
    let component_path = profile.source_root.display().to_string();
    let mut argv = component
        .exec
        .iter()
        .map(|arg| {
            arg.replace("{profile}", &profile_root)
                .replace("{component}", &component_path)
                .replace("{home}", home)
                .replace("{xdg_config_home}", xdg_config_home)
        })
        .collect::<Vec<_>>();
    let executable = argv
        .first()
        .ok_or_else(|| Error::Message(format!("component {id} has no exec command")))?;
    if executable.is_empty() || executable.contains('\0') {
        return Err(Error::Message(format!(
            "component {id} has an invalid executable"
        )));
    }
    argv.extend_from_slice(extra);
    Ok(argv)
}
pub fn packages_for_state(
    state: &UserState,
    profiles: &BTreeMap<String, Profile>,
    optional: bool,
) -> Result<BTreeSet<String>> {
    let mut packages = BTreeSet::new();
    for reference in state.components.values() {
        let (_, component) = resolve(profiles, reference)?;
        packages.extend(component.packages.iter().cloned());
        if optional {
            packages.extend(component.optional_packages.iter().cloned())
        }
    }
    Ok(packages)
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UserRecord {
    pub uid: u32,
    pub pending_generation: u64,
    pub active_generation: u64,
    pub state_path: PathBuf,
    pub components: BTreeMap<String, String>,
    pub active_components: BTreeMap<String, String>,
    pub active_requirements: BTreeMap<String, BTreeSet<String>>,
    pub pending_requirements: BTreeMap<String, BTreeSet<String>>,
}
impl UserRecord {
    pub fn from_state(
        uid: u32,
        state_path: &Path,
        state: &UserState,
        profiles: &BTreeMap<String, Profile>,
        optional: bool,
    ) -> Result<Self> {
        let active_requirements =
            requirements_for_components(&state.active_components, profiles, optional)?;
        let pending_requirements = if state.active_generation == state.generation
            && state.active_components == state.components
        {
            BTreeMap::new()
        } else {
            requirements_for_components(&state.components, profiles, optional)?
        };
        Ok(Self {
            uid,
            pending_generation: state.generation,
            active_generation: state.active_generation,
            state_path: state_path.into(),
            components: state.components.clone(),
            active_components: state.active_components.clone(),
            active_requirements,
            pending_requirements,
        })
    }
}
fn requirements_for_components(
    components: &BTreeMap<String, String>,
    profiles: &BTreeMap<String, Profile>,
    optional: bool,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut requirements = BTreeMap::new();
    for reference in components.values() {
        let (_, component) = resolve(profiles, reference)?;
        for package in component.packages.iter().chain(if optional {
            component.optional_packages.iter()
        } else {
            [].iter()
        }) {
            requirements
                .entry(package.clone())
                .or_insert_with(BTreeSet::new)
                .insert(reference.clone());
        }
    }
    Ok(requirements)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageReference {
    pub uid: u32,
    pub component: String,
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
        for (package, references) in record
            .active_requirements
            .iter()
            .chain(&record.pending_requirements)
        {
            let requirement = requirements.entry(package.clone()).or_insert(Requirement {
                uids: vec![],
                references: vec![],
            });
            requirement.uids.push(record.uid);
            requirement
                .references
                .extend(
                    references
                        .iter()
                        .cloned()
                        .map(|component| PackageReference {
                            uid: record.uid,
                            component,
                        }),
                );
        }
    }
    requirements
}

pub fn aggregate_packages(records: &[UserRecord]) -> BTreeSet<String> {
    records
        .iter()
        .flat_map(|record| {
            record
                .active_requirements
                .keys()
                .chain(record.pending_requirements.keys())
                .cloned()
        })
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
