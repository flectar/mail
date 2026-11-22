#!/usr/bin/env bash
# ci-build-scope.sh COMMAND [ARG...]
#
# The app crate's release build peaks near 15 GiB on a 16 GiB hosted Linux
# runner and spills several GiB into swap (scripts/ci-swap.sh). Left to
# itself, the kernel then swaps out the runner's own agent too, until it
# misses its heartbeats and GitHub drops the job as "lost communication".
#
# The agent runs in the system slice. This holds the runner user's slice
# 2 GiB below the runner's RAM and runs the command in it, so reclaim takes
# the build's pages and the agent stays in memory; the build gets slower near
# its peak instead of losing the runner. The limit is on the whole slice
# because Flatpak starts each build step in a systemd scope of its own, which a
# limit on the command's own scope would not cover.
#
# Hosted runners give the runner user a systemd session. Where there is
# none, as in a container job, the command runs as it is; cap the container
# itself instead (docker run --memory). Either way scripts/ci-protect-runner.sh
# also keeps memory for the runner's agent.
set -euo pipefail

if (($# == 0)); then
  echo "Usage: $0 COMMAND [ARG...]" >&2
  exit 2
fi
"$(dirname "$0")/ci-protect-runner.sh"
total_kib=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)
high_kib=$((total_kib - 2 * 1024 * 1024))
slice="user-$(id -u).slice"

if systemd-run --user --scope --quiet true 2>/dev/null &&
  sudo -n systemctl set-property --runtime "$slice" \
    MemoryHigh="${high_kib}K" MemorySwapMax=infinity; then
  exec systemd-run --user --scope --quiet -- "$@"
fi
echo "::warning::No systemd user session to hold the build in; building without a memory limit."
exec "$@"
