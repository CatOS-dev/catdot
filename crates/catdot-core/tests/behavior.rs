use catdot_core::{
    ActivationJournal, UserState, activate_configuration, build_activation_plan, discover_profiles,
    initialize_state_from_default, read_state, recover_activation_journals, select_profile,
    write_state,
};
use std::fs;
use tempfile::tempdir;

fn apply_configuration(
    plan: &catdot_core::ActivationPlan,
    registry: &std::path::Path,
    home: &std::path::Path,
) -> catdot_core::Result<()> {
    let state = home.join(".local/state/catdot/state.toml");
    let mut journal = ActivationJournal::begin(&state, Default::default(), Default::default())?;
    journal.mark_applying()?;
    let result = activate_configuration(plan, registry, &mut journal);
    if result.is_err() {
        recover_activation_journals(&state)?;
    } else {
        journal.complete()?;
    }
    result
}

fn profile(source_root: &str, inline: &str, files: &str, defaults: &str) -> String {
    format!(
        "schema = 2\n{files}[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{source_root}\"\n[defaults]\n{defaults}\n{inline}"
    )
}

fn component(id: &str, role: &str) -> String {
    format!(
        "[[components]]\nid = \"{id}\"\nrole = \"{role}\"\npackages = [\"{id}\"]\n[components.exec]\nargv = [\"{id}\"]\n[[components.configuration]]\ntarget = \".config/{id}/config\"\nlifecycle = \"overwrite\"\nmode = \"symlink\"\nsource = \".config/{id}/config\"\n"
    )
}

fn external_component(id: &str, role: &str) -> String {
    format!(
        "[component]\nid = \"{id}\"\nrole = \"{role}\"\npackages = [\"{id}\"]\n[component.exec]\nargv = [\"{id}\"]\n"
    )
}

// Protects installation of a profile descriptor before optional component
// packages: discovery must read only metadata and never require source files.
#[test]
fn discovery_accepts_separate_missing_source_tree() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("not-installed-yet");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            &component("tool", "terminal"),
            "",
            "terminal = \"tool\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    assert_eq!(profiles["demo"].source_root, source);
    assert_eq!(profiles["demo"].components["tool"].packages, ["tool"]);
}

// Protects explicit, reviewable component composition. The old implementation
// had no external declarations and treated its metadata directory as content.
#[test]
fn discovery_combines_explicit_external_and_inline_components_only() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("terminal.toml"),
        external_component("foot", "terminal"),
    )
    .unwrap();
    fs::write(
        metadata.join("unlisted.toml"),
        external_component("ignored", "launcher"),
    )
    .unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            &component("bar", "bar"),
            "component_files = [\"terminal.toml\"]\n",
            "terminal = \"foot\"\nbar = \"bar\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    assert!(profiles["demo"].components.contains_key("foot"));
    assert!(profiles["demo"].components.contains_key("bar"));
    assert!(!profiles["demo"].components.contains_key("ignored"));
}

// Protects against metadata traversal selecting arbitrary declarations.
#[test]
fn discovery_rejects_missing_or_escaping_component_files() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            "",
            "component_files = [\"../outside.toml\"]\n",
            "",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            "",
            "component_files = [\"missing.toml\"]\n",
            "",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
}

// Metadata is never a deployable source tree, even when both paths are valid.
#[test]
fn discovery_rejects_source_root_inside_metadata_tree() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &metadata.display().to_string(),
            &component("one", "terminal"),
            "",
            "terminal = \"one\"\n",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
}

// A source tree may not contain metadata either, and a symlink must not hide
// the same overlap after the source package has been installed.
#[test]
fn discovery_rejects_metadata_ancestor_and_source_symlink_aliases() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &temp.path().join("profiles").display().to_string(),
            &component("one", "terminal"),
            "",
            "terminal = \"one\"\n",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
    let source_alias = temp.path().join("source-alias");
    std::os::unix::fs::symlink(&metadata, &source_alias).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source_alias.display().to_string(),
            &component("one", "terminal"),
            "",
            "terminal = \"one\"\n",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
}

