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
    install -d /usr/share/catdot/profiles/bash-test/component
    cat > /usr/share/catdot/profiles/bash-test/profile.toml <<"EOF"
schema = 1
[profile]
id = "bash-test"
name = "Finalize retry test"
description = "Exercises finalize recovery"
[defaults]
shell = "bash"
[components.bash]
role = "shell"
path = "component"
packages = ["bash"]
EOF

    cat > /tmp/pkexec-shim.c <<"EOF"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  char uid[32];
  if (argc < 2) return 64;
  if (argc > 2 && strcmp(argv[2], "finalize") == 0 && access("/tmp/finalize-failed", F_OK) != 0) {
    FILE *marker = fopen("/tmp/finalize-failed", "w");
    if (marker) fclose(marker);
    return 1;
  }
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
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot select bash-test
    if runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --yes; then
      exit 1
    fi
    grep -F "bash-test/bash" /var/lib/catdot/users/1000.toml
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --yes
    grep -A4 "active_requirements" /var/lib/catdot/users/1000.toml | grep -F "bash-test/bash"
    if grep -A4 "pending_requirements" /var/lib/catdot/users/1000.toml | grep -F "bash-test/bash"; then
      exit 1
    fi
    grep -A8 "name = \"bash\"" /var/lib/catdot/packages.toml | grep -F "component = \"bash-test/bash\""
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot prune --dry-run > /tmp/active-prune.toml
    ! grep -F "bash" /tmp/active-prune.toml
  '
