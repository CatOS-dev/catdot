use catdot_core::{
    ActivationMode, ProfileState, UserState, apply_activation_plan, build_activation_plan,
    cache_profile_content, discover_profiles, profile_cache_path, prune_candidates,
    retained_packages, state_path,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
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

fn backup_file(backup: &Path, relative: &str) -> String {
    fs::read_to_string(backup.join("home").join(relative)).unwrap()
}

// Protects the deliberately small manifest language: package declarations are
// exact pacman package names and do not embed version or dependency syntax.
#[test]
fn manifest_rejects_package_version_constraints() {
    let root = tempdir().unwrap();
    install_profile(root.path(), "demo", &["demo>=2"], &[], &[]);
    assert!(discover_profiles(&root.path().join("usr/share/catdot/profiles")).is_err());
}

// Protects first activation semantics: both managed and seed content overwrite
// existing targets, and every overwritten target is copied into one backup.
#[test]
fn first_select_overwrites_managed_and_seed_after_backup() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &["niri"],
        &[".config/demo/managed"],
        &[
            (".config/demo/managed", "managed-new"),
            (".config/demo/seed", "seed-new"),
        ],
    );
    let installed = profiles(root.path());
    let profile = &installed["demo"];
    let home = root.path().join("home/alice");
    let state_file = state_path(&home);
    fs::create_dir_all(home.join(".config/demo")).unwrap();
    fs::write(home.join(".config/demo/managed"), "managed-old").unwrap();
    fs::write(home.join(".config/demo/seed"), "seed-old").unwrap();

    let cache = profile_cache_path(&state_file, "demo").unwrap();
    cache_profile_content(profile, &cache).unwrap();
    let target = ProfileState::from_profile(profile, false).unwrap();
    let plan = build_activation_plan(
        &UserState::default(),
        "demo",
        &target,
        &cache,
        &home,
        ActivationMode::Select,
    )
    .unwrap();
    let backup = apply_activation_plan(&plan, &home, &state_file)
        .unwrap()
        .unwrap();

    assert_eq!(
        fs::read_to_string(home.join(".config/demo/managed")).unwrap(),
        "managed-new"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/seed")).unwrap(),
        "seed-new"
    );
    assert_eq!(backup_file(&backup, ".config/demo/managed"), "managed-old");
    assert_eq!(backup_file(&backup, ".config/demo/seed"), "seed-old");
}

// Protects the ownership boundary after initialization: selecting the same
// retained Profile overwrites managed content but leaves seed content alone.
#[test]
fn repeated_select_overwrites_only_managed_content() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/demo/managed"],
        &[
            (".config/demo/managed", "managed-profile"),
            (".config/demo/seed", "seed-profile"),
        ],
    );
    let installed = profiles(root.path());
    let profile = &installed["demo"];
    let home = root.path().join("home/alice");
    let state_file = state_path(&home);
    fs::create_dir_all(home.join(".config/demo")).unwrap();
    fs::write(home.join(".config/demo/managed"), "managed-user").unwrap();
    fs::write(home.join(".config/demo/seed"), "seed-user").unwrap();
    let cache = profile_cache_path(&state_file, "demo").unwrap();
    cache_profile_content(profile, &cache).unwrap();

    let mut state = UserState {
        active_profile: Some("demo".into()),
        ..UserState::default()
    };
    state.profiles.insert(
        "demo".into(),
        ProfileState::from_profile(profile, true).unwrap(),
    );
    let plan = build_activation_plan(
        &state,
        "demo",
        &state.profiles["demo"],
        &cache,
        &home,
        ActivationMode::Select,
    )
    .unwrap();
    let backup = apply_activation_plan(&plan, &home, &state_file)
        .unwrap()
        .unwrap();

    assert_eq!(
        fs::read_to_string(home.join(".config/demo/managed")).unwrap(),
        "managed-profile"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/seed")).unwrap(),
        "seed-user"
    );
    assert_eq!(backup_file(&backup, ".config/demo/managed"), "managed-user");
    assert!(!backup.join("home/.config/demo/seed").exists());
}

