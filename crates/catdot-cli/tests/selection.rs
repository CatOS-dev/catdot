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

// Protects desktop binds and autostart commands: `catdot exec` must read only
// active state, invoke the provider without a shell, and preserve extra argv.
// The old runtime had no active-role boundary and could not diagnose a pending
// replacement separately from an executable that is missing.
#[test]
fn exec_uses_active_provider_and_passes_extra_argv_without_a_shell() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    let bin = home.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, output) in [("active", "active"), ("desired", "desired")] {
        let script = bin.join(name);
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '{output}:%s\\n' \"$1\"\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();
        }
    }
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"active\"\nrole = \"terminal\"\n[components.exec]\nargv = [\"{}\"]\n[[components]]\nid = \"desired\"\nrole = \"terminal\"\n[components.exec]\nargv = [\"{}\"]\n",
            root.path().join("share").display(), bin.join("active").display(), bin.join("desired").display()
        ),
    ).unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    let niri = home.path().join(".config/niri/config.kdl");
    fs::create_dir_all(niri.parent().unwrap()).unwrap();
    fs::write(
        &niri,
        "spawn \"catdot\" \"exec\" \"terminal\"\nspawn-at-startup \"catdot\" \"exec\" \"bar\"\n",
    )
    .unwrap();
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, "schema = 1\ngeneration = 2\nactive_generation = 1\n[components]\nterminal = \"demo/desired\"\n[active_components]\nterminal = \"demo/active\"\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["exec", "terminal", "literal;not-a-shell-command"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "active:literal;not-a-shell-command\n"
    );
    assert_eq!(
        fs::read_to_string(niri).unwrap(),
        "spawn \"catdot\" \"exec\" \"terminal\"\nspawn-at-startup \"catdot\" \"exec\" \"bar\"\n"
    );
}

// Protects bind/autostart diagnostics: roles must fail before any implicit
// resolve, with the failure describing the state the desktop is actually in.
#[test]
fn exec_reports_role_state_and_provider_failures() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join("profile.toml"), format!("schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"empty\"\nrole = \"launcher\"\n[[components]]\nid = \"missing\"\nrole = \"terminal\"\n[components.exec]\nargv = [\"catdot-no-such-binary\"]\n", root.path().join("share").display())).unwrap();
    let run = |role: &str| {
        Command::new(env!("CARGO_BIN_EXE_catdot"))
            .args(["exec", role])
            .env("CATDOT_PROFILE_ROOT", root.path())
            .env("HOME", home.path())
            .output()
            .unwrap()
    };
    assert!(String::from_utf8_lossy(&run("unknown").stderr).contains("unknown role unknown"));
    assert!(
        String::from_utf8_lossy(&run("terminal").stderr).contains("role terminal is not selected")
    );
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(
        &state,
        "schema = 1\ngeneration = 1\n[components]\nterminal = \"demo/missing\"\n",
    )
    .unwrap();
    assert!(
        String::from_utf8_lossy(&run("terminal").stderr).contains("selected but not activated")
    );
    fs::write(
        &state,
        "schema = 1\n[active_components]\nlauncher = \"demo/empty\"\n",
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&run("launcher").stderr).contains("has no exec command"));
    fs::write(
        &state,
        "schema = 1\n[active_components]\nterminal = \"demo/missing\"\n",
    )
    .unwrap();
    assert!(
        String::from_utf8_lossy(&run("terminal").stderr)
            .contains("binary catdot-no-such-binary is missing")
    );
}

// Protects the error boundary used by desktop binds: an active reference that
// no longer exists must identify the missing component rather than silently
// falling back to desired state or another provider.
#[test]
fn exec_reports_a_missing_active_component() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"foot\"\nrole = \"terminal\"\n[components.exec]\nargv = [\"foot\"]\n",
            root.path().join("share").display()
        ),
    )
    .unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(
        state,
        "schema = 1\n[active_components]\nterminal = \"demo/removed\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["exec", "terminal"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("component demo/removed is no longer installed")
    );
}

// Protects the no-shell exec contract: unsafe argv is rejected while reading
// the profile, so a desktop cannot activate a shell provider by mistake.
#[test]
fn exec_reports_an_invalid_provider_argv() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"unsafe\"\nrole = \"terminal\"\n[components.exec]\nargv = [\"sh\"]\n",
            root.path().join("share").display()
        ),
    )
    .unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(
        state,
        "schema = 1\n[active_components]\nterminal = \"demo/unsafe\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["exec", "terminal"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid exec argv for component unsafe")
    );
}

// Protects actual autostart consumption: a compositor's unchanged
// `spawn-at-startup \"catdot\" \"exec\" \"bar\"` command resolves the active
// bar provider directly, without generating a role-specific config fragment.
#[test]
fn exec_resolves_an_active_bar_autostart_role() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    let bin = home.path().join("bin");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let bar = bin.join("bar");
    fs::write(&bar, "#!/bin/sh\nprintf 'bar started\\n'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&bar).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bar, permissions).unwrap();
    }
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"panel\"\nrole = \"bar\"\n[components.exec]\nargv = [\"{}\"]\n",
            root.path().join("share").display(),
            bar.display()
        ),
    )
    .unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(
        state,
        "schema = 1\n[active_components]\nbar = \"demo/panel\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["exec", "bar"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "bar started\n");
    assert!(!home.path().join(".config/niri/bar.kdl").exists());
}

// Protects dry-run's read-only guarantee. The package helper is deliberately
// a PATH-local fake so this validates the real CLI planning path without a
// privileged transaction; old behavior omitted XDG changes from that plan.
#[test]
fn resolve_dry_run_prints_xdg_defaults_and_preserves_home_and_state() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    let bin = root.path().join("bin");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"browser\"\nrole = \"browser\"\n[components.xdg]\ndesktop_entry = \"browser.desktop\"\nmime_types = [\"text/html\"]\n",
            root.path().join("share").display()
        ),
    )
    .unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(
        &state,
        "schema = 1\n[components]\nbrowser = \"demo/browser\"\n",
    )
    .unwrap();
    let xdg_registry = home.path().join(".config/catdot/xdg.toml");
    fs::create_dir_all(xdg_registry.parent().unwrap()).unwrap();
    fs::write(
        &xdg_registry,
        "[previous]\n\"text/plain\" = \"editor.desktop\"\n",
    )
    .unwrap();
    let fake_pkexec = bin.join("pkexec");
    fs::write(
        &fake_pkexec,
        "#!/bin/sh\nprintf '%s\\n' 'system_update_required = false' '[plan]' 'install = []' 'remove = []' 'replacements = []' 'satisfied = []' '[requirements]'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&fake_pkexec).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_pkexec, permissions).unwrap();
    }
    let state_before = fs::read(&state).unwrap();
    let registry_before = fs::read(&xdg_registry).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["resolve", "--dry-run"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("PATH", &bin)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("xdg default: text/html -> browser.desktop"));
    assert!(stdout.contains("xdg restore: text/plain"));
    assert_eq!(fs::read(&state).unwrap(), state_before);
    assert_eq!(fs::read(&xdg_registry).unwrap(), registry_before);
    assert!(!home.path().join(".config/mimeapps.list").exists());
}
