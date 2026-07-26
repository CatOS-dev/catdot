use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use tempfile::tempdir;

fn install_profile(root: &std::path::Path, managed: &str, seed: &str) -> std::path::PathBuf {
    let metadata = root.join("usr/share/catdot/profiles/demo");
    let content = root.join("usr/share/demo/.config/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&content).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        "schema = 4\nname = \"Demo\"\ndescription = \"test\"\npackages = []\nmanage = [\".config/demo/managed\"]\n",
    )
    .unwrap();
    fs::write(content.join("managed"), managed).unwrap();
    fs::write(content.join("seed"), seed).unwrap();
    root.join("usr/share/catdot/profiles")
}

fn fake_pkexec(root: &std::path::Path) -> std::path::PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("pkexec");
    fs::write(
        &script,
        r#"#!/bin/sh
case "$2" in
  resolve-plan)
    printf '%s\n' 'system_update_required = false' '[plan]' 'install = []' 'remove = []' 'replacements = []' 'satisfied = []' '[requirements]'
    ;;
  *) exit 0 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn select_installs_complete_profile_and_removed_commands_stay_removed() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "managed-v1", "seed-v1");
    let bin = fake_pkexec(root.path());
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_catdot"))
            .args(args)
            .env("CATDOT_PROFILE_ROOT", &profile_root)
            .env("HOME", home.path())
            .env("PATH", &bin)
            .output()
            .unwrap()
    };

    let output = run(&["select", "demo", "--yes"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/managed")).unwrap(),
        "managed-v1"
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/seed")).unwrap(),
        "seed-v1"
    );
    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(state.contains("active_profile = \"demo\""));

    for removed in ["resolve", "disable", "exec", "apply"] {
        let output = run(&[removed]);
        assert!(
            !output.status.success(),
            "removed command {removed} unexpectedly succeeded"
        );
    }
}

#[test]
fn update_is_the_only_command_that_refreshes_active_managed_content() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "managed-v1", "seed-v1");
    let bin = fake_pkexec(root.path());
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_catdot"))
            .args(args)
            .env("CATDOT_PROFILE_ROOT", &profile_root)
            .env("HOME", home.path())
            .env("PATH", &bin)
            .output()
            .unwrap()
    };
    assert!(run(&["select", "demo", "--yes"]).status.success());
    let before = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    fs::write(
        root.path().join("usr/share/demo/.config/demo/managed"),
        "managed-v2",
    )
    .unwrap();
    fs::write(home.path().join(".config/demo/seed"), "user-seed").unwrap();
    let repeated = run(&["select", "demo", "--yes"]);
    assert!(repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("No changes are required."));
    assert_eq!(
        fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap(),
        before
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/managed")).unwrap(),
        "managed-v1"
    );
    assert!(run(&["update", "--yes"]).status.success());
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/managed")).unwrap(),
        "managed-v2"
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/seed")).unwrap(),
        "user-seed"
    );
}

#[test]
fn dry_run_restores_absent_state_and_does_not_touch_home() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "managed", "seed");
    let bin = fake_pkexec(root.path());
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["select", "demo", "--dry-run"])
        .env("CATDOT_PROFILE_ROOT", &profile_root)
        .env("HOME", home.path())
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.path().join(".local/state/catdot/state.toml").exists());
    assert!(!home.path().join(".config/demo").exists());
}

#[test]
fn inactive_profile_can_be_updated_without_switching_to_it() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "alpha-v1", "alpha-seed");
    let beta_metadata = root.path().join("usr/share/catdot/profiles/beta");
    let beta_content = root.path().join("usr/share/beta/.config/demo");
    fs::create_dir_all(&beta_metadata).unwrap();
    fs::create_dir_all(&beta_content).unwrap();
    fs::write(
        beta_metadata.join("profile.toml"),
        "schema = 4\nname = \"Beta\"\ndescription = \"test\"\npackages = []\nmanage = [\".config/demo/managed\"]\n",
    )
    .unwrap();
    fs::write(beta_content.join("managed"), "beta").unwrap();
    let bin = fake_pkexec(root.path());
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_catdot"))
            .args(args)
            .env("CATDOT_PROFILE_ROOT", &profile_root)
            .env("HOME", home.path())
            .env("PATH", &bin)
            .output()
            .unwrap()
    };

    assert!(run(&["select", "demo", "--yes"]).status.success());
    assert!(run(&["select", "beta", "--yes"]).status.success());
    fs::write(
        root.path().join("usr/share/demo/.config/demo/managed"),
        "alpha-v2",
    )
    .unwrap();

    let update = run(&["update", "demo", "--yes"]);
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert!(
        String::from_utf8_lossy(&update.stdout)
            .contains("updated without changing the active profile")
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/managed")).unwrap(),
        "beta"
    );
    assert!(run(&["select", "demo", "--yes"]).status.success());
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/managed")).unwrap(),
        "alpha-v2"
    );
}
