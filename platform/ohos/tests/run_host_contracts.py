#!/usr/bin/env python3
"""Run the OHOS owner-loop contracts using already-built host Slint 1.18.1 rlibs.

No Cargo, app startup, device commands, or production-source edits are performed.
The native host hooks and old-ordering negative control exist only in a temp dir.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


TESTS = Path(__file__).resolve().parent
RUST = TESTS.parent / "rust"


def replace_once(source, before, after, label):
    if source.count(before) != 1:
        raise RuntimeError(f"{label}: expected exactly one source match; review the production change")
    return source.replace(before, after, 1)


def dependency(deps, explicit, crate, package):
    choices = [Path(explicit).resolve()] if explicit else sorted(deps.glob(f"lib{crate}-*.rlib"))
    if len(choices) != 1:
        raise RuntimeError(f"expected one {crate} rlib; specify --{'slint' if crate == 'slint' else 'core'}-rlib")
    library = choices[0]
    if library.parent != deps or not library.is_file():
        raise RuntimeError(f"{library}: must be an existing rlib inside --deps-dir")
    depinfo = library.with_name(library.name[3:]).with_suffix(".d")
    versions = set(re.findall(rf"/{re.escape(package)}-([0-9][^/\s]+)/", depinfo.read_text()))
    if versions != {"1.18.1"}:
        raise RuntimeError(f"{depinfo}: require exact {package} 1.18.1 dep-info, found {versions}")
    return library


def host_native(source):
    source = replace_once(source, """        let _ = damage;
        let _ = (generation, pixels, width, height);
        true""", """        if let Some(result) = crate::pacing::present(generation, pixels, width, height, damage) {
            return result;
        }
        let _ = damage;
        let _ = (generation, pixels, width, height);
        true""", "native host present hook")
    return replace_once(source, """    #[cfg(not(target_env = "ohos"))]
    {
        false
    }
}""", """    #[cfg(not(target_env = "ohos"))]
    {
        crate::pacing::request_frame().unwrap_or(false)
    }
}""", "native host VSync hook")


def post_present_negative(source):
    block = """                // Request the next tick before CPU drawing. If drawing spans
                // that tick, its queued Frame can immediately unblock the next
                // frame instead of waiting for another refresh after present.
                // A fast frame still waits for its one outstanding VSync.
                frame_ready = !native::request_frame();
"""
    source = replace_once(source, block, "", "early VSync request")
    anchor = """                if presented {
                    self.window.redraw.set(false);
                }
