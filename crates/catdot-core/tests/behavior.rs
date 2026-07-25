use catdot_core::{
    ActivationJournal, ComponentDef, Profile, UserState, XdgProvider, activate_configuration,
    activate_xdg, build_activation_plan, build_activation_preview, build_xdg_plan,
    discover_profiles, initialize_state_from_default, read_state, recover_activation_journals,
    select_profile, write_state,
};
use std::{collections::BTreeMap, fs};
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

// Protects a real application entry point: templates must create
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
    let error = discover_profiles(&temp.path().join("profiles")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("default configuration target conflict")
    );
    assert!(!temp.path().join("home").exists());
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
    let error = discover_profiles(&temp.path().join("profiles")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("default configuration target conflict")
    );
    assert!(!temp.path().join("home").exists());
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

// Protects package-update reapplication: generated and copied inputs must be
// rewritten, a correct symlink must stay untouched while seeing new source
// content, and a changed symlink source path must be repaired.
#[test]
fn reapply_updates_generate_and_file_and_repairs_only_changed_symlinks() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/demo")).unwrap();
    fs::write(source.join(".config/demo/file"), "first file").unwrap();
    fs::write(source.join(".config/demo/link"), "first link").unwrap();
    fs::write(source.join(".config/demo/link2"), "second path").unwrap();
    fs::create_dir_all(&metadata).unwrap();
    let manifest = metadata.join("profile.toml");
    fs::write(
        &manifest,
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "desktop"
role = "desktop"
[[components.configuration]]
target = ".config/demo/generated"
lifecycle = "generate"
template = "first generated"
[[components.configuration]]
target = ".config/demo/file"
lifecycle = "overwrite"
mode = "file"
source = ".config/demo/file"
[[components.configuration]]
target = ".config/demo/link"
lifecycle = "overwrite"
mode = "symlink"
source = ".config/demo/link"
"#,
            "",
            r#"desktop = "desktop"
"#,
        ),
    )
    .unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    let target = home.join(".config/demo/link");
    let link_inode = fs::symlink_metadata(&target).unwrap().ino();
    fs::write(source.join(".config/demo/file"), "second file").unwrap();
    fs::write(source.join(".config/demo/link"), "second link").unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/file")).unwrap(),
        "second file"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "second link");
    assert_eq!(fs::symlink_metadata(&target).unwrap().ino(), link_inode);
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("first generated", "second generated")
            .replace(
                "source = \".config/demo/link\"",
                "source = \".config/demo/link2\"",
            ),
    )
    .unwrap();
    let changed = discover_profiles(&temp.path().join("profiles")).unwrap();
    let changed_state = select_profile(&changed["demo"]).unwrap();
    apply_configuration(
        &build_activation_plan(&changed, &changed_state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/generated")).unwrap(),
        "second generated"
    );
    assert_eq!(
        fs::read_link(target).unwrap(),
        source.join(".config/demo/link2")
    );
}

// Successful activations retain a bounded recovery history. Without this,
// ordinary profile updates would grow a user's state directory indefinitely.
#[test]
fn activation_backup_retention_keeps_the_latest_five_generations() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "desktop"
role = "desktop"
[[components.configuration]]
target = ".config/demo/generated"
lifecycle = "generate"
template = "generated"
"#,
            "",
            r#"desktop = "desktop"
"#,
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    for _ in 0..6 {
        apply_configuration(&plan, &registry, &home).unwrap();
    }
    let backups = home.join(".local/state/catdot/backups");
    assert_eq!(fs::read_dir(backups).unwrap().count(), 5);
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

// A Profile may name real Arch dependencies, not only Catdot-style IDs.
// This protects auxiliary desktop packages and versioned/virtual requirements.
#[test]
fn discovery_accepts_arch_dependency_expressions() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "desktop"
role = "desktop"
packages = [
  "afl++",
  "db5.3",
  "lib32-lm_sensors",
  "niri>=25.05",
  "virtual-provider=2:1.0-1",
]
"#,
            "",
            "desktop = \"desktop\"\n",
        ),
    )
    .unwrap();

    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    assert_eq!(
        profiles["demo"].components["desktop"].packages,
        [
            "afl++",
            "db5.3",
            "lib32-lm_sensors",
            "niri>=25.05",
            "virtual-provider=2:1.0-1",
        ]
    );
}

