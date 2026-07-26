use catdot_core::{
    ActivationJournal, ActivationMode, ProfileState, UserState, activate_configuration,
    build_activation_plan, discover_profiles, managed_targets_path, packages_for_state,
    read_managed_registry, state_path, write_state,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use tempfile::tempdir;

fn install_profile(
    root: &Path,
    id: &str,
    packages: &[&str],
    manage: &[&str],
    files: &[(&str, &str)],
) {
    let metadata = root.join("usr/share/catdot/profiles").join(id);
    let content = root.join("usr/share").join(id);
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&content).unwrap();
    let packages = packages
        .iter()
        .map(|package| format!("\"{package}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manage = manage
        .iter()
        .map(|path| format!("\"{path}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 4\nname = \"{id}\"\ndescription = \"test profile\"\npackages = [{packages}]\nmanage = [{manage}]\n"
        ),
    )
    .unwrap();
    for (path, contents) in files {
        let target = content.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, contents).unwrap();
    }
}

fn profiles(root: &Path) -> BTreeMap<String, catdot_core::Profile> {
    discover_profiles(&root.join("usr/share/catdot/profiles")).unwrap()
}

fn retained(active: Option<&str>, ids: &[&str]) -> UserState {
    UserState {
        schema: catdot_core::USER_STATE_SCHEMA,
        generation: 1,
        active_profile: active.map(str::to_owned),
        profiles: ids
            .iter()
            .map(|id| ((*id).to_owned(), ProfileState::default()))
            .collect(),
    }
}

fn apply(
    root: &Path,
    profiles: &BTreeMap<String, catdot_core::Profile>,
    old: &UserState,
    mut new: UserState,
    target: &str,
    mode: ActivationMode,
) -> UserState {
    let home = root.join("home/alice");
    let state_file = state_path(&home);
    let registry = managed_targets_path(&state_file).unwrap();
    fs::create_dir_all(&home).unwrap();
    write_state(&state_file, old).unwrap();
    catdot_core::prepare_profile_state(&mut new, &profiles[target], mode).unwrap();
    let plan = build_activation_plan(profiles, &new, target, &home, &registry, mode).unwrap();
    plan.record_seeded_paths(&mut new).unwrap();
    new.active_profile = Some(target.to_owned());
    new.generation += 1;
    let mut journal = ActivationJournal::begin(&state_file, old.clone(), new.clone()).unwrap();
    journal.mark_applying().unwrap();
    activate_configuration(&plan, &registry, &mut journal).unwrap();
    write_state(&state_file, &new).unwrap();
    journal.mark_state_written().unwrap();
    journal.complete().unwrap();
    new
}

// Protects the new KISS manifest contract: profile identity comes from the
// metadata directory, content comes from /usr/share/<id>, and only packages
// plus recursively managed paths are declared.
#[test]
fn discovery_uses_fixed_content_root_and_recursive_manage() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &["niri", "ghostty>=1.0"],
        &[".config/niri", ".config/environment.d/demo.conf"],
        &[
            (".config/niri/config.kdl", "niri"),
            (".config/niri/fragments/binds.kdl", "binds"),
            (".config/environment.d/demo.conf", "A=1"),
            (".config/ghostty/config", "seed"),
        ],
    );
    let installed = profiles(root.path());
    let profile = &installed["demo"];
    assert_eq!(profile.source_root, root.path().join("usr/share/demo"));
    assert_eq!(profile.packages, vec!["niri", "ghostty>=1.0"]);
    assert_eq!(
        profile.manage,
        BTreeSet::from([
            PathBuf::from(".config/environment.d/demo.conf"),
            PathBuf::from(".config/niri"),
        ])
    );
    assert!(profile.is_managed(Path::new(".config/niri/fragments/binds.kdl")));
    assert!(!profile.is_managed(Path::new(".config/ghostty/config")));
}

