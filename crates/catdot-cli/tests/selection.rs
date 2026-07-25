use std::{fs, process::Command};
use tempfile::tempdir;

// Protects the CLI promise that selection changes only desired state. The old
// schema coupled selection to a manifest-local content path.
#[test]
fn select_uses_profile_defaults_without_materializing_the_source_tree() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    let missing_source = root.path().join("not-installed-component-content");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\nterminal = \"foot\"\n[[components]]\nid = \"foot\"\nrole = \"terminal\"\npackages = [\"catdot-test-package-that-is-not-installed\"]\n[components.exec]\nargv = [\"foot\"]\n",
            missing_source.display()
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["select", "demo"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env_remove("CATDOT_DEFAULT_DECLARATION")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state = fs::read_to_string(home.path().join(".local/state/catdot/state.toml")).unwrap();
    assert!(state.contains("terminal = \"demo/foot\""));
    assert!(state.contains("active_generation = 0"));
    assert!(!missing_source.exists());
    assert!(
        !home.path().join(".config").exists(),
        "select must not write HOME configuration"
    );
}

// Protects the lifecycle boundary that replaces the old link-manager product:
// unmanaged targets are backed up transactionally by resolve, never adopted by
// a separate command. The previous CLI still exposed `adopt`.
#[test]
fn adopt_is_not_a_catdot_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["adopt", "terminal"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand 'adopt'"));
}