// A component package may provide its source tree. The reviewable dry-run must
// therefore work before installation, while real activation must still verify it.
#[test]
fn activation_preview_defers_missing_component_sources() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("not-installed-yet");
    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "terminal"
role = "terminal"
packages = ["ghostty"]
[[components.configuration]]
target = ".config/ghostty/config"
lifecycle = "overwrite"
mode = "file"
source = ".config/ghostty/config"
"#,
            "",
            "terminal = \"terminal\"\n",
        ),
    )
    .unwrap();

    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    assert!(build_activation_preview(&profiles, &state, &home, &registry).is_ok());
    assert!(build_activation_plan(&profiles, &state, &home, &registry).is_err());
}

// Changing a path from Catdot-managed overwrite to user ownership must release
// it without deleting the user's current writable content.
#[test]
fn materialization_releases_overwrite_file_to_user_without_deleting_it() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(source.join(".config/app/config"), "managed").unwrap();
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
    let registry = home.join(".local/state/catdot/managed.toml");
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&plan, &registry, &home).unwrap();
    let target = home.join(".config/app/config");
    fs::write(&target, "mine").unwrap();

    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "app"
role = "app"
[[components.configuration]]
target = ".config/app/config"
lifecycle = "user"
"#,
            "",
            "app = \"app\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&plan, &registry, &home).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "mine");
    assert!(
        catdot_core::read_managed_registry(&registry)
            .unwrap()
            .entries
            .is_empty()
    );
}

// Releasing a managed symlink to user ownership must detach it from the
// package-owned source, otherwise an upgrade or uninstall can still change it.
#[test]
fn materialization_detaches_overwrite_symlink_when_released_to_user() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(source.join(".config/app/config"), "managed").unwrap();
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
mode = "symlink"
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
    let registry = home.join(".local/state/catdot/managed.toml");
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    let target = home.join(".config/app/config");
    assert!(
        fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    fs::write(
        metadata.join("profile.toml"),
        profile(
            &source.display().to_string(),
            r#"[[components]]
id = "app"
role = "app"
[[components.configuration]]
target = ".config/app/config"
lifecycle = "user"
"#,
            "",
            "app = \"app\"\n",
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();

    assert!(
        !fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "managed");
}

// A user-owned target is initialized once, not whenever it happens to be
// absent. Deleting it is itself a user decision and normal apply/update must
// preserve that absence until an explicit reset.
#[test]
fn materialization_does_not_reseed_a_deleted_user_target() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join("custom")).unwrap();
    fs::write(source.join("custom/seed"), "seed").unwrap();
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
seed = "custom"
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
    let first = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&first, &registry, &home).unwrap();
    let target = home.join(".config/app/custom");
    assert!(target.join("seed").is_file());
    fs::remove_dir_all(&target).unwrap();
    let second = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    apply_configuration(&second, &registry, &home).unwrap();
    assert!(!target.exists());
}