// Protects the safety boundary created by removing explicit source/target
// mappings: every managed path must exist in the fixed content tree, paths may
// not overlap, and profile content may not contain symbolic links.
#[test]
fn discovery_rejects_missing_overlap_and_symlink_content() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "missing",
        &[],
        &[".config/missing"],
        &[(".config/other", "x")],
    );
    assert!(discover_profiles(&root.path().join("usr/share/catdot/profiles")).is_err());

    fs::remove_dir_all(root.path().join("usr/share/catdot/profiles/missing")).unwrap();
    fs::remove_dir_all(root.path().join("usr/share/missing")).unwrap();
    install_profile(
        root.path(),
        "overlap",
        &[],
        &[".config/niri", ".config/niri/config.kdl"],
        &[(".config/niri/config.kdl", "x")],
    );
    assert!(discover_profiles(&root.path().join("usr/share/catdot/profiles")).is_err());

    fs::remove_dir_all(root.path().join("usr/share/catdot/profiles/overlap")).unwrap();
    fs::remove_dir_all(root.path().join("usr/share/overlap")).unwrap();
    install_profile(root.path(), "linked", &[], &[], &[(".config/real", "x")]);
    symlink("real", root.path().join("usr/share/linked/.config/linked")).unwrap();
    assert!(discover_profiles(&root.path().join("usr/share/catdot/profiles")).is_err());
}

// Protects first installation: managed files replace existing content through
// the activation journal, missing seed files are initialized, and existing
// user-owned seed targets are preserved but recorded for this profile.
#[test]
fn first_select_overwrites_managed_and_seeds_once_per_profile() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/niri/config.kdl"],
        &[
            (".config/niri/config.kdl", "managed-v1"),
            (".config/ghostty/config", "seed-default"),
            (".config/app/new.conf", "new-seed"),
        ],
    );
    let profiles = profiles(root.path());
    let home = root.path().join("home/alice");
    fs::create_dir_all(home.join(".config/niri")).unwrap();
    fs::create_dir_all(home.join(".config/ghostty")).unwrap();
    fs::write(home.join(".config/niri/config.kdl"), "user-old").unwrap();
    fs::write(home.join(".config/ghostty/config"), "user-seed").unwrap();

    let old = UserState::default();
    let new = retained(None, &["demo"]);
    let new = apply(
        root.path(),
        &profiles,
        &old,
        new,
        "demo",
        ActivationMode::Select,
    );

    assert_eq!(
        fs::read_to_string(home.join(".config/niri/config.kdl")).unwrap(),
        "managed-v1"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/ghostty/config")).unwrap(),
        "user-seed"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/app/new.conf")).unwrap(),
        "new-seed"
    );
    assert_eq!(
        new.profiles["demo"].seeded,
        BTreeSet::from([
            PathBuf::from(".config/app/new.conf"),
            PathBuf::from(".config/ghostty/config"),
        ])
    );
}

// Protects the explicit update boundary: selecting the active profile does not
// refresh package-provided managed files, while update refreshes only managed
// content and never rewrites seed content.
#[test]
fn managed_refresh_requires_update_and_never_refreshes_seed() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/niri/config.kdl"],
        &[
            (".config/niri/config.kdl", "managed-v1"),
            (".config/ghostty/config", "seed-v1"),
        ],
    );
    let mut installed = profiles(root.path());
    let initial = apply(
        root.path(),
        &installed,
        &UserState::default(),
        retained(None, &["demo"]),
        "demo",
        ActivationMode::Select,
    );
    let home = root.path().join("home/alice");
    fs::write(home.join(".config/niri/config.kdl"), "user-managed-edit").unwrap();
    fs::write(home.join(".config/ghostty/config"), "user-seed-edit").unwrap();
    fs::write(
        root.path().join("usr/share/demo/.config/niri/config.kdl"),
        "managed-v2",
    )
    .unwrap();
    fs::write(
        root.path().join("usr/share/demo/.config/ghostty/config"),
        "seed-v2",
    )
    .unwrap();
    installed = profiles(root.path());

    let registry = managed_targets_path(&state_path(&home)).unwrap();
    let select = build_activation_plan(
        &installed,
        &initial,
        "demo",
        &home,
        &registry,
        ActivationMode::Select,
    )
    .unwrap();
    assert!(!select.has_changes());

    let updated = apply(
        root.path(),
        &installed,
        &initial,
        initial.clone(),
        "demo",
        ActivationMode::Update,
    );
    assert_eq!(updated.active_profile.as_deref(), Some("demo"));
    assert_eq!(
        fs::read_to_string(home.join(".config/niri/config.kdl")).unwrap(),
        "managed-v2"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/ghostty/config")).unwrap(),
        "user-seed-edit"
    );
}

