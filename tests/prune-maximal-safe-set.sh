#!/usr/bin/env bash
set -euxo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"
podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euxo pipefail -c '
    pacman -Sy --noconfirm jq wget tree
    install -Dm755 /catdot-bin/catdot /usr/bin/catdot
    install -Dm755 /catdot-bin/catdot-helper /usr/lib/catdot/catdot-helper
    install -d /usr/share/catdot/profiles/empty/component
    cat >/usr/share/catdot/profiles/empty/profile.toml <<"P"
schema = 1
[profile]
id = "empty"
name = "Empty"
description = "Prune test profile"
[defaults]
P
    cat >/tmp/pkexec.c <<"C"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc,char **argv){char uid[32];if(argc<2)return 64;snprintf(uid,sizeof uid,"%u",(unsigned)getuid());setenv("PKEXEC_UID",uid,1);execv(argv[1],argv+1);return 71;}
C
    cc -O2 /tmp/pkexec.c -o /usr/bin/pkexec
    chown root:root /usr/bin/pkexec
    chmod 4755 /usr/bin/pkexec
    useradd -m alice
    pacman -D --asdeps tree libpsl oniguruma pacman
    install -d -m700 /var/lib/catdot/users /var/lib/catdot/transactions
    : >/var/lib/catdot/packages.toml
    for package in libpsl oniguruma pacman tree; do
      cat >>/var/lib/catdot/packages.toml <<EOT
[packages.$package]
name = "$package"
catdot_installed = true
was_missing_before_catdot = true
install_reason = "Dependency"
references = []
EOT
    done
    chmod 600 /var/lib/catdot/packages.toml
    plan=$(runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state catdot prune --dry-run)
    printf "%s\n" "$plan"
    printf "%s\n" "$plan" | grep -Fx "  tree"
    ! printf "%s\n" "$plan" | grep -Fx "  libpsl"
    ! printf "%s\n" "$plan" | grep -Fx "  oniguruma"
    ! printf "%s\n" "$plan" | grep -Fx "  pacman"
  '
