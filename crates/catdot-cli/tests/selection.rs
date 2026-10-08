use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::tempdir;

fn install_profile(
    root: &Path,
    id: &str,
    packages: &[&str],
    manage: &[&str],
    files: &[(&str, &str)],
) -> PathBuf {
    let metadata = root.join("usr/share/catdot/profiles").join(id);
    let content = root.join("usr/share").join(id);
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&content).unwrap();
    let packages = packages
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manage = manage
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 4\nname = \"{id}\"\ndescription = \"test\"\npackages = [{packages}]\nmanage = [{manage}]\n"
        ),
    )
    .unwrap();
    for (relative, contents) in files {
        let target = content.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, contents).unwrap();
    }
    root.join("usr/share/catdot/profiles")
}

fn rewrite_manifest(root: &Path, id: &str, name: &str, packages: &[&str], manage: &[&str]) {
    let packages = packages
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manage = manage
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        root.join("usr/share/catdot/profiles")
            .join(id)
            .join("profile.toml"),
        format!(
            "schema = 4\nname = \"{name}\"\ndescription = \"updated system manifest\"\npackages = [{packages}]\nmanage = [{manage}]\n"
        ),
    )
    .unwrap();
}

fn fake_commands(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let pacman = bin.join("pacman");
    fs::write(
        &pacman,
        r#"#!/bin/sh
printf 'pacman' >> "$CATDOT_COMMAND_LOG"
for arg in "$@"; do printf ' <%s>' "$arg" >> "$CATDOT_COMMAND_LOG"; done
printf '\n' >> "$CATDOT_COMMAND_LOG"
if [ "$1" = -Qq ]; then
    if [ -e "$CATDOT_QUERY_FAIL_FILE" ]; then
        printf 'simulated pacman database failure\n' >&2
        exit 2
    fi
    cat "$CATDOT_INSTALLED_DB"
    exit 0
fi
if [ "$1" = -S ]; then
    shift 3
    for package in "$@"; do
        grep -Fxq "$package" "$CATDOT_INSTALLED_DB" || printf '%s\n' "$package" >> "$CATDOT_INSTALLED_DB"
    done
    exit 0
fi
if [ "$1" = -Rns ]; then
    shift 2
    tmp="$CATDOT_INSTALLED_DB.tmp"
    cp "$CATDOT_INSTALLED_DB" "$tmp"
    for package in "$@"; do grep -Fxv "$package" "$tmp" > "$tmp.next" || true; mv "$tmp.next" "$tmp"; done
    mv "$tmp" "$CATDOT_INSTALLED_DB"
    exit 0
fi
exit 64
"#,
    )
    .unwrap();
    fs::set_permissions(&pacman, fs::Permissions::from_mode(0o755)).unwrap();
    let sudo = bin.join("sudo");
    fs::write(
        &sudo,
        r#"#!/bin/sh
printf 'sudo' >> "$CATDOT_COMMAND_LOG"
for arg in "$@"; do printf ' <%s>' "$arg" >> "$CATDOT_COMMAND_LOG"; done
printf '\n' >> "$CATDOT_COMMAND_LOG"
exec "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&sudo, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn run_with_state_home(
    root: &Path,
    home: &Path,
    profile_root: &Path,
    bin: &Path,
    state_home: Option<&Path>,
    args: &[&str],
) -> std::process::Output {
    let log = root.join("commands.log");
    let installed = root.join("installed.db");
    if !installed.exists() {
        fs::write(&installed, "").unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_catdot"));
    command
        .args(args)
        .env("CATDOT_PROFILE_ROOT", profile_root)
        .env("HOME", home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("CATDOT_COMMAND_LOG", log)
        .env("CATDOT_INSTALLED_DB", installed)
        .env("CATDOT_QUERY_FAIL_FILE", root.join("query-fail"));
    if let Some(state_home) = state_home {
        command.env("XDG_STATE_HOME", state_home);
    }
    command.output().unwrap()
}

fn run(
    root: &Path,
    home: &Path,
    profile_root: &Path,
    bin: &Path,
    args: &[&str],
) -> std::process::Output {
    run_with_state_home(root, home, profile_root, bin, None, args)
}

#[test]
fn select_delegates_install_to_pacman_and_records_new_direct_packages() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "demo",
        &["already", "new-package"],
        &[".config/demo/managed"],
        &[
            (".config/demo/managed", "managed"),
            (".config/demo/seed", "seed"),
        ],
    );
    fs::write(root.path().join("installed.db"), "already\n").unwrap();
    let bin = fake_commands(root.path());
    let output = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["select", "demo"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = fs::read_to_string(root.path().join("commands.log")).unwrap();
    assert!(log.contains("pacman <-Qq>"));
    assert!(log.contains("sudo <pacman> <-S> <--needed> <--> <already> <new-package>"));
    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(state.contains("new-package"), "{state}");
    assert!(state.contains("active_profile = \"demo\""));
}

#[test]
fn select_backs_up_and_overwrites_managed_and_first_seed() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/demo/managed"],
        &[
            (".config/demo/managed", "managed-new"),
            (".config/demo/seed", "seed-new"),
        ],
    );
    let bin = fake_commands(root.path());
    fs::create_dir_all(home.path().join(".config/demo")).unwrap();
    fs::write(home.path().join(".config/demo/managed"), "managed-old").unwrap();
    fs::write(home.path().join(".config/demo/seed"), "seed-old").unwrap();

    assert!(
        run(
            root.path(),
            home.path(),
            &profile_root,
            &bin,
            &["select", "demo"]
        )
        .status
        .success()
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/managed")).unwrap(),
        "managed-new"
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/seed")).unwrap(),
        "seed-new"
    );
    let backup_root = home.path().join(".local/state/catdot/backups");
    let backup = fs::read_dir(backup_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read_to_string(backup.join("home/.config/demo/managed")).unwrap(),
        "managed-old"
    );
    assert_eq!(
        fs::read_to_string(backup.join("home/.config/demo/seed")).unwrap(),
        "seed-old"
    );
    let state_root = home.path().join(".local/state/catdot");
    assert_eq!(
        fs::metadata(&state_root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(state_root.join("backups"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(state_root.join("state.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(state_root.join("lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn select_preflights_home_paths_before_installing_packages() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "demo",
        &["new-package"],
        &[".config/demo/config"],
        &[(".config/demo/config", "managed")],
    );
    symlink(outside.path(), home.path().join(".config")).unwrap();
    let bin = fake_commands(root.path());

    let output = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["select", "demo"],
    );
    assert!(!output.status.success());
    let log = fs::read_to_string(root.path().join("commands.log")).unwrap_or_default();
    assert!(!log.contains("sudo <pacman>"), "{log}");
    assert!(
        fs::read_to_string(root.path().join("installed.db"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn select_rejects_managed_paths_overlapping_the_actual_state_directory() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "demo",
        &["new-package"],
        &[".state"],
        &[(".state/profile-content", "managed")],
    );
    let bin = fake_commands(root.path());
    let state_home = home.path().join(".state");

    let output = run_with_state_home(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        Some(&state_home),
        &["select", "demo"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Catdot state"));
    let log = fs::read_to_string(root.path().join("commands.log")).unwrap_or_default();
    assert!(!log.contains("sudo <pacman>"), "{log}");
}

#[test]
fn show_and_list_use_the_accepted_retained_snapshot() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "demo",
        &["old-package"],
        &[".config/demo/managed"],
        &[(".config/demo/managed", "v1")],
    );
    let bin = fake_commands(root.path());
    assert!(
        run(
            root.path(),
            home.path(),
            &profile_root,
            &bin,
            &["select", "demo"]
        )
        .status
        .success()
    );

    rewrite_manifest(
        root.path(),
        "demo",
        "Demo v2 not accepted",
        &["new-package"],
        &[".config/demo/managed"],
    );

    for command in [vec!["show", "demo"], vec!["list"]] {
        let output = run(root.path(), home.path(), &profile_root, &bin, &command);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("demo (active)"), "{stdout}");
        assert!(!stdout.contains("update available"), "{stdout}");
        assert!(stdout.contains("packages: old-package"), "{stdout}");
        assert!(!stdout.contains("new-package"), "{stdout}");
        assert!(!stdout.contains("Demo v2 not accepted"), "{stdout}");
    }
}

#[test]
fn list_reports_invalid_profiles_without_hiding_valid_ones_or_failing() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "good", &[], &[], &[]);
    let bad = root.path().join("usr/share/catdot/profiles/bad");
    fs::create_dir_all(&bad).unwrap();
    fs::create_dir_all(root.path().join("usr/share/bad")).unwrap();
    fs::write(
        bad.join("profile.toml"),
        "schema = 99\nname = \"Bad\"\npackages = []\nmanage = []\n",
    )
    .unwrap();
    let bin = fake_commands(root.path());

    let output = run(root.path(), home.path(), &profile_root, &bin, &["list"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("good (available)"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid profile"));
}

#[test]
fn pacman_query_failure_stops_install_without_changing_state() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "demo", &["new-package"], &[], &[]);
    let bin = fake_commands(root.path());
    fs::write(root.path().join("query-fail"), "fail").unwrap();

    let output = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["select", "demo"],
    );
    assert!(!output.status.success());
    let log = fs::read_to_string(root.path().join("commands.log")).unwrap();
    assert!(log.contains("pacman <-Qq>"));
    assert!(!log.contains("sudo <pacman>"));
    assert!(!home.path().join(".local/state/catdot/state.toml").exists());
}

#[test]
fn update_preflights_home_paths_before_installing_new_packages() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "demo",
        &[],
        &[".config/demo/config"],
        &[(".config/demo/config", "v1")],
    );
    let bin = fake_commands(root.path());
    assert!(
        run(
            root.path(),
            home.path(),
            &profile_root,
            &bin,
            &["select", "demo"]
        )
        .status
        .success()
    );

    fs::remove_dir_all(home.path().join(".config")).unwrap();
    symlink(outside.path(), home.path().join(".config")).unwrap();
    rewrite_manifest(
        root.path(),
        "demo",
        "demo",
        &["new-package"],
        &[".config/demo/config"],
    );
    fs::write(root.path().join("commands.log"), "").unwrap();

    let output = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["update", "demo"],
    );
    assert!(!output.status.success());
    let log = fs::read_to_string(root.path().join("commands.log")).unwrap_or_default();
    assert!(!log.contains("sudo <pacman>"), "{log}");
    assert!(
        !fs::read_to_string(root.path().join("installed.db"))
            .unwrap()
            .lines()
            .any(|line| line == "new-package")
    );
}

