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

profile_root="$stage/usr/share/catdot/profiles"
profile_count=$(find "$profile_root" -mindepth 1 -maxdepth 1 -type d | wc -l)
test "$profile_count" -eq 1
test ! -e "$profile_root/catos-niri-default"
test ! -e "$profile_root/catos-sway-default"
test ! -e "$profile_root/catos-graphite"

HOME="$temporary/home" \
CATDOT_PROFILE_ROOT="$profile_root" \
  "$stage/usr/bin/catdot" list | grep -Fx 'catos-default — CatOS Default'