// Protects complete-profile switching: old managed targets are removed, common
// targets are replaced by the new profile, and seed initialization is tracked
// independently for each retained profile.
#[test]
fn switching_profiles_replaces_only_managed_content() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "alpha",
        &[],
        &[".config/wm/config", ".config/alpha-only"],
        &[
            (".config/wm/config", "alpha"),
            (".config/alpha-only", "alpha-only"),
            (".config/shared/seed", "alpha-seed"),
        ],
    );
    install_profile(
        root.path(),
        "beta",
        &[],
        &[".config/wm/config"],
        &[
            (".config/wm/config", "beta"),
            (".config/shared/seed", "beta-seed"),
        ],
    );
    let profiles = profiles(root.path());
    let alpha = apply(
        root.path(),
        &profiles,
        &UserState::default(),
        retained(None, &["alpha"]),
        "alpha",
        ActivationMode::Select,
    );
    let mut beta_state = alpha.clone();
    beta_state
        .profiles
        .insert("beta".into(), ProfileState::default());
    beta_state.generation += 1;
    let beta = apply(
        root.path(),
        &profiles,
        &alpha,
        beta_state,
        "beta",
        ActivationMode::Select,
    );
    let home = root.path().join("home/alice");
    assert_eq!(
        fs::read_to_string(home.join(".config/wm/config")).unwrap(),
        "beta"
    );
    assert!(!home.join(".config/alpha-only").exists());
    assert_eq!(
        fs::read_to_string(home.join(".config/shared/seed")).unwrap(),
        "alpha-seed"
    );
    assert!(
        beta.profiles["alpha"]
            .seeded
            .contains(Path::new(".config/shared/seed"))
    );
    assert!(
        beta.profiles["beta"]
            .seeded
            .contains(Path::new(".config/shared/seed"))
    );
    assert_eq!(
        read_managed_registry(&managed_targets_path(&state_path(&home)).unwrap())
            .unwrap()
            .active_profile
            .as_deref(),
        Some("beta")
    );
}

// Protects explicit reset semantics: all profile-provided content is backed up
// and replaced, including paths that normally behave as seeds.
#[test]
fn reset_reinstalls_managed_and_seed_content() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/managed"],
        &[(".config/managed", "managed"), (".config/seed", "seed")],
    );
    let profiles = profiles(root.path());
    let initial = apply(
        root.path(),
        &profiles,
        &UserState::default(),
        retained(None, &["demo"]),
        "demo",
        ActivationMode::Select,
    );
    let home = root.path().join("home/alice");
    fs::write(home.join(".config/managed"), "changed-managed").unwrap();
    fs::write(home.join(".config/seed"), "changed-seed").unwrap();
    let reset = apply(
        root.path(),
        &profiles,
        &initial,
        initial.clone(),
        "demo",
        ActivationMode::Reset,
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/managed")).unwrap(),
        "managed"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/seed")).unwrap(),
        "seed"
    );
    assert_eq!(reset.profiles["demo"].seeded.len(), 1);
}

// Protects package ownership after component removal: every retained profile
// contributes its complete package declaration even when it is not active.
#[test]
fn packages_are_aggregated_from_all_retained_profiles() {
    let root = tempdir().unwrap();
    install_profile(root.path(), "alpha", &["niri", "shared"], &[], &[]);
    install_profile(root.path(), "beta", &["kwin", "shared"], &[], &[]);
    let profiles = profiles(root.path());
    let mut state = retained(Some("alpha"), &["alpha", "beta"]);
    catdot_core::prepare_profile_state(&mut state, &profiles["alpha"], ActivationMode::Select)
        .unwrap();
    catdot_core::prepare_profile_state(&mut state, &profiles["beta"], ActivationMode::Select)
        .unwrap();
    assert_eq!(
        packages_for_state(&state, &profiles).unwrap(),
        BTreeSet::from(["kwin".into(), "niri".into(), "shared".into()])
    );
}