#[test]
fn updating_an_inactive_profile_does_not_validate_or_modify_home_targets() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "alpha",
        &[],
        &[".config/alpha/config"],
        &[(".config/alpha/config", "alpha-v1")],
    );
    install_profile(root.path(), "beta", &[], &[], &[]);
    let bin = fake_commands(root.path());

    for command in [vec!["select", "alpha"], vec!["select", "beta"]] {
        let output = run(root.path(), home.path(), &profile_root, &bin, &command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fs::remove_dir_all(home.path().join(".config")).unwrap();
    symlink(outside.path(), home.path().join(".config")).unwrap();
    rewrite_manifest(
        root.path(),
        "alpha",
        "alpha-v2",
        &["new-package"],
        &[".config/alpha/config"],
    );
    fs::write(
        root.path().join("usr/share/alpha/.config/alpha/config"),
        "alpha-v2",
    )
    .unwrap();

    let output = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["update", "alpha"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(root.path().join("installed.db"))
            .unwrap()
            .lines()
            .any(|line| line == "new-package")
    );
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());

    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(state.contains("active_profile = \"beta\""), "{state}");
    assert!(state.contains("new-package"), "{state}");
    let cache = home
        .path()
        .join(".local/state/catdot/profiles/alpha/managed/.config/alpha/config");
    assert_eq!(fs::read_to_string(cache).unwrap(), "alpha-v2");
}