// Reapplying an already-satisfied XDG declaration is a no-op. This keeps
// resolve idempotent and avoids replacing mimeapps.list on every invocation.
#[test]
fn xdg_activation_is_idempotent_when_defaults_are_already_satisfied() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempdir().unwrap();
    let config = temp.path().join(".config");
    fs::create_dir_all(&config).unwrap();
    let mut profile = Profile {
        id: "demo".into(),
        name: "Demo".into(),
        description: "test".into(),
        source_root: temp.path().join("share"),
        defaults: BTreeMap::new(),
        components: BTreeMap::new(),
    };
    profile.components.insert(
        "browser".into(),
        ComponentDef {
            role: "browser".into(),
            packages: vec![],
            exec: vec![],
            xdg: XdgProvider {
                command: None,
                environment: vec![],
                desktop_entry: Some("demo.desktop".into()),
                mime_types: vec!["text/html".into()],
                uri_schemes: vec![],
            },
            wm: None,
            configuration: vec![],
        },
    );
    let profiles = [("demo".into(), profile)].into_iter().collect();
    let mut state = UserState::default();
    state
        .components
        .insert("browser".into(), "demo/browser".into());
    let state_path = temp.path().join("state/catdot/state.toml");
    let first = build_xdg_plan(&profiles, &state, &config).unwrap();
    let mut journal =
        ActivationJournal::begin(&state_path, UserState::default(), UserState::default()).unwrap();
    journal.mark_applying().unwrap();
    activate_xdg(&first, &mut journal).unwrap();
    journal.complete().unwrap();
    let mimeapps = config.join("mimeapps.list");
    let inode = fs::metadata(&mimeapps).unwrap().ino();
    let second = build_xdg_plan(&profiles, &state, &config).unwrap();
    let mut journal =
        ActivationJournal::begin(&state_path, UserState::default(), UserState::default()).unwrap();
    journal.mark_applying().unwrap();
    activate_xdg(&second, &mut journal).unwrap();
    journal.complete().unwrap();
    assert_eq!(fs::metadata(mimeapps).unwrap().ino(), inode);
}

// Protects the schema 3 installation contract: a fresh profile can install
// generated files, static links, mergeable defaults and user-owned seeds
// without relying on the removed overwrite/file lifecycle.
#[test]
fn schema3_first_install_materializes_the_four_lifecycles() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(source.join("defaults")).unwrap();
    fs::create_dir_all(source.join("custom")).unwrap();
    fs::write(source.join("defaults/generated"), "generated from source\n").unwrap();
    fs::write(source.join("defaults/static"), "static\n").unwrap();
    fs::write(source.join("defaults/binds"), "binds v1\n").unwrap();
    fs::write(source.join("custom/seed"), "seed\n").unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
desktop = "desktop"
[[components]]
id = "desktop"
role = "desktop"
[[components.configuration]]
target = ".config/demo/root"
lifecycle = "generate"
template = "root\n"
[[components.configuration]]
target = ".config/demo/generated"
lifecycle = "generate"
source = "defaults/generated"
[[components.configuration]]
target = ".config/demo/static"
lifecycle = "symlink"
source = "defaults/static"
[[components.configuration]]
target = ".config/demo/binds"
lifecycle = "merge"
source = "defaults/binds"
[[components.configuration]]
target = ".config/demo/custom"
lifecycle = "user"
seed = "custom"
"#,
            source.display()
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
        fs::read_to_string(home.join(".config/demo/root")).unwrap(),
        "root\n"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/generated")).unwrap(),
        "generated from source\n"
    );
    assert_eq!(
        fs::read_link(home.join(".config/demo/static")).unwrap(),
        source.join("defaults/static")
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/binds")).unwrap(),
        "binds v1\n"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config/demo/custom/seed")).unwrap(),
        "seed\n"
    );
}

// Schema 3 must not silently preserve the old copy lifecycle under another
// spelling. Static writable files are generated; immutable resources are links.
#[test]
fn schema3_rejects_overwrite_and_mode() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 3\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\n[[components.configuration]]\ntarget = \".config/demo/config\"\nlifecycle = \"overwrite\"\nmode = \"file\"\nsource = \"config\"\n",
            source.display()
        ),
    )
    .unwrap();
    assert!(discover_profiles(&temp.path().join("profiles")).is_err());
}

