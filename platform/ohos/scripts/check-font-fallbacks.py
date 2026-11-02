#!/usr/bin/env python3
"""Guard the duplicated non-Latin fallback script lists.

The Slint UI registration (`backend/src/fonts.rs`) and the Blitz/Parley mail
renderer (`src/renderer.rs`) intentionally use separate fontique collections,
so their script lists cannot share a Rust type. Keep the lists identical and
fail CI if one side gains a script without the other.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
PATHS = {
    "backend": ROOT / "platform/ohos/backend/src/fonts.rs",
    "renderer": ROOT / "src/renderer.rs",
}


def extract(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    match = re.search(
        r"FALLBACK_SCRIPTS\s*:\s*\[&\[u8;\s*4\];\s*\d+\]\s*=\s*\[(.*?)\];",
        text,
        re.DOTALL,
    )
    if not match:
        raise SystemExit(f"FALLBACK_SCRIPTS not found in {path}")
    tags = re.findall(r'b"([A-Za-z0-9]{4})"', match.group(1))
    if not tags:
        raise SystemExit(f"no script tags found in {path}")
    return tags


def main() -> int:
    lists = {name: extract(path) for name, path in PATHS.items()}
    backend = lists["backend"]
    renderer = lists["renderer"]
    if backend != renderer:
        print("font fallback script lists differ:", file=sys.stderr)
        print(f"  backend : {backend}", file=sys.stderr)
        print(f"  renderer: {renderer}", file=sys.stderr)
        return 1
    if len(set(backend)) != len(backend):
        print("font fallback script list contains duplicates", file=sys.stderr)
        return 1
    print(f"font fallback scripts match ({len(backend)} entries)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
