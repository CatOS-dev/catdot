#!/bin/sh
set -eu

repository=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d)
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT HUP INT TERM

source_tree="$temporary/catdot-0.1.0"
stage="$temporary/pkg"
mkdir -p "$source_tree"
tar -C "$repository" \
  --exclude=.git \
  --exclude=target \
  --exclude='packaging/pkg' \
  --exclude='packaging/src' \
  --exclude='packaging/*.pkg.tar.*' \
  -cf - . | tar -C "$source_tree" -xf -

cd "$source_tree"
cargo build --release --locked
sh packaging/install.sh "$source_tree" "$stage"

test -x "$stage/usr/bin/catdot"
test -x "$stage/usr/lib/catdot/catdot-helper"
test -x "$stage/usr/lib/catdot/catdot-query-helper"
test -x "$stage/usr/lib/catdot/mark-generation"
test -f "$stage/usr/share/libalpm/hooks/catdot-profile-generation.hook"
test -f "$stage/usr/lib/systemd/user/catdot-update.service"
test -f "$stage/usr/lib/systemd/user/catdot-update.path"
test -f "$stage/usr/share/licenses/catdot/LICENSE"
test -f "$stage/usr/share/doc/catdot/README.md"

test -f "$stage/usr/share/catdot/profiles/catos-default/profile.toml"
test -f "$stage/etc/skel/.config/catdot/default.toml"
test ! -e "$stage/etc/skel/.gtkrc-2.0"
test ! -e "$stage/etc/skel/.config/gtk-3.0"
test ! -e "$stage/etc/skel/.config/gtk-4.0"
test ! -e "$stage/usr/share/catos-default"

profile_root="$stage/usr/share/catdot/profiles"
profile_count=$(find "$profile_root" -mindepth 1 -maxdepth 1 -type d | wc -l)
test "$profile_count" -eq 1

home="$temporary/home"
mkdir -p "$home"
HOME="$home" \
XDG_STATE_HOME="$home/.local/state" \
CATDOT_PROFILE_ROOT="$profile_root" \
CATDOT_DEFAULT_DECLARATION="$stage/etc/skel/.config/catdot/default.toml" \
  "$stage/usr/bin/catdot" list | grep -Fx 'catos-default — CatOS Default'

current=$(HOME="$home" \
  XDG_STATE_HOME="$home/.local/state" \
  CATDOT_PROFILE_ROOT="$profile_root" \
  CATDOT_DEFAULT_DECLARATION="$stage/etc/skel/.config/catdot/default.toml" \
  "$stage/usr/bin/catdot" current)
printf '%s\n' "$current" | grep -Fx 'Pending selection:'
printf '%s\n' "$current" | grep -Fx '  gtk-theme: activate catos-default/gtk'
! printf '%s\n' "$current" | grep -F 'qt-theme'
