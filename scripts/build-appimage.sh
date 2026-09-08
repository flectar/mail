#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
build_dir="$project_dir/target/appimage"
app_dir="$build_dir/flectar-mail.AppDir"
output_path="$build_dir/flectar-mail.AppImage"
staged_output="$build_dir/.flectar-mail.AppImage.new"
tools_dir="$build_dir/tools"

download_tool() {
  local url="$1"
  local destination="$2"
  if [[ -x "$destination" ]]; then
    return
  fi
  mkdir -p "$(dirname "$destination")"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error "$url" --output "$destination.new"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$destination.new"
  else
    printf 'curl or wget is required to obtain the AppImage deployment tools.\n' >&2
    exit 1
  fi
  chmod +x "$destination.new"
  mv -f "$destination.new" "$destination"
}

mkdir -p "$build_dir"
# The normal distributable includes the runtime-gated remote-image capability
# but stays on the software-rendered low-memory profile. A benchmark artifact
# can opt into the GPU renderer explicitly:
# FLECTAR_APP_FEATURES=gpu-renderer ./scripts/build-appimage.sh
app_features="${FLECTAR_APP_FEATURES:-}"
build_args=(--locked --bin flectar-mail --manifest-path "$project_dir/Cargo.toml" --release --no-default-features --features remote-content)
if [[ -n "$app_features" ]]; then
  build_args+=(--features "$app_features")
fi
# The controls are app-owned and the Linux shell uses winit directly.
cargo build "${build_args[@]}"

if ldd "$project_dir/target/release/flectar-mail" | grep -q 'libQt'; then
  printf 'The release binary unexpectedly links Qt; refusing to package it.\n' >&2
  exit 1
fi

rm -rf "$app_dir"
mkdir -p \
  "$app_dir/usr/bin" \
  "$app_dir/usr/share/applications" \
  "$app_dir/usr/share/doc/flectar-mail/google-sans-flex" \
  "$app_dir/usr/share/icons/hicolor/512x512/apps" \
  "$app_dir/usr/share/icons/hicolor/scalable/apps" \
  "$app_dir/usr/share/metainfo"
cp "$project_dir/target/release/flectar-mail" "$app_dir/usr/bin/flectar-mail"
cp "$project_dir/resources/com.flectar.mail.desktop" "$app_dir/usr/share/applications/com.flectar.mail.desktop"
cp "$project_dir/resources/app-icon/flectar-mail-masked-512.png" "$app_dir/usr/share/icons/hicolor/512x512/apps/com.flectar.mail.png"
cp "$project_dir/resources/app-icon/flectar-mail-masked.svg" "$app_dir/usr/share/icons/hicolor/scalable/apps/com.flectar.mail.svg"
cp "$project_dir/resources/com.flectar.mail.metainfo.xml" \
  "$app_dir/usr/share/metainfo/com.flectar.mail.metainfo.xml"
cp "$project_dir/LICENSE" "$app_dir/usr/share/doc/flectar-mail/LICENSE"
cp -R "$project_dir/LICENSES" "$app_dir/usr/share/doc/flectar-mail/LICENSES"
cp "$project_dir/THIRD_PARTY_NOTICES.md" "$app_dir/usr/share/doc/flectar-mail/THIRD_PARTY_NOTICES.md"
cp "$project_dir/resources/fonts/google-sans-flex/OFL.txt" \
  "$app_dir/usr/share/doc/flectar-mail/google-sans-flex/OFL.txt"
cp "$project_dir/resources/fonts/google-sans-flex/README.md" \
  "$app_dir/usr/share/doc/flectar-mail/google-sans-flex/README.md"
cp "$project_dir/resources/AppRun" "$app_dir/AppRun"
chmod +x "$app_dir/AppRun" "$app_dir/usr/bin/flectar-mail"

linuxdeploy="${LINUXDEPLOY:-$tools_dir/linuxdeploy-x86_64.AppImage}"
download_tool \
  'https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage' \
  "$linuxdeploy"
appimagetool="${APPIMAGETOOL:-$tools_dir/appimagetool-x86_64.AppImage}"
download_tool \
  'https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage' \
  "$appimagetool"

# Deploy ordinary ELF dependencies. Winit uses the host Wayland/X11 client
# libraries through its normal backend discovery rather than a toolkit plugin.
APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" \
  "$linuxdeploy" \
  --appdir "$app_dir" \
  --executable "$app_dir/usr/bin/flectar-mail"

# Preserve the allocator tuning and stable process name from our launcher.
cp "$project_dir/resources/AppRun" "$app_dir/AppRun"
chmod +x "$app_dir/AppRun"

# AppDir specifies a PNG .DirIcon for file-manager thumbnails. linuxdeploy
# prefers the scalable icon for the root entry, so restore the PNG thumbnail
# explicitly while retaining the SVG and themed icon copies for integration.
cp "$project_dir/resources/app-icon/flectar-mail-masked-512.png" "$app_dir/flectar-mail.png"
ln -sfn flectar-mail.png "$app_dir/.DirIcon"

mkdir -p "$(dirname "$output_path")"
rm -f "$staged_output"
APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" \
  "$appimagetool" "$app_dir" "$staged_output"
# Renaming over an executing AppImage is atomic on Linux; the running process
# keeps its old inode while new launches receive the freshly packaged build.
mv -f "$staged_output" "$output_path"

printf 'Built %s\n' "$output_path"
