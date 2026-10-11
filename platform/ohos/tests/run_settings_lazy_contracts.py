#!/usr/bin/env python3
"""Exercise the actual SettingsDialog with existing exact Slint 1.18.1 host libs.

Temporary observer/command exports access private production state; they do not
bind or preserve that state. All original UI bodies and functions are compiled.
No Cargo, account, device or full-application build is involved.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import struct
import tempfile
import zlib
from run_host_contracts import dependency, replace_once

TESTS = Path(__file__).resolve().parent
ROOT = TESTS.parents[2]
SETTINGS = ROOT / "ui/dialogs/settings-dialog.slint"
THEME = ROOT / "ui/components/theme-editor.slint"
FIXTURE = TESTS / "settings_lazy_contracts.rs"
PROBE = TESTS / "settings_lazy_probe.slint"
PAGES = {"Contacts": "contacts-settings-scroll", "Accounts": "accounts-scroll",
         "Data": "data-scroll", "Notifications": "notifications-scroll",
         "Shortcuts": "shortcuts-scroll", "About": "about-scroll"}
MODALS = ["caldav", "imap", "profile-editor", "account-profile-picker"]


def render_png(ppm):
    raw = ppm.read_bytes()
    header, raw = raw.split(b"\n255\n", 1)
    magic, size = header.split(b"\n")
    if magic != b"P6":
        raise RuntimeError("expected fixture RGB PPM")
    width, height = map(int, size.split())
    if len(raw) != width * height * 3:
        raise RuntimeError("fixture pixel length differs from rendered geometry")
    def chunk(name, payload):
        return struct.pack("!I", len(payload)) + name + payload + struct.pack("!I", zlib.crc32(name + payload))
    scanlines = b"".join(b"\0" + raw[y * width * 3:(y + 1) * width * 3] for y in range(height))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack("!IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(scanlines)) + chunk(b"IEND", b"")
    path = ppm.with_suffix(".png")
    path.write_bytes(png)
    path.chmod(0o600); ppm.chmod(0o600)
    return str(path)


def absolute_resources(source, path):
    # Rebase only file references when an instrumented copy lives in /tmp.
    source = re.sub(r'"(\.\.?/[^"\n]+)"',
                    lambda m: json.dumps(str((path.parent / m[1]).resolve())), source)
    return re.sub(r'(from )"([^/"\n]+\.slint)"',
                  lambda m: m[0] if m[2] == "std-widgets.slint" else m[1] + json.dumps(str(path.parent / m[2])), source)


def instrument_settings(source, theme):
    hooks = """
    // Host contract instrumentation only: delegates to original functions.
    callback contract-created(string);
    callback contract-scrolled(string, float);
    in-out property <string> contract-scroll-page: "";
    in-out property <float> contract-scroll-value: 0;
    in-out property <bool> contract-scroll-write: false;
    in-out property <int> contract-scroll-epoch: 0;
    callback contract-scroll(string, float, bool);
    contract-scroll(page, value, write) => {
        root.contract-scroll-page = page;
        root.contract-scroll-value = value;
        root.contract-scroll-write = write;
        root.contract-scroll-epoch += 1;
    }
    callback contract-search(string);
    contract-search(value) => { root.update-settings-search(value); }
    callback contract-preview(color);
    contract-preview(value) => { theme-editor.contract-preview(value); }
    callback contract-modal(string, bool);
    contract-modal(kind, open) => {
        if kind == "caldav" {
            if open { root.open-caldav-settings(root.connected_accounts[0]); }
            else { root.close-caldav-settings(); }
        } else if kind == "imap" {
            if open { root.open_new_imap(); }
            else { root.imap_form_open = false; }
        } else if kind == "profile-editor" {
            if open { root.open-new-profile("Contract profile", #3b82f6); }
            else { root.close-profile-editor(); }
        } else {
            if open { root.open-account-profile-picker(root.connected_accounts[0]); }
            else { root.close-account-profile-picker(); }
        }
    }
    out property <string> contract-profile-name: root.profile-editor-name;
    out property <int> contract-picker-id: root.account-profile-picker-id;
    out property <bool> contract-profile-open: root.profile-editor-open;
    out property <bool> contract-picker-open: root.account-profile-picker-open;
    in property <int> contract-command-epoch: 0;
    in property <string> contract-command-kind: "";
    in property <string> contract-command-name: "";
    in property <float> contract-command-value: 0;
    in property <bool> contract-command-flag: false;
    in property <color> contract-command-color;
    callback contract-status(int, string, int, bool, bool, bool);
    changed contract-command-epoch => {
        if root.contract-command-kind == "scroll" {
            root.contract-scroll(root.contract-command-name, root.contract-command-value, root.contract-command-flag);
        } else if root.contract-command-kind == "search" {
            root.contract-search(root.contract-command-name);
        } else if root.contract-command-kind == "modal" {
            root.contract-modal(root.contract-command-name, root.contract-command-flag);
        } else if root.contract-command-kind == "preview" {
            root.contract-preview(root.contract-command-color);
        } else if root.contract-command-kind == "report" {
            root.contract-status(root.caldav_account_id, root.profile-editor-name,
                root.account-profile-picker-id, root.profile-editor-open,
                root.account-profile-picker-open, root.imap_form_open);
        }
    }
"""
    source = replace_once(source, "export component SettingsDialog inherits Rectangle {",
                          "export component SettingsDialog inherits Rectangle {" + hooks, "private commands")
    source = replace_once(source, "general-scroll := ScrollView {",
                          'general-scroll := ScrollView {\n init => { root.contract-created("General"); }',
                          "General construction observer")
    for name, element in PAGES.items():
        latch = name.lower() + "-page-loaded"
        before = f"init => {{ root.{latch} = true; }}"
        source = replace_once(source, before,
                              before[:-2] + f' root.contract-created("{name}"); }}', name + " construction observer")
        scroll_hook = f"""
        private property <int> contract-observed-epoch: root.contract-scroll-epoch;
        changed contract-observed-epoch => {{
            if root.contract-scroll-page == "{name}" {{
                if root.contract-scroll-write {{ self.viewport-y = root.contract-scroll-value * 1px; }}
                root.contract-scrolled("{name}", self.viewport-y / 1px);
            }}
        }}
"""
        source = replace_once(source, f"{element} := ScrollView {{",
                              f"{element} := ScrollView {{" + scroll_hook, name + " actual viewport commands")
    for name in MODALS:
        before = f"init => {{ root.{name}-modal-loaded = true; }}"
        source = replace_once(source, before,
                              before[:-2] + f' root.contract-created("{name}"); }}', name + " modal observer")
    source = absolute_resources(source, SETTINGS)
    return replace_once(source, json.dumps(str(THEME)), json.dumps(str(theme)), "instrumented actual ThemeEditor import")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deps-dir", type=Path, required=True)
    parser.add_argument("--slint-rlib")
    parser.add_argument("--build-deps-dir", type=Path, default=ROOT / "target/debug/deps")
    parser.add_argument("--slint-build-rlib", required=True)
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "target/ohos-host-contracts/settings-lazy")
    parser.add_argument("--skip-negative", action="store_true", help="development only; full acceptance requires negative control")
    args = parser.parse_args()
    deps, output = args.deps_dir.resolve(), args.output_dir.resolve()
    slint = dependency(deps, args.slint_rlib, "slint", "slint")
    build_deps = args.build_deps_dir.resolve()
    slint_build = dependency(build_deps, args.slint_build_rlib, "slint_build", "slint-build")
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, SLINT_STYLE="fluent", SETTINGS_CONTRACT_OUTPUT=str(output))
    results = []
    production_hash = hashlib.sha256(SETTINGS.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="feihong-settings-lazy-") as directory:
        work = Path(directory)
        compiler_source = work / "compiler.rs"
        compiler = work / "compiler"
        compiler_source.write_text("""fn main() {
            let args: Vec<_> = std::env::args_os().collect();
            let config = slint_build::CompilerConfiguration::new().with_style("fluent".into())
                .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
            let inputs = slint_build::compile_with_output_path(&args[1], &args[2], config).unwrap();
            for path in inputs { println!("{}", path.display()); }
        }""")
        result = subprocess.run([args.rustc, "--edition=2024", str(compiler_source), "-L", f"dependency={build_deps}",
                                 "--extern", f"slint_build={slint_build}", "-o", str(compiler)],
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=120)
        log = output / "compiler-compile.log"
        log.write_text(result.stdout); log.chmod(0o600)
        if result.returncode:
            print(result.stdout)
            raise RuntimeError("exact Slint build helper compilation failed")
        theme = work / "theme-editor.slint"
        theme_source = replace_once(THEME.read_text(),
            "export component ThemeEditor inherits Rectangle {", """export component ThemeEditor inherits Rectangle {
            callback contract-preview(color);
            contract-preview(value) => {
                root.editing-dark = false;
                root.selected-token = "primary";
                root.set-current-value(value);
            }
""", "actual ThemeEditor method export")
        theme.write_text(absolute_resources(theme_source, THEME))
        current = instrument_settings(SETTINGS.read_text(), theme)
        variants = [("current", current)]
        if not args.skip_negative:
            negative = current
            for name in PAGES:
                negative = negative.replace(f" || root.{name.lower()}-page-loaded", "")
            variants.append(("destroy-on-switch", negative))
        for name, source in variants:
            settings = work / f"{name}.slint"
            settings.write_text(source)
            probe = replace_once(PROBE.read_text(), '"SETTINGS_SOURCE"', json.dumps(str(settings)), "production UI import")
            probe = replace_once(probe, '"MODELS_SOURCE"', json.dumps(str(ROOT / "ui/models.slint")), "production model import")
            probe = replace_once(probe, '"DESIGN_SOURCE"', json.dumps(str(ROOT / "ui/design-system.slint")), "production globals import")
            probe_path, generated = work / f"{name}-probe.slint", work / f"{name}-generated.rs"
            probe_path.write_text(probe)
            result = subprocess.run([str(compiler), str(probe_path), str(generated)], env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=120)
            log = output / f"{name}-generate.log"
            log.write_text(result.stdout); log.chmod(0o600)
            if result.returncode:
                print(result.stdout)
                raise RuntimeError(f"{name} production source generation failed")
            harness = replace_once(FIXTURE.read_text(), '"GENERATED_UI"', json.dumps(str(generated)), "actual generated UI")
            source_path, binary = work / f"{name}.rs", work / name
            source_path.write_text(harness)
            commands = [[args.rustc, "--edition=2024", "--test", str(source_path), "-C", "debuginfo=0",
                         "-C", "codegen-units=256", "-L", f"dependency={deps}", "--extern", f"slint={slint}", "-o", str(binary)],
                        [str(binary), "--test-threads=1", "--nocapture"]]
            if name != "current":
                commands[1] += ["visited_page_retains_its_actual_scrollview", "--exact"]
            for phase, command in zip(["compile", "run"], commands):
                result = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        text=True, timeout=360)
                log = output / f"{name}-{phase}.log"
                log.write_text(result.stdout)
                log.chmod(0o600)
                expected = 101 if phase == "run" and name != "current" else 0
                passed = result.returncode == expected
                if expected == 101:
                    passed = passed and "visited page must keep its actual ScrollView instance" in result.stdout
                results.append({"variant": name, "phase": phase, "returncode": result.returncode,
                                "expected": expected, "pass": passed, "command": command})
                if not passed:
                    print(result.stdout)
                    raise RuntimeError(f"{name}/{phase} failed; inspect {log}")
    if hashlib.sha256(SETTINGS.read_bytes()).hexdigest() != production_hash:
        raise RuntimeError("production SettingsDialog changed during validation; rerun before freeze")
    screenshots = [render_png(path) for path in sorted(output.glob("*.ppm"))]
    report = {"status": "pass", "slint_version": "1.18.1", "host_only": True,
              "production_component": "SettingsDialog with complete original dependent UI",
              "instrumentation": "Temporary observers of actual construction and viewport-y, delegating private method exports; no retention bindings added.",
              "source_sha256": {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
                                for p in [SETTINGS, THEME, FIXTURE, PROBE, Path(__file__).resolve()]},
              "negative_control": "not run" if args.skip_negative else "destroy-on-switch fails actual ScrollView retention as expected",
              "screenshots": screenshots,
              "results": results,
              "limits": "Actual SVG/images/fonts are pre-embedded for the cached host software renderer; resource representation differs from the device. This proves no device timing, appearance or FPS."}
    path = output / "validation.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    path.chmod(0o600)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
