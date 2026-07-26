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
test -f "$stage/usr/share/licenses/catdot/LICENSE"
test -f "$stage/usr/share/doc/catdot/README.md"
test ! -e "$stage/usr/lib/catdot"

"$stage/usr/bin/catdot" --help | grep -F 'back up overwritten files'
