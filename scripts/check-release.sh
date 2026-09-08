#!/usr/bin/env bash
# Fast, offline release checks. Does not compile the app or contact GitHub.
set -euo pipefail
project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$project_dir"

for command in python3 desktop-file-validate dpkg-deb dpkg-shlibdeps; do
  command -v "$command" >/dev/null || { printf 'Required tool is missing: %s\n' "$command" >&2; exit 1; }
done
appstream="${APPSTREAMCLI:-appstreamcli}"
command -v "$appstream" >/dev/null || { printf 'AppStream validator is missing: %s\n' "$appstream" >&2; exit 1; }

# Python 3.11+ is required by the release scripts (tomllib, hashlib.file_digest).
python3 -c 'import sys; assert sys.version_info >= (3, 11), "Python 3.11+ is required"'
python3 scripts/release-metadata.py "$@"
GITHUB_OUTPUT= python3 scripts/test-release.py
desktop-file-validate resources/com.flectar.mail.desktop
"$appstream" --version
"$appstream" validate --no-net resources/com.flectar.mail.metainfo.xml

python3 - <<'PY'
import ast
import struct
import subprocess
from pathlib import Path

for path in Path("scripts").glob("*.sh"):
    subprocess.run(["bash", "-n", str(path)], check=True)
for path in Path("scripts").glob("*.py"):
    ast.parse(path.read_text(), filename=str(path))
icon = Path("resources/app-icon/flectar-mail.ico").read_bytes()
reserved, kind, count = struct.unpack_from("<HHH", icon)
assert reserved == 0 and kind == 1 and count > 0, "Windows ICO header is invalid"
for index in range(count):
    size, offset = struct.unpack_from("<II", icon, 6 + index * 16 + 8)
    assert size > 0 and offset + size <= len(icon), "Windows ICO image is missing"
print("Release preflight passed; no application build was needed.")
PY
