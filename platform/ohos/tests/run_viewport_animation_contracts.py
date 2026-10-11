#!/usr/bin/env python3
"""Compile the production Slint viewport scheduler and its throttle regression.

No device, account, Cargo, or application build is involved. Requires existing
host Slint 1.18.1 libraries, checked through their dependency metadata.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
from run_host_contracts import dependency, replace_once

TESTS = Path(__file__).resolve().parent
ROOT = TESTS.parents[2]
SCHEDULER = ROOT / "ui/components/viewport-render-scheduler.slint"
FIXTURE = TESTS / "viewport_animation_contracts.rs"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deps-dir", type=Path, required=True)
    parser.add_argument("--slint-rlib")
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--output-dir", type=Path,
                        default=ROOT / "target/ohos-host-contracts/viewport-animation")
    args = parser.parse_args()
    deps, output = args.deps_dir.resolve(), args.output_dir.resolve()
    slint = dependency(deps, args.slint_rlib, "slint", "slint")
    source = SCHEDULER.read_text()
    negative = replace_once(source,
        "if root.debounce { viewport-timer.restart(); }", "",
        "negative control restores periodic reflow during animation")
    output.mkdir(parents=True, exist_ok=True)
    results = []
    with tempfile.TemporaryDirectory(prefix="feihong-viewport-animation-") as directory:
        work = Path(directory)
        old_scheduler = work / "throttled-viewport.slint"
        old_scheduler.write_text(negative)
        for name, component in [("current", SCHEDULER), ("old-throttle", old_scheduler)]:
            fixture = replace_once(FIXTURE.read_text(),
                '"../../../ui/components/viewport-render-scheduler.slint"',
                json.dumps(str(component)), "production scheduler import")
            harness, binary = work / f"{name}.rs", work / name
            harness.write_text(fixture)
            commands = [
                [args.rustc, "--edition=2024", "--test", str(harness),
                 "-L", f"dependency={deps}", "--extern", f"slint={slint}", "-o", str(binary)],
                [str(binary), "--test-threads=1", "--nocapture"],
            ]
            if name == "old-throttle":
                commands[1] += ["continuous_resize_commits_once_after_the_final_geometry", "--exact"]
            for phase, command in zip(["compile", "run"], commands):
                result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        text=True, env=os.environ.copy(), timeout=120)
                log = output / f"{name}-{phase}.log"
                log.write_text(result.stdout)
                log.chmod(0o600)
                expected = 101 if phase == "run" and name == "old-throttle" else 0
                success = result.returncode == expected
                if expected == 101:
                    success = success and "HTML must not reflow during continuous geometry changes" in result.stdout
                results.append({"variant": name, "phase": phase,
                                "exit_code": result.returncode, "expected_exit_code": expected,
                                "pass": success})
                if not success:
                    print(result.stdout)
                    raise RuntimeError(f"{name}/{phase} failed; inspect retained log")
    report = {
        "status": "pass", "slint_version": "1.18.1", "production_tests": 3,
        "old_throttle_negative_control": "failed as expected",
        "source_sha256": {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
                          for path in [SCHEDULER, FIXTURE]},
        "results": results, "device_acceptance": False,
    }
    target = output / "validation.json"
    target.write_text(json.dumps(report, indent=2) + "\n")
    target.chmod(0o600)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
