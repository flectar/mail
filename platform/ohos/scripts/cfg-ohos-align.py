#!/usr/bin/env python3
"""Align Flectar Mail's platform cfg gates with OpenHarmony.

OpenHarmony reports `target_os = "linux"` and `target_env = "ohos"`, so every
existing gate that splits desktop (linux) from mobile (android/ios) puts OHOS
on the desktop side. This tool performs the mechanical part of the M3
convergence:

  * mobile sets gain `target_env = "ohos"`:
      not(any(target_os = "android", target_os = "ios"))            -> + ohos
      not(any(target_os = "android", target_os = "ios", macos))     -> + ohos
      any(target_os = "android", target_os = "ios", feature=flatpak)-> + ohos
      any(target_os = "android", target_os = "ios")                 -> + ohos
  * the desktop secret-service dev fallback excludes OHOS:
      all(target_os = "linux", debug_assertions)                    -> + not(ohos)

Desktop-linux-only modules that must not run on OHOS are handled explicitly
per file (browser portals, X11 primary-selection clipboard).

Usage:
  python3 platform/ohos/scripts/cfg-ohos-align.py --dry-run   # default
  python3 platform/ohos/scripts/cfg-ohos-align.py --apply
"""

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]

SUBSTITUTIONS = [
    # Order matters: the `not(any(...))` and multi-element variants first so the
    # bare `any(android, ios)` rule only sees the remaining sites.
    (
        re.compile(
            r'not\(\s*any\(\s*target_os = "android",\s*target_os = "ios"\s*,?\s*\)\s*\)'
        ),
        'not(any(target_os = "android", target_os = "ios", target_env = "ohos"))',
    ),
    (
        re.compile(
            r'not\(\s*any\(\s*target_os = "android",\s*target_os = "ios",\s*target_os = "macos"\s*,?\s*\)\s*\)'
        ),
        'not(any(target_os = "android", target_os = "ios", target_os = "macos", target_env = "ohos"))',
    ),
    (
        re.compile(
            r'any\(\s*target_os = "android",\s*target_os = "ios",\s*feature = "flatpak"\s*,?\s*\)'
        ),
        'any(target_os = "android", target_os = "ios", target_env = "ohos", feature = "flatpak")',
    ),
    (
        re.compile(r'any\(\s*target_os = "android",\s*target_os = "ios"\s*,?\s*\)'),
        'any(target_os = "android", target_os = "ios", target_env = "ohos")',
    ),
    (
        re.compile(r'all\(\s*target_os = "linux",\s*debug_assertions\s*,?\s*\)'),
        'all(target_os = "linux", debug_assertions, not(target_env = "ohos"))',
    ),
]

# Per-file explicit transforms for desktop-only modules.
EXPLICIT = {
    "src/browser.rs": [
        ('#[cfg(target_os = "linux")]',
         '#[cfg(all(target_os = "linux", not(target_env = "ohos")))]'),
        ('#[cfg(not(target_os = "linux"))]',
         '#[cfg(any(not(target_os = "linux"), target_env = "ohos"))]'),
    ],
    "src/reader_clipboard.rs": [
        ('#[cfg(target_os = "linux")]',
         '#[cfg(all(target_os = "linux", not(target_env = "ohos")))]'),
    ],
}

TARGETS = [REPO / "src", REPO / "crates" / "flectar-mail-core" / "src"]


def files():
    for root in TARGETS:
        yield from sorted(root.rglob("*.rs"))


def main():
    apply = "--apply" in sys.argv
    total = 0
    touched = []
    for path in files():
        text = path.read_text()
        original = text
        for pattern, replacement in SUBSTITUTIONS:
            text, n = pattern.subn(replacement, text)
            if n:
                total += n
                touched.append((path.relative_to(REPO), n, pattern.pattern[:50]))
        rel = path.relative_to(REPO).as_posix()
        if rel in EXPLICIT:
            for old, new in EXPLICIT[rel]:
                n = text.count(old)
                text = text.replace(old, new)
                if n:
                    total += n
                    touched.append((path.relative_to(REPO), n, old))
        if text != original:
            print(f"{'APPLY' if apply else 'DRY  '} {path.relative_to(REPO)}")
            if apply:
                path.write_text(text)
    for rel, n, pat in touched:
        print(f"  {n:3d} x {rel}: {pat}")
    print(f"{'applied' if apply else 'planned'}: {total} substitutions")
    if not apply:
        print("run again with --apply to write")


if __name__ == "__main__":
    main()