// A user-owned custom area can be seeded once, but cannot carry generator or
// overwrite-only fields that would make its ownership ambiguous.
#[test]
fn discovery_accepts_user_seed_and_rejects_cross_lifecycle_fields() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    let user = "[[components]]\nid = \"one\"\nrole = \"terminal\"\n[[components.configuration]]\ntarget = \".config/app/custom\"\nlifecycle = \"user\"\nseed = \".config/app/custom\"\n";
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            user,
            "",
            "terminal = \"one\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    assert_eq!(
        profiles["demo"].components["one"].configuration[0]
            .seed
            .as_deref(),
        Some(std::path::Path::new(".config/app/custom"))
    );
    let invalid = user.replace(
        "seed = \".config/app/custom\"",
        "seed = \".config/app/custom\"\ntemplate = \"wrong\"",
    );
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            &invalid,
            "",
            "terminal = \"one\"\n",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
}

// Protects unambiguous component ownership and desired defaults.
#[test]
fn discovery_rejects_duplicate_component_and_default_role() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            &(component("one", "terminal") + &component("one", "terminal")),
            "",
            "terminal = \"one\"\n",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            &component("one", "terminal"),
            "",
            "terminal = \"one\"\nterminal = \"one\"\n",
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
}

// Protects new users getting a desired selection without implicit package work
// or an active selection. Old state initialization had no skel declaration.
#[test]
fn skel_default_initializes_desired_state_without_active_state() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            &component("foot", "terminal"),
            "",
            "terminal = \"foot\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let declaration = temp.path().join("etc/skel/.config/catdot/default.toml");
    fs::create_dir_all(declaration.parent().unwrap()).unwrap();
    fs::write(&declaration, "schema = 1\nprofile = \"demo\"\n").unwrap();
    let state_file = temp.path().join("home/.local/state/catdot/state.toml");
    let state = initialize_state_from_default(&state_file, &declaration, &profiles).unwrap();
    assert_eq!(state.components["terminal"], "demo/foot");
    assert!(state.active_components.is_empty());
    assert_eq!(read_state(&state_file).unwrap(), state);
    assert!(
        !source.exists(),
        "initialization must not materialize or install"
    );
}

// Protects a real application entry point: templates and adapters must create
// regular HOME files, while static profile fragments remain top-level links.
// The old link-only activation path could not generate either entry point.
#[test]
fn materialization_generates_entries_and_links_static_fragments() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/niri")).unwrap();
    fs::write(source.join(".config/niri/default.kdl"), "layout {}").unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "niri"
role = "desktop"
[[components.configuration]]
target = ".config/niri/config.kdl"
lifecycle = "generate"
template = "include \"default.kdl\"\ninclude \"custom/config.kdl\"\n"
[[components.configuration]]
target = ".config/niri/default.kdl"
lifecycle = "overwrite"
mode = "symlink"
source = ".config/niri/default.kdl"
"#,
            "",
            "desktop = \"niri\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&plan, &registry, &home).unwrap();
    assert_eq!(
        fs::read_to_string(home.join(".config/niri/config.kdl")).unwrap(),
        "include \"default.kdl\"\ninclude \"custom/config.kdl\"\n"
    );
    assert_eq!(
        fs::read_link(home.join(".config/niri/default.kdl")).unwrap(),
        source.join(".config/niri/default.kdl")
    );
}

// Protects Niri's native include entry point without executing shell text:
// the named adapter must generate HOME-relative includes only.
#[test]
fn materialization_generates_niri_include_adapter() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "niri"
role = "desktop"
[[components.configuration]]
target = ".config/niri/config.kdl"
lifecycle = "generate"
adapter = "niri-includes"
"#,
            "",
            "desktop = \"niri\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&plan, &registry, &home).unwrap();
    let generated = fs::read_to_string(home.join(".config/niri/config.kdl")).unwrap();
    assert!(generated.contains("include \"default.kdl\""));
    assert!(generated.contains("include \"custom/config.kdl\""));
    assert!(!generated.contains("/usr/share"));
}

