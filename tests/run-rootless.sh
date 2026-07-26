#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
for test in \
  zero-dependency-lifecycle.sh \
  strict-umask-multi-user.sh \
  real-libalpm-install.sh \
  explicit-upgrade-ownership.sh \
  multi-user-prune.sh \
  prune-recovery.sh \
  prune-maximal-safe-set.sh \
  install-conflict-plan.sh \
  doctor-system-state.sh \
  query-helper-boundary.sh
do
  echo "==> $test"
  "$repo_root/tests/$test"
done
