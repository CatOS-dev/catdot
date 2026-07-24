use std::{fs, process::Command};
use tempfile::tempdir;

fn write_profile(root: &std::path::Path, id: &str, component: &str, role: &str) {
    let profile = root.join(id);
    fs::create_dir_all(profile.join(component)).unwrap();
    fs::write(
        profile.join("profile.toml"),
        format!(
            "schema = 1\n[profile]\nid = \"{id}\"\nname = \"{id}\"\ndescription = \"test\"\n[defaults]\n{role} = \"{component}\"\n[components.{component}]\nrole = \"{role}\"\npath = \"{component}\"\n"
        ),
    )
    .unwrap();
}

#[test]
fn select_persists_profile_defaults_and_cross_profile_override() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    write_profile(root.path(), "one", "bar", "bar");
    write_profile(root.path(), "two", "other-bar", "bar");
    let binary = env!("CARGO_BIN_EXE_catdot");

    let status = Command::new(binary)
        .args(["select", "one"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .status()
        .unwrap();
    assert!(status.success());

    let status = Command::new(binary)
        .args(["select", "bar", "two/other-bar"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .status()
        .unwrap();
    assert!(status.success());

    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(state.contains("bar = \"two/other-bar\""));

    fs::remove_dir_all(root.path().join("two")).unwrap();
    let output = Command::new(binary)
        .arg("doctor")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).contains("error: broken: bar"));

    let output = Command::new(binary)
        .args(["disable", "bar"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(output.status.success());
    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(!state.contains("bar ="));
}

#[test]
fn adopt_touches_only_the_requested_role() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile = root.path().join("demo");
    fs::create_dir_all(profile.join("a")).unwrap();
    fs::create_dir_all(profile.join("b")).unwrap();
    fs::write(profile.join("a/config"), "a").unwrap();
    fs::write(profile.join("b/config"), "b").unwrap();
    fs::write(
        profile.join("profile.toml"),
        "schema = 1\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\n[defaults]\na = \"a\"\nb = \"b\"\n[components.a]\nrole = \"a\"\npath = \"a\"\n[[components.a.links]]\nsource = \"config\"\ntarget = \"{xdg_config_home}/a/config\"\n[components.b]\nrole = \"b\"\npath = \"b\"\n[[components.b.links]]\nsource = \"config\"\ntarget = \"{xdg_config_home}/b/config\"\n",
    )
    .unwrap();
    let state_dir = home.path().join(".local/state/catdot");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("state.toml"),
        "generation = 1\n[components]\na = \"demo/a\"\nb = \"demo/b\"\n",
    )
    .unwrap();
    let config = home.path().join(".config");
    fs::create_dir_all(config.join("a")).unwrap();
    fs::create_dir_all(config.join("b")).unwrap();
    fs::write(config.join("a/config"), "keep a").unwrap();
    fs::write(config.join("b/config"), "replace b").unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["adopt", "b"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(config.join("a/config")).unwrap(),
        "keep a"
    );
    assert!(
        fs::symlink_metadata(config.join("b/config"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn adopt_uses_the_active_component_when_a_new_provider_is_pending() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile = root.path().join("demo");
    fs::create_dir_all(profile.join("a")).unwrap();
    fs::create_dir_all(profile.join("b")).unwrap();
    fs::write(profile.join("a/config"), "a").unwrap();
    fs::write(profile.join("b/config"), "b").unwrap();
    fs::write(
        profile.join("profile.toml"),
        "schema = 1\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\n[defaults]\nrole = \"a\"\n[components.a]\nrole = \"role\"\npath = \"a\"\n[[components.a.links]]\nsource = \"config\"\ntarget = \"{xdg_config_home}/role/config\"\n[components.b]\nrole = \"role\"\npath = \"b\"\n[[components.b.links]]\nsource = \"config\"\ntarget = \"{xdg_config_home}/role/config\"\n",
    )
    .unwrap();
    let state_dir = home.path().join(".local/state/catdot");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("state.toml"),
        "schema = 1\ngeneration = 2\nactive_generation = 1\n[components]\nrole = \"demo/b\"\n[active_components]\nrole = \"demo/a\"\n",
    )
    .unwrap();
    let target = home.path().join(".config/role/config");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "unmanaged").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["adopt", "role"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(fs::read_link(&target).unwrap(), profile.join("a/config"));
    let state = fs::read_to_string(state_dir.join("state.toml")).unwrap();
    assert!(state.contains("role = \"demo/b\""));
    assert!(state.contains("role = \"demo/a\""));
}

#[test]
fn unresolved_component_cannot_be_executed() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile = root.path().join("demo");
    fs::create_dir_all(profile.join("terminal")).unwrap();
    fs::write(
        profile.join("profile.toml"),
        "schema = 1\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\n[defaults]\nterminal = \"terminal\"\n[components.terminal]\nrole = \"terminal\"\npath = \"terminal\"\npackages = [\"catdot-test-package-that-is-not-installed\"]\nexec = [\"false\"]\n",
    )
    .unwrap();
    let state_dir = home.path().join(".local/state/catdot");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("state.toml"),
        "generation = 1\n[components]\nterminal = \"demo/terminal\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["exec", "terminal"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is unresolved"));
}

