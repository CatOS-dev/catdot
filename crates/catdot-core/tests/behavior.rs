use catdot_core::*;
use fs2::FileExt;
use std::{fs, os::unix::fs::MetadataExt};
use tempfile::tempdir;

struct FakeBackend(std::collections::BTreeMap<String, PackageAvailability>);

impl PackageBackend for FakeBackend {
    fn availability(&self, package: &str) -> catdot_core::Result<PackageAvailability> {
        Ok(*self
            .0
            .get(package)
            .unwrap_or(&PackageAvailability::Unavailable))
    }

    fn can_remove(&self, _package: &str) -> catdot_core::Result<bool> {
        Ok(true)
    }
}

fn manifest() -> &'static str {
    r#"
schema = 1
[profile]
id = "demo"
name = "Demo"
description = "test"
[defaults]
bar = "waybar"
[components.waybar]
role = "bar"
path = "waybar"
packages = ["waybar"]
exec = ["waybar", "--config", "{component}/config"]
[[components.waybar.links]]
source = "config"
target = "{xdg_config_home}/waybar/config"
"#
}

#[test]
fn discovery_rejects_unknown_fields_and_escape() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("demo");
    fs::create_dir(&p).unwrap();
    fs::write(p.join("profile.toml"), manifest()).unwrap();
    fs::create_dir(p.join("waybar")).unwrap();
    fs::write(p.join("waybar/config"), "{}").unwrap();
    assert_eq!(discover_profiles(dir.path()).unwrap().len(), 1);
    fs::write(
        p.join("profile.toml"),
        manifest().replace("schema = 1", "schema = 1\ntypo = true"),
    )
    .unwrap();
    assert!(discover_profiles(dir.path()).is_err());
    fs::write(
        p.join("profile.toml"),
        manifest().replace("path = \"waybar\"", "path = \"../outside\""),
    )
    .unwrap();
    assert!(discover_profiles(dir.path()).is_err());
    fs::write(
        p.join("profile.toml"),
        manifest().replace("{xdg_config_home}/waybar/config", "{home}/../outside"),
    )
    .unwrap();
    assert!(discover_profiles(dir.path()).is_err());
    fs::write(
        p.join("profile.toml"),
        manifest().replace(
            "exec = [\"waybar\", \"--config\", \"{component}/config\"]",
            "backend = \"gtk\"\n[components.waybar.settings]\ntheme = \"Dark\"\nicon_theme = \"Icons\"\ncursor_theme = \"Cursor\"\nfont = \"Sans 10\"\ncolor_scheme = \"prefer-dark\"\ntypo = \"bad\"",
        ),
    )
    .unwrap();
    assert!(discover_profiles(dir.path()).is_err());
    fs::write(
        p.join("profile.toml"),
        manifest().replace(
            "exec = [\"waybar\", \"--config\", \"{component}/config\"]",
            "exec = [\"/usr/bin/env\", \"sh\", \"-c\", \"echo unsafe\"]",
        ),
    )
    .unwrap();
    assert!(discover_profiles(dir.path()).is_err());
}

#[test]
fn discovery_keeps_valid_profiles_when_another_manifest_is_invalid() {
    let dir = tempdir().unwrap();
    let valid = dir.path().join("valid");
    let invalid = dir.path().join("invalid");
    fs::create_dir_all(valid.join("waybar")).unwrap();
    fs::create_dir_all(&invalid).unwrap();
    fs::write(valid.join("waybar/config"), "config").unwrap();
    fs::write(
        valid.join("profile.toml"),
        manifest().replace("demo", "valid"),
    )
    .unwrap();
    fs::write(invalid.join("profile.toml"), "not valid = [toml").unwrap();

    let registry = discover_profile_registry(dir.path()).unwrap();

    assert!(registry.valid_profiles.contains_key("valid"));
    assert_eq!(registry.diagnostics.len(), 1);
    let diagnostic = &registry.diagnostics[0];
    assert_eq!(diagnostic.profile_directory, invalid);
    assert_eq!(diagnostic.manifest_path, invalid.join("profile.toml"));
    assert_eq!(diagnostic.kind, ProfileDiagnosticKind::Toml);
    assert!(!diagnostic.message.is_empty());
}

#[test]
fn discovery_rejects_a_profile_directory_symlink_outside_the_profile_root() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("profiles");
    let outside = dir.path().join("outside");
    fs::create_dir_all(outside.join("waybar")).unwrap();
    fs::write(outside.join("waybar/config"), "{}").unwrap();
    fs::write(outside.join("profile.toml"), manifest()).unwrap();
    fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("demo")).unwrap();
    assert!(discover_profiles(&root).is_err());
}

