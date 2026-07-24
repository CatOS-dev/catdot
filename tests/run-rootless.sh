#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
for test in \
  zero-dependency-lifecycle.sh \
  strict-umask-multi-user.sh \
  real-libalpm-install.sh \
  finalize-retry.sh \
  adopt-pending-isolation.sh \
  multi-user-prune.sh \
  prune-maximal-safe-set.sh \
  install-conflict-plan.sh \
  doctor-system-state.sh \
  default-profile-resources.sh
do
  echo "==> $test"
  "$repo_root/tests/$test"
done