"""
    return replace_once(source, anchor, anchor + "                frame_ready = !native::request_frame();\n", "negative-control post-present request")


def input_clock_negative(source):
    before = "                timers_elapsed += update_clock(&performance);\n"
    if source.count(before) != 2:
        raise RuntimeError("input clock negative control: expected two before-dispatch clock updates")
    # Retain the render-time tick, restoring the original stale-input ordering.
    return source.replace(before, "")


def driver(native, backend):
    modules = {"native": native, "backend": backend,
               "framebuffer": RUST / "src/framebuffer.rs", "performance": RUST / "src/performance.rs",
               "fifo_contracts": TESTS / "fifo_contracts.rs", "pacing_contracts": TESTS / "pacing_contracts.rs",
               "input_clock_contracts": TESTS / "input_clock_contracts.rs"}
    code = "#![allow(dead_code)]\nfn diagnostics_enabled() -> bool { false }\n"
    for module, path in modules.items():
        code += f"#[path = {json.dumps(str(path), ensure_ascii=False)}] mod {module};\n"
    return code + "pub(crate) use pacing_contracts::hooks as pacing;\n"


def execute(command, output, env, timeout):
    result = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, timeout=timeout)
    output.write_text(result.stdout)
    output.chmod(0o600)
    if result.returncode:
        print(result.stdout)
    return result


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deps-dir", type=Path, required=True, help="host target debug/deps with Slint 1.18.1 rlibs and .d files")
    parser.add_argument("--slint-rlib", help="exact rlib when --deps-dir has multiple Slint builds")
    parser.add_argument("--core-rlib", help="exact rlib when --deps-dir has multiple i-slint-core builds")
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--output-dir", type=Path, default=TESTS.parents[2] / "target/ohos-host-contracts/evidence")
    args = parser.parse_args()
    deps, output = args.deps_dir.resolve(), args.output_dir.resolve()
    slint = dependency(deps, args.slint_rlib, "slint", "slint")
    core = dependency(deps, args.core_rlib, "i_slint_core", "i-slint-core")
    # Construct both variants before compiling: stale source anchors must fail loudly.
    native = host_native((RUST / "src/native.rs").read_text())
    backend_source = (RUST / "src/backend.rs").read_text()
    negative = post_present_negative(backend_source)
    identity_negative = replace_once(backend_source,
        "if framebuffer_identity != Some(identity) {", "if framebuffer_identity.is_none() {",
        "negative-control missing framebuffer invalidation")
    clock_negative = input_clock_negative(backend_source)
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_MANIFEST_DIR=str(RUST))
    commands = []
    with tempfile.TemporaryDirectory(prefix="feihong-host-contracts-") as directory:
        work = Path(directory)
        native_path = work / "native.rs"
        native_path.write_text(native)
        negative_path = work / "post_present_backend.rs"
        negative_path.write_text(negative)
        identity_path = work / "initial_frame_only_backend.rs"
        identity_path.write_text(identity_negative)
        clock_path = work / "stale_input_clock_backend.rs"
        clock_path.write_text(clock_negative)
        for name, backend in [("current", RUST / "src/backend.rs"), ("negative", negative_path),
                              ("identity-negative", identity_path), ("input-clock-negative", clock_path)]:
            harness, binary = work / f"{name}.rs", work / name
            harness.write_text(driver(native_path, backend))
            command = [args.rustc, "--edition=2024", "--test", str(harness), "-L", f"dependency={deps}",
                       "--extern", f"slint={slint}", "--extern", f"i_slint_core={core}", "-o", str(binary)]
            commands.append(command)
            if execute(command, output / f"{name}-compile.log", env, 120).returncode:
                raise RuntimeError(f"{name}: rustc failed; see compile log")
            run = [str(binary), "--test-threads=1", "--nocapture"]
            if name == "negative":
                run += ["pacing_contracts::early_vsync_during_present_unblocks_next_dirty_frame", "--exact"]
            elif name == "identity-negative":
                run += ["pacing_contracts::framebuffer_generation_change_repaints_warm_source_completely", "--exact"]
            elif name == "input-clock-negative":
                run += ["input_clock_contracts::queued_touch_events_observe_current_clock_before_dispatch", "--exact"]
            commands.append(run)
            result = execute(run, output / f"{name}-tests.log", env, 30)
            if name == "current":
                match = re.search(r"test result: ok\. (\d+) passed; 0 failed; 0 ignored", result.stdout)
                if result.returncode or not match or int(match[1]) < 20:
                    raise RuntimeError("current host contracts failed or fewer than 20 tests ran")
                passes = int(match[1])
                print(result.stdout)
            else:
                message = {"negative": "request must precede every presentation",
                           "identity-negative": "a changed framebuffer identity must force a complete source render",
                           "input-clock-negative": "due timers must run before real touch callbacks"}[name]
                if result.returncode != 101 or message not in result.stdout:
                    raise RuntimeError(f"{name}: negative control did not fail at the expected assertion")
    report = {"passed_tests": passes, "negative_control": "expected failure (exit 101)",
              "framebuffer_negative_control": "initial-frame-only invalidation fails generation contract (exit 101)",
              "input_clock_negative_control": "original post-input timer update fails due-timer-before-touch contract (exit 101)",
              "slint_version": "1.18.1", "production_source_edits": False,
              "cargo_or_device_operations": False, "commands": commands,
              "source_sha256": {str(p.relative_to(TESTS.parents[2])): digest(p)
                                for p in [RUST / f"src/{name}.rs" for name in ("backend", "native", "framebuffer", "performance")]
                                + [TESTS / "fifo_contracts.rs", TESTS / "pacing_contracts.rs",
                                   TESTS / "input_clock_contracts.rs", Path(__file__).resolve()]},
              "dependencies": {"slint": str(slint), "i-slint-core": str(core)},
              "limits": "Native request/present timing is simulated; host tests do not verify device VSync, NativeWindow, FPS or power."}
    report_path = output / "validation.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    report_path.chmod(0o600)
    print(f"PASS: {passes} host contracts and three expected negative controls; {report_path}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
        raise SystemExit(str(error))
