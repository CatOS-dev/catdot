# Catdot

Catdot is CatOS's Rust desktop profile manager. Profiles installed below
`/usr/share/catdot/profiles` define replaceable desktop components, safe
user-configuration links, argv-based launch commands, theme settings, and
package requirements.

The `catdot` package ships only the `catos-default` GTK/Qt appearance profile.
Desktop profiles such as Niri or Sway are supplied by their own CatOS profile
packages.

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
activation changes before confirmation. A successful run installs required
packages, applies user configuration, switches the active components, and
finalizes the multi-user package records. Repeating it when nothing changed
prints `Catdot is already up to date.`

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
```

Lifecycle-managed targets are backed up and replaced transactionally during
`resolve`; there is no separate adoption workflow.

## Reliability model

Catdot keeps desired and active selections separate. Package and activation
plans are regenerated from installed manifests, tied to a state generation,
and checked by digest before a privileged transaction. The helper never accepts
an arbitrary shell command or a client-supplied package list.

User configuration changes use atomic files, a per-user lock, a managed-target
registry, and an activation journal. Package installation, finalization, and
pruning use root-owned records and recovery journals under `/var/lib/catdot`.
If a process or machine stops after an ALPM transaction but before record
commit, the next mutating operation completes the safe recovery or refuses an
uncertain partial result.

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
schema = 1

[profile]
id = "catos-niri-default"
name = "CatOS Niri Default"
description = "Default CatOS Niri desktop profile"

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

Manifest commands are argv arrays. Catdot does not invoke `/bin/sh -c`, and
profile paths and placeholders are validated before use.

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
conflict replacement, stale users, privileged helper separation, and default
theme resources. It never modifies host packages.

The release PKGBUILD intentionally references a future tagged source archive.
A real tag and checksum are added only when the project is ready for release.
