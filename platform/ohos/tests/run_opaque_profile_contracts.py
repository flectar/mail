#!/usr/bin/env python3
"""Compare only host caller/generic opt2 vs opt3 using existing Slint1.18.1 rlibs.

No Cargo, application build, device operation or production-source edit occurs.
The SHA2 rlib is also preexisting; render-only timings exclude hashes/file IO.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess

from run_host_contracts import dependency, execute


def summary(values):
    ordered = sorted(values)
    return {"samples": len(values), "median_ms": (ordered[(len(values)-1)//2] + ordered[len(values)//2]) / 2e6,
            "p95_ms": ordered[math.ceil(.95 * len(values))-1] / 1e6, "max_ms": max(values) / 1e6}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deps-dir", type=Path, required=True)
    parser.add_argument("--slint-rlib")
    parser.add_argument("--sha2-rlib", type=Path, required=True)
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--output-dir", type=Path,
                        default=Path(__file__).resolve().parents[3] / "target/ohos-host-contracts/opaque-profile")
    args = parser.parse_args()
    tests = Path(__file__).resolve().parent
    root = tests.parents[2]
    output, deps, sha2 = args.output_dir.resolve(), args.deps_dir.resolve(), args.sha2_rlib.resolve()
    slint = dependency(deps, args.slint_rlib, "slint", "slint")
    renderer = dependency(deps, None, "i_slint_renderer_software", "i-slint-renderer-software")
    if not sha2.is_file() or not sha2.name.startswith("libsha2-"):
        raise RuntimeError("require a preexisting SHA2 host rlib")
    depinfo = sha2.with_name(sha2.name[3:]).with_suffix(".d")
    sha2_versions = set(re.findall(r"/sha2-([^/\s]+)/", depinfo.read_text()))
    if len(sha2_versions) != 1:
        raise RuntimeError("require unambiguous SHA2 source-version dep-info")
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_MANIFEST_DIR=str(tests))
    source = tests / "opaque_profile_probe.rs"
    production = root / "platform/ohos/rust/src/framebuffer.rs"
    before = hashlib.sha256(production.read_bytes()).hexdigest()
    commands = []
    for level in [2, 3]:
        command = [args.rustc, "--edition=2024", str(source), "-C", f"opt-level={level}",
                   "-C", "debug-assertions=on", "-C", "overflow-checks=on", "-C", "debuginfo=1",
                   "-L", f"dependency={deps}", "-L", f"dependency={sha2.parent}",
                   "--extern", f"slint={slint}", "--extern", f"sha2={sha2}",
                   "-o", str(output / f"opaque-profile-opt{level}")]
        commands.append(command)
        if execute(command, output / f"opt{level}-compile.log", env, 120).returncode:
            raise RuntimeError(f"opt{level} isolated fixture compile failed; no Cargo fallback")
    runs = []
    for index, level in enumerate([2, 3, 3, 2], 1):
        folder = output / f"run-{index}-opt{level}"
        folder.mkdir(exist_ok=True)
        command = [str(output / f"opaque-profile-opt{level}"), str(folder)]
        result = execute(command, folder / "run.log", env, 120)
        if result.returncode:
            raise RuntimeError(f"isolated profile run {index} failed")
        scenes = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{"scene":')]
        if len(scenes) != 3 or any(len(s["render_ns"]) != 16 for s in scenes):
            raise RuntimeError("expected three actual 16-frame scene sequences")
        for scene in scenes:
            scene["render_summary"] = summary(scene["render_ns"])
        for snapshot in folder.glob("*.ppm"):
            snapshot.chmod(0o600)
        runs.append({"index": index, "opt_level": level, "scenes": scenes})
        print(json.dumps({"run": index, "opt": level, "scenes": {s["scene"]: s["render_summary"] for s in scenes}}))
    reference = {s["scene"]: s for s in runs[0]["scenes"]}
    for run in runs:
        for s in run["scenes"]:
            base = reference[s["scene"]]
            if s["pixel_sha256"] != base["pixel_sha256"] or s["damage_sizes"] != base["damage_sizes"]:
                raise RuntimeError("opt-level or repeated run changed actual rendered pixels/damage")
            if len(set(s["pixel_sha256"])) != 8:
                raise RuntimeError("eight states must have genuinely different rendered pixels")
    after = hashlib.sha256(production.read_bytes()).hexdigest()
    if before != after:
        raise RuntimeError("production framebuffer changed during isolated test")
    dependencies = {"slint": slint, "renderer": renderer, "sha2": sha2}
    report = {"status": "PASS", "slint_version": "1.18.1", "run_order": [2, 3, 3, 2],
              "runs": runs, "pixel_checks": "All 192 scene-frame SHA256 values and damage sizes match across four runs; each scene has eight distinct states, repeated twice.",
              "production_framebuffer_sha256": before, "fixture_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
              "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "dependencies": {k: {"path": str(p), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for k, p in dependencies.items()},
              "sha2_version": next(iter(sha2_versions)), "commands": commands,
              "rustc_version": subprocess.check_output([args.rustc, "--version"], text=True).strip(),
              "limits": ["Host caller/generic code generation only; this fixture's generated small UI also shares the selected opt level.",
                         "Preexisting renderer/font/non-generic rlib compiler flags are not established by this runner; identical cached artifacts are linked in both cases.",
                         "Renderer/image/font resources and eight-state prewarming are identical; new process caches are recreated for each run.",
                         "Render timing excludes property setters/timer dispatch, byte hashing, opacity assertions and snapshot IO.",
                         "The OHOS native copy/stride/fence/VSync/lifecycle path, actual application and device fonts are absent. No device FPS or performance guarantee inferred.",
                         "Four counterbalanced runs reduce order bias but are not device statistical acceptance or thermal/frequency control."],
              "cargo_or_device_operations": False, "production_source_edits": False}
    path = output / "validation.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    path.chmod(0o600)
    print(f"PASS: identical pixels in 2/3/3/2 isolated host probe; {path}")


if __name__ == "__main__":
    main()