#[test]
fn pacman_query_failure_stops_prune_without_changing_state() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "desktop", &["desktop-pkg"], &[], &[]);
    install_profile(root.path(), "empty", &[], &[], &[]);
    let bin = fake_commands(root.path());
    for command in [
        vec!["select", "desktop"],
        vec!["select", "empty"],
        vec!["remove", "desktop"],
    ] {
        assert!(
            run(root.path(), home.path(), &profile_root, &bin, &command)
                .status
                .success()
        );
    }
    let state_path = home.path().join(".local/state/catdot/state.toml");
    let before = fs::read_to_string(&state_path).unwrap();
    fs::write(root.path().join("query-fail"), "fail").unwrap();

    let output = run(root.path(), home.path(), &profile_root, &bin, &["prune"]);
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(state_path).unwrap(), before);
    assert!(
        fs::read_to_string(root.path().join("installed.db"))
            .unwrap()
            .lines()
            .any(|line| line == "desktop-pkg")
    );
}

#[test]
fn prune_delegates_unreferenced_introduced_packages_to_pacman() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "desktop", &["desktop-pkg"], &[], &[]);
    install_profile(root.path(), "empty", &[], &[], &[]);
    let bin = fake_commands(root.path());

    assert!(
        run(
            root.path(),
            home.path(),
            &profile_root,
            &bin,
            &["select", "desktop"]
        )
        .status
        .success()
    );
    assert!(
        run(
            root.path(),
            home.path(),
            &profile_root,
            &bin,
            &["select", "empty"]
        )
        .status
        .success()
    );
    assert!(
        run(
            root.path(),
            home.path(),
            &profile_root,
            &bin,
            &["remove", "desktop"]
        )
        .status
        .success()
    );
    let prune = run(root.path(), home.path(), &profile_root, &bin, &["prune"]);
    assert!(
        prune.status.success(),
        "stdout={} stderr={} db={}",
        String::from_utf8_lossy(&prune.stdout),
        String::from_utf8_lossy(&prune.stderr),
        fs::read_to_string(root.path().join("installed.db")).unwrap()
    );

    let log = fs::read_to_string(root.path().join("commands.log")).unwrap();
    assert!(
        log.contains("sudo <pacman> <-Rns> <--> <desktop-pkg>"),
        "log={log} db={} prune={}",
        fs::read_to_string(root.path().join("installed.db")).unwrap(),
        String::from_utf8_lossy(&prune.stdout)
    );
    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(!state.contains("desktop-pkg"));
}

