#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
build_dir="$project_dir/target/deb"
package_root="$build_dir/debian/flectar-mail"
version="$(sed -n '/^\[package\]$/,/^\[/s/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -n 1)"
# Debian sorts ~beta before the final version. A hyphen means a Debian revision.
version="${version/-/\~}"
output_path="$build_dir/flectar-mail_${version}_amd64.deb"

if [[ -z "$version" ]]; then
  printf 'Could not read the package version from Cargo.toml.\n' >&2
  exit 1
fi

app_features="${FLECTAR_APP_FEATURES:-}"
build_args=(--locked --bin flectar-mail --manifest-path "$project_dir/Cargo.toml" --release --no-default-features --features remote-content)
if [[ -n "$app_features" ]]; then
  build_args+=(--features "$app_features")
fi
cargo build "${build_args[@]}"

rm -rf "$package_root"
mkdir -p \
  "$package_root/DEBIAN" \
  "$package_root/usr/bin" \
  "$package_root/usr/share/applications" \
  "$package_root/usr/share/doc/flectar-mail/google-sans-flex" \
  "$package_root/usr/share/icons/hicolor/512x512/apps" \
  "$package_root/usr/share/icons/hicolor/scalable/apps" \
  "$package_root/usr/share/metainfo"

install -m755 "$project_dir/target/release/flectar-mail" \
  "$package_root/usr/bin/flectar-mail"
python3 "$project_dir/scripts/stage-linux-metadata.py" "$package_root"
install -m644 "$project_dir/resources/app-icon/flectar-mail-masked-512.png" \
  "$package_root/usr/share/icons/hicolor/512x512/apps/com.flectar.mail.png"
install -m644 "$project_dir/resources/app-icon/flectar-mail-masked.svg" \
  "$package_root/usr/share/icons/hicolor/scalable/apps/com.flectar.mail.svg"
install -m644 "$project_dir/LICENSE" \
  "$package_root/usr/share/doc/flectar-mail/LICENSE"
cp -R "$project_dir/LICENSES" "$package_root/usr/share/doc/flectar-mail/LICENSES"
install -m644 "$project_dir/THIRD_PARTY_NOTICES.md" \
  "$package_root/usr/share/doc/flectar-mail/THIRD_PARTY_NOTICES.md"
install -m644 "$project_dir/resources/fonts/google-sans-flex/OFL.txt" \
  "$package_root/usr/share/doc/flectar-mail/google-sans-flex/OFL.txt"
install -m644 "$project_dir/resources/fonts/google-sans-flex/README.md" \
  "$package_root/usr/share/doc/flectar-mail/google-sans-flex/README.md"

# Derive directly linked ABI dependencies from the staged ELF using Debian's
# package tooling. Slint/winit discovers its display and font libraries with
# dlopen, so those runtime packages remain explicit alongside the generated
# shlibs substitution.
cp "$project_dir/resources/debian/source-control.in" "$build_dir/debian/control"
shlib_substitution="$(
  cd "$build_dir"
  dpkg-shlibdeps -O -edebian/flectar-mail/usr/bin/flectar-mail
)"
runtime_dependencies="${shlib_substitution#shlibs:Depends=}"
runtime_dependencies+=", libfontconfig1, libwayland-client0, libx11-6, libx11-xcb1, libxkbcommon0, libxkbcommon-x11-0, hicolor-icon-theme"
sed \
  -e "s/@VERSION@/$version/g" \
  -e "s/@DEPENDS@/$runtime_dependencies/g" \
  "$project_dir/resources/debian/control.in" \
  > "$package_root/DEBIAN/control"

dpkg-deb --root-owner-group --build "$package_root" "$output_path"
printf 'Built %s\n' "$output_path"
