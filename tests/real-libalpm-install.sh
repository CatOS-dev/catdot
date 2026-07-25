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
    install -d /usr/share/catdot/profiles/{jq-test,source-check,install-failure,materialize-failure,state-write-failure} /usr/share/{materialize-failure,state-write-failure}
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
[components.exec]
argv = ["/usr/bin/jq", "--version"]
EOF
    printf state > /usr/share/state-write-failure/config
    cat > /usr/share/catdot/profiles/state-write-failure/profile.toml <<"EOF"
schema = 2
[profile]
id = "state-write-failure"
name = "State write failure test"
description = "Rolls back HOME when committing active state fails"
source_root = "/usr/share/state-write-failure"
[defaults]
tool = "state-write"
[[components]]
id = "state-write"
role = "tool"
[[components.configuration]]
target = ".config/state-write-failure/config"
lifecycle = "overwrite"
mode = "file"
source = "config"
EOF
    printf test > /usr/share/materialize-failure/config
    cat > /usr/share/catdot/profiles/materialize-failure/profile.toml <<"EOF"
schema = 2
[profile]
id = "materialize-failure"
name = "Materialization failure test"
description = "Rolls back when HOME cannot receive managed configuration"
source_root = "/usr/share/materialize-failure"
[defaults]
tool = "blocked"
[[components]]
id = "blocked"
role = "tool"
packages = ["tree"]
[[components.configuration]]
target = ".config/materialize-failure/config"
lifecycle = "overwrite"
mode = "file"
source = "config"
EOF
    cat > /usr/share/catdot/profiles/install-failure/profile.toml <<"EOF"
schema = 2

[profile]
id = "install-failure"
name = "Package failure test"
description = "Keeps the active profile when libalpm rejects a dependency"
source_root = "/usr/share/install-failure"

[defaults]
tool = "missing-package"

[[components]]
id = "missing-package"
role = "tool"
packages = ["catdot-package-that-does-not-exist"]
EOF
    cat > /usr/share/catdot/profiles/source-check/profile.toml <<"EOF"
schema = 2

[profile]
id = "source-check"
name = "Missing source test"
description = "Refuses activation after dependency planning when source is absent"
source_root = "/usr/share/source-check"

[defaults]
tool = "missing-source"

[[components]]
id = "missing-source"
role = "tool"
packages = ["tree"]

[[components.configuration]]
target = ".config/source-check/config"
lifecycle = "overwrite"
mode = "file"
source = "config"
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
    cat > /tmp/fail-state-rename.c <<"EOF"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int rename(const char *old, const char *new) {
  static int (*real_rename)(const char *, const char *);
  if (!real_rename) real_rename = dlsym(RTLD_NEXT, "rename");
  if (strstr(new, "/catdot/state.toml") && access("/tmp/state-rename-failed", F_OK)) {
    FILE *marker = fopen("/tmp/state-rename-failed", "w");
    if (marker) fclose(marker);
    errno = EIO;
    return -1;
  }
  return real_rename(old, new);
}
EOF
    cc -shared -fPIC -ldl -o /tmp/fail-state-rename.so /tmp/fail-state-rename.c

    install -Dm644 /dev/stdin /etc/skel/.config/catdot/default.toml <<"EOF"
schema = 1
profile = "jq-test"
EOF
    useradd --create-home alice
    pacman -Sy --noconfirm
    dry_run=$(runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --dry-run)
    printf "%s\n" "$dry_run" | grep -F "Install:"
    printf "%s\n" "$dry_run" | grep -Fx "  jq"
    printf "%s\n" "$dry_run" | grep -Fx "  oniguruma"
    printf "%s\n" "$dry_run" | grep -F "required by uid 1000: jq-test/jq"
    ! test -e /home/alice/.local/state/catdot
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot select jq-test
    ! pacman -Q jq
    ! test -e /home/alice/.config
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --yes

    pacman -Q jq oniguruma
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot exec tool | grep -E "^jq-"
    test -f /var/lib/catdot/packages.toml
    test -f /var/lib/catdot/users/1000.toml

    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot select source-check
    if runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --yes; then
      exit 1
    fi
    python3 - <<"PY"
import tomllib

with open("/home/alice/.local/state/catdot/state.toml", "rb") as file:
    state = tomllib.load(file)
assert state["components"]["tool"] == "source-check/missing-source"
assert state["active_components"]["tool"] == "jq-test/jq"
PY

    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot select state-write-failure
    if runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      LD_PRELOAD=/tmp/fail-state-rename.so catdot resolve --yes; then
      exit 1
    fi
    test ! -e /home/alice/.config/state-write-failure/config
    python3 - <<"PY"
import tomllib
with open("/home/alice/.local/state/catdot/state.toml", "rb") as file:
    state = tomllib.load(file)
assert state["components"]["tool"] == "state-write-failure/state-write"
assert state["active_components"]["tool"] == "jq-test/jq"
PY

    mkdir -p /home/alice/.config
    chown alice:alice /home/alice/.config
    chmod 500 /home/alice/.config
    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot select materialize-failure
    if runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --yes; then
      exit 1
    fi
    chmod 700 /home/alice/.config
    python3 - <<"PY"
import tomllib
with open("/home/alice/.local/state/catdot/state.toml", "rb") as file:
    state = tomllib.load(file)
assert state["components"]["tool"] == "materialize-failure/blocked"
assert state["active_components"]["tool"] == "jq-test/jq"
PY

    runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot select install-failure
    if runuser -u alice -- env HOME=/home/alice XDG_STATE_HOME=/home/alice/.local/state \
      catdot resolve --yes; then
      exit 1
    fi
    python3 - <<"PY"
import tomllib

with open("/home/alice/.local/state/catdot/state.toml", "rb") as file:
    state = tomllib.load(file)
assert state["components"]["tool"] == "install-failure/missing-package"
assert state["active_components"]["tool"] == "jq-test/jq"
PY
  '
