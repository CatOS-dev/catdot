# Catdot

Catdot is CatOS's Rust desktop Profile and component manager. Profile metadata
installed below `/usr/share/catdot/profiles` defines replaceable desktop
components, configuration lifecycle, argv-based launch commands, XDG defaults,
and package requirements.

The `catdot` package always ships the GTK-only `catos-default` Profile and its
new-user default declaration. The `catos-gtk-settings` package owns the GTK
source files under `/etc/skel`. Selecting another complete Profile replaces
`catos-default`; it is the guaranteed initial Profile, not a permanent layer.
Desktop Profiles such as `catos-niri-dms` are supplied by their own packages.

## User workflow

List installed profiles and inspect one before selecting it:

```sh
catdot list
catdot list catos-default
```

Select a complete profile, or replace one role with a component from another
profile:

```sh
catdot select catos-default
catdot select terminal catos-common/foot
```

Selection changes the **desired** configuration only. The previous **active**
configuration remains usable until dependency resolution and activation finish
successfully. Review and apply pending changes with:

```sh
catdot resolve --dry-run
catdot resolve
```

`resolve` displays package installation, replacement, removal, and component
activation changes before confirmation. Profile `packages` accept normal Arch
dependency expressions, including versions and virtual providers. A successful
run installs required packages, applies user configuration, switches the active
components, and finalizes the multi-user package records. Repeating it when
nothing changed prints `Catdot is already up to date.`

Inspect current state and launch the active provider for a role:

```sh
catdot current
catdot current --verbose
catdot exec terminal
catdot exec browser https://catos.dev/
```

The default `current` output is user-oriented. `--verbose` additionally shows
state generations used for transaction diagnosis.

Other common operations:

```text
catdot disable <role>              Stop selecting a role after resolve
catdot apply                       Reapply the active configuration
catdot prune [--dry-run] [--yes]   Remove safe, unused Catdot packages
catdot doctor                      Diagnose user and system state
catdot users list                  List system user records
catdot users prune [--yes]         Remove records for deleted users
catdot validate PROFILE_ROOT       Validate profile packages without user state
```

Schema 3 has four configuration lifecycles:

- `generate` owns a writable result produced from an inline template, a source
  file, or a backend result. User edits are allowed but are overwritten on the
  next apply.
- `symlink` exposes an immutable file or directory from the Profile source tree.
- `merge` installs an editable text default and performs a best-effort three-way
  merge when that default changes. A conflict preserves the live file and writes
  `base`, `local`, `upstream`, and `merged` recovery inputs below Catdot state.
- `user` seeds a file or directory once and does not update it afterward.

Lifecycle-managed targets are backed up and replaced transactionally during
`resolve`; there is no separate adoption workflow. Switching away from a
`merge` target backs it up and removes the live target. Selecting that Profile
again installs the current packaged default as a fresh configuration. Schema 2
`overwrite/file` and `overwrite/symlink` declarations remain readable only for
compatibility with already published Profiles.

## Reliability model

Catdot guarantees the safe installation and switching mechanics for valid
Profiles: a failed activation does not advance active state, and the previous
working configuration remains available. Official Profile defaults are checked
for target ownership conflicts, XDG single-value conflicts, invalid WM startup
orders, and selected startup roles without executable providers. Package builds
and CI can run `catdot validate PROFILE_ROOT` without creating user state.

Arbitrary cross-Profile component mixing, automatic three-way merge results,
and migration of heavily modified old configurations are best-effort features.
Catdot attempts them deterministically, reports warnings or conflicts, and must
not sacrifice the previous active configuration to force a result. WM startup
ordering guarantees emitted command order, not process readiness.

Catdot keeps desired and active selections separate. Package and activation
plans are regenerated from installed manifests, tied to a state generation,
and checked by digest before a privileged transaction. The helper never accepts
an arbitrary shell command or a client-supplied package list.

User configuration changes use atomic files, a per-user lock, a managed-target
registry, and an activation journal. Package installation, finalization, and
pruning use root-owned records and recovery journals under `/var/lib/catdot`.
If a process or machine stops after an ALPM transaction but before record
commit, the next mutating operation completes only a provably safe recovery.
An uncertain `Prepared` journal remains blocked and is inspected with:

