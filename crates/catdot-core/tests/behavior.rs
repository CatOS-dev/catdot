use catdot_core::{discover_profiles, initialize_state_from_default, read_state};
use std::fs;
use tempfile::tempdir;

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
