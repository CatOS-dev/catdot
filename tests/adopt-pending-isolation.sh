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
    for profile in provider-a provider-b; do
      install -d "/usr/share/catdot/profiles/$profile/component"
      printf "%s\\n" "$profile" > "/usr/share/catdot/profiles/$profile/component/config"
    done
    cat > /usr/share/catdot/profiles/provider-a/profile.toml <<"EOF"
schema = 1
[profile]
id = "provider-a"
name = "Active provider"
description = "Active component for adopt"
[defaults]
tool = "tool"
[components.tool]
role = "tool"
path = "component"
exec = ["/usr/bin/printf", "provider-a\\n"]
[[components.tool.links]]
source = "config"
target = "{xdg_config_home}/adopt/config"
EOF
    cat > /usr/share/catdot/profiles/provider-b/profile.toml <<"EOF"
schema = 1
[profile]
id = "provider-b"
name = "Pending provider"
description = "Pending component for adopt"
[defaults]
tool = "tool"
[components.tool]
role = "tool"
path = "component"
exec = ["/usr/bin/printf", "provider-b\\n"]
[[components.tool.links]]
source = "config"
target = "{xdg_config_home}/adopt/config"
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
    alice="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state XDG_CONFIG_HOME=/home/alice/.config"
    runuser -u alice -- env $alice catdot select provider-a
    runuser -u alice -- env $alice catdot resolve --yes
    runuser -u alice -- env $alice catdot select provider-b
    runuser -u alice -- env $alice catdot exec tool | grep -Fx provider-a

    rm /home/alice/.local/state/catdot/managed-links.toml
    rm /home/alice/.config/adopt/config
    printf "unmanaged\\n" > /home/alice/.config/adopt/config
    chown -R alice:alice /home/alice/.config
    runuser -u alice -- env $alice catdot adopt tool
    test "$(readlink /home/alice/.config/adopt/config)" = \
      /usr/share/catdot/profiles/provider-a/component/config
    runuser -u alice -- env $alice catdot exec tool | grep -Fx provider-a
    grep -F "tool = \"provider-b/tool\"" /home/alice/.local/state/catdot/state.toml
    grep -F "tool = \"provider-a/tool\"" /home/alice/.local/state/catdot/state.toml
  '
