#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"

podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euo pipefail -c '
    pacman -Sy --noconfirm
    useradd --create-home builder
    install -d -o builder -g builder /build /repo

    make_package() {
      directory=$1 pkgname=$2 pkgver=$3 depends=$4
      install -d -o builder -g builder "/build/$directory"
      cat > "/build/$directory/PKGBUILD" <<PKG
pkgname=$pkgname
pkgver=$pkgver
pkgrel=1
arch=(any)
license=(MIT)
depends=($depends)
package() { install -Dm644 /dev/null "\$pkgdir/usr/share/$pkgname/version-$pkgver"; }
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
    for profile in upgrade-test empty; do
      install -d "/usr/share/catdot/profiles/$profile" "/usr/share/$profile"
    done
    cat > /usr/share/catdot/profiles/upgrade-test/profile.toml <<"P"
schema = 4
name = "Explicit upgrade ownership test"
description = "Preserves a pre-existing explicit dependency after prune"
packages = ["catdot-upgrade-consumer"]
manage = []
P
    cat > /usr/share/catdot/profiles/empty/profile.toml <<"P"
schema = 4
name = "Empty"
description = "No packages"
packages = []
manage = []
P

    cat > /tmp/pkexec.c <<"C"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) { char uid[32]; if (argc < 2) return 64; snprintf(uid, sizeof uid, "%u", (unsigned)getuid()); setenv("PKEXEC_UID", uid, 1); execv(argv[1], argv + 1); return 71; }
C
    cc -O2 /tmp/pkexec.c -o /usr/bin/pkexec
    chown root:root /usr/bin/pkexec
    chmod 4755 /usr/bin/pkexec
    useradd --create-home alice
    envs="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"

    runuser -u alice -- env $envs catdot select upgrade-test --yes
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

    runuser -u alice -- env $envs catdot select empty --yes
    runuser -u alice -- env $envs catdot remove upgrade-test --yes
    runuser -u alice -- env $envs catdot prune --yes
    pacman -Q catdot-upgrade-base
    ! pacman -Q catdot-upgrade-consumer
    pacman -Qqe catdot-upgrade-base | grep -Fx catdot-upgrade-base
  '
