#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
build_dir="$project_dir/target/appimage"
app_dir="$build_dir/flectar-mail.AppDir"
output_path="$build_dir/flectar-mail.AppImage"
staged_output="$build_dir/.flectar-mail.AppImage.new"
tools_dir="$build_dir/tools"
executable="$project_dir/target/release/flectar-mail"

# The deployment tools and the runtime that starts every AppImage come from
# these releases, checked against the SHA-256 checksums below, as PDFium is
# (scripts/stage-pdfium.py). Update a release and its checksums together.
# LINUXDEPLOY, PATCHELF, APPIMAGETOOL and APPIMAGE_RUNTIME can name local
# files to use instead, unchecked, to try other versions.
linuxdeploy_release=1-alpha-20251107-1
patchelf_release=0.19.3
appimagetool_release=1.9.1
runtime_release=20251108

sha256_of() {
  local sum
  sum="$(sha256sum <"$1")"
  printf '%s' "${sum%% *}"
}

# Downloads a pinned file once and checks it every time it is used, so a
# changed or damaged file is never run or embedded.
fetch_pinned() {
  local url="$1"
  local checksum="$2"
  local destination="$3"
  if [[ -f "$destination" && "$(sha256_of "$destination")" == "$checksum" ]]; then
    return
  fi
  mkdir -p "$(dirname "$destination")"
  rm -f "$destination.new"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error "$url" --output "$destination.new"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$destination.new"
  else
    printf 'curl or wget is required to obtain the AppImage deployment tools.\n' >&2
    exit 1
  fi
  if [[ "$(sha256_of "$destination.new")" != "$checksum" ]]; then
    rm -f "$destination.new"
    printf '%s does not match its pinned SHA-256 checksum.\n' "$url" >&2
    exit 1
  fi
  mv -f "$destination.new" "$destination"
}

mkdir -p "$build_dir"
# Include both renderer choices; CPU remains the initial runtime selection.
# FLECTAR_APP_FEATURES can enable additional application features.
app_features="${FLECTAR_APP_FEATURES:-}"
build_args=(--locked --bin flectar-mail --manifest-path "$project_dir/Cargo.toml" --release --no-default-features --features remote-content,gpu-renderer)
if [[ -n "$app_features" ]]; then
  build_args+=(--features "$app_features")
fi
# The controls are app-owned and the Linux shell uses winit directly.
if [[ "${FLECTAR_SKIP_BUILD:-0}" == "1" ]]; then
  if [[ ! -x "$executable" ]]; then
    printf 'FLECTAR_SKIP_BUILD=1 requires an existing release executable.\n' >&2
    exit 1
  fi
else
  cargo build "${build_args[@]}"
fi
# Name the package and its tools after the executable's architecture.
architecture="$(python3 "$project_dir/scripts/linux-elf.py" arch "$executable")"
read -r machine _ pdfium_target <<<"$architecture"

if ldd "$executable" | grep -q 'libQt'; then
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
cp "$executable" "$app_dir/usr/bin/flectar-mail"
python3 "$project_dir/scripts/stage-pdfium.py" "$pdfium_target" "$app_dir/usr/lib/flectar-mail"
app_version="$(python3 "$project_dir/scripts/stage-linux-metadata.py" --arch "$machine" "$app_dir")"
cp "$project_dir/resources/app-icon/flectar-mail-masked-512.png" "$app_dir/usr/share/icons/hicolor/512x512/apps/com.flectar.mail.png"
cp "$project_dir/resources/app-icon/flectar-mail-masked.svg" "$app_dir/usr/share/icons/hicolor/scalable/apps/com.flectar.mail.svg"
cp "$project_dir/LICENSE" "$app_dir/usr/share/doc/flectar-mail/LICENSE"
cp -R "$project_dir/LICENSES" "$app_dir/usr/share/doc/flectar-mail/LICENSES"
cp "$project_dir/THIRD_PARTY_NOTICES.md" "$app_dir/usr/share/doc/flectar-mail/THIRD_PARTY_NOTICES.md"
cp "$project_dir/resources/fonts/google-sans-flex/OFL.txt" \
  "$app_dir/usr/share/doc/flectar-mail/google-sans-flex/OFL.txt"
cp "$project_dir/resources/fonts/google-sans-flex/README.md" \
  "$app_dir/usr/share/doc/flectar-mail/google-sans-flex/README.md"
cp "$project_dir/resources/AppRun" "$app_dir/AppRun"
chmod +x "$app_dir/AppRun" "$app_dir/usr/bin/flectar-mail"

case "$machine" in
  x86_64)
    linuxdeploy_sha256=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
    patchelf_sha256=38861b171079e440b25ac86483284f690d0ee7921dba98b0c8fe04a9b0a11667
    appimagetool_sha256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
    runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
    ;;
  aarch64)
    linuxdeploy_sha256=620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff
    patchelf_sha256=2281dd071995b449b9ce7c53c71d6d56ec3a280ae1cc66b779442abf624f7c10
    appimagetool_sha256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
    runtime_sha256=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
    ;;
  *)
    printf 'No AppImage tools are pinned for %s.\n' "$machine" >&2
    exit 1
    ;;
esac
linuxdeploy="${LINUXDEPLOY:-}"
if [[ -z "$linuxdeploy" ]]; then
  linuxdeploy="$tools_dir/linuxdeploy-$machine.AppImage"
  fetch_pinned \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/$linuxdeploy_release/linuxdeploy-$machine.AppImage" \
    "$linuxdeploy_sha256" "$linuxdeploy"
  chmod +x "$linuxdeploy"
