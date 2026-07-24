#!/usr/bin/env bash
set -euxo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"
podman run --rm --security-opt label=disable \
  -v "$repo_root/target/release:/catdot-bin:ro" archlinux:base-devel \
  /bin/bash -euxo pipefail -c '
    install -Dm755 /catdot-bin/catdot /usr/bin/catdot
    install -Dm755 /catdot-bin/catdot-helper /usr/lib/catdot/catdot-helper
    install -d /usr/share/catdot/profiles/demo/component
    cat >/usr/share/catdot/profiles/demo/profile.toml <<"P"
schema = 1
[profile]
id = "demo"
name = "Doctor test"
description = "Exercises privileged system diagnostics"
[defaults]
tool = "tool"
[components.tool]
role = "tool"
path = "component"
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
    envs="HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state"
    runuser -u alice -- env $envs catdot select demo
    runuser -u alice -- env $envs catdot resolve --yes
    output=$(runuser -u alice -- env $envs catdot doctor)
    printf "%s\n" "$output"
    printf "%s\n" "$output" | grep -F "system user record: uid 1000: valid"
  '
