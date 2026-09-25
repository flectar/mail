#!/usr/bin/env python3
"""Measure an unsnapshotted, empty-profile Linux launch at a fixed time.

This intentionally does not enable FLECTAR_STARTUP_METRICS, which forces frame
snapshots in the regular startup benchmark. The sample is timed from process
launch, not from a rendered-frame event.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path


spec = importlib.util.spec_from_file_location(
    "startup_benchmark", Path(__file__).with_name("benchmark-startup.py")
)
startup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(startup)


def run_once(binary: Path, sample_seconds: float, include_mappings: bool) -> dict:
    with tempfile.TemporaryDirectory(prefix="flectar-normal-startup-") as temporary:
        root = Path(temporary)
        environment = os.environ.copy()
        for name in (
            "FLECTAR_STARTUP_METRICS",
            "FLECTAR_BENCHMARK_EXIT_AFTER_MS",
            "FLECTAR_BENCHMARK_SCREENSHOT_DIR",
            "FLECTAR_BENCHMARK_TRAY_INTERVAL_MS",
        ):
            environment.pop(name, None)
        environment.update(
            XDG_DATA_HOME=str(root / "data"),
            XDG_CACHE_HOME=str(root / "cache"),
            FLECTAR_RENDERER="cpu",
            FLECTAR_BENCHMARK_DISABLE_SYNC="1",
        )
        with tempfile.TemporaryFile(mode="w+t") as log:
            process = subprocess.Popen(
                [str(binary)],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=log,
                text=True,
            )
            try:
                started = time.monotonic()
                time.sleep(sample_seconds)
                if process.poll() is not None:
                    raise RuntimeError(f"application exited before sample: {process.returncode}")
                resources = startup.read_proc_resources(process.pid)
                mappings = (
                    startup.read_proc_mappings(process.pid, binary)
                    if include_mappings
                    else None
                )
                sampled_after = time.monotonic() - started
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
            log.seek(0)
            renderer = None
            for line in log:
                if line.startswith(startup.RENDERER_PREFIX):
                    payload = json.loads(line[len(startup.RENDERER_PREFIX) :])
                    if payload.get("event") == "selected":
                        renderer = payload
            if not renderer or renderer.get("active") != "cpu" or renderer.get("wgpu_initialized") is not False:
                raise RuntimeError(f"expected CPU renderer without WGPU, got {renderer}")

        result = {
            "idle": resources,
            "sampled_after_seconds": sampled_after,
            "renderer_selected": renderer,
        }
        if mappings is not None:
            result["mapping_report"] = mappings
        return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--sample-seconds", type=float, default=5.5)
    parser.add_argument("--include-mappings", action="store_true")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this benchmark requires Linux /proc")
    if args.rounds < 1 or args.sample_seconds <= 0:
        parser.error("rounds and sample seconds must be positive")
    if not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
        parser.error("a display is required")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"binary is not executable: {binary}")

    runs = []
    for index in range(args.rounds):
        run = run_once(binary, args.sample_seconds, args.include_mappings)
        run["round"] = index + 1
        runs.append(run)
        print(
            f"round={index + 1} rss_kib={run['idle']['rss_kib']} "
            f"pss_kib={run['idle']['pss_kib']}",
            flush=True,
        )
    names = sorted({name for run in runs for name in run["idle"]})
    report = {
        "schema_version": 1,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "identity": {
            "binary": str(binary),
            "binary_sha256": startup.file_sha256(binary),
            "git_commit": startup.command_output("git", "rev-parse", "HEAD"),
            "platform": platform.platform(),
            "display": os.environ.get("DISPLAY"),
            "wayland_display": os.environ.get("WAYLAND_DISPLAY"),
        },
        "configuration": {
            "rounds": args.rounds,
            "sample_seconds": args.sample_seconds,
            "include_mappings": args.include_mappings,
            "forced_snapshots": False,
            "provider_sync_disabled": True,
        },
        "summary": {
            "median_idle": {
                name: statistics.median(run["idle"][name] for run in runs)
                for name in names
            }
        },
        "runs": runs,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"report={args.output.resolve()}")


if __name__ == "__main__":
    main()
