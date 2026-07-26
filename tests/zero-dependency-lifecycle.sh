#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"

podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euo pipefail -c '
    pacman -Sy --noconfirm
    install -Dm755 /catdot-bin/catdot /usr/bin/catdot
    install -Dm755 /catdot-bin/catdot-helper /usr/lib/catdot/catdot-helper
    install -Dm755 /catdot-bin/catdot-query-helper /usr/lib/catdot/catdot-query-helper

    for profile in alpha beta; do
      install -d "/usr/share/catdot/profiles/$profile" "/usr/share/$profile/.config/demo"
      cat > "/usr/share/catdot/profiles/$profile/profile.toml" <<PROFILE
schema = 4
name = "$profile"
description = "Complete profile lifecycle test"
packages = []
manage = [".config/demo/managed"]
PROFILE
    done
    printf alpha-v1 > /usr/share/alpha/.config/demo/managed
    printf alpha-seed > /usr/share/alpha/.config/demo/seed
    printf beta-v1 > /usr/share/beta/.config/demo/managed
    printf beta-seed > /usr/share/beta/.config/demo/seed

    cat > /tmp/pkexec.c <<"C"
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
    cc -O2 /tmp/pkexec.c -o /usr/bin/pkexec
    chown root:root /usr/bin/pkexec
    chmod 4755 /usr/bin/pkexec

    useradd --create-home alice
    envs="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"
    install -d -o alice -g alice /home/alice/.config/demo
    printf user-managed > /home/alice/.config/demo/managed
    printf user-seed > /home/alice/.config/demo/seed
    chown -R alice:alice /home/alice/.config

    runuser -u alice -- env $envs catdot validate /usr/share/catdot/profiles
    first=$(runuser -u alice -- env $envs catdot select alpha --yes)
    printf "%s\n" "$first" | grep -F "BACKUP AND OVERWRITE managed"
    test "$(cat /home/alice/.config/demo/managed)" = alpha-v1
    test "$(cat /home/alice/.config/demo/seed)" = user-seed

    current=$(runuser -u alice -- env $envs catdot current)
    printf "%s\n" "$current" | grep -Fx "Active profile: alpha"
    printf "%s\n" "$current" | grep -Fx "Retained profiles: alpha"

    printf alpha-v2 > /usr/share/alpha/.config/demo/managed
    runuser -u alice -- env $envs catdot select beta --yes
    test "$(cat /home/alice/.config/demo/managed)" = beta-v1
    test "$(cat /home/alice/.config/demo/seed)" = user-seed

    runuser -u alice -- env $envs catdot select alpha --yes
    test "$(cat /home/alice/.config/demo/managed)" = alpha-v1
    runuser -u alice -- env $envs catdot update --yes
    test "$(cat /home/alice/.config/demo/managed)" = alpha-v2
    test "$(cat /home/alice/.config/demo/seed)" = user-seed

    runuser -u alice -- env $envs catdot reset alpha --yes
    test "$(cat /home/alice/.config/demo/seed)" = alpha-seed

    runuser -u alice -- env $envs catdot remove beta --yes
    ! grep -F "beta" /home/alice/.local/state/catdot/state.toml
    runuser -u alice -- env $envs catdot prune --yes | grep -F "No packages are eligible"

    for command in resolve disable exec apply; do
      ! runuser -u alice -- env $envs catdot "$command"
    done
  '
