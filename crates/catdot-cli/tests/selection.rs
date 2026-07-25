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

// Protects the upgrade boundary: a root-owned generation marker can cause a
// user-owned reapply, but it must preserve an existing custom area and never
// need a package transaction for an already-active component.
#[test]
fn update_reapplies_active_profile_without_touching_custom() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    let source = root.path().join("share");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(source.join(".config/demo/custom")).unwrap();
    fs::write(source.join(".config/demo/custom/seed"), "seed").unwrap();
    fs::write(metadata.join("profile.toml"), format!(
        "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\n[[components.configuration]]\ntarget = \".config/demo/config\"\nlifecycle = \"generate\"\ntemplate = \"managed\"\n[[components.configuration]]\ntarget = \".config/demo/custom\"\nlifecycle = \"user\"\nseed = \".config/demo/custom\"\n", source.display()
    )).unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, "schema = 1\ngeneration = 1\nactive_generation = 1\n[components]\ndesktop = \"demo/desktop\"\n[active_components]\ndesktop = \"demo/desktop\"\n").unwrap();
    let custom = home.path().join(".config/demo/custom");
    fs::create_dir_all(&custom).unwrap();
    fs::write(custom.join("mine"), "do not replace").unwrap();
    let marker = root.path().join("generation");
    fs::write(&marker, "7\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .arg("update")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("CATDOT_SYSTEM_GENERATION", &marker)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(custom.join("mine")).unwrap(),
        "do not replace"
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/config")).unwrap(),
        "managed"
    );
    assert!(
        fs::read_to_string(&state)
            .unwrap()
            .contains("active_system_generation = 7")
    );
    let manifest = metadata.join("profile.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("managed", "changed"),
    )
    .unwrap();
    let second = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .arg("update")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("CATDOT_SYSTEM_GENERATION", &marker)
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/config")).unwrap(),
        "changed"
    );
}

// Protects the reset exception to the normal user boundary: only an explicit
// reset may remove custom data, and it must then reseed custom and repair the
// managed entry in the same activation transaction.
#[test]
fn reset_reseeds_custom_and_repairs_managed_files() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    let source = root.path().join("share");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(source.join(".config/demo/custom")).unwrap();
    fs::write(source.join(".config/demo/custom/seed"), "seed").unwrap();
    fs::write(metadata.join("profile.toml"), format!(
        "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\n[[components.configuration]]\ntarget = \".config/demo/config\"\nlifecycle = \"generate\"\ntemplate = \"managed\"\n[[components.configuration]]\ntarget = \".config/demo/custom\"\nlifecycle = \"user\"\nseed = \".config/demo/custom\"\n", source.display()
    )).unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, "schema = 1\ngeneration = 1\nactive_generation = 1\n[components]\ndesktop = \"demo/desktop\"\n[active_components]\ndesktop = \"demo/desktop\"\n").unwrap();
    let custom = home.path().join(".config/demo/custom");
    fs::create_dir_all(&custom).unwrap();
    fs::write(custom.join("mine"), "remove me").unwrap();
    fs::write(home.path().join(".config/demo/config"), "damaged").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["reset", "demo"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!custom.join("mine").exists());
    assert_eq!(fs::read_to_string(custom.join("seed")).unwrap(), "seed");
    assert_eq!(
        fs::read_to_string(home.path().join(".config/demo/config")).unwrap(),
        "managed"
    );
    assert!(home.path().join(".local/state/catdot/backups").exists());
    let wrong_profile = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["reset", "other"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(!wrong_profile.status.success());
    assert!(String::from_utf8_lossy(&wrong_profile.stderr).contains("not the current active"));
}

// A profile package update may add an unavailable component dependency.  The
// login service must record that explicit resolve is needed; it must not turn
// a background generation check into a package installation transaction.
#[test]
fn update_marks_needs_resolve_without_installing_packages() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join("profile.toml"), format!(
        "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\npackages = [\"catdot-update-test-missing\"]\n", root.path().join("share").display()
    )).unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, "schema = 1\ngeneration = 1\nactive_generation = 1\n[components]\ndesktop = \"demo/desktop\"\n[active_components]\ndesktop = \"demo/desktop\"\n").unwrap();
    let marker = root.path().join("generation");
    fs::write(&marker, "1\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .arg("update")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("CATDOT_SYSTEM_GENERATION", &marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dependency declarations changed")
            || String::from_utf8_lossy(&output.stderr).contains("needs resolve")
    );
    assert!(
        fs::read_to_string(state)
            .unwrap()
            .contains("needs_resolve = true")
    );
}

