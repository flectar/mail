#!/usr/bin/env bash
# ci-protect-runner.sh
#
# Keeps 1 GiB of memory for GitHub's runner agent during a release build.
# scripts/ci-build-scope.sh holds the build below the runner's RAM, but near
# the build's peak the kernel's global reclaim still takes the agent's pages,
# and an agent that is swapped out misses its heartbeats ("lost
# communication"). memory.min exempts that much of the agent's memory from
# reclaim. Its parent slice needs the same value, because a cgroup can't be
# protected beyond its parent.
#
# Host jobs run this through ci-build-scope.sh. Container jobs need the host's
# cgroups, so they run with --privileged --cgroupns=host.
set -euo pipefail

cgroups=/sys/fs/cgroup
agent=$cgroups/system.slice/hosted-compute-agent.service
if [[ ! -d "$agent" ]]; then
  echo "::notice::No hosted runner agent here; leaving its memory alone."
  exit 0
fi
as_root() {
  if ((EUID == 0)); then
    "$@"
  else
    sudo -n "$@"
  fi
}
for cgroup in "$cgroups/system.slice" "$agent"; do
  echo 1G | as_root tee "$cgroup/memory.min" >/dev/null
done
echo "Runner agent memory.min: $(cat "$agent/memory.min")"
