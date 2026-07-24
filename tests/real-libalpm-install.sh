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
    install -d /usr/share/catdot/profiles/jq-test
    install -d /usr/share/catdot/profiles/jq-test/jq
    cat > /usr/share/catdot/profiles/jq-test/profile.toml <<"EOF"
schema = 1

[profile]
id = "jq-test"
name = "JQ transaction test"
description = "Exercises a real libalpm transaction"

[defaults]
tool = "jq"

[components.jq]
role = "tool"
path = "jq"
packages = ["jq"]
exec = ["/usr/bin/jq", "--version"]
EOF

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
    pacman -Sy --noconfirm
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot select jq-test
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --yes

    pacman -Q jq oniguruma
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot exec tool | grep -E "^jq-"
    test -f /var/lib/catdot/packages.toml
    test -f /var/lib/catdot/users/1000.toml
  '
