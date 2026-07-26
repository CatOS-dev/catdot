# Catdot

Catdot is CatOS's complete user-configuration Profile switcher. It installs the
packages declared by a Profile, initializes user-owned defaults once, and
transactionally switches the files that the Profile explicitly manages.

Catdot does not understand desktop components, roles, window-manager syntax,
XDG providers, or systemd units. Those are ordinary files supplied by a
Profile. Catdot does not run hooks, reload applications, merge user edits, or
update a Profile automatically after its package changes.

## Profile layout

Profile metadata and content are installed separately but use the same stable
Profile ID:

```text
/usr/share/catdot/profiles/catos-niri-dms/profile.toml
/usr/share/catos-niri-dms/.config/niri/config.kdl
/usr/share/catos-niri-dms/.config/ghostty/config
```

The metadata directory name is the Profile ID. Every path below
`/usr/share/<profile-id>` maps directly to the same path below the user's HOME.
The manifest therefore contains no source or target paths:

```toml
schema = 4
name = "CatOS Niri DMS"
description = "CatOS Niri desktop powered by DMS"

packages = [
  "niri",
  "dms-shell-niri>=1.5.1",
  "ghostty",
]

manage = [
  ".config/niri/config.kdl",
  ".config/environment.d/80-catos-niri.conf",
]
```

A path in `manage` may be a file or a directory. Directories are managed
recursively. Managed declarations may not overlap each other.

## File ownership

Catdot derives two behaviors from the Profile content tree:

- **Managed**: content covered by `manage`. Catdot backs up an existing target,
  overwrites it during activation, removes obsolete managed targets during a
  switch, and restores the previous state if activation fails.
- **Seed**: every other file in the Profile content tree. Catdot processes it
  once per Profile. A missing target is copied; an existing target is preserved.
  The result is then user-owned and is neither removed nor updated by switching.

Paths absent from the Profile content tree are outside Catdot's ownership.
Profile content may contain only real files and directories; symbolic links,
FIFOs, sockets, and device nodes are rejected.

## Explicit updates

Package upgrades do not modify an active or retained Profile. Catdot keeps a
per-user snapshot of each Profile's managed revision. Switching away and back
uses that snapshot.

```sh
catdot update
catdot update catos-niri-dms
```

`update` is the only normal command that refreshes a retained Profile's package
declaration and managed snapshot from `/usr/share/<profile-id>`. Updating the
active Profile also backs up and overwrites its managed HOME targets. Updating
an inactive Profile refreshes only its cached revision and does not switch to
it. Neither form changes seed files.

`reset PROFILE` is stronger: it backs up and reinstalls both managed and seed
content from the currently installed Profile package.

## Package ownership

All retained Profiles keep package references, even when inactive. Catdot
records which packages it introduced and which were already installed. It never
removes a package that predates Catdot.

Switching does not remove old packages. `remove PROFILE` forgets an inactive
Profile and releases its references. Actual package removal is a separate,
explicit operation:

```sh
catdot prune --dry-run
catdot prune
```

Prune also respects references held by other system users and dependencies from
packages outside Catdot.

## Commands

```text
catdot list
catdot show PROFILE
catdot current
catdot select PROFILE
catdot update [PROFILE]
catdot reset PROFILE
catdot remove PROFILE
catdot prune
catdot doctor
catdot recover list|accept|discard
catdot validate PROFILE_ROOT
```

`select`, `update`, `reset`, `remove`, and `prune` display their plan and require
confirmation. `--dry-run` is read-only; `--yes` accepts a displayed plan in
non-interactive use.

A Profile switch may require logging out and back in. Catdot deliberately does
not call `systemctl --user`, restart the desktop, or execute Profile hooks.

## Reliability boundary

Catdot retains the safety mechanisms needed by a file switcher:

- HOME-relative path validation and symbolic-link-parent rejection;
- package and file plan confirmation;
- binary-safe backups preserving modes, directories, and existing symlinks;
- journaled file activation and interrupted-operation recovery;
- bounded backup retention;
- multi-user package reference tracking and conservative prune.

It intentionally provides no component mixing, automatic configuration update,
three-way merge, generated desktop integration, or command-provider runtime.