// Protects applications whose prior config target is a non-empty directory:
// staged generation replaces it through Linux's atomic exchange path.
#[test]
fn materialization_replaces_existing_nonempty_directory_with_generate() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "app"
role = "app"
[[components.configuration]]
target = ".config/app/config"
lifecycle = "generate"
template = "generated"
"#,
            "",
            "app = \"app\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let old = home.join(".config/app/config");
    fs::create_dir_all(old.join("nested")).unwrap();
    fs::write(old.join("nested/old"), "old").unwrap();
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&plan, &registry, &home).unwrap();
    assert_eq!(fs::read_to_string(old).unwrap(), "generated");
}

// Protects planner-time ownership validation: an on-disk parent symlink may
// not hide a child target that Catdot would otherwise write through.
#[test]
fn materialization_rejects_existing_symlink_parent_before_writing_child() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::write(source.join(".config/app/config"), "managed").unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "app"
role = "app"
[[components.configuration]]
target = ".config/app/config"
lifecycle = "overwrite"
mode = "file"
source = ".config/app/config"
"#,
            "",
            "app = \"app\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(temp.path().join("outside"), home.join(".config")).unwrap();
    let registry = home.join(".local/state/catdot/managed.toml");
    assert!(build_activation_plan(&profiles, &state, &home, &registry).is_err());
}

// Protects writable generated resources and application-owned custom areas.
// The former single-file link manager followed neither binary data nor a
// one-time seed rule, so a normal reapply could overwrite user work.
#[test]
fn materialization_copies_binary_trees_preserves_mode_and_seeds_once() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/app/runtime/empty")).unwrap();
    let binary = source.join(".config/app/runtime/theme.bin");
    fs::write(&binary, [0_u8, 255, 17]).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o751)).unwrap();
    fs::create_dir_all(source.join(".config/app/custom")).unwrap();
    fs::write(source.join(".config/app/custom/config"), "seed").unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "app"
role = "app"
[[components.configuration]]
target = ".config/app/runtime"
lifecycle = "overwrite"
mode = "file"
source = ".config/app/runtime"
[[components.configuration]]
target = ".config/app/custom"
lifecycle = "user"
seed = ".config/app/custom"
"#,
            "",
            "app = \"app\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&plan, &registry, &home).unwrap();
    let target = home.join(".config/app/runtime/theme.bin");
    assert_eq!(fs::read(target).unwrap(), [0_u8, 255, 17]);
    assert_eq!(
        fs::metadata(home.join(".config/app/runtime/theme.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    fs::write(home.join(".config/app/custom/config"), "mine").unwrap();
    apply_configuration(&plan, &registry, &home).unwrap();
    assert_eq!(
        fs::read_to_string(home.join(".config/app/custom/config")).unwrap(),
        "mine"
    );
}

// Protects first-login custom ownership: if a later managed resource fails,
// a newly seeded custom tree must not survive as a half-applied profile.
#[test]
fn materialization_rolls_back_new_user_seed_after_later_failure() {
    use std::os::unix::ffi::OsStrExt;
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/app/custom")).unwrap();
    fs::write(source.join(".config/app/custom/config"), "seed").unwrap();
    let bad = source.join(".config/app/bad");
    let bad_c = std::ffi::CString::new(bad.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(bad_c.as_ptr(), 0o600) }, 0);
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "app"
role = "app"
[[components.configuration]]
target = ".config/app/custom"
lifecycle = "user"
seed = ".config/app/custom"
[[components.configuration]]
target = ".config/app/bad"
lifecycle = "overwrite"
mode = "file"
source = ".config/app/bad"
"#,
            "",
            "app = \"app\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    assert!(apply_configuration(&plan, &registry, &home).is_err());
    assert!(!home.join(".config/app/custom").exists());
}

// Protects profile switching: an incomplete materialization must restore a
// replaced file and leave no partial tree. Old journals only supported UTF-8
// regular files and symlinks.
#[test]
fn materialization_rolls_back_binary_target_after_later_failure() {
    use std::os::unix::ffi::OsStrExt;
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::write(source.join(".config/app/good"), [3_u8, 2, 1]).unwrap();
    let unsupported = source.join(".config/app/unsupported");
    let unsupported_c = std::ffi::CString::new(unsupported.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(unsupported_c.as_ptr(), 0o600) }, 0);
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "app"
role = "app"
[[components.configuration]]
target = ".config/app/good"
lifecycle = "overwrite"
mode = "file"
source = ".config/app/good"
[[components.configuration]]
target = ".config/app/unsupported"
lifecycle = "overwrite"
mode = "file"
source = ".config/app/unsupported"
"#,
            "",
            "app = \"app\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let old = home.join(".config/app/good");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, [9_u8, 8, 7]).unwrap();
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    assert!(apply_configuration(&plan, &registry, &home).is_err());
    assert_eq!(fs::read(old).unwrap(), [9_u8, 8, 7]);
}

