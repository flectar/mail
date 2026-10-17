#!/usr/bin/env bash
# Verify the vendored fontique tree against its pinned upstream archive and
# patch series. Mirrors scripts/blitz-patches.sh for a single crate.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
PATCH_DIR="$REPO_ROOT/patches/fontique"
METADATA_FILE="$PATCH_DIR/upstream.toml"
SERIES_FILE="$PATCH_DIR/series"
CRATE=fontique

usage() {
    printf 'usage: %s verify\n' "$0" >&2
    exit 2
}

[[ $# -eq 1 && $1 == verify ]] || usage

version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$METADATA_FILE")
expected_checksum=$(awk -v section="[crates.${CRATE}]" '
    $0 == section { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^sha256 = / {
        value = $0
        sub(/^sha256 = "/, "", value)
        sub(/"$/, "", value)
        print value
        exit
    }
' "$METADATA_FILE")
if [[ -z "$version" || ! "$expected_checksum" =~ ^[0-9a-f]{64}$ ]]; then
    printf 'Missing or invalid metadata in %s\n' "$METADATA_FILE" >&2
    exit 1
fi

work_root=$(mktemp -d "${TMPDIR:-/tmp}/flectar-fontique-verify.XXXXXX")
trap 'rm -rf "$work_root"' EXIT

archive="$work_root/$CRATE-$version.crate"
curl --fail --silent --show-error --location --retry 3 \
    --output "$archive" \
    "https://static.crates.io/crates/${CRATE}/${CRATE}-${version}.crate"
actual_checksum=$(sha256sum "$archive" | awk '{ print $1 }')
if [[ "$actual_checksum" != "$expected_checksum" ]]; then
    printf 'Checksum mismatch for %s %s\nexpected: %s\nactual:   %s\n' \
        "$CRATE" "$version" "$expected_checksum" "$actual_checksum" >&2
    exit 1
fi

mkdir -p "$work_root/upstream/crates/$CRATE" "$work_root/patched"
tar -xzf "$archive" -C "$work_root/upstream/crates/$CRATE" --strip-components=1
cp -R "$work_root/upstream/crates/." "$work_root/patched/crates/" 2>/dev/null \
    || { mkdir -p "$work_root/patched/crates"; cp -R "$work_root/upstream/crates/." "$work_root/patched/crates/"; }

while IFS= read -r patch || [[ -n "$patch" ]]; do
    [[ -z "$patch" || "$patch" == \#* ]] && continue
    if ! git -C "$work_root/patched" apply --check "$PATCH_DIR/$patch"; then
        printf 'Patch does not apply: %s\n' "$patch" >&2
        exit 1
    fi
    git -C "$work_root/patched" apply "$PATCH_DIR/$patch"
done < "$SERIES_FILE"

differences=$(diff -qr "$work_root/patched/crates/$CRATE" "$REPO_ROOT/crates/$CRATE" \
    | grep -vE '(^|/)\.cargo-ok$|: \.cargo-ok$' || true)
if [[ -n "$differences" ]]; then
    printf 'Checked-in %s differs from the documented patch result:\n%s\n' "$CRATE" "$differences" >&2
    exit 1
fi

for requirement in \
    "fontique = { path = \"crates/fontique\" }"; do
    if ! grep -Fqx "$requirement" "$REPO_ROOT/Cargo.toml"; then
        printf 'Root Cargo.toml must contain: %s\n' "$requirement" >&2
        exit 1
    fi
done
if ! grep -Fqx 'fontique = { path = "../../crates/fontique" }' "$REPO_ROOT/platform/ohos/Cargo.toml"; then
    printf 'platform/ohos/Cargo.toml must point its fontique patch at ../../crates/fontique\n' >&2
    exit 1
fi

printf 'Fontique patch queue verified (%s %s).\n' "$CRATE" "$version"