// Protects best-effort configuration updates: clean upstream changes update a
// locally untouched file, while a user-only edit survives an unchanged upstream.
#[test]
fn merge_updates_clean_files_and_preserves_user_only_edits() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&source).unwrap();
    let upstream = source.join("binds.kdl");
    fs::write(&upstream, "v1\n").unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 3\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\ndesktop = \"desktop\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\n[[components.configuration]]\ntarget = \".config/demo/binds.kdl\"\nlifecycle = \"merge\"\nsource = \"binds.kdl\"\n",
            source.display()
        ),
    )
    .unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    let target = home.join(".config/demo/binds.kdl");

    fs::write(&upstream, "v2\n").unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "v2\n");

    fs::write(&target, "mine\n").unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "mine\n");
}

// Protects profile switching: a merge-owned file is backed up before removal,
// and selecting the profile again starts from the current packaged default.
#[test]
fn switching_away_from_merge_backs_up_then_reinstalls_fresh() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("binds.kdl"), "default\n").unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 3\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\ndesktop = \"one\"\n[[components]]\nid = \"one\"\nrole = \"desktop\"\n[[components.configuration]]\ntarget = \".config/demo/binds.kdl\"\nlifecycle = \"merge\"\nsource = \"binds.kdl\"\n[[components]]\nid = \"two\"\nrole = \"desktop\"\n",
            source.display()
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let mut state = select_profile(&profiles["demo"]).unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    let target = home.join(".config/demo/binds.kdl");
    fs::write(&target, "mine\n").unwrap();

    state.components.insert("desktop".into(), "demo/two".into());
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert!(!target.exists());
    let backups = home.join(".local/state/catdot/backups");
    assert!(fs::read_dir(&backups).unwrap().any(|entry| {
        let path = entry.unwrap().path().join("home/.config/demo/binds.kdl");
        fs::read_to_string(path).ok().as_deref() == Some("mine\n")
    }));

    fs::write(source.join("binds.kdl"), "fresh\n").unwrap();
    state.components.insert("desktop".into(), "demo/one".into());
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(target).unwrap(), "fresh\n");
}

// Protects a user's working desktop when both the packaged default and local
// merge file changed: no conflict markers may enter the live configuration,
// and all three inputs must be available for manual recovery.
#[test]
fn merge_conflict_preserves_live_file_and_writes_recovery_inputs() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&source).unwrap();
    let upstream = source.join("binds.kdl");
    fs::write(&upstream, "base\n").unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 3\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\ndesktop = \"desktop\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\n[[components.configuration]]\ntarget = \".config/demo/binds.kdl\"\nlifecycle = \"merge\"\nsource = \"binds.kdl\"\n",
            source.display()
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    let target = home.join(".config/demo/binds.kdl");
    fs::write(&target, "local\n").unwrap();
    fs::write(&upstream, "upstream\n").unwrap();

    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let plan = build_activation_plan(&profiles, &state, &home, &registry).unwrap();
    assert!(apply_configuration(&plan, &registry, &home).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "local\n");

    let conflicts = home.join(".local/state/catdot/conflicts");
    let conflict = fs::read_dir(conflicts)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read_to_string(conflict.join("base")).unwrap(), "base\n");
    assert_eq!(
        fs::read_to_string(conflict.join("local")).unwrap(),
        "local\n"
    );
    assert_eq!(
        fs::read_to_string(conflict.join("upstream")).unwrap(),
        "upstream\n"
    );
    assert!(
        fs::read_to_string(conflict.join("merged"))
            .unwrap()
            .contains("<<<<<<< local")
    );
}

