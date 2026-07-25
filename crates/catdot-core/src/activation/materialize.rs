use crate::{
    ActivationJournal, ComponentDef, Error, Lifecycle, Profile, Result, UserState, WmConfig,
    atomic_write,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedTarget {
    pub owner: String,
    pub lifecycle: String,
    pub source: Option<PathBuf>,
    #[serde(default)]
    pub merge_base: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedRegistry {
    pub entries: BTreeMap<String, ManagedTarget>,
    #[serde(default)]
    pub user_initialized: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct MergeConflict {
    directory: PathBuf,
    base: String,
    local: String,
    upstream: String,
    merged: String,
}

#[derive(Debug, Clone)]
pub enum Materialization {
    Generate {
        contents: Vec<u8>,
    },
    Symlink {
        source: PathBuf,
    },
    File {
        source: PathBuf,
    },
    Merge {
        source: PathBuf,
        contents: Vec<u8>,
        base: String,
        ready: bool,
        conflict: Option<MergeConflict>,
    },
    User {
        seed: Option<PathBuf>,
        release_managed: bool,
        initialized: bool,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedTarget {
    pub target: PathBuf,
    pub owner: String,
    pub materialization: Materialization,
}

#[derive(Debug, Clone)]
pub struct ActivationPlan {
    pub entries: Vec<PlannedTarget>,
    pub removals: Vec<PathBuf>,
    registry: ManagedRegistry,
    registry_changed: bool,
}

impl ActivationPlan {
    pub fn has_changes(&self) -> bool {
        self.registry_changed
            || self.removals.iter().any(|target| target.exists())
            || self.entries.iter().any(entry_has_changes)
    }

    pub fn identity_digest(&self) -> String {
        let mut digest = Sha256::new();
        for entry in &self.entries {
            digest.update(b"entry\0");
            digest.update(entry.target.as_os_str().as_encoded_bytes());
            digest.update([0]);
            digest.update(entry.owner.as_bytes());
            digest.update([0]);
            match &entry.materialization {
                Materialization::Generate { contents } => {
                    digest.update(b"generate\0");
                    digest.update(contents);
                }
                Materialization::Symlink { source } => {
                    digest.update(b"symlink\0");
                    digest.update(source.as_os_str().as_encoded_bytes());
                }
                Materialization::File { source } => {
                    digest.update(b"generate-source\0");
                    digest.update(source.as_os_str().as_encoded_bytes());
                }
                Materialization::Merge { source, .. } => {
                    digest.update(b"merge\0");
                    digest.update(source.as_os_str().as_encoded_bytes());
                }
                Materialization::User {
                    seed,
                    release_managed,
                    initialized,
                } => {
                    digest.update(b"user\0");
                    if let Some(seed) = seed {
                        digest.update(seed.as_os_str().as_encoded_bytes());
                    }
                    digest.update([0, *release_managed as u8, *initialized as u8]);
                }
            }
            digest.update([0xff]);
        }
        for removal in &self.removals {
            digest.update(b"remove\0");
            digest.update(removal.as_os_str().as_encoded_bytes());
            digest.update([0xff]);
        }
        digest.update([self.registry_changed as u8]);
        for (target, managed) in &self.registry.entries {
            digest.update(target.as_bytes());
            digest.update([0]);
            digest.update(managed.owner.as_bytes());
            digest.update([0]);
            digest.update(managed.lifecycle.as_bytes());
            digest.update([0]);
            if let Some(source) = &managed.source {
                digest.update(source.as_os_str().as_encoded_bytes());
            }
            digest.update([0xff]);
        }
        for (target, owner) in &self.registry.user_initialized {
            digest.update(target.as_bytes());
            digest.update([0]);
            digest.update(owner.as_bytes());
            digest.update([0xff]);
        }
        format!("{:x}", digest.finalize())
    }
}

pub fn managed_targets_path(state_path: &Path) -> Result<PathBuf> {
    let parent = state_path
        .parent()
        .ok_or_else(|| Error::Message("state path has no parent".into()))?;
    Ok(parent.join("managed.toml"))
}

pub fn read_managed_registry(path: &Path) -> Result<ManagedRegistry> {
    if !path.exists() {
        return Ok(ManagedRegistry::default());
    }
    toml::from_str(&fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?)
    .map_err(|source| Error::Toml {
        path: path.display().to_string(),
        source,
    })
}

pub fn build_activation_plan(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    home: &Path,
    registry_path: &Path,
) -> Result<ActivationPlan> {
    build_activation_plan_with_sources(profiles, state, home, registry_path, true)
}

pub fn build_activation_preview(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    home: &Path,
    registry_path: &Path,
) -> Result<ActivationPlan> {
    build_activation_plan_with_sources(profiles, state, home, registry_path, false)
}

struct PlanningContext<'a> {
    home: &'a Path,
    previous: &'a ManagedRegistry,
    conflict_root: PathBuf,
    verify_sources: bool,
}

fn build_activation_plan_with_sources(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    home: &Path,
    registry_path: &Path,
    verify_sources: bool,
) -> Result<ActivationPlan> {
    let previous = read_managed_registry(registry_path)?;
    let context = PlanningContext {
        home,
        previous: &previous,
        conflict_root: registry_path
            .parent()
            .ok_or_else(|| Error::Message("managed registry has no parent".into()))?
            .join("conflicts"),
        verify_sources,
    };
    let mut entries = Vec::new();
    for (role, reference) in &state.components {
        let (profile_id, component_id) = reference
            .split_once('/')
            .ok_or_else(|| Error::Message(format!("invalid component reference {reference}")))?;
        let profile = profiles
            .get(profile_id)
            .ok_or_else(|| Error::Message(format!("profile {profile_id} is not installed")))?;
        let component = profile
            .components
            .get(component_id)
            .ok_or_else(|| Error::Message(format!("component {reference} is not installed")))?;
        if component.role != *role {
            return Err(Error::Message(format!(
                "{reference} does not provide role {role}"
            )));
        }
        add_component_entries(&mut entries, profile, component, reference, &context)?;
    }
    add_wm_entries(&mut entries, profiles, state, home)?;
    validate_conflicts(&entries, home)?;
    for entry in &mut entries {
        if let Materialization::User {
            release_managed,
            initialized,
            ..
        } = &mut entry.materialization
        {
            let target = entry.target.display().to_string();
            *release_managed = previous.entries.contains_key(&target);
            *initialized = previous.user_initialized.contains_key(&target);
        }
    }
    if let Some(target) = previous
        .entries
        .keys()
        .map(Path::new)
        .find(|target| !target.starts_with(home))
    {
        return Err(Error::Message(format!(
            "managed registry target escapes home: {}",
            target.display()
        )));
    }
    let current: BTreeSet<_> = entries
        .iter()
        .map(|entry| entry.target.display().to_string())
        .collect();
    let removals = previous
        .entries
        .keys()
        .filter(|target| !current.contains(*target))
        .map(PathBuf::from)
        .collect();
    let mut registry = ManagedRegistry {
        entries: BTreeMap::new(),
        user_initialized: previous.user_initialized.clone(),
    };
    for entry in &entries {
        if let Materialization::User { .. } = entry.materialization {
            registry
                .user_initialized
                .insert(entry.target.display().to_string(), entry.owner.clone());
            continue;
        }
        let (lifecycle, source, merge_base) = match &entry.materialization {
            Materialization::Generate { .. } => ("generate".into(), None, None),
            Materialization::Symlink { source } => ("symlink".into(), Some(source.clone()), None),
            Materialization::File { source } => ("generate".into(), Some(source.clone()), None),
            Materialization::Merge { source, base, .. } => {
                ("merge".into(), Some(source.clone()), Some(base.clone()))
            }
            Materialization::User { .. } => unreachable!(),
        };
        registry.entries.insert(
            entry.target.display().to_string(),
            ManagedTarget {
                owner: entry.owner.clone(),
                lifecycle,
                source,
                merge_base,
            },
        );
    }
    let registry_changed = registry != previous;
    Ok(ActivationPlan {
        entries,
        removals,
        registry,
        registry_changed,
    })
}

fn add_wm_entries(
    entries: &mut Vec<PlannedTarget>,
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
    home: &Path,
) -> Result<()> {
    let mut owners = Vec::new();
    for (role, reference) in &state.components {
        let (profile_id, component_id) = reference
            .split_once('/')
            .ok_or_else(|| Error::Message(format!("invalid component reference {reference}")))?;
        let component = profiles
            .get(profile_id)
            .and_then(|profile| profile.components.get(component_id))
            .ok_or_else(|| Error::Message(format!("component {reference} is not installed")))?;
        if component.role != *role {
            return Err(Error::Message(format!(
                "{reference} does not provide role {role}"
            )));
        }
        if let Some(wm) = &component.wm {
            owners.push((reference.as_str(), wm));
        }
    }
    let Some((owner, wm)) = owners.first().copied() else {
        return Ok(());
    };
    if owners.len() > 1 {
        return Err(Error::Message(format!(
            "multiple active wm configuration owners: {}",
            owners
                .iter()
                .map(|(owner, _)| *owner)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let contents = render_wm_autostart(wm, profiles, state)?;
    entries.push(PlannedTarget {
        target: home.join(&wm.autostart_target),
        owner: owner.into(),
        materialization: Materialization::Generate { contents },
    });
    Ok(())
}

fn render_wm_autostart(
    wm: &WmConfig,
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
) -> Result<Vec<u8>> {
    let active = wm
        .autostart
        .iter()
        .filter(|entry| state.components.contains_key(&entry.role))
        .map(|entry| (entry.role.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    for role in active.keys() {
        let reference = state.components.get(role).expect("active role");
        let (profile_id, component_id) = reference
            .split_once('/')
            .ok_or_else(|| Error::Message(format!("invalid component reference {reference}")))?;
        let component = profiles
            .get(profile_id)
            .and_then(|profile| profile.components.get(component_id))
            .ok_or_else(|| Error::Message(format!("component {reference} is not installed")))?;
        if component.exec.is_empty() {
            return Err(Error::Message(format!(
                "wm autostart role {role} uses {reference}, which has no exec provider"
            )));
        }
    }

    let mut outgoing = active
        .keys()
        .map(|role| (role.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut indegree = active
        .keys()
        .map(|role| (role.clone(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    for (role, entry) in &active {
        for before in &entry.before {
            if active.contains_key(before)
                && outgoing
                    .get_mut(role)
                    .expect("active role")
                    .insert(before.clone())
            {
                *indegree.get_mut(before).expect("active role") += 1;
            }
        }
        for after in &entry.after {
            if active.contains_key(after)
                && outgoing
                    .get_mut(after)
                    .expect("active role")
                    .insert(role.clone())
            {
                *indegree.get_mut(role).expect("active role") += 1;
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(role, _)| role.clone())
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::new();
    while let Some(role) = ready.pop_first() {
        ordered.push(role.clone());
        for next in outgoing.get(&role).expect("active role") {
            let degree = indegree.get_mut(next).expect("active role");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(next.clone());
            }
        }
    }
    if ordered.len() != active.len() {
        return Err(Error::Message("wm autostart cycle in active roles".into()));
    }
    let mut rendered = String::new();
    for role in ordered {
        rendered.push_str(&wm.autostart_template.replace("{role}", &role));
        rendered.push('\n');
    }
    Ok(rendered.into_bytes())
}

fn add_component_entries(
    entries: &mut Vec<PlannedTarget>,
    profile: &Profile,
    component: &ComponentDef,
    owner: &str,
    context: &PlanningContext<'_>,
) -> Result<()> {
    for configuration in &component.configuration {
        let target = context.home.join(&configuration.target);
        let materialization = match &configuration.lifecycle {
            Lifecycle::Generate => {
                if let Some(template) = &configuration.template {
                    Materialization::Generate {
                        contents: template.as_bytes().to_vec(),
                    }
                } else {
                    Materialization::File {
                        source: source(
                            profile,
                            configuration.source.as_ref().expect("validated source"),
                            context.verify_sources,
                        )?,
                    }
                }
            }
            Lifecycle::Symlink => Materialization::Symlink {
                source: source(
                    profile,
                    configuration.source.as_ref().expect("validated source"),
                    context.verify_sources,
                )?,
            },
            Lifecycle::Merge => build_merge_materialization(
                profile,
                configuration.source.as_ref().expect("validated source"),
                &target,
                context.previous,
                &context.conflict_root,
                context.verify_sources,
            )?,
            Lifecycle::User => Materialization::User {
                seed: configuration
                    .seed
                    .as_ref()
                    .map(|seed| source(profile, seed, context.verify_sources))
                    .transpose()?,
                release_managed: false,
                initialized: false,
            },
        };
        entries.push(PlannedTarget {
            target,
            owner: owner.into(),
            materialization,
        });
    }
    Ok(())
}

fn build_merge_materialization(
    profile: &Profile,
    relative: &Path,
    target: &Path,
    previous: &ManagedRegistry,
    conflict_root: &Path,
    verify_sources: bool,
) -> Result<Materialization> {
    let source = source(profile, relative, verify_sources)?;
    if !source.exists() && !verify_sources {
        return Ok(Materialization::Merge {
            source,
            contents: Vec::new(),
            base: previous
                .entries
                .get(&target.display().to_string())
                .and_then(|entry| entry.merge_base.clone())
                .unwrap_or_default(),
            ready: false,
            conflict: None,
        });
    }
    let upstream = fs::read_to_string(&source).map_err(|source_error| Error::Io {
        path: source.display().to_string(),
        source: source_error,
    })?;
    let old_base = previous
        .entries
        .get(&target.display().to_string())
        .filter(|entry| entry.lifecycle == "merge")
        .and_then(|entry| entry.merge_base.as_deref());
    let local = match fs::read_to_string(target) {
        Ok(local) => Some(local),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source_error) => {
            return Err(Error::Io {
                path: target.display().to_string(),
                source: source_error,
            });
        }
    };
    let merged = match (old_base, local.as_deref()) {
        (None, _) | (_, None) => upstream.clone(),
        (Some(base), Some(local)) if local == base => upstream.clone(),
        (Some(base), Some(local)) if upstream == base || local == upstream => local.to_owned(),
        (Some(base), Some(local)) => {
            let mut digest = Sha256::new();
            digest.update(target.as_os_str().as_encoded_bytes());
            let directory = conflict_root.join(format!("{:x}", digest.finalize()));
            let conflict = MergeConflict {
                directory,
                base: base.to_owned(),
                local: local.to_owned(),
                upstream: upstream.clone(),
                merged: format!("<<<<<<< local\n{local}=======\n{upstream}>>>>>>> upstream\n"),
            };
            return Ok(Materialization::Merge {
                source,
                contents: local.as_bytes().to_vec(),
                base: upstream,
                ready: true,
                conflict: Some(conflict),
            });
        }
    };
    Ok(Materialization::Merge {
        source,
        contents: merged.into_bytes(),
        base: upstream,
        ready: true,
        conflict: None,
    })
}

fn source(profile: &Profile, relative: &Path, verify_sources: bool) -> Result<PathBuf> {
    let path = profile.source_root.join(relative);
    if verify_sources && fs::symlink_metadata(&path).is_err() {
        return Err(Error::Message(format!(
            "configuration source {} does not exist",
            path.display()
        )));
    }
    Ok(path)
}

fn validate_conflicts(entries: &[PlannedTarget], home: &Path) -> Result<()> {
    let mut seen = BTreeMap::<&Path, &PlannedTarget>::new();
    for entry in entries {
        if !entry.target.starts_with(home) || entry.target == home {
            return Err(Error::Message(format!(
                "target escapes home: {}",
                entry.target.display()
            )));
        }
        if let Some(previous) = seen.insert(&entry.target, entry) {
            return Err(Error::Message(format!(
                "configuration target conflict: {} ({}, {})",
                entry.target.display(),
                previous.owner,
                entry.owner
            )));
        }
    }
    for (target, entry) in &seen {
        for ancestor in target.ancestors().skip(1).take_while(|path| *path != home) {
            if let Some(parent) = seen.get(ancestor) {
                return Err(Error::Message(format!(
                    "configuration path conflict: {} owned by {} conflicts with descendant {} owned by {}",
                    ancestor.display(),
                    parent.owner,
                    target.display(),
                    entry.owner
                )));
            }
            if fs::symlink_metadata(ancestor)
                .ok()
                .is_some_and(|m| m.file_type().is_symlink())
            {
                return Err(Error::Message(format!(
                    "configuration target {} has symlink parent {}",
                    target.display(),
                    ancestor.display()
                )));
            }
        }
    }
    Ok(())
}

fn entry_has_changes(entry: &PlannedTarget) -> bool {
    match &entry.materialization {
        Materialization::Generate { contents } => {
            fs::read(&entry.target).map_or(true, |existing| existing != *contents)
                || fs::metadata(&entry.target).map_or(true, |metadata| {
                    metadata.permissions().mode() & 0o777 != 0o644
                })
        }
        Materialization::Symlink { source } => {
            fs::read_link(&entry.target).map_or(true, |existing| existing != *source)
        }
        Materialization::File { source } => !paths_equal(source, &entry.target),
        Materialization::Merge {
            contents, ready, ..
        } => {
            !*ready
                || fs::read(&entry.target).map_or(true, |existing| existing != *contents)
                || fs::metadata(&entry.target).map_or(true, |metadata| {
                    metadata.permissions().mode() & 0o777 != 0o644
                })
        }
        Materialization::User {
            release_managed,
            initialized,
            ..
        } => *release_managed || (!*initialized && !entry.target.exists()),
    }
}

fn paths_equal(source: &Path, target: &Path) -> bool {
    let Ok(source_metadata) = fs::symlink_metadata(source) else {
        return false;
    };
    let Ok(target_metadata) = fs::symlink_metadata(target) else {
        return false;
    };
    if source_metadata.file_type().is_symlink() || target_metadata.file_type().is_symlink() {
        return source_metadata.file_type().is_symlink()
            && target_metadata.file_type().is_symlink()
            && fs::read_link(source).ok() == fs::read_link(target).ok();
    }
    if source_metadata.is_file() || target_metadata.is_file() {
        return source_metadata.is_file()
            && target_metadata.is_file()
            && source_metadata.permissions().mode() & 0o777
                == target_metadata.permissions().mode() & 0o777
            && fs::read(source).ok() == fs::read(target).ok();
    }
    if !source_metadata.is_dir() || !target_metadata.is_dir() {
        return false;
    }
    if source_metadata.permissions().mode() & 0o777 != target_metadata.permissions().mode() & 0o777
    {
        return false;
    }
    let Ok(source_entries) = fs::read_dir(source) else {
        return false;
    };
    let Ok(target_entries) = fs::read_dir(target) else {
        return false;
    };
    let mut source_names = source_entries
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect::<Vec<_>>();
    let mut target_names = target_entries
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect::<Vec<_>>();
    source_names.sort();
    target_names.sort();
    source_names == target_names
        && source_names
            .iter()
            .all(|name| paths_equal(&source.join(name), &target.join(name)))
}

pub fn forget_user_initialization(
    registry_path: &Path,
    targets: impl IntoIterator<Item = PathBuf>,
) -> Result<()> {
    let mut registry = read_managed_registry(registry_path)?;
    for target in targets {
        registry
            .user_initialized
            .remove(&target.display().to_string());
    }
    atomic_write(
        registry_path,
        &toml::to_string_pretty(&registry).map_err(|error| Error::Message(error.to_string()))?,
    )
}

pub fn activate_configuration(
    plan: &ActivationPlan,
    registry_path: &Path,
    journal: &mut ActivationJournal,
) -> Result<()> {
    for entry in &plan.entries {
        journal.track_path(&entry.target)?;
    }
    for target in &plan.removals {
        journal.track_path(target)?;
    }
    journal.track_path(registry_path)?;
    for entry in &plan.entries {
        apply_entry(entry)?;
        journal.mark_applied()?;
    }
    for target in &plan.removals {
        remove_path(target)?;
        journal.mark_applied()?;
    }
    atomic_write(
        registry_path,
        &toml::to_string_pretty(&plan.registry)
            .map_err(|error| Error::Message(error.to_string()))?,
    )?;
    journal.mark_applied()
}

fn apply_entry(entry: &PlannedTarget) -> Result<()> {
    match &entry.materialization {
        Materialization::Generate { contents } => replace_file(&entry.target, contents, 0o644),
        Materialization::Symlink { source } => {
            if fs::symlink_metadata(&entry.target)
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_symlink())
                && fs::read_link(&entry.target).ok().as_ref() == Some(source)
            {
                return Ok(());
            }
            parent(&entry.target)?;
            let temporary = sibling(&entry.target, "link");
            symlink(source, &temporary).map_err(|source| Error::Io {
                path: temporary.display().to_string(),
                source,
            })?;
            rename_staged(&temporary, &entry.target)
        }
        Materialization::File { source } => replace_from(source, &entry.target),
        Materialization::Merge {
            contents,
            ready,
            conflict,
            ..
        } => {
            if !ready {
                return Err(Error::Message(format!(
                    "merge source for {} is not available",
                    entry.target.display()
                )));
            }
            if let Some(conflict) = conflict {
                write_merge_conflict(conflict)?;
                return Err(Error::Message(format!(
                    "merge conflict for {}; current configuration was preserved; recovery inputs: {}",
                    entry.target.display(),
                    conflict.directory.display()
                )));
            }
            replace_file(&entry.target, contents, 0o644)
        }
        Materialization::User {
            seed,
            release_managed,
            initialized,
        } => match fs::symlink_metadata(&entry.target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if *initialized && !*release_managed {
                    return Ok(());
                }
                if let Some(seed) = seed {
                    copy_tree(seed, &entry.target)
                } else {
                    fs::create_dir_all(&entry.target).map_err(|source| Error::Io {
                        path: entry.target.display().to_string(),
                        source,
                    })
                }
            }
            Ok(metadata) if *release_managed && metadata.file_type().is_symlink() => {
                let resolved = fs::canonicalize(&entry.target).map_err(|source| Error::Io {
                    path: entry.target.display().to_string(),
                    source,
                })?;
                replace_from(&resolved, &entry.target)
            }
            Ok(_) => Ok(()),
            Err(source) => Err(Error::Io {
                path: entry.target.display().to_string(),
                source,
            }),
        },
    }
}

fn write_merge_conflict(conflict: &MergeConflict) -> Result<()> {
    fs::create_dir_all(&conflict.directory).map_err(|source| Error::Io {
        path: conflict.directory.display().to_string(),
        source,
    })?;
    atomic_write(&conflict.directory.join("base"), &conflict.base)?;
    atomic_write(&conflict.directory.join("local"), &conflict.local)?;
    atomic_write(&conflict.directory.join("upstream"), &conflict.upstream)?;
    atomic_write(&conflict.directory.join("merged"), &conflict.merged)
}

fn replace_file(target: &Path, contents: &[u8], mode: u32) -> Result<()> {
    parent(target)?;
    let temporary = sibling(target, "new");
    fs::write(&temporary, contents).map_err(|source| Error::Io {
        path: temporary.display().to_string(),
        source,
    })?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).map_err(|source| {
        Error::Io {
            path: temporary.display().to_string(),
            source,
        }
    })?;
    rename_staged(&temporary, target)
}

fn replace_from(source: &Path, target: &Path) -> Result<()> {
    let temporary = sibling(target, "new");
    copy_tree(source, &temporary)?;
    parent(target)?;
    rename_staged(&temporary, target)
}

fn rename_staged(temporary: &Path, target: &Path) -> Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        let temporary_c = std::ffi::CString::new(temporary.as_os_str().as_bytes())
            .map_err(|_| Error::Message("temporary path contains NUL".into()))?;
        let target_c = std::ffi::CString::new(target.as_os_str().as_bytes())
            .map_err(|_| Error::Message("target path contains NUL".into()))?;
        let exchanged = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                temporary_c.as_ptr(),
                libc::AT_FDCWD,
                target_c.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        if exchanged == 0 {
            return remove_path(temporary);
        }
        return Err(Error::Io {
            path: target.display().to_string(),
            source: std::io::Error::last_os_error(),
        });
    }
    fs::rename(temporary, target).map_err(|source| Error::Io {
        path: target.display().to_string(),
        source,
    })
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|source_error| Error::Io {
        path: source.display().to_string(),
        source: source_error,
    })?;
    if metadata.file_type().is_symlink() {
        parent(target)?;
        return symlink(
            fs::read_link(source).map_err(|source_error| Error::Io {
                path: source.display().to_string(),
                source: source_error,
            })?,
            target,
        )
        .map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        });
    }
    if metadata.is_file() {
        parent(target)?;
        fs::copy(source, target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
    } else if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|source_error| Error::Io {
            path: target.display().to_string(),
            source: source_error,
        })?;
        for child in fs::read_dir(source).map_err(|source_error| Error::Io {
            path: source.display().to_string(),
            source: source_error,
        })? {
            let child = child.map_err(|source_error| Error::Io {
                path: source.display().to_string(),
                source: source_error,
            })?;
            copy_tree(&child.path(), &target.join(child.file_name()))?;
        }
    } else {
        return Err(Error::Message(format!(
            "unsupported configuration source {}",
            source.display()
        )));
    }
    fs::set_permissions(
        target,
        fs::Permissions::from_mode(metadata.permissions().mode()),
    )
    .map_err(|source_error| Error::Io {
        path: target.display().to_string(),
        source: source_error,
    })
}

fn parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    Ok(())
}
fn remove_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })
        }
        Ok(_) => fs::remove_file(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".{}.{}-{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            suffix,
            std::process::id()
        ))
}
