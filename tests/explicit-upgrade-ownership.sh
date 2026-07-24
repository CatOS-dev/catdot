#!/usr/bin/env bash
set -euxo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"

podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euxo pipefail -c '
    pacman -Sy --noconfirm
    useradd --create-home builder
    install -d -o builder -g builder /build /repo

    make_package() {
      directory=$1
      pkgname=$2
      pkgver=$3
      depends=$4
      install -d -o builder -g builder "/build/$directory"
      cat > "/build/$directory/PKGBUILD" <<PKG
pkgname=$pkgname
pkgver=$pkgver
pkgrel=1
arch=(any)
license=(MIT)
depends=($depends)
package() {
  install -Dm644 /dev/null "\$pkgdir/usr/share/$pkgname/version-$pkgver"
}
PKG
      chown builder:builder "/build/$directory/PKGBUILD"
      runuser -u builder -- bash -lc "cd /build/$directory && makepkg --noconfirm --nodeps"
    }

    make_package base-v1 catdot-upgrade-base 1 ""
    make_package base-v2 catdot-upgrade-base 2 ""
    make_package consumer catdot-upgrade-consumer 1 "catdot-upgrade-base\\>=2"
    cp /build/base-v2/*.pkg.tar.zst /build/consumer/*.pkg.tar.zst /repo/
    repo-add /repo/catdot-test.db.tar.gz /repo/*.pkg.tar.zst
    cat >> /etc/pacman.conf <<"PACMAN"
[catdot-test]
SigLevel = Optional TrustAll
Server = file:///repo
PACMAN
    pacman -Sy --noconfirm
    pacman -U --noconfirm /build/base-v1/*.pkg.tar.zst
    pacman -D --asexplicit catdot-upgrade-base

    install -Dm755 /catdot-bin/catdot /usr/bin/catdot
    install -Dm755 /catdot-bin/catdot-helper /usr/lib/catdot/catdot-helper
    install -Dm755 /catdot-bin/catdot-query-helper /usr/lib/catdot/catdot-query-helper
    install -d /usr/share/catdot/profiles/upgrade-test/component
    cat > /usr/share/catdot/profiles/upgrade-test/profile.toml <<"PROFILE"
schema = 1
[profile]
id = "upgrade-test"
name = "Explicit upgrade ownership test"
description = "Ensures pre-existing explicit packages remain user-owned"
[defaults]
tool = "consumer"
[components.consumer]
role = "tool"
path = "component"
packages = ["catdot-upgrade-consumer"]
PROFILE

    cat > /tmp/pkexec-shim.c <<"C"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {
  char uid[32];
  if (argc < 2) return 64;
  snprintf(uid, sizeof uid, "%u", (unsigned)getuid());
  if (setenv("PKEXEC_UID", uid, 1) != 0) return 70;
  execv(argv[1], argv + 1);
  return 71;
}
C
    cc -O2 -o /usr/bin/pkexec /tmp/pkexec-shim.c
    chown root:root /usr/bin/pkexec
    chmod 4755 /usr/bin/pkexec

    useradd --create-home alice
    user_env="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"
    runuser -u alice -- env $user_env catdot select upgrade-test
    runuser -u alice -- env $user_env catdot resolve --yes

    test "$(pacman -Q catdot-upgrade-base | awk "{print \$2}")" = 2-1
    pacman -Qqe catdot-upgrade-base | grep -Fx catdot-upgrade-base
    python3 - <<"PY"
import tomllib
with open("/var/lib/catdot/packages.toml", "rb") as file:
    state = tomllib.load(file)
base = state["packages"]["catdot-upgrade-base"]
assert base["catdot_installed"] is False
assert base["was_missing_before_catdot"] is False
assert base["install_reason"] == "Explicit"
PY

    runuser -u alice -- env $user_env catdot disable tool
    runuser -u alice -- env $user_env catdot resolve --yes
    runuser -u alice -- env $user_env catdot prune --yes
    pacman -Q catdot-upgrade-base
    ! pacman -Q catdot-upgrade-consumer
    pacman -Qqe catdot-upgrade-base | grep -Fx catdot-upgrade-base
  '
