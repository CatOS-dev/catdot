# Catdot

Catdot is CatOS's Rust desktop Profile manager. Profiles installed beneath
`/usr/share/catdot/profiles` define components, their safe user-configuration
links, argv-based launch commands, and package requirements. This repository
ships only the `catos-default` GTK/Qt appearance profile. Desktop profiles are
provided by their respective CatOS profile packages.

```toml
schema = 1
[profile]
id = "catos-niri-haha"
name = "CatOS Niri Haha"
description = "Niri desktop"
[defaults]
wm = "niri"
bar = "waybar"
[components.niri]
role = "wm"
path = "niri"
packages = ["niri"]
[[components.niri.links]]
source = "config.kdl"
target = "{xdg_config_home}/niri/config.kdl"
[components.waybar]
role = "bar"
path = "waybar"
packages = ["waybar"]
exec = ["waybar", "--config", "{component}/config.jsonc"]
```

Main commands:

```text
catdot list [profile]
catdot current
catdot select <profile>
catdot select <role> <profile/component>
catdot disable <role>
catdot apply
catdot adopt <role>
catdot exec <role> [arguments...]
catdot resolve [--dry-run] [--yes] [--with-optional]
catdot prune [--dry-run] [--yes]
catdot doctor
catdot users list
catdot users prune [--yes]
```

`select` persists unresolved choices without installing packages; run
`resolve` to show the libalpm-validated system plan and install dependencies.
`prune` only removes Catdot-installed, unreferenced packages that libalpm can
safely remove. `adopt` backs up an existing non-managed target before linking
the requested role. Set `CATDOT_PROFILE_ROOT` in tests to select a temporary
profile root.

The read-only resolve/prune plan runs as the calling user and accepts only that
user's UID. The confirmed transaction obtains the actual caller UID from
`pkexec`, reparses installed manifests, and computes packages itself; it never
accepts a shell command or package list. For custom `XDG_STATE_HOME`, the CLI
supplies the state path, but the helper accepts only an absolute, regular file
owned by that caller UID (no symlink or foreign-owned path). Package
transactions use libalpm.
`packages.toml` records whether Catdot first installed a package and the
concrete component references; prune only attempts Catdot-installed,
unreferenced dependency packages and lets libalpm validate the removal
transaction.

Run `packaging/verify-local-source.sh` to build from a clean temporary source
tree and verify the staged package contents. The release PKGBUILD consumes a
tagged source archive.
