#!/usr/bin/env python3
"""Keeps the app's corners on the design system's values.

Every corner should follow `LayoutMetrics.corner-base` through the values
derived from it in ui/design-system.slint (docs/design-system.md), so that
the whole app turns squarer or rounder together. Corners written as a fixed
number of pixels don't. Files may keep the ones they have until their area
moves over, but none may gain more: this fails when a file has more fixed
corners than scripts/design-corners-baseline.json allows, and lowers the
allowance with --update once some are gone.
"""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts" / "design-corners-baseline.json"
FIXED = re.compile(r"\bborder-radius\s*:\s*[0-9.]+px\s*;")


def fixed_corners():
    counts = {}
    for path in sorted((ROOT / "ui").rglob("*.slint")):
        if any(part.startswith(".") for part in path.relative_to(ROOT).parts):
            continue
        count = len(FIXED.findall(path.read_text(encoding="utf-8")))
        if count:
            counts[path.relative_to(ROOT).as_posix()] = count
    return counts


def main():
    counts = fixed_corners()
    if sys.argv[1:] == ["--update"]:
        allowed = json.loads(BASELINE.read_text()) if BASELINE.exists() else counts
        lowered = {name: min(count, allowed.get(name, count)) for name, count in counts.items()}
        BASELINE.write_text(json.dumps(lowered, indent=2, sort_keys=True) + "\n")
        print(f"{sum(lowered.values())} fixed corners allowed in {len(lowered)} files")
        return 0
    allowed = json.loads(BASELINE.read_text())
    over = {name: (count, allowed.get(name, 0)) for name, count in counts.items() if count > allowed.get(name, 0)}
    for name, (count, limit) in sorted(over.items()):
        print(f"{name}: {count} corners in fixed pixels, at most {limit}. "
              "Use LayoutMetrics.corner-radius, container-radius or small-radius (docs/design-system.md).",
              file=sys.stderr)
    fewer = sum(allowed.get(name, 0) - counts.get(name, 0) for name in allowed if counts.get(name, 0) < allowed[name])
    if fewer:
        print(f"{fewer} fixed corners are gone; run scripts/check-design-corners.py --update to lower the allowance.")
    return 1 if over else 0


if __name__ == "__main__":
    sys.exit(main())