// Protects the initial symbolic-link ban independently of manifest validation:
// copied content must remain regular and preserve executable permissions.
#[test]
fn activation_preserves_regular_file_modes() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".local/bin/demo"],
        &[(".local/bin/demo", "#!/bin/sh\n")],
    );
    let source = root.path().join("usr/share/demo/.local/bin/demo");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    let profiles = profiles(root.path());
    apply(
        root.path(),
        &profiles,
        &UserState::default(),
        retained(None, &["demo"]),
        "demo",
        ActivationMode::Select,
    );
    assert_eq!(
        fs::metadata(root.path().join("home/alice/.local/bin/demo"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

// Protects the transactional file boundary retained by the KISS design:
// interrupted activation must restore binary data, modes, directories, and
// symbolic links that existed in the user's old configuration.
#[test]
fn activation_recovery_restores_the_previous_tree() {
    use catdot_core::recover_activation_journals;

    let temp = tempdir().unwrap();
    let state = temp.path().join("home/.local/state/catdot/state.toml");
    let target = temp.path().join("home/.config/app");
    fs::create_dir_all(target.join("empty")).unwrap();
    fs::write(target.join("bin"), [0_u8, 7, 255]).unwrap();
    fs::set_permissions(target.join("bin"), fs::Permissions::from_mode(0o751)).unwrap();
    symlink("bin", target.join("current")).unwrap();
    let old = UserState::default();
    let mut new = old.clone();
    new.generation = 1;
    write_state(&state, &old).unwrap();

    let mut journal = ActivationJournal::begin(&state, old.clone(), new).unwrap();
    journal.track_path(&target).unwrap();
    journal.mark_applying().unwrap();
    fs::remove_dir_all(&target).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("changed"), "broken").unwrap();
    journal.mark_applied().unwrap();
    drop(journal);

    recover_activation_journals(&state).unwrap();
    assert_eq!(fs::read(target.join("bin")).unwrap(), [0_u8, 7, 255]);
    assert_eq!(
        fs::metadata(target.join("bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    assert!(target.join("empty").is_dir());
    assert_eq!(
        fs::read_link(target.join("current")).unwrap(),
        Path::new("bin")
    );
    assert_eq!(catdot_core::read_state(&state).unwrap(), old);
}

// Protects concurrent user state: recovery must not roll back a third state
// value that was not produced by the interrupted activation.
#[test]
fn activation_recovery_refuses_externally_changed_state() {
    use catdot_core::recover_activation_journals;

    let temp = tempdir().unwrap();
    let state = temp.path().join("home/.local/state/catdot/state.toml");
    let old = UserState::default();
    let mut new = old.clone();
    new.generation = 1;
    write_state(&state, &old).unwrap();
    let mut journal = ActivationJournal::begin(&state, old.clone(), new).unwrap();
    journal.mark_applying().unwrap();
    let mut external = old;
    external.generation = 2;
    write_state(&state, &external).unwrap();
    drop(journal);

    assert!(recover_activation_journals(&state).is_err());
    assert_eq!(catdot_core::read_state(&state).unwrap(), external);
}

// Protects bounded recovery storage: successful switches and updates must not
// grow the per-user backup directory without limit.
#[test]
fn activation_backup_retention_keeps_five_generations() {
    let temp = tempdir().unwrap();
    let state = temp.path().join("home/.local/state/catdot/state.toml");
    let target = temp.path().join("home/.config/demo");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "old").unwrap();
    let old = UserState::default();
    write_state(&state, &old).unwrap();

    for generation in 1..=6 {
        let mut new = old.clone();
        new.generation = generation;
        let mut journal = ActivationJournal::begin(&state, old.clone(), new).unwrap();
        journal.track_path(&target).unwrap();
        journal.mark_applying().unwrap();
        fs::write(&target, format!("generation-{generation}")).unwrap();
        journal.mark_applied().unwrap();
        journal.complete().unwrap();
    }

    assert_eq!(
        fs::read_dir(temp.path().join("home/.local/state/catdot/backups"))
            .unwrap()
            .count(),
        5
    );
}

// Protects HOME containment even with a safe manifest path: activation may not
// traverse a symbolic-link parent created by the user.
#[test]
fn activation_rejects_symbolic_link_parents() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/demo/config"],
        &[(".config/demo/config", "managed")],
    );
    let installed = profiles(root.path());
    let home = root.path().join("home/alice");
    let outside = root.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&home).unwrap();
    symlink(&outside, home.join(".config")).unwrap();
    let state = retained(None, &["demo"]);
    let registry = managed_targets_path(&state_path(&home)).unwrap();
    assert!(
        build_activation_plan(
            &installed,
            &state,
            "demo",
            &home,
            &registry,
            ActivationMode::Select,
        )
        .is_err()
    );
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

// Protects the explicit-refresh contract: a package upgrade while a profile is
// inactive must not change that profile when it is selected again. Only update
// may replace the cached managed revision with the new package contents.
#[test]
fn switching_back_uses_cached_managed_revision_until_update() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "alpha",
        &[],
        &[".config/wm/config"],
        &[(".config/wm/config", "alpha-v1")],
    );
    install_profile(
        root.path(),
        "beta",
        &[],
        &[".config/wm/config"],
        &[(".config/wm/config", "beta")],
    );
    let mut installed = profiles(root.path());
    let alpha = apply(
        root.path(),
        &installed,
        &UserState::default(),
        retained(None, &["alpha"]),
        "alpha",
        ActivationMode::Select,
    );
    let mut with_beta = alpha.clone();
    with_beta
        .profiles
        .insert("beta".into(), ProfileState::default());
    with_beta.generation += 1;
    let beta = apply(
        root.path(),
        &installed,
        &alpha,
        with_beta,
        "beta",
        ActivationMode::Select,
    );
    fs::write(
        root.path().join("usr/share/alpha/.config/wm/config"),
        "alpha-v2",
    )
    .unwrap();
    installed = profiles(root.path());
    let selected_alpha = apply(
        root.path(),
        &installed,
        &beta,
        beta.clone(),
        "alpha",
        ActivationMode::Select,
    );
    let home = root.path().join("home/alice");
    assert_eq!(
        fs::read_to_string(home.join(".config/wm/config")).unwrap(),
        "alpha-v1"
    );
    apply(
        root.path(),
        &installed,
        &selected_alpha,
        selected_alpha.clone(),
        "alpha",
        ActivationMode::Update,
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/wm/config")).unwrap(),
        "alpha-v2"
    );
}

