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
    install -d /usr/share/catdot/profiles/jq-test /usr/share/jq-test
    cat > /usr/share/catdot/profiles/jq-test/profile.toml <<"P"
schema = 4
name = "JQ transaction test"
description = "Strict umask and privileged record test"
packages = ["jq"]
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

    runuser -u alice -- /bin/bash -c "umask 077; HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state catdot select jq-test --yes"
    test "$(stat -c %a /var/lib/catdot)" = 755
    test "$(stat -c %U:%G /var/lib/catdot)" = root:root
    test "$(stat -c %a /var/lib/catdot/users)" = 700
    test "$(stat -c %a /var/lib/catdot/users/1000.toml)" = 600
    test "$(stat -c %U:%G /var/lib/catdot/users/1000.toml)" = root:root
    ! runuser -u alice -- test -r /var/lib/catdot/users/1000.toml

    if runuser -u bob -- /usr/lib/catdot/catdot-query-helper resolve-plan \
      --uid 1001 --generation 1 --state-path /home/bob/.local/state/catdot/state.toml; then
      exit 1
    fi
  '