#[test]
fn invalid_profile_does_not_block_list_doctor_or_an_unrelated_exec() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let valid = root.path().join("valid");
    let invalid = root.path().join("invalid");
    fs::create_dir_all(valid.join("terminal")).unwrap();
    fs::create_dir_all(&invalid).unwrap();
    fs::write(
        valid.join("profile.toml"),
        "schema = 1\n[profile]\nid = \"valid\"\nname = \"Valid\"\ndescription = \"test\"\n[defaults]\nterminal = \"terminal\"\n[components.terminal]\nrole = \"terminal\"\npath = \"terminal\"\nexec = [\"/usr/bin/true\"]\n",
    )
    .unwrap();
    fs::write(invalid.join("profile.toml"), "this = [bad").unwrap();
    let state_dir = home.path().join(".local/state/catdot");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("state.toml"),
        "generation = 1\n[components]\nterminal = \"valid/terminal\"\n",
    )
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_catdot");

    let list = Command::new(binary)
        .arg("list")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    let list = String::from_utf8(list.stdout).unwrap();
    assert!(list.contains("valid") && list.contains("Invalid profiles: 1"));

    let doctor = Command::new(binary)
        .arg("doctor")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(doctor.status.code(), Some(1));
    let doctor = String::from_utf8(doctor.stdout).unwrap();
    assert!(doctor.contains("invalid/profile.toml") && doctor.contains("Toml"));

    let exec = Command::new(binary)
        .args(["exec", "terminal"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .status()
        .unwrap();
    assert!(exec.success());
}

#[test]
fn select_reports_missing_packages_and_the_resolve_command() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile = root.path().join("demo");
    fs::create_dir_all(profile.join("launcher")).unwrap();
    fs::write(
        profile.join("profile.toml"),
        "schema = 1\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\n[defaults]\nlauncher = \"launcher\"\n[components.launcher]\nrole = \"launcher\"\npath = \"launcher\"\npackages = [\"catdot-test-package-that-is-not-installed\"]\nexec = [\"false\"]\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["select", "launcher", "demo/launcher"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Selected desired launcher: demo/launcher"));
    assert!(stdout.contains("Active launcher remains: none"));
    assert!(stdout.contains("Missing packages:\n  catdot-test-package-that-is-not-installed"));
    assert_eq!(stdout.matches("catdot resolve").count(), 1);
}

#[test]
fn select_keeps_a_linked_component_inactive_until_resolve() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile = root.path().join("demo");
    fs::create_dir_all(profile.join("terminal")).unwrap();
    fs::write(profile.join("terminal/config"), "config").unwrap();
    fs::write(
        profile.join("profile.toml"),
        "schema = 1\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\n[defaults]\n[components.terminal]\nrole = \"terminal\"\npath = \"terminal\"\npackages = []\nexec = [\"true\"]\n[[components.terminal.links]]\nsource = \"config\"\ntarget = \"{xdg_config_home}/demo/config\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["select", "terminal", "demo/terminal"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.path().join(".config/demo/config").exists());
    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(state.contains("terminal = \"demo/terminal\""));
    assert!(!state.contains("[active_components]\nterminal"));
}

#[test]
fn exec_uses_the_old_active_component_while_a_new_choice_is_pending() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let profile = root.path().join("demo");
    fs::create_dir_all(profile.join("old")).unwrap();
    fs::create_dir_all(profile.join("new")).unwrap();
    fs::write(
        profile.join("profile.toml"),
        "schema = 1\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\n[defaults]\n[components.old]\nrole = \"terminal\"\npath = \"old\"\nexec = [\"true\"]\n[components.new]\nrole = \"terminal\"\npath = \"new\"\nexec = [\"false\"]\n",
    )
    .unwrap();
    let state_dir = home.path().join(".local/state/catdot");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("state.toml"),
        "schema = 1\ngeneration = 2\nactive_generation = 1\n[components]\nterminal = \"demo/new\"\n[active_components]\nterminal = \"demo/old\"\n",
    )
    .unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["exec", "terminal"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .status()
        .unwrap();

    assert!(status.success());
}

#[test]
fn doctor_uses_zero_for_healthy_state_and_two_for_broken_state() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    write_profile(root.path(), "demo", "terminal", "terminal");
    let binary = env!("CARGO_BIN_EXE_catdot");
    let healthy = Command::new(binary)
        .arg("doctor")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(healthy.status.code(), Some(0));

    let state_dir = home.path().join(".local/state/catdot");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("state.toml"),
        "schema = 1\ngeneration = 1\nactive_generation = 1\n[components]\nterminal = \"missing/terminal\"\n[active_components]\nterminal = \"missing/terminal\"\n",
    )
    .unwrap();
    let broken = Command::new(binary)
        .arg("doctor")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(broken.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&broken.stdout).contains("error: broken state"));
}

#[test]
fn help_explains_commands_and_both_selection_forms() {
    let binary = env!("CARGO_BIN_EXE_catdot");
    let help = Command::new(binary).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("List installed profiles"));
    assert!(help.contains("Show selected and active components"));
    assert!(help.contains("Diagnose user and system state"));

    let select = Command::new(binary)
        .args(["select", "--help"])
        .output()
        .unwrap();
    assert!(select.status.success());
    let select = String::from_utf8(select.stdout).unwrap();
    assert!(select.contains("catdot select <PROFILE>"));
    assert!(select.contains("catdot select <ROLE> <PROFILE/COMPONENT>"));
    assert!(!select.contains("<FIRST>"));
}

#[test]
fn empty_state_outputs_are_actionable() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    write_profile(root.path(), "demo", "terminal", "terminal");
    let binary = env!("CARGO_BIN_EXE_catdot");

    let current = Command::new(binary)
        .arg("current")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(current.status.success());
    let current = String::from_utf8(current.stdout).unwrap();
    assert!(current.contains("No profile components are selected."));
    assert!(current.contains("catdot list"));

    let doctor = Command::new(binary)
        .arg("doctor")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert_eq!(doctor.status.code(), Some(0));
    assert!(
        String::from_utf8(doctor.stdout)
            .unwrap()
            .contains("ok: Catdot is healthy; no profile components are selected")
    );
}
