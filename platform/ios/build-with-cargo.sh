#!/usr/bin/env bash
set -euo pipefail

binary_name="${1:?binary name is required}"
project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
profile=debug
release_args=()
if [[ "${CONFIGURATION:-Debug}" != "Debug" ]]; then
  profile=release
  release_args=(--release)
fi

export PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH:$HOME/.cargo/bin"
export CARGO_PROFILE_RELEASE_DEBUG="${CARGO_PROFILE_RELEASE_DEBUG:-1}"
export CARGO_TARGET_DIR="${DERIVED_FILE_DIR:?}/cargo"

is_simulator=0
if [[ "${PLATFORM_NAME:-}" == "iphonesimulator" || "${LLVM_TARGET_TRIPLE_SUFFIX:-}" == "-simulator" ]]; then
  is_simulator=1
fi

executables=()
pdfium_libraries=()
for arch in $ARCHS; do
  case "$arch" in
    arm64)
      if [[ "$is_simulator" -eq 1 ]]; then
        cargo_target=aarch64-apple-ios-sim
      else
        cargo_target=aarch64-apple-ios
      fi
      ;;
    x86_64)
      cargo_target=x86_64-apple-ios
      ;;
    *)
      printf 'Unsupported Xcode architecture: %s\n' "$arch" >&2
      exit 1
      ;;
  esac

  cargo build \
    --manifest-path "$project_dir/Cargo.toml" \
    --locked \
    --target "$cargo_target" \
    --bin "$binary_name" \
    --no-default-features \
    --features remote-content,ios \
    "${release_args[@]}"
  executables+=("$CARGO_TARGET_DIR/$cargo_target/$profile/$binary_name")
  case "$cargo_target" in
    aarch64-apple-ios) pdfium_target=ios-device-arm64 ;;
    aarch64-apple-ios-sim) pdfium_target=ios-simulator-arm64 ;;
    x86_64-apple-ios) pdfium_target=ios-simulator-x64 ;;
  esac
  pdfium_stage="$DERIVED_FILE_DIR/pdfium/$pdfium_target"
  python3 "$project_dir/scripts/stage-pdfium.py" "$pdfium_target" "$pdfium_stage" \
    --cache "$project_dir/target/pdfium-downloads"
  pdfium_libraries+=("$pdfium_stage/libpdfium.dylib")
  pdfium_notices="$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH/Licenses/PDFium/$pdfium_target"
  mkdir -p "$pdfium_notices"
  cp -R "$pdfium_stage/pdfium-licenses/." "$pdfium_notices/"
done

lipo -create -output "$TARGET_BUILD_DIR/$EXECUTABLE_PATH" "${executables[@]}"

# iOS permits embedded dynamic frameworks, not loose third-party dylibs.
# Device and simulator slices are never mixed; Xcode chooses the SDK above.
framework="$TARGET_BUILD_DIR/${FRAMEWORKS_FOLDER_PATH:?}/PDFium.framework"
mkdir -p "$framework"
lipo -create -output "$framework/PDFium" "${pdfium_libraries[@]}"
chmod 755 "$framework/PDFium"
install_name_tool -id '@rpath/PDFium.framework/PDFium' "$framework/PDFium"
python3 "$project_dir/scripts/write-pdfium-framework-plist.py" "$framework/Info.plist" "$is_simulator"
if [[ "${CODE_SIGNING_ALLOWED:-YES}" != "NO" ]]; then
  # Simulator frameworks also need an ad-hoc signature after install_name_tool.
  if [[ "$is_simulator" -eq 1 ]]; then
    framework_identity="${EXPANDED_CODE_SIGN_IDENTITY:--}"
  else
    framework_identity="${EXPANDED_CODE_SIGN_IDENTITY:?A device build requires an Xcode signing identity}"
  fi
  codesign --force --sign "$framework_identity" "$framework"
  codesign --verify --strict "$framework"
fi
# Fail packaging if an unexpected dependency or install name escaped staging.
framework_dependencies="$(otool -L "$framework/PDFium")"
if printf '%s\n' "$framework_dependencies" | awk '/^[[:space:]]/ {print $1}' \
  | grep -Ev '^(@rpath/PDFium.framework/PDFium|/System/Library/|/usr/lib/)'; then
  printf 'PDFium framework has an unbundled dependency.\n' >&2
  exit 1
fi

# The UI fonts are embedded in the executable. Ship their OFL notices in the
# application bundle as required for redistributed copies of the fonts.
font_license_root="$TARGET_BUILD_DIR/${UNLOCALIZED_RESOURCES_FOLDER_PATH:?}/Licenses"
google_font_license_dir="$font_license_root/Google Sans Flex"
emoji_font_license_dir="$font_license_root/Noto Emoji"
mkdir -p "$google_font_license_dir" "$emoji_font_license_dir"
cp "$project_dir/resources/fonts/google-sans-flex/OFL.txt" "$google_font_license_dir/OFL.txt"
cp "$project_dir/resources/fonts/google-sans-flex/README.md" "$google_font_license_dir/README.md"
cp "$project_dir/resources/fonts/noto-emoji/OFL.txt" "$emoji_font_license_dir/OFL.txt"
cp "$project_dir/resources/fonts/noto-emoji/README.md" "$emoji_font_license_dir/README.md"

if [[ -n "${DWARF_DSYM_FOLDER_PATH:-}" && -n "${DWARF_DSYM_FILE_NAME:-}" ]]; then
  mkdir -p "$DWARF_DSYM_FOLDER_PATH"
  dsymutil "$TARGET_BUILD_DIR/$EXECUTABLE_PATH" \
    -o "$DWARF_DSYM_FOLDER_PATH/$DWARF_DSYM_FILE_NAME"
fi

if [[ "$is_simulator" -eq 0 && "${CODE_SIGNING_ALLOWED:-YES}" != "NO" && -n "${EXPANDED_CODE_SIGN_IDENTITY:-}" ]]; then
  codesign --force --sign "$EXPANDED_CODE_SIGN_IDENTITY" "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
fi