// Even when a newly declared package already exists, changing the dependency
// declaration needs explicit `resolve`; an update service may never silently
// accept that product change or invoke a package transaction.
#[test]
fn update_requires_resolve_when_an_installed_dependency_declaration_changes() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join("profile.toml"), format!(
        "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\npackages = [\"already-installed\"]\n", root.path().join("share").display()
    )).unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, "schema = 1\ngeneration = 1\nactive_generation = 1\n[components]\ndesktop = \"demo/desktop\"\n[active_components]\ndesktop = \"demo/desktop\"\n[active_package_digests]\ndesktop = \"old-declaration\"\n").unwrap();
    let marker = root.path().join("generation");
    fs::write(&marker, "2\n").unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = root.path().join("pacman.calls");
    let pacman = bin.join("pacman");
    fs::write(
        &pacman,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            calls.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&pacman).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&pacman, permissions).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .arg("update")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("CATDOT_SYSTEM_GENERATION", &marker)
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("dependency declarations changed"));
    assert!(
        !calls.exists(),
        "update must not query or transact packages before resolve"
    );
    assert!(
        fs::read_to_string(state)
            .unwrap()
            .contains("needs_resolve = true")
    );
}

// Roles may deliberately come from more than one profile.  Reset must prove
// ownership per active component instead of refusing the normal cross-profile
// selection model or deleting another profile's custom data.
#[test]
fn reset_only_clears_the_named_active_profiles_custom_area() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    for (profile, role) in [("desktop", "desktop"), ("common", "terminal")] {
        let metadata = root.path().join(profile);
        let source = root.path().join(format!("{profile}-share"));
        fs::create_dir_all(source.join(format!(".config/{profile}/custom"))).unwrap();
        fs::write(
            source.join(format!(".config/{profile}/custom/seed")),
            profile,
        )
        .unwrap();
        fs::create_dir_all(&metadata).unwrap();
        fs::write(metadata.join("profile.toml"), format!(
            "schema = 2\n[profile]\nid = \"{profile}\"\nname = \"{profile}\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"main\"\nrole = \"{role}\"\n[[components.configuration]]\ntarget = \".config/{profile}/custom\"\nlifecycle = \"user\"\nseed = \".config/{profile}/custom\"\n", source.display()
        )).unwrap();
    }
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, "schema = 1\ngeneration = 1\nactive_generation = 1\n[components]\ndesktop = \"desktop/main\"\nterminal = \"common/main\"\n[active_components]\ndesktop = \"desktop/main\"\nterminal = \"common/main\"\n").unwrap();
    for profile in ["desktop", "common"] {
        let custom = home.path().join(format!(".config/{profile}/custom"));
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("mine"), profile).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["reset", "desktop"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.path().join(".config/desktop/custom/mine").exists());
    assert!(home.path().join(".config/desktop/custom/seed").is_file());
    assert_eq!(
        fs::read_to_string(home.path().join(".config/common/custom/mine")).unwrap(),
        "common"
    );
}

// Doctor must make a stopped installed user watcher visible, otherwise users
// cannot tell why a root hook generation is waiting for their next login.
#[test]
fn doctor_reports_an_inactive_installed_user_update_service() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let unit = root.path().join("catdot-update.path");
    fs::write(&unit, "[Path]\n").unwrap();
    let marker = root.path().join("generation");
    fs::write(&marker, "3\n").unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let systemctl = bin.join("systemctl");
    fs::write(&systemctl, "#!/bin/sh\nexit 3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&systemctl).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&systemctl, permissions).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .arg("doctor")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("CATDOT_USER_UPDATE_UNIT", &unit)
        .env("CATDOT_SYSTEM_GENERATION", &marker)
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("user update service is inactive"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("system profile generation changed"));
}

// The pacman hook is root-side bookkeeping only.  It may advance the system
// marker, but must never traverse or write a user's HOME.
#[test]
fn generation_marker_hook_only_writes_the_redirected_system_marker() {
    let root = tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let sentinel = home.join("untouched");
    fs::write(&sentinel, "user data").unwrap();
    let marker = root.path().join("system/generation");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("packaging/mark-generation");
    for expected in ["1\n", "2\n"] {
        let status = Command::new("sh")
            .arg(&script)
            .env("CATDOT_SYSTEM_GENERATION", &marker)
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read_to_string(&marker).unwrap(), expected);
    }
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "user data");
}