#[test]
fn selection_exec_and_dependency_aggregation_are_deterministic() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("demo");
    fs::create_dir(&p).unwrap();
    fs::create_dir(p.join("waybar")).unwrap();
    fs::write(p.join("waybar/config"), "{}").unwrap();
    fs::write(p.join("profile.toml"), manifest()).unwrap();
    let profiles = discover_profiles(dir.path()).unwrap();
    let profile = profiles.get("demo").unwrap();
    let state = select_profile(profile).unwrap();
    assert_eq!(state.components["bar"], "demo/waybar");
    assert_eq!(
        expand_exec(
            profile,
            "waybar",
            "/home/a",
            "/home/a/.config",
            &["x".into()]
        )
        .unwrap(),
        vec![
            "waybar".to_string(),
            "--config".to_string(),
            p.join("waybar/config").display().to_string(),
            "x".to_string()
        ]
    );
    assert_eq!(
        expand_exec(
            profile,
            "waybar",
            "/home/a",
            "/home/a/.config",
            &[";touch /tmp/not-a-command".into()]
        )
        .unwrap()
        .last()
        .unwrap(),
        ";touch /tmp/not-a-command"
    );
    let a = UserRecord::from_state(1000, &state, &profiles, false).unwrap();
    let b = UserRecord::from_state(1001, &state, &profiles, false).unwrap();
    let needs = aggregate_requirements(&[a, b]);
    assert_eq!(needs["waybar"].uids, vec![1000, 1001]);
    assert_eq!(needs["waybar"].references[0].uid, 1000);
    assert_eq!(needs["waybar"].references[1].uid, 1001);
}

#[test]
fn user_state_validation_rejects_wrong_role_and_malformed_references() {
    let dir = tempdir().unwrap();
    let profile_dir = dir.path().join("demo");
    fs::create_dir_all(&profile_dir).unwrap();
    let profile = Profile {
        id: "demo".into(),
        name: "Demo".into(),
        description: "test".into(),
        root: profile_dir,
        defaults: Default::default(),
        components: [
            (
                "terminal".into(),
                ComponentDef {
                    role: "terminal".into(),
                    path: dir.path().into(),
                    packages: vec![],
                    optional_packages: vec![],
                    exec: vec![],
                    links: vec![],
                    backend: None,
                    settings: Default::default(),
                },
            ),
            (
                "waybar".into(),
                ComponentDef {
                    role: "bar".into(),
                    path: dir.path().into(),
                    packages: vec![],
                    optional_packages: vec![],
                    exec: vec![],
                    links: vec![],
                    backend: None,
                    settings: Default::default(),
                },
            ),
        ]
        .into_iter()
        .collect(),
    };
    let profiles = [("demo".into(), profile)].into_iter().collect();

    for reference in ["demo/waybar", "demo//terminal", "demo/", "/terminal"] {
        let state = UserState {
            generation: 1,
            components: [("terminal".into(), reference.into())]
                .into_iter()
                .collect(),
        };
        assert!(
            validate_user_state(&state, &profiles).is_err(),
            "{reference}"
        );
    }
}

#[test]
fn user_state_lock_prevents_overlapping_updates_and_releases() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.toml");
    let lock_path = state_lock_path(&path).unwrap();
    let held = lock(&lock_path).unwrap();
    let second = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(second.try_lock_exclusive().is_err());
    drop(held);
    second.lock_exclusive().unwrap();
}

#[test]
fn plan_digest_and_prune_rules_protect_packages() {
    let plan = PackagePlan {
        install: vec!["waybar".into()],
        remove: vec![],
        satisfied: vec![],
    };
    assert_ne!(
        plan.digest(),
        PackagePlan {
            install: vec!["mako".into()],
            remove: vec![],
            satisfied: vec![]
        }
        .digest()
    );
    let owned = ManagedPackage {
        name: "waybar".into(),
        catdot_installed: true,
        was_missing_before_catdot: true,
        install_reason: InstallReason::Dependency,
        references: vec![],
    };
    assert!(prunable(&owned, false));
    assert!(!prunable(
        &ManagedPackage {
            references: vec![PackageReference {
                uid: 1000,
                component: "demo/waybar".into(),
            }],
            ..owned.clone()
        },
        false
    ));
    assert!(!prunable(
        &ManagedPackage {
            install_reason: InstallReason::Explicit,
            ..owned.clone()
        },
        false
    ));
    assert!(!prunable(
        &ManagedPackage {
            was_missing_before_catdot: false,
            ..owned
        },
        false
    ));
}

