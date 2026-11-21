#!/usr/bin/env bash
# ci-swap.sh on [GIB] | off
#
# A release build of the app crate peaks near 15 GiB, about all the memory a
# hosted Linux runner has, and the kernel then kills rustc. Swap gives it
# room, and scripts/ci-build-scope.sh keeps the build from swapping out the
# runner's agent. "on" adds a swap file of GIB gibibytes (8 by default) when
# the disk can spare it with plenty left for the build. Where the kernel has
# zram, it first adds 12 GiB of compressed swap in memory: the build's pages
# compress about 4:1, so the overflow stays off the disk, and the runner no
# longer stalls on swap I/O near the peak. The file stays behind it. "off"
# removes both again, for steps that measure memory and must not use swap.
#
# The file goes in the runner's temporary directory, which is on the
# runner's own disk in container jobs too: swap can't live on a container's
# overlay filesystem, and a container job needs --privileged to turn it on.
set -euo pipefail

swapfile="${RUNNER_TEMP:-/tmp}/swapfile-release-build"
# Names the zram device "on" added, for "off".
zram_record="${RUNNER_TEMP:-/tmp}/zram-release-build"

as_root() {
  if ((EUID == 0)); then
    "$@"
  else
    sudo "$@"
  fi
}

case "${1:-}" in
on)
  size=${2:-8}
  # Containers can't load modules; they keep the file alone.
  if as_root modprobe zram 2>/dev/null &&
    zram=$(as_root zramctl --find --size 12G --algorithm zstd 2>/dev/null); then
    as_root mkswap "$zram" >/dev/null
    as_root swapon --priority 100 "$zram"
    echo "$zram" >"$zram_record"
  else
    echo "No zram here; swapping to a file only"
  fi
  free=$(df --output=avail -BG "$(dirname "$swapfile")" | tail -1 | tr -dc 0-9)
  if ((free < size + 40)); then
    echo "Only ${free} GiB free; building without an extra swap file"
    exit 0
  fi
  as_root fallocate -l "${size}G" "$swapfile"
  as_root chmod 600 "$swapfile"
  as_root mkswap "$swapfile" >/dev/null
  as_root swapon "$swapfile"
  swapon --show
  ;;
off)
  if swapon --show=NAME --noheadings | grep -qxF "$swapfile"; then
    as_root swapoff "$swapfile"
  fi
  as_root rm -f "$swapfile"
  if [[ -f "$zram_record" ]]; then
    zram=$(<"$zram_record")
    as_root swapoff "$zram"
    as_root zramctl --reset "$zram"
    rm -f "$zram_record"
  fi
  ;;
*)
  echo "usage: $0 on [GIB] | off" >&2
  exit 2
  ;;
esac
