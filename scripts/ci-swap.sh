#!/usr/bin/env bash
# ci-swap.sh on [GIB] | off
#
# A release build of the app crate peaks near 15 GiB, about all the memory a
# hosted Linux runner has, and the kernel then kills rustc. A swap file gives
# it room. "on" adds one of GIB gibibytes (8 by default) when the disk can
# spare it with plenty left for the build. "off" removes it again, for steps
# that measure memory and must not use swap.
#
# The file goes in the runner's temporary directory, which is on the
# runner's own disk in container jobs too: swap can't live on a container's
# overlay filesystem, and a container job needs --privileged to turn it on.
set -euo pipefail

swapfile="${RUNNER_TEMP:-/tmp}/swapfile-release-build"

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
  ;;
*)
  echo "usage: $0 on [GIB] | off" >&2
  exit 2
  ;;
esac
