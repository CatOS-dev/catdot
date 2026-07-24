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
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("broken: bar"));

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
    assert!(stdout.contains("Selected launcher: demo/launcher"));
    assert!(stdout.contains("Missing packages:\n  catdot-test-package-that-is-not-installed"));
    assert!(stdout.contains("Run:\n  catdot resolve"));
}
