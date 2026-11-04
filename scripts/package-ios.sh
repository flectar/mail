#!/usr/bin/env bash
# Build the unsigned iOS app and package it as an IPA for sideloading.
# AltStore, SideStore and Sideloadly sign it with the user's own Apple ID.
set -euo pipefail

output_dir="${1:?output directory is required}"
project_dir="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
mkdir -p "$output_dir"
output_dir="$(CDPATH='' cd -- "$output_dir" && pwd)"

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -1)"
# CFBundleShortVersionString takes only numbers, so prereleases share it.
# CFBundleVersion follows the Android version sequence (alpha < beta < rc <
# release), which AltStore and SideStore compare to offer updates.
short_version="${version%%-*}"
build_version="$(python3 "$project_dir/scripts/android-version-code.py" "$version")"

xcodegen generate --spec "$project_dir/platform/ios/project.yml"
archive="$output_dir/FlectarMail.xcarchive"
rm -rf "$archive"
xcodebuild \
  -project "$project_dir/platform/ios/FlectarMail.xcodeproj" \
  -scheme FlectarMail \
  -configuration Release \
  -sdk iphoneos \
  -destination 'generic/platform=iOS' \
  -archivePath "$archive" \
  CODE_SIGNING_ALLOWED=NO \
  MARKETING_VERSION="$short_version" \
  CURRENT_PROJECT_VERSION="$build_version" \
  archive

# A signature would only be replaced, and entitlements would have to be
# declared in the AltStore source.
app="$archive/Products/Applications/FlectarMail.app"
for code in "$app" "$app"/Frameworks/*.framework; do
  if codesign --display "$code" 2>/dev/null; then
    printf '%s is signed; the IPA must be unsigned.\n' "$code" >&2
    exit 1
  fi
done

python3 "$project_dir/scripts/ios-release.py" ipa "$app" "$output_dir/flectar-mail-ios-arm64.ipa" \
  --short-version "$short_version" --build-version "$build_version"
printf 'Built %s (%s, build %s)\n' "$output_dir/flectar-mail-ios-arm64.ipa" "$version" "$build_version"