// Protects ownership contraction during an explicit update: a path removed
// from manage becomes user-owned in place instead of being deleted.
#[test]
fn update_releases_removed_managed_paths_without_deleting_them() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/demo/kept", ".config/demo/released"],
        &[
            (".config/demo/kept", "kept-v1"),
            (".config/demo/released", "released-v1"),
        ],
    );
    let mut installed = profiles(root.path());
    let initial = apply(
        root.path(),
        &installed,
        &UserState::default(),
        retained(None, &["demo"]),
        "demo",
        ActivationMode::Select,
    );
    let home = root.path().join("home/alice");
    fs::write(home.join(".config/demo/released"), "user-edit").unwrap();

    fs::write(
        root.path().join("usr/share/catdot/profiles/demo/profile.toml"),
        "schema = 4\nname = \"demo\"\ndescription = \"test profile\"\npackages = []\nmanage = [\".config/demo/kept\"]\n",
    )
    .unwrap();
    fs::write(
        root.path().join("usr/share/demo/.config/demo/kept"),
        "kept-v2",
    )
    .unwrap();
    installed = profiles(root.path());
    let updated = apply(
        root.path(),
        &installed,
        &initial,
        initial.clone(),
        "demo",
        ActivationMode::Update,
    );

    assert_eq!(
        fs::read_to_string(home.join(".config/demo/kept")).unwrap(),
        "kept-v2"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/released")).unwrap(),
        "user-edit"
    );
    assert!(
        !updated.profiles["demo"]
            .managed
            .contains(Path::new(".config/demo/released"))
    );
    let registry =
        read_managed_registry(&managed_targets_path(&state_path(&home)).unwrap()).unwrap();
    assert!(!registry.entries.contains_key(".config/demo/released"));
}

// Protects the simple managed-to-seed transition used during full Profile
// switching: an existing managed target is released in place and becomes
// user-owned instead of disappearing. Its pre-takeover contents remain in the
// activation backup for manual recovery.
#[test]
fn switching_from_managed_to_seed_releases_the_existing_target() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "managed",
        &[],
        &[".config/app/config"],
        &[(".config/app/config", "managed-value")],
    );
    install_profile(
        root.path(),
        "seed",
        &[],
        &[],
        &[(".config/app/config", "seed-default")],
    );
    let installed = profiles(root.path());
    let managed = apply(
        root.path(),
        &installed,
        &UserState::default(),
        retained(None, &["managed"]),
        "managed",
        ActivationMode::Select,
    );
    let mut seed_state = managed.clone();
    seed_state
        .profiles
        .insert("seed".into(), ProfileState::default());
    let seed = apply(
        root.path(),
        &installed,
        &managed,
        seed_state,
        "seed",
        ActivationMode::Select,
    );
    let home = root.path().join("home/alice");
    assert_eq!(
        fs::read_to_string(home.join(".config/app/config")).unwrap(),
        "managed-value"
    );
    assert!(
        seed.profiles["seed"]
            .seeded
            .contains(Path::new(".config/app/config"))
    );
    assert!(
        read_managed_registry(&managed_targets_path(&state_path(&home)).unwrap())
            .unwrap()
            .entries
            .is_empty()
    );
}
