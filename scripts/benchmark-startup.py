#!/usr/bin/env python3
"""Measure Flectar Mail startup milestones and idle Linux process resources.

The application emits structured startup events only while this harness sets
FLECTAR_STARTUP_METRICS. Each run uses an isolated copy of the supplied local
profile, disables provider sync, waits for a real rendered core-ready frame,
then samples /proc after a configurable idle interval.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import queue
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any


METRIC_PREFIX = "FLECTAR_STARTUP_METRIC "
RENDERER_PREFIX = "FLECTAR_RENDERER "
RESOURCE_KEYS = (
    "Rss",
    "Pss",
    "Pss_Anon",
    "Pss_File",
    "Pss_Shmem",
    "Private_Clean",
    "Private_Dirty",
    "Shared_Clean",
    "Shared_Dirty",
    "Swap",
    "SwapPss",
)
SMAPS_HEADER = re.compile(r"^[0-9a-f]+-[0-9a-f]+\s")
MAPPING_KEYS = (
    "Size",
    "Rss",
    "Pss",
    "Anonymous",
    "Private_Clean",
    "Private_Dirty",
    "Shared_Clean",
    "Shared_Dirty",
    "Swap",
    "SwapPss",
)


def command_output(*command: str) -> str:
    try:
        return subprocess.check_output(command, text=True, stderr=subprocess.DEVNULL).strip()
    except (OSError, subprocess.CalledProcessError):
        return "unavailable"


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def copy_profile(source: Path | None, destination: Path) -> None:
    if source is None:
        destination.mkdir(parents=True, exist_ok=True)
        return
    shutil.copytree(source, destination, symlinks=True)


def read_proc_resources(pid: int) -> dict[str, int]:
    resources: dict[str, int] = {}
    smaps = Path(f"/proc/{pid}/smaps_rollup").read_text(encoding="utf-8")
    for line in smaps.splitlines():
        name, separator, remainder = line.partition(":")
        if separator and name in RESOURCE_KEYS:
            field = "swap_pss" if name == "SwapPss" else name.lower()
            resources[f"{field}_kib"] = int(remainder.split()[0])
    for line in Path(f"/proc/{pid}/status").read_text(encoding="utf-8").splitlines():
        if line.startswith("Threads:"):
            resources["threads"] = int(line.split()[1])
            break
    resources["file_descriptors"] = len(list(Path(f"/proc/{pid}/fd").iterdir()))
    return resources


def mapping_kind(path: str, executable: Path) -> str:
    path = path.removesuffix(" (deleted)")
    if path == str(executable):
        return "executable"
    if path == "[heap]":
        return "heap"
    if path.startswith("[stack"):
        return "stack"
    if path.startswith("/SYSV") or (
        (path.startswith("/dev/shm/") or "memfd:" in path)
        and any(name in path.lower() for name in ("wayland", "x11", "slint", "softbuffer"))
    ):
        return "shared_surface_candidate"
    if path.startswith("/dev/shm/") or "memfd:" in path or path.startswith("[anon_shmem"):
        return "shared_memory"
    if not path or path.startswith("[anon"):
        return "anonymous"
    if path.startswith("["):
        return "kernel_mapping"
    name = path.rsplit("/", 1)[-1].lower()
    if name.endswith((".ttf", ".ttc", ".otf", ".otc")):
        return "font_file"
    if name.endswith((".db", ".sqlite", ".sqlite3", "-wal", "-shm")):
        return "database_file"
    if ".so" in name and (name.endswith(".so") or ".so." in name):
        return "shared_library"
    return "other_file"


def parse_smaps(contents: str, executable: Path) -> dict[str, Any]:
    """Return per-mapping counters without retaining process or profile paths."""
    mappings: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    for line in contents.splitlines():
        if SMAPS_HEADER.match(line):
            fields = line.split(maxsplit=5)
            if len(fields) < 5:
                raise ValueError("invalid smaps header")
            if current is not None:
                mappings.append(current)
            current = {
                "kind": mapping_kind(fields[5] if len(fields) == 6 else "", executable),
                "permissions": fields[1],
            }
            continue
        if current is None:
            continue
        name, separator, value = line.partition(":")
        if separator and name in MAPPING_KEYS:
            field = "swap_pss" if name == "SwapPss" else name.lower()
            current[f"{field}_kib"] = int(value.split()[0])
    if current is not None:
        mappings.append(current)

    by_kind: dict[str, dict[str, int]] = {}
    for mapping in mappings:
        totals = by_kind.setdefault(mapping["kind"], {"mapping_count": 0})
        totals["mapping_count"] += 1
        for key, value in mapping.items():
            if key.endswith("_kib"):
                totals[key] = totals.get(key, 0) + value
    return {"mapping_count": len(mappings), "by_kind": by_kind, "mappings": mappings}


def read_proc_mappings(pid: int, executable: Path) -> dict[str, Any]:
    return parse_smaps(
        Path(f"/proc/{pid}/smaps").read_text(encoding="utf-8"), executable
    )


def read_cpu_seconds(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").split()
    ticks = int(fields[13]) + int(fields[14])
    return ticks / os.sysconf(os.sysconf_names["SC_CLK_TCK"])


def stderr_reader(stream: Any, messages: queue.Queue[str]) -> None:
    for line in iter(stream.readline, ""):
        messages.put(line.rstrip("\n"))
    messages.put("")


def record_stderr_line(
    line: str, events: dict[str, dict[str, Any]], stderr_tail: list[str]
) -> None:
    for prefix, name in ((METRIC_PREFIX, None), (RENDERER_PREFIX, "renderer_selected")):
        if not line.startswith(prefix):
            continue
        try:
            payload = json.loads(line[len(prefix) :])
            if not isinstance(payload, dict):
                break
            if name is None:
                events[payload["event"]] = payload
                return
            if payload.get("event") == "selected":
                events[name] = payload
                return
        except (json.JSONDecodeError, KeyError):
            break
    if line:
        stderr_tail.append(line)
        del stderr_tail[:-40]


def wait_for_metric(
    process: subprocess.Popen[str],
    messages: queue.Queue[str],
    events: dict[str, dict[str, Any]],
    stderr_tail: list[str],
    wanted: str,
    timeout: float,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if wanted in events:
            return
        if process.poll() is not None and messages.empty():
            break
        try:
            line = messages.get(timeout=min(0.1, max(0.01, deadline - time.monotonic())))
        except queue.Empty:
            continue
        if not line:
            continue
        record_stderr_line(line, events, stderr_tail)
    if wanted not in events:
        tail = "\n".join(stderr_tail[-12:])
        raise RuntimeError(f"timed out waiting for {wanted!r}; recent stderr:\n{tail}")


def drain_messages(
    messages: queue.Queue[str],
    events: dict[str, dict[str, Any]],
    stderr_tail: list[str],
) -> None:
    while True:
        try:
            line = messages.get_nowait()
        except queue.Empty:
            return
        record_stderr_line(line, events, stderr_tail)


def run_once(
    binary: Path,
    profile_data: Path | None,
    profile_cache: Path | None,
    settle_seconds: float,
    idle_seconds: float,
    timeout_seconds: float,
    round_number: int,
    include_mappings: bool = False,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="flectar-startup-benchmark-") as temporary:
        root = Path(temporary)
        data_home = root / "data-home"
        cache_home = root / "cache-home"
        copy_profile(profile_data, data_home / "flectar-mail")
        copy_profile(profile_cache, cache_home / "flectar-mail")

        environment = os.environ.copy()
        environment.update(
            {
                "XDG_DATA_HOME": str(data_home),
                "XDG_CACHE_HOME": str(cache_home),
                "FLECTAR_STARTUP_METRICS": "1",
                "FLECTAR_RENDERER": "cpu",
                "FLECTAR_BENCHMARK_DISABLE_SYNC": "1",
                "FLECTAR_BENCHMARK_EXIT_AFTER_MS": str(
                    int((settle_seconds + idle_seconds) * 1000) + 1500
                ),
            }
        )
        process = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=environment,
        )
        assert process.stderr is not None
        messages: queue.Queue[str] = queue.Queue()
        reader = threading.Thread(
            target=stderr_reader,
            args=(process.stderr, messages),
            daemon=True,
        )
        reader.start()
        events: dict[str, dict[str, Any]] = {}
        stderr_tail: list[str] = []
        try:
            wait_for_metric(
                process,
                messages,
                events,
                stderr_tail,
                "core_ready_frame",
                timeout_seconds,
            )
            time.sleep(settle_seconds)
            if process.poll() is not None:
                raise RuntimeError("application exited during the idle-settle interval")
            initial_cpu = read_cpu_seconds(process.pid)
            idle_started = time.monotonic()
            time.sleep(idle_seconds)
            idle_elapsed = time.monotonic() - idle_started
            final_cpu = read_cpu_seconds(process.pid)
            resources = read_proc_resources(process.pid)
            mapping_report = (
                read_proc_mappings(process.pid, binary) if include_mappings else None
            )
            resources["idle_cpu_percent"] = (
                max(0.0, final_cpu - initial_cpu) / idle_elapsed * 100.0
            )
            resources["idle_sample_seconds"] = idle_elapsed
            resources["idle_settle_seconds"] = settle_seconds
            process.wait(timeout=5)
            drain_messages(messages, events, stderr_tail)
            renderer = events.get("renderer_selected", {})
            if renderer.get("active") != "cpu" or renderer.get("wgpu_initialized") is not False:
                raise RuntimeError(f"expected CPU renderer without WGPU, got {renderer}")
            if process.returncode != 0:
                raise RuntimeError(
                    f"application exited with {process.returncode}:\n"
                    + "\n".join(stderr_tail[-12:])
                )
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)

        milestones = {
            name: payload.get("elapsed_ms")
            for name, payload in events.items()
            if "elapsed_ms" in payload
        }
        warm_event = events.get("warm_cache_loaded", {})
        result = {
            "round": round_number,
            "pid": process.pid,
            "warm_cache_hit": bool(warm_event.get("hit", False)),
            "milestones_ms": milestones,
            "idle": resources,
            "events": events,
        }
        if mapping_report is not None:
            result["mapping_report"] = mapping_report
        return result


def median(values: list[float | int]) -> float | None:
    return statistics.median(values) if values else None


def summarize(runs: list[dict[str, Any]]) -> dict[str, Any]:
    milestone_names = sorted(
        {name for run in runs for name in run["milestones_ms"].keys()}
    )
    resource_names = sorted({name for run in runs for name in run["idle"].keys()})
    return {
        "rounds": len(runs),
        "warm_cache_hits": sum(run["warm_cache_hit"] for run in runs),
        "median_milestones_ms": {
            name: median(
                [
                    run["milestones_ms"][name]
                    for run in runs
                    if run["milestones_ms"].get(name) is not None
                ]
            )
            for name in milestone_names
        },
        "median_idle": {
            name: median(
                [run["idle"][name] for run in runs if run["idle"].get(name) is not None]
            )
            for name in resource_names
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument(
        "--profile-data-dir",
        type=Path,
        help="Optional flectar-mail data directory copied into every isolated run.",
    )
    parser.add_argument(
        "--profile-cache-dir",
        type=Path,
        help="Optional flectar-mail cache directory containing the warm projection.",
    )
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument(
        "--settle-seconds",
        type=float,
        default=2.0,
        help="Time after the ready frame before idle CPU sampling begins.",
    )
    parser.add_argument("--idle-seconds", type=float, default=5.0)
    parser.add_argument("--timeout-seconds", type=float, default=45.0)
    parser.add_argument(
        "--include-mappings",
        action="store_true",
        help="Include anonymized per-mapping /proc smaps counters and category totals.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("target/startup-benchmark/startup-benchmark.json"),
    )
    args = parser.parse_args()

    if sys.platform != "linux":
        parser.error("this harness currently requires Linux /proc resource counters")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"binary is not executable: {binary}")
    if not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
        parser.error(
            "no display is available; run under xvfb-run, for example: "
            "xvfb-run -a ./scripts/benchmark-startup.py --binary …"
        )
    if (
        args.rounds < 1
        or args.settle_seconds < 0
        or args.idle_seconds <= 0
        or args.timeout_seconds <= 0
    ):
        parser.error("rounds and sample timings must be positive; settle may be zero")

    for label, source in (
        ("profile data", args.profile_data_dir),
        ("profile cache", args.profile_cache_dir),
    ):
        if source is not None and not source.is_dir():
            parser.error(f"{label} directory does not exist: {source}")

    git_status = command_output("git", "status", "--porcelain")
    identity = {
        "git_commit": command_output("git", "rev-parse", "HEAD"),
        "git_dirty": git_status not in ("", "unavailable"),
        "rustc": command_output("rustc", "--version", "--verbose"),
        "binary": str(binary),
        "binary_sha256": file_sha256(binary),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "display": os.environ.get("DISPLAY"),
        "wayland_display": os.environ.get("WAYLAND_DISPLAY"),
        "renderer_requested": "cpu",
    }
    runs = []
    for round_number in range(1, args.rounds + 1):
        result = run_once(
            binary,
            args.profile_data_dir,
            args.profile_cache_dir,
            args.settle_seconds,
            args.idle_seconds,
            args.timeout_seconds,
            round_number,
            args.include_mappings,
        )
        runs.append(result)
        print(
            f"round={round_number} "
            f"first_frame_ms={result['milestones_ms'].get('first_frame')} "
            f"warm_frame_ms={result['milestones_ms'].get('warm_cache_frame')} "
            f"core_frame_ms={result['milestones_ms'].get('core_ready_frame')} "
            f"pss_kib={result['idle'].get('pss_kib')} "
            f"idle_cpu_percent={result['idle'].get('idle_cpu_percent'):.3f}"
        )

    report = {
        "schema_version": 2,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "identity": identity,
        "configuration": {
            "rounds": args.rounds,
            "settle_seconds": args.settle_seconds,
            "idle_seconds": args.idle_seconds,
            "timeout_seconds": args.timeout_seconds,
            "include_mappings": args.include_mappings,
            "profile_data_supplied": args.profile_data_dir is not None,
            "profile_cache_supplied": args.profile_cache_dir is not None,
        },
        "summary": summarize(runs),
        "runs": runs,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"report={args.output.resolve()}")


if __name__ == "__main__":
    main()
