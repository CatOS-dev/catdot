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

    for profile in jq-alice jq-bob empty; do
      install -d "/usr/share/catdot/profiles/$profile" "/usr/share/$profile"
    done
    for profile in jq-alice jq-bob; do
      cat > "/usr/share/catdot/profiles/$profile/profile.toml" <<P
schema = 4
name = "$profile"
description = "Multi-user package ownership"
packages = ["jq"]
manage = []
P
    done
    cat > /usr/share/catdot/profiles/empty/profile.toml <<"P"
schema = 4
name = "Empty"
description = "Profile used before removing a retained profile"
packages = []
manage = []
P

    cat > /tmp/pkexec.c <<"C"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {
  char uid[32];
  if (argc < 2) return 64;
  snprintf(uid, sizeof uid, "%u", (unsigned)getuid());
  setenv("PKEXEC_UID", uid, 1);
  execv(argv[1], argv + 1);
  return 71;
}
C
    cc -O2 /tmp/pkexec.c -o /usr/bin/pkexec
    chown root:root /usr/bin/pkexec
    chmod 4755 /usr/bin/pkexec
    useradd --create-home alice
    useradd --create-home bob
    alice="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"
    bob="HOME=/home/bob XDG_STATE_HOME=/home/bob/.local/state"

    runuser -u alice -- env $alice catdot select jq-alice --yes
    runuser -u bob -- env $bob catdot select jq-bob --yes
    pacman -Q jq oniguruma

    runuser -u alice -- env $alice catdot select empty --yes
    runuser -u alice -- env $alice catdot remove jq-alice --yes
    ! grep -F "profile = \"jq-alice\"" /var/lib/catdot/packages.toml
    grep -F "profile = \"jq-bob\"" /var/lib/catdot/packages.toml
    alice_plan=$(runuser -u alice -- env $alice catdot prune --dry-run)
    ! printf "%s\n" "$alice_plan" | grep -E "^  (jq|oniguruma)$"
    pacman -Q jq oniguruma

    runuser -u bob -- env $bob catdot select empty --yes
    runuser -u bob -- env $bob catdot remove jq-bob --yes
    bob_plan=$(runuser -u bob -- env $bob catdot prune --dry-run)
    printf "%s\n" "$bob_plan" | grep -E "^  (jq|oniguruma)$"
    runuser -u bob -- env $bob catdot prune --yes
    ! pacman -Q jq
    ! pacman -Q oniguruma
  '
