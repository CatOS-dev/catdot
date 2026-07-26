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
    install -d /usr/share/catdot/profiles/demo /usr/share/demo
    cat > /usr/share/catdot/profiles/demo/profile.toml <<"P"
schema = 4
name = "Doctor test"
description = "Exercises privileged system diagnostics"
packages = []
manage = []
P
    cat > /tmp/pkexec.c <<"C"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc,char **argv){char uid[32];if(argc<2)return 64;snprintf(uid,sizeof uid,"%u",(unsigned)getuid());setenv("PKEXEC_UID",uid,1);execv(argv[1],argv+1);return 71;}
C
    cc -O2 /tmp/pkexec.c -o /usr/bin/pkexec
    chown root:root /usr/bin/pkexec
    chmod 4755 /usr/bin/pkexec
    useradd -m alice
    envs="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"
    runuser -u alice -- env $envs catdot select demo --yes
    output=$(runuser -u alice -- env $envs catdot doctor)
    printf "%s\n" "$output" | grep -Fx "user state: valid"
    printf "%s\n" "$output" | grep -F "system user record: uid 1000: valid"
  '