fi
# The linuxdeploy release bundles patchelf 0.15.0, which linuxdeploy has
# since replaced because it can corrupt GNU hash tables. PATCHELF hands
# linuxdeploy this release instead.
patchelf="${PATCHELF:-}"
if [[ -z "$patchelf" ]]; then
  fetch_pinned \
    "https://github.com/NixOS/patchelf/releases/download/$patchelf_release/patchelf-$patchelf_release-$machine.tar.gz" \
    "$patchelf_sha256" "$tools_dir/patchelf-$machine.tar.gz"
  rm -rf "$tools_dir/patchelf-$machine"
  mkdir -p "$tools_dir/patchelf-$machine"
  tar -xzf "$tools_dir/patchelf-$machine.tar.gz" -C "$tools_dir/patchelf-$machine" ./bin/patchelf
  patchelf="$tools_dir/patchelf-$machine/bin/patchelf"
fi
appimagetool="${APPIMAGETOOL:-}"
if [[ -z "$appimagetool" ]]; then
  appimagetool="$tools_dir/appimagetool-$machine.AppImage"
  fetch_pinned \
    "https://github.com/AppImage/appimagetool/releases/download/$appimagetool_release/appimagetool-$machine.AppImage" \
    "$appimagetool_sha256" "$appimagetool"
  chmod +x "$appimagetool"
fi
# Without --runtime-file, appimagetool would download and embed whatever
# runtime is newest.
runtime="${APPIMAGE_RUNTIME:-}"
if [[ -z "$runtime" ]]; then
  runtime="$tools_dir/runtime-$machine"
  fetch_pinned \
    "https://github.com/AppImage/type2-runtime/releases/download/$runtime_release/runtime-$machine" \
    "$runtime_sha256" "$runtime"
fi

# Winit opens libxkbcommon-x11 with dlopen(), so linuxdeploy cannot discover
# it from the executable. Hosts without it (Ubuntu without Qt, minimal X11
# sessions such as the AppImageHub test runner) panic before the first window.
# It is not on the AppImage excludelist; bundle it with its xkbcommon/xcb-xkb
# dependencies, like the deb and rpm packages declare it.
xkbcommon_x11_dir="$(pkg-config --variable=libdir xkbcommon-x11)"
xkbcommon_x11="$xkbcommon_x11_dir/libxkbcommon-x11.so.0"
if [[ ! -f "$xkbcommon_x11" ]]; then
  printf 'libxkbcommon-x11.so.0 was not found; install libxkbcommon-x11-dev.\n' >&2
  exit 1
fi

# Deploy ordinary ELF dependencies. Winit uses the host Wayland/X11 client
# libraries through its normal backend discovery rather than a toolkit plugin.
# NO_STRIP keeps linuxdeploy's own strip, from binutils 2.35, away from every
# file. The release profile strips the executable, and the libraries it
# bundles come stripped from the distribution, so strip would only rewrite
# them before patchelf does.
VERSION="$app_version" APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" \
  NO_STRIP=1 PATCHELF="$patchelf" \
  "$linuxdeploy" \
  --appdir "$app_dir" \
  --executable "$app_dir/usr/bin/flectar-mail" \
  --library "$xkbcommon_x11"
for library in libxkbcommon-x11.so.0 libxkbcommon.so.0; do
  if [[ ! -e "$app_dir/usr/lib/$library" ]]; then
    printf 'linuxdeploy did not bundle %s.\n' "$library" >&2
    exit 1
  fi
done

# AppRun leaves LD_LIBRARY_PATH to the host, so the executable and bundled
# libraries must find usr/lib through the RUNPATH linuxdeploy sets.
require_runpath() {
  local dynamic
  dynamic="$(readelf -d "$1")"
  if [[ "$dynamic" != *"Library runpath: [$2]"* ]]; then
    printf '%s does not have RUNPATH %s.\n' "$1" "$2" >&2
    exit 1
  fi
}
require_runpath "$app_dir/usr/bin/flectar-mail" '$ORIGIN/../lib'
for library in "$app_dir"/usr/lib/*.so*; do
  require_runpath "$library" '$ORIGIN'
done

# Preserve the renderer default and stable process name from our launcher.
cp "$project_dir/resources/AppRun" "$app_dir/AppRun"
chmod +x "$app_dir/AppRun"

# The pinned PDFium runtime depends only on the ordinary glibc runtime. Keep
# its private path intact and verify it after deployment has adjusted the ELF files.
python3 "$project_dir/scripts/test-pdf-preview.py" "$app_dir/usr/bin/flectar-mail"
# Deployment rewrites ELF files; they must still load on 16 and 64 KiB pages.
python3 "$project_dir/scripts/linux-elf.py" verify "$app_dir"

# AppDir specifies a PNG .DirIcon for file-manager thumbnails. linuxdeploy
# prefers the scalable icon for the root entry, so restore the PNG thumbnail
# explicitly while retaining the SVG and themed icon copies for integration.
cp "$project_dir/resources/app-icon/flectar-mail-masked-512.png" "$app_dir/flectar-mail.png"
ln -sfn flectar-mail.png "$app_dir/.DirIcon"

mkdir -p "$(dirname "$output_path")"
rm -f "$staged_output"
VERSION="$app_version" APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" \
  "$appimagetool" --runtime-file "$runtime" "$app_dir" "$staged_output"
# The AppImage starts with appimagetool's runtime, which is an ELF executable.
python3 "$project_dir/scripts/linux-elf.py" verify "$staged_output"
# Renaming over an executing AppImage is atomic on Linux; the running process
# keeps its old inode while new launches receive the freshly packaged build.
mv -f "$staged_output" "$output_path"

printf 'Built %s\n' "$output_path"