// Protects desktop integration across a provider switch: environment variables
// use the selected component's declared real program name and disappear when
// the role is disabled, without wrappers or stale values.
#[test]
fn xdg_environment_tracks_real_provider_commands_across_switches() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
terminal = "ghostty"
[[components]]
id = "ghostty"
role = "terminal"
[components.xdg]
command = "ghostty"
environment = ["TERMINAL"]
[[components]]
id = "foot"
role = "terminal"
[components.xdg]
command = "foot"
environment = ["TERMINAL"]
"#,
            source.display()
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let config = temp.path().join("home/.config");
    let state_path = temp.path().join("home/.local/state/catdot/state.toml");
    let env_path = config.join("environment.d/90-catdot.conf");
    let mut state = select_profile(&profiles["demo"]).unwrap();

    for expected in [Some("ghostty"), Some("foot"), None] {
        if expected == Some("foot") {
            state
                .components
                .insert("terminal".into(), "demo/foot".into());
        } else if expected.is_none() {
            state.components.remove("terminal");
        }
        let plan = build_xdg_plan(&profiles, &state, &config).unwrap();
        let mut journal =
            ActivationJournal::begin(&state_path, UserState::default(), UserState::default())
                .unwrap();
        journal.mark_applying().unwrap();
        activate_xdg(&plan, &mut journal).unwrap();
        journal.complete().unwrap();
        match expected {
            Some(command) => assert_eq!(
                fs::read_to_string(&env_path).unwrap(),
                format!("TERMINAL={command}\n")
            ),
            None => assert!(!env_path.exists()),
        }
    }
}

// Runtime mixing is best effort: malformed third-party combinations must not
// abort profile switching merely because two components claim one XDG variable.
// The component whose role semantically owns the variable wins deterministically.
#[test]
fn xdg_environment_conflict_uses_the_semantic_role_and_warns() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[[components]]
id = "browser"
role = "browser"
[components.xdg]
command = "browser-editor"
environment = ["EDITOR"]
[[components]]
id = "editor"
role = "editor"
[components.xdg]
command = "nvim"
environment = ["EDITOR", "VISUAL"]
"#,
            source.display()
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let mut state = UserState::default();
    state
        .components
        .insert("browser".into(), "demo/browser".into());
    state
        .components
        .insert("editor".into(), "demo/editor".into());
    let plan = build_xdg_plan(&profiles, &state, &temp.path().join("home/.config")).unwrap();
    assert_eq!(plan.environment["EDITOR"], "nvim");
    assert_eq!(plan.environment["VISUAL"], "nvim");
    assert_eq!(plan.warnings.len(), 1);
    assert!(plan.warnings[0].contains("EDITOR"));
}

// Protects the primary WM installation and switch path: the compositor owns a
// generated autostart fragment whose arbitrary selected roles are emitted in
// declared dependency order and removed when a provider is disabled.
#[test]
fn wm_autostart_is_compiled_from_selected_roles_across_switches() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
desktop = "niri"
bar = "panel"
xwayland = "satellite"
[[components]]
id = "niri"
role = "desktop"
[components.wm]
autostart_target = ".config/niri/autostart.kdl"
autostart_template = 'spawn-at-startup "catdot" "exec" "{{role}}"'
[[components.wm.autostart]]
role = "xwayland"
[[components.wm.autostart]]
role = "bar"
after = ["xwayland"]
[[components]]
id = "panel"
role = "bar"
[components.exec]
argv = ["panel"]
[[components]]
id = "satellite"
role = "xwayland"
[components.exec]
argv = ["xwayland-satellite"]
"#,
            source.display()
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let mut state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    let target = home.join(".config/niri/autostart.kdl");

    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "spawn-at-startup \"catdot\" \"exec\" \"xwayland\"\nspawn-at-startup \"catdot\" \"exec\" \"bar\"\n"
    );

    state.components.remove("bar");
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        "spawn-at-startup \"catdot\" \"exec\" \"xwayland\"\n"
    );
}

// A cyclic startup declaration cannot provide a stable switch result. It must
// be rejected by the planner before a fresh HOME receives any profile files.
#[test]
fn wm_autostart_cycle_is_rejected_before_materialization() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
desktop = "wm"
one = "one"
two = "two"
[[components]]
id = "wm"
role = "desktop"
[components.wm]
autostart_target = ".config/wm/autostart.conf"
autostart_template = "exec catdot exec {{role}}"
[[components.wm.autostart]]
role = "one"
after = ["two"]
[[components.wm.autostart]]
role = "two"
after = ["one"]
[[components]]
id = "one"
role = "one"
[[components]]
id = "two"
role = "two"
"#,
            source.display()
        ),
    )
    .unwrap();
    let error = discover_profiles(&temp.path().join("profiles")).unwrap_err();
    assert!(error.to_string().contains("autostart cycle"));
    assert!(!temp.path().join("home").exists());
}

