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
install -Dm755 "$source_root/packaging/mark-generation" \
  "$destdir/usr/lib/catdot/mark-generation"
install -Dm644 "$source_root/packaging/catdot-profile-generation.hook" \
  "$destdir/usr/share/libalpm/hooks/catdot-profile-generation.hook"
install -Dm644 "$source_root/packaging/catdot-update.service" \
  "$destdir/usr/lib/systemd/user/catdot-update.service"
install -Dm644 "$source_root/packaging/catdot-update.path" \
  "$destdir/usr/lib/systemd/user/catdot-update.path"
install -d "$destdir/usr/lib/systemd/user/default.target.wants"
ln -sf ../catdot-update.service \
  "$destdir/usr/lib/systemd/user/default.target.wants/catdot-update.service"
ln -sf ../catdot-update.path \
  "$destdir/usr/lib/systemd/user/default.target.wants/catdot-update.path"
install -Dm644 "$source_root/packaging/org.catos.catdot.policy" \
  "$destdir/usr/share/polkit-1/actions/org.catos.catdot.policy"
install -Dm644 "$source_root/LICENSE" \
  "$destdir/usr/share/licenses/catdot/LICENSE"
install -Dm644 "$source_root/README.md" \
  "$destdir/usr/share/doc/catdot/README.md"
install -d "$destdir/usr/share/catdot/profiles"
cp -a "$source_root/profiles/catos-default" \
  "$destdir/usr/share/catdot/profiles/"
