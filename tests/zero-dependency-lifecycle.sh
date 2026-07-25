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
    install -Dm755 /catdot-bin/catdot-query-helper /usr/lib/catdot/catdot-query-helper
    install -d /usr/share/catdot/profiles/lifecycle
    cat > /usr/share/catdot/profiles/lifecycle/profile.toml <<"EOF"
schema = 2
[profile]
id = "lifecycle"
name = "Zero dependency lifecycle"
description = "Exercises activation without package transactions"
source_root = "/usr/share/lifecycle"
[defaults]
tool = "one"
[[components]]
id = "one"
role = "tool"
[components.exec]
argv = ["/usr/bin/printf", "one\\n"]
[[components]]
id = "two"
role = "tool"
[components.exec]
argv = ["/usr/bin/printf", "two\\n"]
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
    useradd --create-home bob
    alice="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state XDG_CONFIG_HOME=/home/alice/.config"
    bob="HOME=/home/bob XDG_STATE_HOME=/home/bob/.local/state XDG_CONFIG_HOME=/home/bob/.config"

    select_output=$(runuser -u alice -- env $alice catdot select lifecycle)
    printf "%s\n" "$select_output"
    test "$(printf "%s\n" "$select_output" | grep -c "catdot resolve")" -eq 1
    resolve_output=$(runuser -u alice -- env $alice catdot resolve --yes)
    printf "%s\n" "$resolve_output"
    printf "%s\n" "$resolve_output" | grep -F "No package changes are required."
    printf "%s\n" "$resolve_output" | grep -F "Activate:"
    printf "%s\n" "$resolve_output" | grep -F "Profile changes applied successfully."
    runuser -u alice -- env $alice catdot resolve --yes | grep -F "Catdot is already up to date."
    runuser -u alice -- env $alice catdot exec tool | grep -Fx one
    runuser -u alice -- env $alice catdot select tool lifecycle/two
    runuser -u alice -- env $alice catdot resolve --yes
    runuser -u alice -- env $alice catdot exec tool | grep -Fx two
    runuser -u alice -- env $alice catdot disable tool
    runuser -u alice -- env $alice catdot resolve --yes
    ! runuser -u alice -- env $alice catdot exec tool
    runuser -u alice -- env $alice catdot doctor
    runuser -u alice -- env $alice catdot prune --yes | grep -F "Nothing to prune."
    runuser -u alice -- env $alice catdot users list | grep -F "uid 1000: valid"

    userdel alice
    runuser -u bob -- env $bob catdot users list | grep -F "uid 1000: missing"
    runuser -u bob -- env $bob catdot users prune --yes | grep -F "removing stale record for uid 1000"
    ! test -e /var/lib/catdot/users/1000.toml
  '
