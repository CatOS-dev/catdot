#!/usr/bin/env bash
set -euxo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"

podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euxo pipefail -c '
    pacman -Sy --noconfirm tree
    pacman -D --asdeps tree
    install -Dm755 /catdot-bin/catdot /usr/bin/catdot
    install -Dm755 /catdot-bin/catdot-helper /usr/lib/catdot/catdot-helper
    install -d /usr/share/catdot/profiles

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

    install -d -m700 /var/lib/catdot/users /var/lib/catdot/transactions
    cat > /var/lib/catdot/packages.toml <<"STATE"
[packages.tree]
name = "tree"
catdot_installed = true
was_missing_before_catdot = true
install_reason = "Dependency"
references = []
STATE
    chmod 600 /var/lib/catdot/packages.toml

    pacman -R --noconfirm tree
    cat > /var/lib/catdot/transactions/prune-crash.toml <<"JOURNAL"
kind = "prune"
id = "crash"
plan_digest = "simulated"
removed_packages = ["tree"]
stage = "Prepared"

[expected_packages.packages]
JOURNAL
    chmod 600 /var/lib/catdot/transactions/prune-crash.toml

    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot prune --yes
    test ! -e /var/lib/catdot/transactions/prune-crash.toml
    python3 - <<"PY"
import tomllib
with open("/var/lib/catdot/packages.toml", "rb") as file:
    state = tomllib.load(file)
assert state.get("packages", {}) == {}
PY
  '
