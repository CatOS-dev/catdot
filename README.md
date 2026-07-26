# Catdot

Catdot switches complete CatOS user-configuration Profiles. A Profile declares
exact pacman package names and a set of HOME-relative paths that Catdot manages.
Everything else in the Profile content tree is a one-time seed.

Catdot deliberately does not understand desktop components, roles, XDG
providers, window-manager syntax, systemd units, hooks, reload actions, or
configuration merges.

## Profile layout

Metadata and content use the same Profile ID:

```text
/usr/share/catdot/profiles/catos-niri-dms/profile.toml
/usr/share/catos-niri-dms/.config/niri/config.kdl
/usr/share/catos-niri-dms/.config/ghostty/config
```

Every path below `/usr/share/<profile-id>` maps to the same path below HOME.
The manifest therefore needs no source or target declarations:

```toml
schema = 4
name = "CatOS Niri DMS"
description = "CatOS Niri desktop powered by DMS"

packages = [
  "niri",
  "dms-shell-niri",
  "ghostty",
]

manage = [
  ".config/niri/config.kdl",
  ".config/environment.d/80-catos-niri.conf",
]
```

Package entries are exact package names. Version expressions and dependency
syntax are rejected. Pacman resolves dependencies, providers, replacements,
and conflicts.

A path in `manage` may be a file or directory. Directory ownership is recursive,
and managed declarations may not overlap. At activation time Catdot also rejects
any path that overlaps its actual state directory, including a custom
`XDG_STATE_HOME`. Profile content may contain only real files and directories;
symbolic links, FIFOs, sockets, and device nodes are rejected.

## Managed and seed files

Before Catdot deletes or overwrites an existing HOME target, it copies the old
content into one operation backup:

```text
~/.local/state/catdot/backups/<operation>/home/...
```

The five most recent backup generations are retained.

- **Managed** content is covered by `manage`. Selecting a Profile always removes
  the previous active Profile's managed paths and overwrites the target
  Profile's managed paths from its accepted cache.
- **Seed** content is every other file in the Profile tree. On the first
  activation of that Profile, Catdot backs up and overwrites the corresponding
  HOME files. Later selections and updates do not touch seed content.
- Paths absent from the Profile tree are outside Catdot's ownership.

There is no activation journal or automatic rollback. If an operating-system
error interrupts copying, the backup remains available and the command can be
run again after the underlying problem is fixed.

## Explicit updates

Catdot records each retained Profile's accepted metadata and caches only its
managed content under the user state directory. Package upgrades alone do not
change the accepted configuration revision. `list` and `show` display the
accepted snapshot and mark a changed installed manifest as `update available`.

```sh
catdot update
catdot update catos-niri-dms
```

`update` refreshes the retained Profile's accepted metadata and managed cache
from `/usr/share/<profile-id>`. If it is active, its old managed paths are backed
up and replaced. Updating an inactive Profile does not switch to it or modify
HOME. Seed content is never cached or reapplied by update.

## Package installation and prune

Installation is delegated directly to pacman:

```text
sudo pacman -S --needed -- <Profile packages...>
```

Before installation, Catdot queries which direct package names are already
installed. Only direct packages missing before a successful install are added
to `introduced_packages`; dependency packages remain pacman's responsibility.

`remove PROFILE` forgets an inactive Profile and removes its cached content. It
does not uninstall packages. `prune` computes:

```text
introduced direct packages - packages referenced by retained Profiles
```

and delegates those candidates to:

```text
sudo pacman -Rns -- <candidates...>
```

Pacman displays and confirms the final removal transaction, calculates the
dependency closure, and rejects unsafe removals. Catdot removes package records
only after pacman succeeds. Packages that were already installed before Catdot
first requested them are never added to the prune set.

## Commands

```text
catdot list
catdot show PROFILE
catdot current
catdot select PROFILE
catdot update [PROFILE]
catdot remove PROFILE
catdot prune
catdot validate PROFILE_ROOT
```

A Profile switch may require logging out and back in. Catdot does not restart
applications, reload services, or execute Profile hooks.