#[test]
fn package_plan_uses_injected_backend_without_touching_the_system() {
    let backend = FakeBackend(
        [
            ("fuzzel".into(), PackageAvailability::Available),
            ("waybar".into(), PackageAvailability::Installed),
        ]
        .into_iter()
        .collect(),
    );
    let packages = ["fuzzel".into(), "waybar".into()].into_iter().collect();
    let plan = install_plan(&backend, &packages).unwrap();
    assert_eq!(plan.install, ["fuzzel"]);
    assert_eq!(plan.satisfied, ["waybar"]);
    assert!(install_plan(&backend, &["missing".into()].into_iter().collect()).is_err());
}

#[test]
fn atomic_write_replaces_complete_state_without_leaving_a_temporary_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.toml");
    fs::write(&path, "generation = 1\n").unwrap();
    atomic_write(&path, "generation = 2\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "generation = 2\n");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn activation_requires_a_registered_link_owner_and_ini_merge_preserves_keys() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("config");
    let registry = dir.path().join("managed-links.toml");
    fs::write(&target, "mine").unwrap();
    assert!(activate_managed_link(&registry, dir.path(), &target, false).is_err());

    fs::remove_file(&target).unwrap();
    let spoofed_source = dir.path().join("not-catdot");
    fs::write(&spoofed_source, "config").unwrap();
    std::os::unix::fs::symlink(&spoofed_source, &target).unwrap();
    assert!(activate_managed_link(&registry, dir.path(), &target, false).is_err());

    let source = dir.path().join("profile-config");
    fs::write(&source, "config").unwrap();
    activate_managed_link(&registry, &source, &target, true).unwrap();
    assert_eq!(fs::read_link(&target).unwrap(), source);
    assert_eq!(
        read_link_registry(&registry).unwrap().entries[&target.display().to_string()],
        source.display().to_string()
    );
    let merged = merge_ini(
        "[Settings]\nother=x\n[User]\nkeep=this\n",
        "Settings",
        &[("gtk-theme-name", "Dark")],
    )
    .unwrap();
    assert!(
        merged.contains("other=x")
            && merged.contains("keep=this")
            && merged.contains("gtk-theme-name=Dark")
    );
}

