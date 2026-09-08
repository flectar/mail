#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
binary_path="${1:-$project_dir/target/release/flectar-mail}"
output_dir="${2:-$project_dir/target/macos}"
app_dir="$output_dir/Flectar Mail.app"

if [[ ! -x "$binary_path" ]]; then
  printf 'Missing release binary: %s\n' "$binary_path" >&2
  exit 1
fi

rm -rf "$app_dir"
mkdir -p \
  "$app_dir/Contents/MacOS" \
  "$app_dir/Contents/Resources/Licenses/Flectar Mail" \
  "$app_dir/Contents/Resources/Licenses/Google Sans Flex" \
  "$app_dir/Contents/Resources/Licenses/Noto Emoji"
cp "$binary_path" "$app_dir/Contents/MacOS/flectar-mail"

icon_source="$project_dir/resources/app-icon/flectar-mail-masked.png"
iconset_dir="$output_dir/FlectarMail.iconset"
rm -rf "$iconset_dir"
mkdir -p "$iconset_dir"
sips -z 16 16 "$icon_source" --out "$iconset_dir/icon_16x16.png" >/dev/null
sips -z 32 32 "$icon_source" --out "$iconset_dir/icon_16x16@2x.png" >/dev/null
sips -z 32 32 "$icon_source" --out "$iconset_dir/icon_32x32.png" >/dev/null
sips -z 64 64 "$icon_source" --out "$iconset_dir/icon_32x32@2x.png" >/dev/null
sips -z 128 128 "$icon_source" --out "$iconset_dir/icon_128x128.png" >/dev/null
sips -z 256 256 "$icon_source" --out "$iconset_dir/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$icon_source" --out "$iconset_dir/icon_256x256.png" >/dev/null
sips -z 512 512 "$icon_source" --out "$iconset_dir/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$icon_source" --out "$iconset_dir/icon_512x512.png" >/dev/null
cp "$icon_source" "$iconset_dir/icon_512x512@2x.png"
iconutil -c icns "$iconset_dir" -o "$app_dir/Contents/Resources/FlectarMail.icns"
rm -rf "$iconset_dir"

cp "$project_dir/LICENSE" "$app_dir/Contents/Resources/Licenses/Flectar Mail/LICENSE"
cp -R "$project_dir/LICENSES" "$app_dir/Contents/Resources/Licenses/Flectar Mail/LICENSES"
cp "$project_dir/THIRD_PARTY_NOTICES.md" \
  "$app_dir/Contents/Resources/Licenses/Flectar Mail/THIRD_PARTY_NOTICES.md"
cp "$project_dir/resources/fonts/google-sans-flex/OFL.txt" \
  "$app_dir/Contents/Resources/Licenses/Google Sans Flex/OFL.txt"
cp "$project_dir/resources/fonts/google-sans-flex/README.md" \
  "$app_dir/Contents/Resources/Licenses/Google Sans Flex/README.md"
cp "$project_dir/resources/fonts/noto-emoji/OFL.txt" \
  "$app_dir/Contents/Resources/Licenses/Noto Emoji/OFL.txt"
cp "$project_dir/resources/fonts/noto-emoji/README.md" \
  "$app_dir/Contents/Resources/Licenses/Noto Emoji/README.md"

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -1)"
native_version="${version%%-*}"
sed \
  -e "s/@VERSION@/$native_version/g" \
  "$project_dir/platform/macos/Info.plist.in" > "$app_dir/Contents/Info.plist"

# An ad-hoc signature makes the CI artifact internally consistent. Distribution
# signing and notarization replace it when Apple credentials are configured.
codesign --force --deep --sign - "$app_dir"

mkdir -p "$output_dir"
ditto -c -k --sequesterRsrc --keepParent "$app_dir" "$output_dir/flectar-mail-macos-arm64.zip"
printf 'Built %s\n' "$output_dir/flectar-mail-macos-arm64.zip"

# Use a separate source folder so the disk image contains only the app and the
# usual Applications shortcut, not the ZIP or a previous disk image.
staging="$output_dir/dmg-staging"
rm -rf "$staging"
mkdir -p "$staging"
ditto "$app_dir" "$staging/Flectar Mail.app"
ln -s /Applications "$staging/Applications"
hdiutil create -volname "Flectar Mail" -srcfolder "$staging" -ov -format UDZO \
  "$output_dir/flectar-mail-macos-arm64.dmg"
hdiutil verify "$output_dir/flectar-mail-macos-arm64.dmg"
codesign --verify --deep --strict "$app_dir"
rm -rf "$staging"
printf 'Built %s\n' "$output_dir/flectar-mail-macos-arm64.dmg"
