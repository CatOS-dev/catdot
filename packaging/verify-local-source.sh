#!/bin/sh
set -eu

repository=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d)
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT HUP INT TERM

mkdir -p "$temporary/catdot-0.1.0"
tar -C "$repository" \
  --exclude=.git \
  --exclude=target \
  --exclude='packaging/*.pkg.tar.*' \
  -cf - . | tar -C "$temporary/catdot-0.1.0" -xf -

cd "$temporary/catdot-0.1.0"
cargo build --release --locked
test -x target/release/catdot
test -x target/release/catdot-helper
test -f LICENSE
test -f README.md
