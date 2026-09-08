#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
PATCH_DIR="$REPO_ROOT/patches/blitz"
METADATA_FILE="$PATCH_DIR/upstream.toml"
SERIES_FILE="$PATCH_DIR/series"

usage() {
    cat <<'EOF'
Usage:
  scripts/blitz-patches.sh verify
  scripts/blitz-patches.sh stage <version>

verify downloads the pinned crates, applies the patch queue in a temporary
directory, and checks that the result exactly matches crates/blitz-*.

stage prepares pristine and patched trees for a new crates.io version. It does
not modify the repository. If a patch conflicts, the staged tree and reject
files are retained for manual resolution.
EOF
}

metadata_value() {
    local key=$1
    sed -n "s/^${key} = \"\([^\"]*\)\"$/\1/p" "$METADATA_FILE"
}

metadata_checksum() {
    local crate=$1
    awk -v section="[crates.${crate}]" '
        $0 == section { in_section = 1; next }
        /^\[/ { in_section = 0 }
        in_section && /^sha256 = / {
            value = $0
            sub(/^sha256 = "/, "", value)
            sub(/"$/, "", value)
            print value
            exit
        }
    ' "$METADATA_FILE"
}

sha256_file() {
    local path=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{ print $1 }'
    else
        shasum -a 256 "$path" | awk '{ print $1 }'
    fi
}

registry_checksum() {
    local crate=$1
    local version=$2
    local record
    record=$(curl --fail --silent --show-error --location --retry 3 \
        "https://index.crates.io/bl/it/${crate}" \
        | { grep -F "\"vers\":\"${version}\"" || true; } \
        | tail -n 1)
    if [[ -z "$record" ]]; then
        printf 'No crates.io release found for %s %s\n' "$crate" "$version" >&2
        return 1
    fi
    if [[ "$record" == *'"yanked":true'* ]]; then
        printf 'Refusing to stage yanked release %s %s\n' "$crate" "$version" >&2
        return 1
    fi
    printf '%s\n' "$record" | sed -n 's/.*"cksum":"\([0-9a-f]*\)".*/\1/p'
}

download_and_extract() {
    local crate=$1
    local version=$2
    local expected_checksum=$3
    local work_root=$4
    local archive="$work_root/${crate}-${version}.crate"
    local destination="$work_root/upstream/crates/${crate}"
    local actual_checksum

    curl --fail --silent --show-error --location --retry 3 \
        --output "$archive" \
        "https://static.crates.io/crates/${crate}/${crate}-${version}.crate"
    actual_checksum=$(sha256_file "$archive")
    if [[ "$actual_checksum" != "$expected_checksum" ]]; then
        printf 'Checksum mismatch for %s %s\nexpected: %s\nactual:   %s\n' \
            "$crate" "$version" "$expected_checksum" "$actual_checksum" >&2
        return 1
    fi

    mkdir -p "$destination"
    tar -xzf "$archive" -C "$destination" --strip-components=1
}

apply_series() {
    local patched_root=$1
    local retain_rejects=$2
    local patch

    while IFS= read -r patch || [[ -n "$patch" ]]; do
        [[ -z "$patch" || "$patch" == \#* ]] && continue
        if ! git -C "$patched_root" apply --check "$PATCH_DIR/$patch"; then
            printf 'Patch does not apply: %s\n' "$patch" >&2
            if [[ "$retain_rejects" == true ]]; then
                git -C "$patched_root" apply --reject "$PATCH_DIR/$patch" || true
                printf 'Resolve the .rej files under %s and regenerate this patch.\n' \
                    "$patched_root" >&2
            fi
            return 1
        fi
        git -C "$patched_root" apply "$PATCH_DIR/$patch"
    done < "$SERIES_FILE"
}

prepare_trees() {
    local version=$1
    local mode=$2
    local work_root=$3
    local crate
    local checksum

    mkdir -p "$work_root/upstream/crates" "$work_root/patched/crates"
    for crate in blitz-dom blitz-paint; do
        if [[ "$mode" == verify ]]; then
            checksum=$(metadata_checksum "$crate")
        else
            checksum=$(registry_checksum "$crate" "$version")
        fi
        if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]]; then
            printf 'Invalid or missing checksum for %s: %s\n' "$crate" "$checksum" >&2
            return 1
        fi
        printf '%s %s: %s\n' "$crate" "$version" "$checksum"
        download_and_extract "$crate" "$version" "$checksum" "$work_root"
    done

    cp -R "$work_root/upstream/crates/." "$work_root/patched/crates/"
}

verify_revision() {
    local upstream_root=$1
    local expected_revision=$2
    local crate
    local actual_revision

    for crate in blitz-dom blitz-paint; do
        actual_revision=$(awk -F'"' '/"sha1"/ { print $4; exit }' \
            "$upstream_root/crates/$crate/.cargo_vcs_info.json")
        if [[ "$actual_revision" != "$expected_revision" ]]; then
            printf 'Upstream revision mismatch for %s: expected %s, got %s\n' \
                "$crate" "$expected_revision" "$actual_revision" >&2
            return 1
        fi
    done
}

verify_tree() {
    local patched_root=$1
    local crate
    local failed=false

    for crate in blitz-dom blitz-paint; do
        if ! diff -qr "$patched_root/crates/$crate" "$REPO_ROOT/crates/$crate"; then
            printf 'Checked-in %s differs from the documented patch result.\n' "$crate" >&2
            git diff --no-index --stat \
                "$patched_root/crates/$crate" "$REPO_ROOT/crates/$crate" || true
            failed=true
        fi
    done
    [[ "$failed" == false ]]
}

verify_manifest_wiring() {
    local version=$1
    local crate
    local requirement

    for crate in blitz-dom blitz-html blitz-paint blitz-traits; do
        requirement="${crate} = \"=${version}\""
        if [[ "$crate" == blitz-dom ]]; then
            requirement="${crate} = { version = \"=${version}\", features = [\"floats\"] }"
        fi
        if ! grep -Fqx "$requirement" "$REPO_ROOT/Cargo.toml"; then
            printf 'Root Cargo.toml must contain: %s\n' "$requirement" >&2
            return 1
        fi
    done
    for requirement in \
        'blitz-dom = { path = "crates/blitz-dom" }' \
        'blitz-paint = { path = "crates/blitz-paint" }'; do
        if ! grep -Fqx "$requirement" "$REPO_ROOT/Cargo.toml"; then
            printf 'Root Cargo.toml must contain: %s\n' "$requirement" >&2
            return 1
        fi
    done
    for requirement in \
        'blitz-dom = { path = "../../crates/blitz-dom" }' \
        'blitz-paint = { path = "../../crates/blitz-paint" }'; do
        if ! grep -Fqx "$requirement" "$REPO_ROOT/platform/android/Cargo.toml"; then
            printf 'Android Cargo.toml must contain: %s\n' "$requirement" >&2
            return 1
        fi
    done
}

command=${1:-verify}
case "$command" in
    verify)
        [[ $# -eq 1 ]] || { usage >&2; exit 2; }
        version=$(metadata_value version)
        revision=$(metadata_value revision)
        if [[ -z "$version" || ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
            printf 'Invalid metadata in %s\n' "$METADATA_FILE" >&2
            exit 1
        fi
        verify_manifest_wiring "$version"
        work_root=$(mktemp -d "${TMPDIR:-/tmp}/flectar-blitz-verify.XXXXXX")
        trap 'rm -rf -- "$work_root"' EXIT
        prepare_trees "$version" verify "$work_root"
        verify_revision "$work_root/upstream" "$revision"
        apply_series "$work_root/patched" false
        verify_tree "$work_root/patched"
        printf 'Blitz %s patch queue is reproducible.\n' "$version"
        ;;
    stage)
        [[ $# -eq 2 ]] || { usage >&2; exit 2; }
        version=$2
        if [[ ! "$version" =~ ^[0-9A-Za-z.+-]+$ ]]; then
            printf 'Invalid crate version: %s\n' "$version" >&2
            exit 2
        fi
        work_root=$(mktemp -d "${TMPDIR:-/tmp}/flectar-blitz-stage.XXXXXX")
        printf 'Staging Blitz %s in %s\n' "$version" "$work_root"
        prepare_trees "$version" stage "$work_root"
        if ! apply_series "$work_root/patched" true; then
            printf 'Staged trees retained at %s\n' "$work_root" >&2
            exit 1
        fi
        printf 'All existing patches applied. Staged trees retained at %s\n' "$work_root"
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