// Protects user custom areas from an owning directory link or recursive copy.
// Such a plan would make a later normal apply overwrite a user's custom file.
#[test]
fn materialization_rejects_parent_and_user_child_conflicts_before_writing_home() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "one"
role = "one"
[[components.configuration]]
target = ".config/app"
lifecycle = "overwrite"
mode = "symlink"
source = ".config/app"
[[components]]
id = "two"
role = "two"
[[components.configuration]]
target = ".config/app/custom"
lifecycle = "user"
"#,
            "",
            "one = \"one\"\ntwo = \"two\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    assert!(build_activation_plan(&profiles, &state, &home, &registry).is_err());
    assert!(!home.exists());
}

// Protects the single-owner rule: two active components cannot silently race
// to replace one application file during activation.
#[test]
fn materialization_rejects_duplicate_component_targets_before_writing_home() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::write(source.join(".config/app/one"), "one").unwrap();
    fs::write(source.join(".config/app/two"), "two").unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "one"
role = "one"
[[components.configuration]]
target = ".config/app/config"
lifecycle = "overwrite"
mode = "file"
source = ".config/app/one"
[[components]]
id = "two"
role = "two"
[[components.configuration]]
target = ".config/app/config"
lifecycle = "overwrite"
mode = "file"
source = ".config/app/two"
"#,
            "",
            "one = \"one\"\ntwo = \"two\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    assert!(build_activation_plan(&profiles, &state, &home, &registry).is_err());
    assert!(!home.exists());
}

// Protects a component switch from leaving executable stale fragments behind.
// The former link registry was limited to links and could not remove a copied
// managed file when the next active component no longer owns that target.
#[test]
fn materialization_removes_old_managed_target_on_component_switch() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/one")).unwrap();
    fs::write(source.join(".config/one/config"), "old").unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "one"
role = "tool"
[[components.configuration]]
target = ".config/one/config"
lifecycle = "overwrite"
mode = "file"
source = ".config/one/config"
[[components]]
id = "two"
role = "tool"
"#,
            "",
            "tool = \"one\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let mut state = select_profile(&profiles["demo"]).unwrap();
    let first = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&first, &registry, &home).unwrap();
    state.components.insert("tool".into(), "demo/two".into());
    let second = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&second, &registry, &home).unwrap();
    assert!(!home.join(".config/one/config").exists());
}

