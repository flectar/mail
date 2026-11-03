#!/usr/bin/env python3
"""Print the memory of running Linux processes the way docs/performance.md reports it.

    scripts/memory-snapshot.py PID [PID ...]
    scripts/memory-snapshot.py --exe-dir /opt/thunderbird   # every process of an app

private: anonymous resident memory (RssAnon), the kind of figure Windows Task
         Manager shows; it excludes code and libraries mapped from files.
pss:     proportional set size; shared pages are split between their users,
         so it adds up correctly across processes.
rss:     resident set size, as in top/htop; summed over several processes it
         counts shared pages more than once.
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path


def read(pid: str) -> dict[str, int]:
    values: dict[str, int] = {}
    for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()[1:]:
        key, _, rest = line.partition(":")
        values[key.strip()] = int(rest.split()[0])
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        key, _, rest = line.partition(":")
        if key in ("RssAnon", "Threads"):
            values[key] = int(rest.split()[0])
    return values


def processes_under(directory: Path) -> list[str]:
    root = str(directory.resolve()) + os.sep
    found = []
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                executable = os.path.realpath(os.readlink(entry / "exe"))
            except OSError:
                continue
            if executable.startswith(root):
                found.append(entry.name)
    return sorted(found, key=int)


def mib(kib: int) -> str:
    return f"{kib / 1024:7.1f}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("pids", nargs="*")
    parser.add_argument("--exe-dir", type=Path, help="measure every process whose executable is in this directory")
    args = parser.parse_args()
    if not sys.platform.startswith("linux"):
        parser.error("reads /proc, so it runs on Linux only")
    pids = list(args.pids) + (processes_under(args.exe_dir) if args.exe_dir else [])
    if not pids:
        parser.error("no processes to measure")
    totals = {"private": 0, "pss": 0, "rss": 0}
    print("    pid  private      pss      rss  threads  (MiB)")
    for pid in pids:
        values = read(pid)
        row = {"private": values["RssAnon"], "pss": values["Pss"], "rss": values["Rss"]}
        for key, value in row.items():
            totals[key] += value
        print(f"{pid:>7} {mib(row['private'])}  {mib(row['pss'])}  {mib(row['rss'])}  {values['Threads']:7}")
    if len(pids) > 1:
        print(f"  total {mib(totals['private'])}  {mib(totals['pss'])}  {mib(totals['rss'])}   (rss total overcounts shared pages)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
