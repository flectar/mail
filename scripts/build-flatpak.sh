#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
manifest="$project_dir/packaging/flatpak/com.flectar.mail.yml"
build_root="$project_dir/target/flatpak"
build_dir="$build_root/build"
repo_dir="$build_root/repo"
output="$build_root/flectar-mail.flatpak"

for command in flatpak flatpak-builder; do
  command -v "$command" >/dev/null || {
    printf 'Required tool is missing: %s\n' "$command" >&2
    exit 1
  }
done

rm -rf "$build_dir" "$repo_dir" "$output"
mkdir -p "$build_root"
builder_args=(
  --force-clean
  --disable-rofiles-fuse
  --user
  --assumeyes
  --repo="$repo_dir"
  --install-deps-from=flathub
)
if [[ -n "${SOURCE_DATE_EPOCH:-}" ]] && flatpak-builder --help | grep -- --override-source-date-epoch >/dev/null; then
  builder_args+=(--override-source-date-epoch="$SOURCE_DATE_EPOCH")
fi
flatpak-builder "${builder_args[@]}" "$build_dir" "$manifest"
flatpak build-bundle \
  --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
  "$repo_dir" "$output" com.flectar.mail
test -s "$output"
printf 'Built %s\n' "$output"