// Protects an existing user environment file across Catdot ownership. The
// first provider switch may replace it transactionally, but disabling the last
// provider must restore the exact pre-Catdot contents rather than deleting it.
#[test]
fn xdg_environment_restores_the_preexisting_file_after_disable() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    let source = temp.path().join("share/demo");
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
terminal = "terminal"
[[components]]
id = "terminal"
role = "terminal"
[components.xdg]
command = "ghostty"
environment = ["TERMINAL"]
"#,
            source.display()
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let mut state = select_profile(&profiles["demo"]).unwrap();
    let config = temp.path().join("home/.config");
    let state_path = temp.path().join("home/.local/state/catdot/state.toml");
    let environment = config.join("environment.d/90-catdot.conf");
    fs::create_dir_all(environment.parent().unwrap()).unwrap();
    fs::write(&environment, "CUSTOM=value\n").unwrap();

    let plan = build_xdg_plan(&profiles, &state, &config).unwrap();
    let mut journal =
        ActivationJournal::begin(&state_path, UserState::default(), UserState::default()).unwrap();
    journal.mark_applying().unwrap();
    activate_xdg(&plan, &mut journal).unwrap();
    journal.complete().unwrap();
    assert_eq!(
        fs::read_to_string(&environment).unwrap(),
        "TERMINAL=ghostty\n"
    );

    state.components.clear();
    let plan = build_xdg_plan(&profiles, &state, &config).unwrap();
    let mut journal =
        ActivationJournal::begin(&state_path, UserState::default(), UserState::default()).unwrap();
    journal.mark_applying().unwrap();
    activate_xdg(&plan, &mut journal).unwrap();
    journal.complete().unwrap();
    assert_eq!(fs::read_to_string(environment).unwrap(), "CUSTOM=value\n");
}

// Official defaults are a release contract, unlike runtime cross-profile
// mixing. A profile whose default closure assigns one environment key to two
// different programs must be rejected during discovery.
#[test]
fn discovery_rejects_default_xdg_conflicts() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
browser = "browser"
editor = "editor"
[[components]]
id = "browser"
role = "browser"
[components.xdg]
command = "browser-editor"
environment = ["EDITOR"]
[[components]]
id = "editor"
role = "editor"
[components.xdg]
command = "nvim"
environment = ["EDITOR"]
"#,
            temp.path().join("share").display()
        ),
    )
    .unwrap();
    let error = discover_profiles(&temp.path().join("profiles")).unwrap_err();
    assert!(error.to_string().contains("default xdg conflict"));
    assert!(error.to_string().contains("EDITOR"));
}

// A default autostart entry that can only fail at login is not a valid
// published profile. Optional undeclared roles remain allowed; selected roles
// must provide an executable command.
#[test]
fn discovery_rejects_default_wm_autostart_without_exec() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
desktop = "wm"
bar = "bar"
[[components]]
id = "wm"
role = "desktop"
[components.wm]
autostart_target = ".config/wm/autostart.conf"
autostart_template = "exec catdot exec {{role}}"
[[components.wm.autostart]]
role = "bar"
[[components]]
id = "bar"
role = "bar"
"#,
            temp.path().join("share").display()
        ),
    )
    .unwrap();
    let error = discover_profiles(&temp.path().join("profiles")).unwrap_err();
    assert!(error.to_string().contains("default autostart role bar"));
    assert!(error.to_string().contains("no exec provider"));
}

