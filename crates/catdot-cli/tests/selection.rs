use std::{
    fs,
    os::unix::fs::PermissionsExt,
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
    grep -Fxq "$3" "$CATDOT_INSTALLED_DB"
    exit $?
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

fn run(
    root: &Path,
    home: &Path,
    profile_root: &Path,
    bin: &Path,
    args: &[&str],
) -> std::process::Output {
    let log = root.join("commands.log");
    let installed = root.join("installed.db");
    if !installed.exists() {
        fs::write(&installed, "").unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(args)
        .env("CATDOT_PROFILE_ROOT", profile_root)
        .env("HOME", home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("CATDOT_COMMAND_LOG", log)
        .env("CATDOT_INSTALLED_DB", installed)
        .output()
        .unwrap()
}

// Protects direct package delegation and introduced-package tracking: Catdot
// queries installed direct packages, then invokes sudo pacman -S --needed.
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
    assert!(log.contains("pacman <-Qq> <--> <already>"));
    assert!(log.contains("pacman <-Qq> <--> <new-package>"));
    assert!(log.contains("sudo <pacman> <-S> <--needed> <--> <already> <new-package>"));
    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(state.contains("new-package"), "{state}");
    assert!(state.contains("active_profile = \"demo\""));
}

// Protects overwrite behavior and backup retention through the public CLI.
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
}

// Protects functional prune: removing a retained Profile releases its direct
// package references, and prune delegates eligible introduced packages to
// sudo pacman -Rns before deleting them from state.
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

// Protects the reduced command surface after deleting helper recovery and
// diagnostic state machines.
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
