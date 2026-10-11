#!/usr/bin/env python3
"""Compare exact production SVG pixels under opt0/opt3 on the Rust host.

Builds only the independent, locked SVG fixture, offline. Never builds or runs
the mail application, connects to a device, or reads account configuration.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import tomllib


REPO = Path(__file__).resolve().parents[3]
FIXTURE = Path(__file__).resolve().parent / "svg_profile_probe"
VERSIONS = {
    "bytemuck": "1.25.2", "kurbo": "0.13.1", "resvg": "0.48.1",
    "strict-num": "0.1.1", "svgtypes": "0.16.1", "tiny-skia": "0.12.0",
    "tiny-skia-path": "0.12.0", "usvg": "0.48.1",
}
ORDER = ("opt0", "opt3", "opt3", "opt0")
STAGES = ("render_us_per_iteration", "parse_us_per_iteration")
ICONS = (
    "ui/icons/lucide/outline/search.svg",
    "ui/icons/lucide/outline/sync.svg",
    "ui/icons/lucide/filled/inbox-flectar.svg",
)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_private(path, data):
    path.write_text(data)
    path.chmod(0o600)


def execute(command, log, environment, timeout, cwd):
    with log.open("w") as stream:
        log.chmod(0o600)
        subprocess.run(command, cwd=cwd, env=environment, timeout=timeout,
                       stdout=stream, stderr=subprocess.STDOUT, check=True)


def check_inputs():
    manifest = tomllib.loads((FIXTURE / "Cargo.toml").read_text())
    locked = tomllib.loads((FIXTURE / "Cargo.lock").read_text())["package"]
    production = tomllib.loads((REPO / "platform/ohos/rust/Cargo.lock").read_text())["package"]
    for name, version in VERSIONS.items():
        for label, packages in (("fixture", locked), ("production", production)):
            if not any(p["name"] == name and p["version"] == version for p in packages):
                raise RuntimeError(f"{label} lock lacks required {name} {version}")
    for profile in ("dev", "svg-opt3"):
        settings = manifest["profile"][profile]
        assert settings["debug-assertions"] is True and settings["opt-level"] == 0
        assert settings["debug"] == 1 and settings["incremental"] is False
        assert settings["package"]["feihong-svg-profile-probe"]["opt-level"] == 2
    optimized = manifest["profile"]["svg-opt3"]["package"]
    for name, version in VERSIONS.items():
        selector = f"{name}:{version}" if name.startswith("tiny-skia") else name
        assert optimized[selector]["opt-level"] == 3
    assert manifest["dependencies"]["resvg"] == {
        "version": "=0.48.1", "default-features": False,
        "features": ["text", "raster-images", "svgz"],
    }


def run(args, target, output):
    check_inputs()
    environment = dict(os.environ)
    # Cross-compilation or profile overrides in a caller's shell must not
    # silently turn this into an OHOS build or change the comparison.
    removed = sorted(key for key in environment if key.startswith("CARGO_PROFILE_"))
    for key in removed:
        environment.pop(key)
    environment.update(RUSTC=args.rustc, RUSTFLAGS="", CARGO_ENCODED_RUSTFLAGS="")
    rustc = subprocess.check_output([args.rustc, "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines()
                if line.startswith("host: "))
    icon_hashes = {path: sha256(REPO / path) for path in ICONS}
    compile_commands = {}
    for profile in ("dev", "svg-opt3"):
        command = [args.cargo, "build", "--offline", "--locked", "--verbose",
                   "--jobs", str(args.jobs), "--profile", profile,
                   "--target", host, "--target-dir", str(target),
                   "--manifest-path", str(FIXTURE / "Cargo.toml")]
        compile_commands[profile] = command
        print(f"Building independent host SVG fixture: {profile}", flush=True)
        execute(command, output / f"{profile}-build.log", environment,
                args.build_timeout, target)

    observed_flags = {}
    optimized_log = (output / "svg-opt3-build.log").read_text()
    for name in VERSIONS:
        lines = [line for line in optimized_log.splitlines()
                 if f"--crate-name {name.replace('-', '_')} " in line]
        for line in lines:
            assert "-C opt-level=3" in line and "-C debug-assertions=on" in line, name
        observed_flags[name] = {
            "declared_opt_level": 3, "declared_debug_assertions": True,
            "observed_rustc_invocations": len(lines),
            "cached_dependency": not lines,
        }

    runs = []
    for ordinal, profile in enumerate(ORDER):
        directory = output / f"{ordinal}-{profile}"
        directory.mkdir(mode=0o700)
        build_profile = "debug" if profile == "opt0" else "svg-opt3"
        binary = target / host / build_profile / "feihong-svg-profile-probe"
        if os.name == "nt":
            binary = binary.with_suffix(".exe")
        execute([str(binary), str(directory), str(REPO)],
                output / f"{ordinal}-{profile}.log", environment, 60, target)
        measure = json.loads((directory / "measurements.json").read_text())
        assert measure["harness_debug_assertions"] is True
        assert (measure["warmup"], measure["rounds"], measure["iterations_per_round"]) == (8, 7, 32)
        for path in directory.iterdir():
            path.chmod(0o600)
        runs.append({"profile": profile, "measure": measure, "directory": directory})
        print(f"Finished host run {ordinal}: {profile}", flush=True)

    expected_ids = {f"{name}-{size}" for name in ("search", "sync", "inbox")
                    for size in (24, 45, 54, 144)}
    for run in runs:
        cases = run["measure"]["cases"]
        assert len(cases) == 12 and {case["id"] for case in cases} == expected_ids
    comparison = []
    for baseline in runs[0]["measure"]["cases"]:
        case_id = baseline["id"]
        cases = [next(c for c in run["measure"]["cases"] if c["id"] == case_id)
                 for run in runs]
        for case in cases:
            for key in ("svg", "physical_size", "raw_color"):
                assert case[key] == baseline[key]
            for stage in STAGES:
                assert len(case[stage]) == 7 and all(value > 0 for value in case[stage])
        buffers = [(run["directory"] / f"{case_id}.rgba").read_bytes() for run in runs]
        assert all(buffer == buffers[0] for buffer in buffers), f"pixel mismatch: {case_id}"
        assert len(buffers[0]) == baseline["physical_size"] ** 2 * 4
        pixels = list(zip(*[iter(buffers[0])] * 4))
        assert any(0 < a < 255 for _, _, _, a in pixels)
        assert all(r <= a and g <= a and b <= a for r, g, b, a in pixels)
        timings = {
            profile: {stage: statistics.median([
                value for run, case in zip(runs, cases) if run["profile"] == profile
                for value in case[stage]
            ]) for stage in STAGES} for profile in ("opt0", "opt3")
        }
        comparison.append({
            "id": case_id, "svg": baseline["svg"], "physical_size": baseline["physical_size"],
            "pixel_equal": True, "bytes": len(buffers[0]),
            "rgba_sha256": hashlib.sha256(buffers[0]).hexdigest(), "timing": timings,
            "render_ratio_opt0_over_opt3": timings["opt0"][STAGES[0]] / timings["opt3"][STAGES[0]],
            "parse_ratio_opt0_over_opt3": timings["opt0"][STAGES[1]] / timings["opt3"][STAGES[1]],
        })
    assert icon_hashes == {path: sha256(REPO / path) for path in ICONS}, "SVG changed during run"
    snapshots = {
        "Cargo.toml": FIXTURE / "Cargo.toml", "Cargo.lock": FIXTURE / "Cargo.lock",
        "main.rs": FIXTURE / "src/main.rs", "run_svg_profile_contracts.py": Path(__file__).resolve(),
    }
    for name, source in snapshots.items():
        destination = output / name
        destination.write_bytes(source.read_bytes())
        destination.chmod(0o600)
    report = {
        "status": "pass", "host_only": True, "device_fps_or_hap_acceptance": False,
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "host": {"system": platform.system(), "machine": platform.machine(), "rustc": rustc.strip()},
        "versions": VERSIONS, "features": ["text", "raster-images", "svgz"],
        "scope": "Production search/sync/inbox SVG at default currentColor black; allocation + resvg raster mirrors Slint 1.18.1 ParsedSVG sizing and transform.",
        "baseline_profile": {"name": "dev", "dependencies_opt_level": 0, "debug_assertions": True, "debug_info": 1, "harness_opt_level": 2},
        "optimized_profile": {"name": "svg-opt3", "other_dependencies_opt_level": 0, "debug_assertions": True, "debug_info": 1, "harness_opt_level": 2, "overrides": observed_flags},
        "compile_commands": compile_commands, "removed_profile_environment_keys": removed,
        "measurement": {"order": ORDER, "warmup_renders": 8, "rounds_per_process": 7,
                        "processes_per_profile": 2, "iterations_per_round": 32,
                        "statistic": "median of 14 batch averages per profile/case",
                        "units": "microseconds per iteration", "timing_assertions": False},
        "pixel_cases": 12, "pixel_mismatches": 0, "cases": comparison,
        "source_sha256": icon_hashes,
        "fixture_sha256": {name: sha256(source) for name, source in snapshots.items()},
        "limitations": [
            "Synthetic host allocations and repeated raster calls do not prove device frame rate.",
            "Runs are interleaved; host load is not controlled and all timings are retained.",
            "No SVG text/raster-image assets, tiny-skia 0.11.4, or full mail UI is tested.",
            "Default black premultiplied raster is compared; downstream Slint colorization is outside the fixture.",
        ],
    }
    write_private(output / "validation.json", json.dumps(report, indent=2) + "\n")
    print(f"PASS: 12 exact pixel cases; host-only timings; {output / 'validation.json'}")
    for case in comparison:
        print(f"{case['id']}: opt0 {case['timing']['opt0'][STAGES[0]]:.3f} us; "
              f"opt3 {case['timing']['opt3'][STAGES[0]]:.3f} us; "
              f"ratio {case['render_ratio_opt0_over_opt3']:.2f}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo", default="cargo")
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--build-timeout", type=int, default=180)
    parser.add_argument("--target-dir", type=Path, help="optional isolated reusable host target")
    parser.add_argument("--output-dir", type=Path,
                        default=REPO / "target/ohos-host-contracts/svg-profile")
    args = parser.parse_args()
    if args.jobs < 1 or args.build_timeout < 1:
        parser.error("jobs and build timeout must be positive")
    output = args.output_dir.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error("output directory must be new or empty; preserve previous evidence")
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    if args.target_dir:
        target = args.target_dir.resolve()
        if target == output or target in output.parents or output in target.parents:
            parser.error("target and evidence directories must be separate")
        target.mkdir(parents=True, exist_ok=True)
        run(args, target, output)
    else:
        with tempfile.TemporaryDirectory(prefix="feihong-svg-profile-") as directory:
            run(args, Path(directory), output)


if __name__ == "__main__":
    main()
