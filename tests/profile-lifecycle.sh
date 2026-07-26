#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"

podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euo pipefail -c '
    pacman -Sy --noconfirm sudo
    install -Dm755 /catdot-bin/catdot /usr/bin/catdot

    for profile in package-test empty; do
      install -d "/usr/share/catdot/profiles/$profile" "/usr/share/$profile"
    done
    install -d /usr/share/package-test/.config/demo
    printf managed-v1 > /usr/share/package-test/.config/demo/managed
    printf seed-v1 > /usr/share/package-test/.config/demo/seed
    cat > /usr/share/catdot/profiles/package-test/profile.toml <<"P"
schema = 4
name = "Package and file lifecycle"
description = "Exercises real pacman delegation, backup, update, and prune"
packages = ["jq"]
manage = [".config/demo/managed"]
P
    cat > /usr/share/catdot/profiles/empty/profile.toml <<"P"
schema = 4
name = "Empty"
description = "No files or packages"
packages = []
manage = []
P

    useradd --create-home alice
    printf "alice ALL=(root) NOPASSWD: /usr/bin/pacman\n" > /etc/sudoers.d/catdot-test
    chmod 440 /etc/sudoers.d/catdot-test
    install -d -o alice -g alice /home/alice/.config/demo
    printf managed-old > /home/alice/.config/demo/managed
    printf seed-old > /home/alice/.config/demo/seed
    chown -R alice:alice /home/alice/.config
    envs="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"

    printf "y\n" | runuser -u alice -- env $envs catdot select package-test
    pacman -Q jq
    test "$(cat /home/alice/.config/demo/managed)" = managed-v1
    test "$(cat /home/alice/.config/demo/seed)" = seed-v1
    backup=$(find /home/alice/.local/state/catdot/backups -mindepth 1 -maxdepth 1 -type d | head -n1)
    test "$(cat "$backup/home/.config/demo/managed")" = managed-old
    test "$(cat "$backup/home/.config/demo/seed")" = seed-old

    printf managed-user > /home/alice/.config/demo/managed
    printf seed-user > /home/alice/.config/demo/seed
    printf "y\n" | runuser -u alice -- env $envs catdot select package-test
    test "$(cat /home/alice/.config/demo/managed)" = managed-v1
    test "$(cat /home/alice/.config/demo/seed)" = seed-user

    printf managed-v2 > /usr/share/package-test/.config/demo/managed
    printf "y\n" | runuser -u alice -- env $envs catdot update package-test
    test "$(cat /home/alice/.config/demo/managed)" = managed-v2
    test "$(cat /home/alice/.config/demo/seed)" = seed-user

    runuser -u alice -- env $envs catdot select empty
    test ! -e /home/alice/.config/demo/managed
    test "$(cat /home/alice/.config/demo/seed)" = seed-user
    runuser -u alice -- env $envs catdot remove package-test
    printf "y\n" | runuser -u alice -- env $envs catdot prune
    ! pacman -Q jq
    ! grep -F jq /home/alice/.local/state/catdot/state.toml
  '