```sh
catdot recover list
```

When every package expected from the confirmed transaction is installed and
every confirmed replacement target is absent, an administrator may commit the
recorded result with:

```sh
catdot recover accept TRANSACTION --yes
```

When no newly introduced package exists and every package scheduled for removal
is still present, the untouched journal may instead be discarded with:

```sh
catdot recover discard TRANSACTION --yes
```

Partial package states and journals created before the recovery metadata schema
are never guessed; both decisions remain unavailable until manually inspected.

Catdot only claims ownership of packages that were absent before its
transaction. Pre-existing explicit packages remain explicit even when ALPM
upgrades them as part of resolving a profile. `prune` removes only packages
introduced by Catdot that are now unreferenced, still dependency-installed, not
protected by `HoldPkg`, and safe to remove according to libalpm.

## Privilege boundary

Two separate privileged executables are installed:

- `/usr/lib/catdot/catdot-query-helper` implements read-only plan and diagnostic
  operations. Its Polkit action allows an active local session without an
  administrator prompt.
- `/usr/lib/catdot/catdot-helper` implements package transactions, finalization,
  stale-record deletion, and recovery writes. Its Polkit action requires
  administrator authentication.

Both helpers verify the original caller UID. The query helper rejects every
mutating subcommand, while the management helper rejects read-only subcommands.
The command-line client still asks for confirmation after displaying a
mutating plan unless `--yes` is supplied. Non-interactive mutation without
`--yes` is refused.

## Profile manifest example

```toml
schema = 3

[profile]
id = "catos-niri-dms"
name = "CatOS Niri DMS"
description = "Complete CatOS Niri desktop Profile"
source_root = "/usr/share/catos-niri-dms"

[defaults]
desktop = "niri"
bar = "dms"
terminal = "ghostty"

[[components]]
id = "niri"
role = "desktop"
packages = ["xdg-desktop-portal-gtk"]

[components.wm]
autostart_target = ".config/niri/autostart.kdl"
autostart_template = 'spawn-at-startup "catdot" "exec" "{role}"'

[[components.wm.autostart]]
role = "bar"

[[components.wm.autostart]]
role = "xwayland"
before = ["bar"]

[[components.configuration]]
target = ".config/niri/config.kdl"
lifecycle = "generate"
source = ".config/niri/config.kdl"

[[components.configuration]]
target = ".config/niri/binds.kdl"
lifecycle = "merge"
source = ".config/niri/binds.kdl"

[[components.configuration]]
target = ".config/niri/static"
lifecycle = "symlink"
source = ".config/niri/static"

[[components.configuration]]
target = ".config/niri/custom"
lifecycle = "user"
seed = ".config/niri/custom"

[[components]]
id = "dms"
role = "bar"

[components.exec]
argv = ["dms", "run", "--session"]

[[components]]
id = "ghostty"
role = "terminal"

[components.exec]
argv = ["ghostty"]

[components.xdg]
command = "ghostty"
environment = ["TERMINAL"]
```

The WM declaration compiles only selected autostart roles, applies stable
`before`/`after` topological ordering, and rejects cycles. Role names are not
hard-coded by Catdot. Binds can call `catdot exec <role>` and therefore do not
need regeneration when only the provider changes. XDG declarations use an
explicit real executable name; `command` does not accept arguments or wrappers.
MIME types, URI schemes, and desktop entries remain part of the same XDG slot.

External component files must be named explicitly in `component_files`;
Catdot does not discover arbitrary TOML files. Manifest commands are argv
arrays and never use `/bin/sh -c`.

## Development checks

Run the normal Rust and clean-source package checks with:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
./packaging/verify-local-source.sh
```

The rootless integration matrix performs real Arch Linux package transactions
inside disposable Podman containers:

```sh
./tests/run-rootless.sh
```

It covers zero-package activation, strict-umask multi-user state, real libalpm
installation and pruning, explicit-package upgrades, transaction recovery,
conflict replacement, stale users, and privileged helper separation. It never
modifies host packages.

The release PKGBUILD intentionally references a future tagged source archive.
A real tag and checksum are added only when the project is ready for release.
