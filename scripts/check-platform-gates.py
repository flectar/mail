#!/usr/bin/env python3
"""Keep operating-system names out of shared code.

Shared code selects behaviour through the capability aliases declared in
build.rs (`mobile`, `desktop`, `linux_desktop`, ...). Only the platform layer
may name a mobile operating system, so adding a platform changes the alias
definitions and its own module instead of every call site. See
platform/README.md.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

SCANNED = ["build.rs", "src", "tests", "examples", "crates/flectar-mail-core", "crates/flectar-spell"]

# The platform layer is where operating systems are named on purpose.
PLATFORM_LAYER = [
    "build.rs",
    "src/platform",
    "crates/flectar-mail-core/build.rs",
]

MOBILE_OS = re.compile(r'target_os\s*=\s*"(android|ios)"|target_env\s*=\s*"ohos"')
# OpenHarmony also reports target_os = "linux". Application code uses
# `linux_desktop` (or `glibc`) so desktop-Linux code never leaks into it.
LINUX = re.compile(r'target_os\s*=\s*"linux"')

RULES = [
    (MOBILE_OS, None, "use the `mobile`/`desktop` aliases or move this into src/platform"),
    (LINUX, ["src", "tests", "examples"], "use the `linux_desktop` or `glibc` alias"),
]


def within(path, prefixes):
    return any(path == prefix or path.startswith(prefix + "/") for prefix in prefixes)


def violations(relative, text):
    if within(relative, PLATFORM_LAYER):
        return
    for number, line in enumerate(text.splitlines(), 1):
        for pattern, scope, hint in RULES:
            if (scope is None or within(relative, scope)) and pattern.search(line):
                yield f"{relative}:{number}: {hint}\n    {line.strip()}"


def main():
    found = []
    for entry in SCANNED:
        base = ROOT / entry
        paths = [base] if base.is_file() else sorted(base.rglob("*.rs"))
        for path in paths:
            relative = path.relative_to(ROOT).as_posix()
            found.extend(violations(relative, path.read_text(encoding="utf-8")))
    if found:
        print("Operating-system names outside the platform layer:", file=sys.stderr)
        print("\n".join(found), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