// Protects complete Profile switching: old managed roots are removed after
// backup, then the new Profile's managed and first-activation seed content win.
#[test]
fn switching_profiles_replaces_old_managed_with_new_seed() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "alpha",
        &[],
        &[".config/app"],
        &[
            (".config/app/config", "alpha-managed"),
            (".config/app/obsolete", "alpha-obsolete"),
        ],
    );
    install_profile(
        root.path(),
        "beta",
        &[],
        &[],
        &[(".config/app/config", "beta-seed")],
    );
    let installed = profiles(root.path());
    let home = root.path().join("home/alice");
    let state_file = state_path(&home);
    fs::create_dir_all(home.join(".config/app")).unwrap();
    fs::write(home.join(".config/app/config"), "alpha-managed").unwrap();
    fs::write(home.join(".config/app/obsolete"), "alpha-obsolete").unwrap();
    let beta_cache = profile_cache_path(&state_file, "beta").unwrap();
    cache_profile_content(&installed["beta"], &beta_cache).unwrap();

    let mut state = UserState {
        active_profile: Some("alpha".into()),
        ..UserState::default()
    };
    state.profiles.insert(
        "alpha".into(),
        ProfileState::from_profile(&installed["alpha"], true).unwrap(),
    );
    let beta = ProfileState::from_profile(&installed["beta"], false).unwrap();
    let plan = build_activation_plan(
        &state,
        "beta",
        &beta,
        &beta_cache,
        &home,
        ActivationMode::Select,
    )
    .unwrap();
    let backup = apply_activation_plan(&plan, &home, &state_file)
        .unwrap()
        .unwrap();

    assert_eq!(
        fs::read_to_string(home.join(".config/app/config")).unwrap(),
        "beta-seed"
    );
    assert!(!home.join(".config/app/obsolete").exists());
    assert_eq!(backup_file(&backup, ".config/app/config"), "alpha-managed");
    assert_eq!(
        backup_file(&backup, ".config/app/obsolete"),
        "alpha-obsolete"
    );
}

// Protects explicit update semantics: active managed content is overwritten
// from the refreshed cache, while seed content remains untouched.
#[test]
fn update_active_profile_overwrites_managed_but_not_seed() {
    let root = tempdir().unwrap();
    install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/demo/managed"],
        &[
            (".config/demo/managed", "managed-v2"),
            (".config/demo/seed", "seed-v2"),
        ],
    );
    let installed = profiles(root.path());
    let home = root.path().join("home/alice");
    let state_file = state_path(&home);
    fs::create_dir_all(home.join(".config/demo")).unwrap();
    fs::write(home.join(".config/demo/managed"), "managed-v1").unwrap();
    fs::write(home.join(".config/demo/seed"), "seed-user").unwrap();
    let cache = profile_cache_path(&state_file, "demo").unwrap();
    cache_profile_content(&installed["demo"], &cache).unwrap();

    let mut state = UserState {
        active_profile: Some("demo".into()),
        ..UserState::default()
    };
    state.profiles.insert(
        "demo".into(),
        ProfileState::from_profile(&installed["demo"], true).unwrap(),
    );
    let plan = build_activation_plan(
        &state,
        "demo",
        &state.profiles["demo"],
        &cache,
        &home,
        ActivationMode::Update,
    )
    .unwrap();
    apply_activation_plan(&plan, &home, &state_file).unwrap();

    assert_eq!(
        fs::read_to_string(home.join(".config/demo/managed")).unwrap(),
        "managed-v2"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/seed")).unwrap(),
        "seed-user"
    );
}

// Protects package pruning without a dependency solver in Catdot: only direct
// packages introduced by Catdot and no longer referenced by retained Profiles
// are candidates; pacman decides the dependency closure later.
#[test]
fn prune_candidates_use_introduced_direct_packages_only() {
    let mut state = UserState {
        introduced_packages: BTreeSet::from([
            "ghostty".to_owned(),
            "niri".to_owned(),
            "unused".to_owned(),
        ]),
        ..UserState::default()
    };
    state.profiles.insert(
        "desktop".into(),
        ProfileState {
            initialized: true,
            packages: BTreeSet::from(["niri".to_owned(), "ghostty".to_owned()]),
            manage: BTreeSet::new(),
        },
    );
    assert_eq!(
        retained_packages(&state),
        BTreeSet::from(["ghostty".to_owned(), "niri".to_owned()])
    );
    assert_eq!(
        prune_candidates(&state),
        BTreeSet::from(["unused".to_owned()])
    );
}

// Protects regular file mode preservation when Profile content is copied into
// the cache and then overlaid into HOME.
#[test]
fn profile_copy_preserves_executable_mode() {
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
    let installed = profiles(root.path());
    let home = root.path().join("home/alice");
    let state_file = state_path(&home);
    let cache = profile_cache_path(&state_file, "demo").unwrap();
    cache_profile_content(&installed["demo"], &cache).unwrap();
    let target = ProfileState::from_profile(&installed["demo"], false).unwrap();
    let plan = build_activation_plan(
        &UserState::default(),
        "demo",
        &target,
        &cache,
        &home,
        ActivationMode::Select,
    )
    .unwrap();
    apply_activation_plan(&plan, &home, &state_file).unwrap();
    assert_eq!(
        fs::metadata(home.join(".local/bin/demo"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}
