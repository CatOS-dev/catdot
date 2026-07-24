#!/usr/bin/env bash
set -euxo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"

podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euxo pipefail -c '
    pacman -Sy --noconfirm
    install -Dm755 /catdot-bin/catdot /usr/bin/catdot
    install -Dm755 /catdot-bin/catdot-helper /usr/lib/catdot/catdot-helper

    for profile in jq-alice jq-bob; do
      install -d "/usr/share/catdot/profiles/$profile/component"
      cat > "/usr/share/catdot/profiles/$profile/profile.toml" <<EOF
schema = 1
[profile]
id = "$profile"
name = "Multi-user prune $profile"
description = "Exercises real shared package ownership"
[defaults]
tool = "jq"
[components.jq]
role = "tool"
path = "component"
packages = ["jq"]
exec = ["/usr/bin/jq", "--version"]
EOF
    done

    cat > /tmp/pkexec-shim.c <<"EOF"
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
EOF
    cc -O2 -o /usr/bin/pkexec /tmp/pkexec-shim.c
    chown root:root /usr/bin/pkexec
    chmod 4755 /usr/bin/pkexec

    useradd --create-home alice
    useradd --create-home bob
    alice="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"
    bob="HOME=/home/bob XDG_STATE_HOME=/home/bob/.local/state"

    runuser -u alice -- env $alice catdot select jq-alice
    runuser -u alice -- env $alice catdot resolve --yes
    pacman -Q jq oniguruma
    runuser -u bob -- env $bob catdot select jq-bob
    runuser -u bob -- env $bob catdot resolve --yes

    runuser -u alice -- env $alice catdot disable tool
    runuser -u alice -- env $alice catdot resolve --yes
    ! grep -F "jq-alice/jq" /var/lib/catdot/users/1000.toml
    ! grep -F "uid = 1000" /var/lib/catdot/packages.toml
    grep -F "uid = 1001" /var/lib/catdot/packages.toml
    runuser -u alice -- env $alice catdot prune --dry-run > /tmp/alice-prune.toml
    ! grep -E "^(jq|oniguruma)$" /tmp/alice-prune.toml
    pacman -Q jq oniguruma

    runuser -u bob -- env $bob catdot disable tool
    runuser -u bob -- env $bob catdot resolve --yes
    runuser -u bob -- env $bob catdot prune --dry-run > /tmp/bob-prune.toml
    grep -E "^  (jq|oniguruma)$" /tmp/bob-prune.toml
    runuser -u bob -- env $bob catdot prune --yes
    ! pacman -Q jq
    ! pacman -Q oniguruma
  '
