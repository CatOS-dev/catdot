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

    for profile in jq-test install-failure materialize-failure; do
      install -d "/usr/share/catdot/profiles/$profile" "/usr/share/$profile"
    done
    install -d /usr/share/jq-test/.config/jq-test /usr/share/materialize-failure/.config/materialize-failure
    printf jq-config > /usr/share/jq-test/.config/jq-test/config
    printf blocked > /usr/share/materialize-failure/.config/materialize-failure/config

    cat > /usr/share/catdot/profiles/jq-test/profile.toml <<"P"
schema = 4
name = "JQ transaction test"
description = "Exercises a real libalpm install and managed file activation"
packages = ["jq"]
manage = [".config/jq-test/config"]
P
    cat > /usr/share/catdot/profiles/install-failure/profile.toml <<"P"
schema = 4
name = "Package failure test"
description = "Keeps the previous profile active when package installation fails"
packages = ["catdot-package-that-does-not-exist"]
manage = []
P
    cat > /usr/share/catdot/profiles/materialize-failure/profile.toml <<"P"
schema = 4
name = "Materialization failure test"
description = "Keeps the previous managed files when HOME activation fails"
packages = ["tree"]
manage = [".config/materialize-failure/config"]
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
    envs="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"

    dry=$(runuser -u alice -- env $envs catdot select jq-test --dry-run)
    printf "%s\n" "$dry" | grep -F "Install packages:"
    printf "%s\n" "$dry" | grep -Fx "  jq"
    ! pacman -Q jq
    ! test -e /home/alice/.local/state/catdot/state.toml

    runuser -u alice -- env $envs catdot select jq-test --yes
    pacman -Q jq oniguruma
    test "$(cat /home/alice/.config/jq-test/config)" = jq-config
    grep -F "active_profile = \"jq-test\"" /home/alice/.local/state/catdot/state.toml
    test -f /var/lib/catdot/packages.toml
    test -f /var/lib/catdot/users/1000.toml

    if runuser -u alice -- env $envs catdot select install-failure --yes; then
      exit 1
    fi
    grep -F "active_profile = \"jq-test\"" /home/alice/.local/state/catdot/state.toml
    test "$(cat /home/alice/.config/jq-test/config)" = jq-config

    install -d -o alice -g alice /home/alice/.config
    chmod 500 /home/alice/.config
    if runuser -u alice -- env $envs catdot select materialize-failure --yes; then
      exit 1
    fi
    chmod 700 /home/alice/.config
    grep -F "active_profile = \"jq-test\"" /home/alice/.local/state/catdot/state.toml
    ! grep -F "materialize-failure" /home/alice/.local/state/catdot/state.toml
    ! grep -F "materialize-failure" /var/lib/catdot/users/1000.toml
    test "$(cat /home/alice/.config/jq-test/config)" = jq-config
    pacman -Q tree
  '
