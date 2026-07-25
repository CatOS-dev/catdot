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

    install -d /usr/share/catdot/profiles/{jq-test,bash-test}
    cat > /usr/share/catdot/profiles/jq-test/profile.toml <<"EOF"
schema = 2
[profile]
id = "jq-test"
name = "JQ transaction test"
description = "Exercises a real libalpm transaction"
source_root = "/usr/share/jq-test"
[defaults]
tool = "jq"
[[components]]
id = "jq"
role = "tool"
packages = ["jq"]
EOF
    cat > /usr/share/catdot/profiles/bash-test/profile.toml <<"EOF"
schema = 2
[profile]
id = "bash-test"
name = "Bash aggregation test"
description = "Exercises multi-user package aggregation"
source_root = "/usr/share/bash-test"
[defaults]
shell = "bash"
[[components]]
id = "bash"
role = "shell"
packages = ["bash"]
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
    pacman -Sy --noconfirm

    runuser -u alice -- /bin/bash -c "
      umask 077
      HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state catdot select jq-test
      HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state catdot resolve --yes
    "
    test "$(stat -c %a /var/lib/catdot)" = 700
    test "$(stat -c %U:%G /var/lib/catdot)" = root:root
    test "$(stat -c %a /var/lib/catdot/users)" = 700
    test "$(stat -c %U:%G /var/lib/catdot/users/1000.toml)" = root:root
    test "$(stat -c %a /var/lib/catdot/users/1000.toml)" = 600

    runuser -u bob -- env HOME=/home/bob XDG_STATE_HOME=/home/bob/.local/state \
      catdot select bash-test
    preview=$(runuser -u bob -- env HOME=/home/bob XDG_STATE_HOME=/home/bob/.local/state \
      catdot resolve --dry-run)
    printf "%s\n" "$preview" | grep -Fx "  jq"
    if runuser -u bob -- /usr/lib/catdot/catdot-helper resolve-plan \
      --uid 1001 --generation 1 --state-path /home/bob/.local/state/catdot/state.toml; then
      exit 1
    fi
    runuser -u bob -- env HOME=/home/bob XDG_STATE_HOME=/home/bob/.local/state \
      catdot resolve --yes
  '