// Default components and the WM backend must retain one physical owner per
// target. Catching this at publication prevents a fresh install from reaching
// the activation transaction only to fail before its first desktop session.
#[test]
fn discovery_rejects_default_configuration_target_conflicts() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            r#"schema = 3
[profile]
id = "demo"
name = "Demo"
description = "test"
source_root = "{}"
[defaults]
desktop = "wm"
bar = "bar"
[[components]]
id = "wm"
role = "desktop"
[components.wm]
autostart_target = ".config/wm/autostart.conf"
autostart_template = "exec catdot exec {{role}}"
[[components.wm.autostart]]
role = "bar"
[[components]]
id = "bar"
role = "bar"
[components.exec]
argv = ["bar"]
[[components.configuration]]
target = ".config/wm/autostart.conf"
lifecycle = "generate"
template = "duplicate"
"#,
            temp.path().join("share").display()
        ),
    )
    .unwrap();
    let error = discover_profiles(&temp.path().join("profiles")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("default configuration target conflict")
    );
    assert!(error.to_string().contains("autostart.conf"));
}

// XDG state files have a dedicated transactional owner. A component may not
// claim the same physical target through a generic lifecycle declaration.
#[test]
fn discovery_rejects_reserved_xdg_configuration_targets() {
    for target in [
        ".config/environment.d/90-catdot.conf",
        ".config/mimeapps.list",
        ".config/catdot/xdg.toml",
    ] {
        let temp = tempdir().unwrap();
        let metadata = temp.path().join("profiles/demo");
        fs::create_dir_all(&metadata).unwrap();
        fs::write(
            metadata.join("profile.toml"),
            format!(
                "schema = 3\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[[components]]\nid = \"main\"\nrole = \"tool\"\n[[components.configuration]]\ntarget = \"{target}\"\nlifecycle = \"generate\"\ntemplate = \"bad\"\n",
                temp.path().join("share").display()
            ),
        )
        .unwrap();
        let error = discover_profiles(&temp.path().join("profiles")).unwrap_err();
        assert!(error.to_string().contains("reserved Catdot target"));
    }
}

// A merge result is part of the confirmed activation plan. Editing either the
// local file or packaged upstream after preview must change the plan identity
// so resolve cannot silently apply different merge inputs.
#[test]
fn merge_plan_identity_tracks_local_and_upstream_inputs() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("profiles/demo");
    let source = temp.path().join("share/demo");
    fs::create_dir_all(&metadata).unwrap();
    fs::create_dir_all(&source).unwrap();
    let upstream = source.join("binds.kdl");
    fs::write(&upstream, "base\n").unwrap();
    fs::write(
        metadata.join("profile.toml"),
        format!(
            "schema = 3\n[profile]\nid = \"demo\"\nname = \"Demo\"\ndescription = \"test\"\nsource_root = \"{}\"\n[defaults]\ndesktop = \"desktop\"\n[[components]]\nid = \"desktop\"\nrole = \"desktop\"\n[[components.configuration]]\ntarget = \".config/demo/binds.kdl\"\nlifecycle = \"merge\"\nsource = \"binds.kdl\"\n",
            source.display()
        ),
    )
    .unwrap();
    let profiles = discover_profiles(&temp.path().join("profiles")).unwrap();
    let state = select_profile(&profiles["demo"]).unwrap();
    let home = temp.path().join("home");
    let registry = home.join(".local/state/catdot/managed.toml");
    apply_configuration(
        &build_activation_plan(&profiles, &state, &home, &registry).unwrap(),
        &registry,
        &home,
    )
    .unwrap();
    let target = home.join(".config/demo/binds.kdl");
    let original = build_activation_plan(&profiles, &state, &home, &registry)
        .unwrap()
        .identity_digest();

    fs::write(&target, "local\n").unwrap();
    let local_changed = build_activation_plan(&profiles, &state, &home, &registry)
        .unwrap()
        .identity_digest();
    assert_ne!(original, local_changed);

    fs::write(&target, "base\n").unwrap();
    fs::write(&upstream, "upstream\n").unwrap();
    let upstream_changed = build_activation_plan(&profiles, &state, &home, &registry)
        .unwrap()
        .identity_digest();
    assert_ne!(original, upstream_changed);
}
