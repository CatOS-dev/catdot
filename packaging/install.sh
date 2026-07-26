#!/bin/sh
set -eu

source_root=$1
destdir=$2

install -Dm755 "$source_root/target/release/catdot" \
  "$destdir/usr/bin/catdot"
install -Dm644 "$source_root/LICENSE" \
  "$destdir/usr/share/licenses/catdot/LICENSE"
install -Dm644 "$source_root/README.md" \
  "$destdir/usr/share/doc/catdot/README.md"
