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
  --exclude=.omo \
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
test -f "$stage/usr/share/polkit-1/actions/org.catos.catdot.policy"
test -f "$stage/usr/share/licenses/catdot/LICENSE"
test -f "$stage/usr/share/doc/catdot/README.md"

test ! -e "$stage/usr/lib/catdot/mark-generation"
test ! -e "$stage/usr/share/libalpm/hooks/catdot-profile-generation.hook"
test ! -e "$stage/usr/lib/systemd/user/catdot-update.service"
test ! -e "$stage/usr/lib/systemd/user/catdot-update.path"
test ! -e "$stage/usr/share/catdot/profiles/catos-default"
test ! -e "$stage/etc/skel/.config/catdot/default.toml"

"$stage/usr/bin/catdot" --help | grep -F 'transactionally switch complete managed configuration profiles'
! "$stage/usr/bin/catdot" resolve >/dev/null 2>&1
! "$stage/usr/bin/catdot" exec terminal >/dev/null 2>&1