// Read-only inspection must not turn the built-in first-run declaration into
// persistent user state. A user should be able to inspect Catdot before making
// any selection or activation decision.
#[test]
fn read_only_commands_do_not_initialize_user_state() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("default");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"default\"\nname = \"Default\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\ntool = \"main\"\n[[components]]\nid = \"main\"\nrole = \"tool\"\n",
            root.path().join("share").display()
        ),
    )
    .unwrap();
    let declaration = root.path().join("default.toml");
    fs::write(&declaration, "schema = 1\nprofile = \"default\"\n").unwrap();
    for arguments in [vec!["list"], vec!["current"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
            .args(arguments)
            .env("CATDOT_PROFILE_ROOT", root.path())
            .env("CATDOT_DEFAULT_DECLARATION", &declaration)
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !home.path().join(".local/state/catdot/state.toml").exists(),
            "read-only command created persistent state"
        );
    }
}

// Desired-state generations identify real changes. Repeating the same profile
// selection or disabling an absent role must not create fake pending work.
#[test]
fn repeated_selection_and_absent_disable_are_noops() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\ntool = \"main\"\n[[components]]\nid = \"main\"\nrole = \"tool\"\n",
            root.path().join("share").display()
        ),
    )
    .unwrap();
    let run = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_catdot"))
            .args(arguments)
            .env("CATDOT_PROFILE_ROOT", root.path())
            .env("HOME", home.path())
            .env_remove("CATDOT_DEFAULT_DECLARATION")
            .output()
            .unwrap()
    };
    assert!(run(&["select", "demo"]).status.success());
    let state_path = home.path().join(".local/state/catdot/state.toml");
    let first = fs::read_to_string(&state_path).unwrap();
    assert!(first.contains("generation = 1"));
    assert!(run(&["select", "demo"]).status.success());
    assert!(run(&["disable", "missing"]).status.success());
    let final_state = fs::read_to_string(state_path).unwrap();
    assert!(final_state.contains("generation = 1"));
}

// Removing a profile package must not trap the user in references that can no
// longer be validated. A complete selection replaces broken desired state and
// drops unavailable active providers so the next resolve can recover normally.
#[test]
fn complete_selection_recovers_from_uninstalled_profiles() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("good");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"good\"\nname = \"Good\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\ntool = \"main\"\n[[components]]\nid = \"main\"\nrole = \"tool\"\n",
            root.path().join("share").display()
        ),
    )
    .unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(
        &state,
        "schema = 1\ngeneration = 4\nactive_generation = 3\n[components]\ndesktop = \"gone-a/main\"\nterminal = \"gone-b/main\"\n[active_components]\ndesktop = \"gone-a/main\"\nterminal = \"gone-b/main\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .args(["select", "good"])
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recovered = fs::read_to_string(state).unwrap();
    assert!(recovered.contains("tool = \"good/main\""));
    assert!(!recovered.contains("gone-a"));
    assert!(!recovered.contains("gone-b"));
}

// A package transaction that does not change any active Profile input should
// only acknowledge the new system generation; it must not rewrite HOME.
#[test]
fn generation_only_update_does_not_rewrite_managed_configuration() {
    use std::os::unix::fs::MetadataExt;
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let metadata = root.path().join("demo");
    let source = root.path().join("share");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("config"), "managed").unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 2\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"main\"\nrole = \"tool\"\n[[components.configuration]]\ntarget = \".config/demo/config\"\nlifecycle = \"overwrite\"\nmode = \"file\"\nsource = \"config\"\n",
            source.display()
        ),
    )
    .unwrap();
    let state = home.path().join(".local/state/catdot/state.toml");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(
        &state,
        "schema = 1\ngeneration = 1\nactive_generation = 1\n[components]\ntool = \"demo/main\"\n[active_components]\ntool = \"demo/main\"\n",
    )
    .unwrap();
    let marker = root.path().join("generation");
    fs::write(&marker, "1\n").unwrap();
    let first = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .arg("update")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("CATDOT_SYSTEM_GENERATION", &marker)
        .output()
        .unwrap();
    assert!(first.status.success());
    let target = home.path().join(".config/demo/config");
    let inode = fs::metadata(&target).unwrap().ino();
    fs::write(&marker, "2\n").unwrap();
    let second = Command::new(env!("CARGO_BIN_EXE_catdot"))
        .arg("update")
        .env("CATDOT_PROFILE_ROOT", root.path())
        .env("HOME", home.path())
        .env("CATDOT_SYSTEM_GENERATION", &marker)
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(fs::metadata(target).unwrap().ino(), inode);
    assert!(String::from_utf8_lossy(&second.stdout).contains("Active profile is current"));
}