// Protects the outer desired/active transaction: a later state-write failure
// must restore an entire prior tree, including empty directories, modes and a
// symlink itself rather than the symlink target's contents.
#[test]
fn activation_journal_restores_binary_directory_symlink_and_modes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempdir().unwrap();
    let state = temp.path().join("state.toml");
    let target = temp.path().join("home/.config/app");
    fs::create_dir_all(target.join("empty")).unwrap();
    fs::write(target.join("bin"), [0_u8, 7, 255]).unwrap();
    fs::set_permissions(target.join("bin"), fs::Permissions::from_mode(0o751)).unwrap();
    symlink("bin", target.join("current")).unwrap();
    let mut journal =
        ActivationJournal::begin(&state, Default::default(), Default::default()).unwrap();
    journal.track_path(&target).unwrap();
    journal.mark_applying().unwrap();
    fs::remove_dir_all(&target).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("changed"), "broken").unwrap();
    journal.mark_applied().unwrap();
    drop(journal);
    recover_activation_journals(&state).unwrap();
    assert_eq!(fs::read(target.join("bin")).unwrap(), [0_u8, 7, 255]);
    assert_eq!(
        fs::metadata(target.join("bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    assert!(target.join("empty").is_dir());
    assert_eq!(
        fs::read_link(target.join("current")).unwrap(),
        std::path::Path::new("bin")
    );
    let backups = temp.path().join("backups");
    let generation = fs::read_dir(&backups)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(generation.join("metadata.toml").is_file());
    assert_eq!(
        fs::read(generation.join("home/home/.config/app/bin")).unwrap(),
        [0_u8, 7, 255]
    );
}

// Protects a concurrent desired-state update: recovery must refuse to replace
// a third state value that was not written by the activation transaction.
#[test]
fn activation_recovery_refuses_externally_changed_state() {
    let temp = tempdir().unwrap();
    let state = temp.path().join("state.toml");
    let old = UserState {
        generation: 1,
        ..Default::default()
    };
    let mut new = old.clone();
    new.generation = 2;
    write_state(&state, &old).unwrap();
    let mut journal = ActivationJournal::begin(&state, old.clone(), new).unwrap();
    journal.mark_applying().unwrap();
    let mut external = old.clone();
    external.generation = 3;
    write_state(&state, &external).unwrap();
    drop(journal);
    assert!(recover_activation_journals(&state).is_err());
    assert_eq!(read_state(&state).unwrap().generation, 3);
}

// Protects the package-finalize boundary: a successful package transaction
// followed by a failed record finalize must not leave the new HOME tree or
// active state visible. The old resolve path completed its journal first.
#[test]
fn activation_journal_can_explicitly_rollback_after_finalize_failure() {
    let temp = tempdir().unwrap();
    let state_path = temp.path().join("home/.local/state/catdot/state.toml");
    let target = temp.path().join("home/.config/app/config");
    let old = UserState {
        generation: 1,
        active_generation: 1,
        ..Default::default()
    };
    let mut new = old.clone();
    new.generation = 2;
    new.active_generation = 2;
    write_state(&state_path, &old).unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "old").unwrap();
    let mut journal = ActivationJournal::begin(&state_path, old.clone(), new.clone()).unwrap();
    journal.track_path(&target).unwrap();
    journal.mark_applying().unwrap();
    fs::write(&target, "new").unwrap();
    journal.mark_applied().unwrap();
    write_state(&state_path, &new).unwrap();
    journal.mark_state_written().unwrap();
    journal.rollback().unwrap();

    assert_eq!(read_state(&state_path).unwrap(), old);
    assert_eq!(fs::read_to_string(target).unwrap(), "old");
}

// Desktop overrides select desired providers only; they never activate one.
#[test]
fn skel_default_desktop_override_replaces_the_profile_default() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            &(component("foot", "terminal") + &component("kitty", "terminal")),
            "",
            "terminal = \"foot\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let declaration = temp.path().join("default.toml");
    fs::write(
        &declaration,
        "schema = 1\nprofile = \"demo\"\n[desktop.niri.components]\nterminal = \"kitty\"\n",
    )
    .unwrap();
    unsafe { std::env::set_var("XDG_CURRENT_DESKTOP", "niri") };
    let state = initialize_state_from_default(
        &temp.path().join("home/state.toml"),
        &declaration,
        &profiles,
    )
    .unwrap();
    unsafe { std::env::remove_var("XDG_CURRENT_DESKTOP") };
    assert_eq!(state.components["terminal"], "demo/kitty");
    assert!(state.active_components.is_empty());
}