#[test]
fn deactivation_removes_only_the_exact_registered_symlink() {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("managed-links.toml");
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::write(&source, "config").unwrap();
    activate_managed_link(&registry, &source, &target, false).unwrap();
    deactivate_managed_link(&registry, &source, &target).unwrap();
    assert!(!target.exists());
    assert!(read_link_registry(&registry).unwrap().entries.is_empty());

    std::os::unix::fs::symlink(&source, &target).unwrap();
    assert!(deactivate_managed_link(&registry, &source, &target).is_err());
    assert!(
        fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn link_transaction_rolls_back_earlier_links_after_a_later_conflict() {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("managed-links.toml");
    let source = dir.path().join("source");
    let first = dir.path().join("first");
    let blocked = dir.path().join("blocked");
    fs::write(&source, "config").unwrap();
    fs::write(&blocked, "user file").unwrap();

    let mut transaction = LinkTransaction::new(&registry).unwrap();
    transaction.stage(&source, &first, false).unwrap();
    transaction.stage(&source, &blocked, false).unwrap();
    assert!(transaction.commit().is_err());
    transaction.rollback().unwrap();

    assert!(!first.exists());
    assert_eq!(fs::read_to_string(&blocked).unwrap(), "user file");
    assert!(read_link_registry(&registry).unwrap().entries.is_empty());
}

#[test]
fn reconciliation_replaces_a_managed_link_with_its_new_profile_source() {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("managed-links.toml");
    let source_a = dir.path().join("profile-a");
    let source_b = dir.path().join("profile-b");
    let target = dir.path().join("config");
    fs::write(&source_a, "a").unwrap();
    fs::write(&source_b, "b").unwrap();
    activate_managed_link(&registry, &source_a, &target, false).unwrap();

    reconcile_managed_links(&registry, &[(target.clone(), source_b.clone())], &[]).unwrap();

    assert_eq!(fs::read_link(&target).unwrap(), source_b);
    assert_eq!(
        read_link_registry(&registry).unwrap().entries[&target.display().to_string()],
        source_b.display().to_string()
    );
}

#[test]
fn reconciliation_removes_orphaned_registered_links_without_the_old_source_file() {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("managed-links.toml");
    let source = dir.path().join("removed-profile-source");
    let target = dir.path().join("bar-config");
    fs::write(&source, "bar").unwrap();
    activate_managed_link(&registry, &source, &target, false).unwrap();
    fs::remove_file(&source).unwrap();

    reconcile_managed_links(&registry, &[], &[]).unwrap();

    assert!(!target.exists());
    assert!(read_link_registry(&registry).unwrap().entries.is_empty());
}

#[test]
fn reconciliation_rejects_a_changed_managed_link_without_partial_changes() {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("managed-links.toml");
    let first_old = dir.path().join("first-old");
    let first_new = dir.path().join("first-new");
    let second_old = dir.path().join("second-old");
    let second_new = dir.path().join("second-new");
    let elsewhere = dir.path().join("elsewhere");
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    for source in [&first_old, &first_new, &second_old, &second_new, &elsewhere] {
        fs::write(source, "config").unwrap();
    }
    activate_managed_link(&registry, &first_old, &first, false).unwrap();
    activate_managed_link(&registry, &second_old, &second, false).unwrap();
    fs::remove_file(&second).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &second).unwrap();

    assert!(
        reconcile_managed_links(
            &registry,
            &[
                (first.clone(), first_new.clone()),
                (second.clone(), second_new)
            ],
            &[],
        )
        .is_err()
    );
    assert_eq!(fs::read_link(&first).unwrap(), first_old);
    assert_eq!(fs::read_link(&second).unwrap(), elsewhere);
}

#[test]
fn reconciliation_keeps_unchanged_links_in_place() {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("managed-links.toml");
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::write(&source, "config").unwrap();
    activate_managed_link(&registry, &source, &target, false).unwrap();
    let before = fs::symlink_metadata(&target).unwrap();

    reconcile_managed_links(&registry, &[(target.clone(), source)], &[]).unwrap();

    assert_eq!(fs::symlink_metadata(&target).unwrap().ino(), before.ino());
}

#[test]
fn shipped_niri_and_sway_profiles_are_valid_and_selectable() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let profiles = discover_profiles(&root).unwrap();
    for id in ["catos-niri-default", "catos-sway-default"] {
        let profile = profiles.get(id).unwrap();
        let state = select_profile(profile).unwrap();
        assert_eq!(state.components["wm"].split_once('/').unwrap().0, id);
        assert_eq!(state.components["browser"].split_once('/').unwrap().0, id);
        assert_eq!(state.components["editor"].split_once('/').unwrap().0, id);
        assert!(
            profile.components[profile.defaults.get("wm").unwrap()]
                .path
                .exists()
        );
    }
}

#[test]
fn gtk_and_qt_backends_merge_only_owned_keys() {
    let dir = tempdir().unwrap();
    let component = ComponentDef {
        role: "gtk-theme".into(),
        path: dir.path().into(),
        packages: vec![],
        optional_packages: vec![],
        exec: vec![],
        links: vec![],
        backend: Some("gtk".into()),
        settings: [
            ("theme".into(), "Dark".into()),
            ("icon_theme".into(), "Icons".into()),
            ("cursor_theme".into(), "Cursor".into()),
            ("font".into(), "Sans 10".into()),
            ("color_scheme".into(), "prefer-dark".into()),
        ]
        .into_iter()
        .collect(),
    };
    std::fs::create_dir_all(dir.path().join("gtk-3.0")).unwrap();
    std::fs::write(
        dir.path().join("gtk-3.0/settings.ini"),
        "[Settings]\ncustom=yes\n",
    )
    .unwrap();
    apply_theme(&component, dir.path(), false).unwrap();
    assert!(
        std::fs::read_to_string(dir.path().join("gtk-3.0/settings.ini"))
            .unwrap()
            .contains("custom=yes")
    );
}

#[test]
fn qtct_kvantum_refuses_plasma_without_writing_any_qt_configuration() {
    let dir = tempdir().unwrap();
    let component = ComponentDef {
        role: "qt-theme".into(),
        path: dir.path().into(),
        packages: vec![],
        optional_packages: vec![],
        exec: vec![],
        links: vec![],
        backend: Some("qtct-kvantum".into()),
        settings: [
            ("qt5_style".into(), "kvantum".into()),
            ("qt6_style".into(), "kvantum".into()),
            ("kvantum_theme".into(), "GraphiteDark".into()),
            ("icon_theme".into(), "Tela".into()),
        ]
        .into_iter()
        .collect(),
    };
    assert!(apply_theme(&component, dir.path(), true).is_err());
    assert!(!dir.path().join("qt5ct/qt5ct.conf").exists());
    assert!(!dir.path().join("qt6ct/qt6ct.conf").exists());
    assert!(!dir.path().join("Kvantum/kvantum.kvconfig").exists());
}
