#!/bin/sh
set -eu

source_root=$1
destdir=$2

install -Dm755 "$source_root/target/release/catdot" \
  "$destdir/usr/bin/catdot"
install -Dm755 "$source_root/target/release/catdot-helper" \
  "$destdir/usr/lib/catdot/catdot-helper"
install -Dm755 "$source_root/target/release/catdot-query-helper" \
  "$destdir/usr/lib/catdot/catdot-query-helper"
install -Dm644 "$source_root/packaging/org.catos.catdot.policy" \
  "$destdir/usr/share/polkit-1/actions/org.catos.catdot.policy"
install -Dm644 "$source_root/LICENSE" \
  "$destdir/usr/share/licenses/catdot/LICENSE"
install -Dm644 "$source_root/README.md" \
  "$destdir/usr/share/doc/catdot/README.md"
install -d "$destdir/usr/share/catdot/profiles"
cp -a "$source_root/profiles/catos-default" \
  "$destdir/usr/share/catdot/profiles/"
