#!/usr/bin/env bash
set -euo pipefail
trap 'status=$?; printf "macOS packaging failed at line %s: %s\n" "$LINENO" "$BASH_COMMAND" >&2; exit "$status"' ERR

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
binary_path="${1:-$project_dir/target/release/flectar-mail}"
output_dir="${2:-$project_dir/target/macos}"
architecture="${3:-$(uname -m)}"
case "$architecture" in
  arm64|aarch64)
    asset_architecture="arm64"
    pdfium_target="mac-arm64"
    ;;
  x64|x86_64|amd64)
    asset_architecture="x64"
    pdfium_target="mac-x64"
    ;;
  *)
    printf 'Unsupported macOS architecture: %s\n' "$architecture" >&2
    exit 1
    ;;
esac
app_dir="$output_dir/Flectar Mail.app"
archive_path="$output_dir/flectar-mail-macos-$asset_architecture"

if [[ ! -x "$binary_path" ]]; then
  printf 'Missing release binary: %s\n' "$binary_path" >&2
  exit 1
fi

rm -rf "$app_dir"
mkdir -p \
  "$app_dir/Contents/MacOS" \
  "$app_dir/Contents/Frameworks" \
  "$app_dir/Contents/Library/LaunchAgents" \
  "$app_dir/Contents/Resources/Licenses/Flectar Mail" \
  "$app_dir/Contents/Resources/Licenses/Google Sans Flex" \
  "$app_dir/Contents/Resources/Licenses/Noto Emoji" \
  "$app_dir/Contents/Resources/Licenses/PDFium"
cp "$binary_path" "$app_dir/Contents/MacOS/flectar-mail"
# Registered with Login Items through SMAppService for "Start automatically".
cp "$project_dir/platform/macos/com.flectar.mail.login.plist" \
  "$app_dir/Contents/Library/LaunchAgents/com.flectar.mail.login.plist"
plutil -lint "$app_dir/Contents/Library/LaunchAgents/com.flectar.mail.login.plist"

python3 "$project_dir/scripts/stage-pdfium.py" \
  "$pdfium_target" "$app_dir/Contents/Frameworks" \
  --licenses-destination "$app_dir/Contents/Resources/Licenses/PDFium"
chmod 0755 "$app_dir/Contents/Frameworks/libpdfium.dylib"

# Every size follows Apple's macOS icon grid (resources/app-icon/generate.py),
# so the Dock shows the icon at the size of other apps' icons.
iconutil -c icns "$project_dir/resources/app-icon/flectar-mail.iconset" \
  -o "$app_dir/Contents/Resources/FlectarMail.icns"

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

# Sign nested code before the containing app. Distribution signing and
# notarization replace these ad-hoc identities when Apple credentials exist.
codesign --force --sign - "$app_dir/Contents/Frameworks/libpdfium.dylib"
codesign --force --sign - "$app_dir"
python3 "$project_dir/scripts/test-pdf-preview.py" "$app_dir/Contents/MacOS/flectar-mail"

mkdir -p "$output_dir"
ditto -c -k --sequesterRsrc --keepParent "$app_dir" "$archive_path.zip"
printf 'Built %s\n' "$archive_path.zip"

# Use a separate source folder so the disk image contains only the app and the
# usual Applications shortcut, not the ZIP or a previous disk image.
staging="$output_dir/dmg-staging"
rm -rf "$staging"
mkdir -p "$staging"
ditto "$app_dir" "$staging/Flectar Mail.app"
ln -s /Applications "$staging/Applications"
hdiutil create -volname "Flectar Mail" -srcfolder "$staging" -ov -format UDZO \
  "$archive_path.dmg"
hdiutil verify "$archive_path.dmg"
codesign --verify --deep --strict "$app_dir"
rm -rf "$staging"
printf 'Built %s\n' "$archive_path.dmg"
