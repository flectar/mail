#!/usr/bin/env python3
"""Verify the production canvas conversion with exact host Slint 1.18.1.

Uses existing dependency rlibs. Does not build the app, start it, or use a device.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile

from run_host_contracts import dependency, execute


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deps-dir", type=Path, required=True)
    parser.add_argument("--slint-rlib")
    parser.add_argument("--core-rlib")
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--output-dir", type=Path,
                        default=Path(__file__).resolve().parents[3] / "target/ohos-host-contracts/opaque-tile")
    args = parser.parse_args()
    deps, output = args.deps_dir.resolve(), args.output_dir.resolve()
    slint = dependency(deps, args.slint_rlib, "slint", "slint")
    core = dependency(deps, args.core_rlib, "i_slint_core", "i-slint-core")
    repo = Path(__file__).resolve().parents[3]
    source = repo / "src/renderer/opaque_tile.rs"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="feihong-opaque-tile-") as directory:
        driver, binary = Path(directory) / "driver.rs", Path(directory) / "contracts"
        driver.write_text("#![allow(dead_code)]\n#[path=" + json.dumps(str(source))
                          + "] mod opaque_tile;\n")
        command = [args.rustc, "--edition=2024", "--test", str(driver), "-L", f"dependency={deps}",
                   "--extern", f"slint={slint}", "--extern", f"i_slint_core={core}", "-o", str(binary)]
        environment = dict(os.environ)
        if execute(command, output / "compile.log", environment, 120).returncode:
            raise RuntimeError("canvas contract compilation failed")
        result = execute([str(binary), "--test-threads=1", "--nocapture"],
                         output / "tests.log", environment, 30)
        if result.returncode or "test result: ok. 3 passed; 0 failed" not in result.stdout:
            raise RuntimeError("canvas contracts failed or the expected tests did not run")
        print(result.stdout)
    report = {
        "passed_tests": 3,
        "slint_version": "1.18.1",
        "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "contracts": ["opaque channels and RGB8 representation",
                      "unexpected transparency preserves RGBA bytes",
                      "36 actual software-renderer comparisons: 3 scales, 4 opacities, 3 geometries; stride padding"],
        "dependencies": {"slint": str(slint), "i-slint-core": str(core)},
        "app_or_device_operations": False,
        "limits": "Pixel equivalence is verified on the host; this does not establish device performance.",
    }
    path = output / "validation.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    path.chmod(0o600)
    print(f"PASS: 3 opaque canvas host contracts; {path}")


if __name__ == "__main__":
    main()
