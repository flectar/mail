#!/usr/bin/env python3
"""Measure CPU-rendered welcome -> tray -> restored memory on isolated profiles.

Requires Linux /proc and a display. Unset WAYLAND_DISPLAY to force X11.
Uses the application's opt-in metrics and production tray lifecycle helpers.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import queue
import statistics
import subprocess
import tempfile
import threading
import time

spec = importlib.util.spec_from_file_location("startup", Path(__file__).with_name("benchmark-startup.py"))
startup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(startup)


def run(binary, screenshots):
    with tempfile.TemporaryDirectory(prefix="flectar-tray-benchmark-") as root:
        env = dict(os.environ, XDG_DATA_HOME=root + "/data", XDG_CACHE_HOME=root + "/cache",
                   FLECTAR_RENDERER="cpu", FLECTAR_STARTUP_METRICS="1",
                   FLECTAR_BENCHMARK_DISABLE_SYNC="1", FLECTAR_BENCHMARK_TRAY_INTERVAL_MS="6000",
                   FLECTAR_BENCHMARK_EXIT_AFTER_MS="18000")
        if screenshots:
            env["FLECTAR_BENCHMARK_SCREENSHOT_DIR"] = str(screenshots.resolve())
        process = subprocess.Popen([str(binary)], env=env, stderr=subprocess.PIPE,
                                   stdout=subprocess.DEVNULL, text=True)
        messages = queue.Queue()
        threading.Thread(target=startup.stderr_reader, args=(process.stderr, messages), daemon=True).start()
        events, tail, phases = {}, [], {}
        try:
            for phase, event in [("visible", "core_ready_frame"), ("tray", "tray_hidden"),
                                 ("restored", "tray_restored_frame")]:
                startup.wait_for_metric(process, messages, events, tail, event, 45)
                if "render_error" in events[event]:
                    raise RuntimeError(events[event])
                time.sleep(2)
                cpu, began = startup.read_cpu_seconds(process.pid), time.monotonic()
                time.sleep(2)
                resources = startup.read_proc_resources(process.pid)
                resources["idle_cpu_percent"] = (startup.read_cpu_seconds(process.pid) - cpu) / (time.monotonic() - began) * 100
                phases[phase] = resources
            process.wait(timeout=8)
            startup.drain_messages(messages, events, tail)
            if process.returncode:
                raise RuntimeError(tail)
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
        return dict(phases=phases, events=events, stderr_tail=tail)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("rounds must be positive")
    binary = args.binary.resolve()
    runs = []
    for index in range(args.rounds):
        result = run(binary, args.screenshots / str(index + 1) if args.screenshots else None)
        runs.append(result)
        print(json.dumps(result["phases"]), flush=True)
    report = dict(binary=str(binary), binary_sha256=startup.file_sha256(binary),
                  display=os.environ.get("DISPLAY"), wayland_display=os.environ.get("WAYLAND_DISPLAY"),
                  platform=startup.platform.platform(), screenshots=bool(args.screenshots), runs=runs,
                  medians={phase: {key: statistics.median(r["phases"][phase][key] for r in runs)
                                   for key in runs[0]["phases"][phase]} for phase in runs[0]["phases"]})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