#[test]
fn removed_transaction_commands_are_not_exposed() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "demo", &[], &[], &[]);
    let bin = fake_commands(root.path());
    for command in ["reset", "recover", "doctor", "resolve", "apply"] {
        let output = run(root.path(), home.path(), &profile_root, &bin, &[command]);
        assert!(
            !output.status.success(),
            "removed command {command} succeeded"
        );
    }
}

#[test]
fn json_query_reports_active_retained_snapshot_and_available_profiles() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(
        root.path(),
        "demo",
        &["initial"],
        &[".config/demo/managed"],
        &[(".config/demo/managed", "v1")],
    );
    install_profile(root.path(), "extra", &["another"], &[], &[]);
    let bin = fake_commands(root.path());
    assert!(
        run(
            root.path(),
            home.path(),
            &profile_root,
            &bin,
            &["select", "demo"]
        )
        .status
        .success()
    );
    rewrite_manifest(
        root.path(),
        "demo",
        "New metadata not accepted",
        &["changed"],
        &[".config/demo/managed"],
    );

    let output = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["list", "--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["activeProfile"], "demo");
    assert_eq!(value["profiles"].as_array().unwrap().len(), 2);
    let selected = value["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "demo")
        .unwrap();
    assert_eq!(selected["status"], "active");
    assert_eq!(selected["packages"], serde_json::json!(["initial"]));
    assert_eq!(selected["name"], "demo");
    let available = value["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "extra")
        .unwrap();
    assert_eq!(available["status"], "available");

    let current = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["current", "--json"],
    );
    assert!(current.status.success());
    let value: serde_json::Value = serde_json::from_slice(&current.stdout).unwrap();
    assert_eq!(value["activeProfile"], "demo");
    assert_eq!(value["retainedProfiles"], serde_json::json!(["demo"]));

    let show = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["show", "demo", "--json"],
    );
    assert!(show.status.success());
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(value["id"], "demo");
    assert_eq!(value["name"], "demo");
    assert_eq!(value["status"], "active");
    assert_eq!(value["packages"], serde_json::json!(["initial"]));
}

#[test]
fn json_list_reports_invalid_profile_without_losing_valid_profiles() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile_root = install_profile(root.path(), "good", &[], &[], &[]);
    let bad = profile_root.join("bad");
    fs::create_dir_all(&bad).unwrap();
    fs::write(bad.join("profile.toml"), "schema = 99\nname = \"broken\"\n").unwrap();
    let bin = fake_commands(root.path());
    let output = run(
        root.path(),
        home.path(),
        &profile_root,
        &bin,
        &["list", "--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["profiles"][0]["id"], "good");
    assert_eq!(value["diagnostics"].as_array().unwrap().len(), 1);
}
