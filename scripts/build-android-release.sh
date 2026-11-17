#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
target_dir="$project_dir/target"
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
android_ndk="${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-}}"
gradle_command="${GRADLE_CMD:-}"

preview=false
if [[ "${1:-}" == --preview && $# == 1 ]]; then
  preview=true
elif [[ $# != 0 ]]; then
  printf 'Usage: %s [--preview]\n' "$0" >&2
  exit 1
fi

required=(
  FLECTAR_ANDROID_VERSION_NAME
  FLECTAR_ANDROID_KEYSTORE
  FLECTAR_ANDROID_KEYSTORE_PASSWORD
  FLECTAR_ANDROID_KEY_ALIAS
  FLECTAR_ANDROID_KEY_PASSWORD
)
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    printf 'Published Android build requires %s.\n' "$name" >&2
    exit 1
  fi
done

version_code="$(python3 "$project_dir/scripts/android-version-code.py" "$FLECTAR_ANDROID_VERSION_NAME")"
if [[ -n "${FLECTAR_ANDROID_VERSION_CODE:-}" && "$FLECTAR_ANDROID_VERSION_CODE" != "$version_code" ]]; then
  printf 'FLECTAR_ANDROID_VERSION_CODE must match the shared release sequence (%s).\n' "$version_code" >&2
  exit 1
fi
export FLECTAR_ANDROID_VERSION_CODE="$version_code"
bash "$project_dir/scripts/verify-android-keystore.sh" >/dev/null

if [[ "$preview" == true ]]; then
  if [[ "$FLECTAR_ANDROID_VERSION_NAME" != *-* ]]; then
    printf 'Stable Android releases cannot use --preview.\n' >&2
    exit 1
  fi
  python3 "$project_dir/scripts/verify-android-oauth.py"
else
  python3 "$project_dir/scripts/verify-android-oauth.py" --require-all
fi
if [[ "$preview" == false || -n "${FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI:-}" ]]; then
  if [[ ! "${FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI:-}" =~ ^msauth://com\.flectar\.mail/.+ ]]; then
    printf 'Microsoft redirect must be msauth://com.flectar.mail/<signature-hash>.\n' >&2
    exit 1
  fi
fi
if [[ -z "$android_sdk" || ! -d "$android_sdk" ]]; then
  printf 'Android SDK not found. Set ANDROID_HOME.\n' >&2
  exit 1
fi
if [[ -z "$android_ndk" || ! -d "$android_ndk/toolchains/llvm/prebuilt" ]]; then
  printf 'Android NDK not found. Set ANDROID_NDK_ROOT or ANDROID_NDK_HOME.\n' >&2
  exit 1
fi
# Slint's prebuilt Skia binaries need the C++ library of NDK r30 or newer.
# With an older NDK the app links, then fails to load at startup.
ndk_version="$(sed -n 's/^Pkg\.Revision *= *//p' "$android_ndk/source.properties" 2>/dev/null | tr -d '\r' || true)"
if [[ ! "$ndk_version" =~ ^([0-9]+)\. ]] || (( BASH_REMATCH[1] < 30 )); then
  printf 'Android NDK r30 or newer is required, but %s is %s.\n' "$android_ndk" "${ndk_version:-of unknown version}" >&2
  exit 1
fi
if [[ -z "$gradle_command" && -x "$project_dir/platform/android/gradle/gradlew" ]]; then
  gradle_command="$project_dir/platform/android/gradle/gradlew"
fi
if [[ -z "$gradle_command" ]] && command -v gradle >/dev/null 2>&1; then
  gradle_command="$(command -v gradle)"
fi
if [[ -z "$gradle_command" || ! -x "$gradle_command" ]]; then
  printf 'Gradle wrapper not found. Restore platform/android/gradle/gradlew or set GRADLE_CMD.\n' >&2
  exit 1
fi

export ANDROID_HOME="$android_sdk"
export CARGO_TARGET_DIR="$target_dir"
toolchain="$(find "$android_ndk/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d -print | head -n 1)/bin"
if [[ ! -x "$toolchain/aarch64-linux-android26-clang" ]]; then
  printf 'Android ARM64 API 26 compiler not found under %s.\n' "$toolchain" >&2
  exit 1
fi
export CC_aarch64_linux_android="$toolchain/aarch64-linux-android26-clang"
export CXX_aarch64_linux_android="$toolchain/aarch64-linux-android26-clang++"
export AR_aarch64_linux_android="$toolchain/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$toolchain/aarch64-linux-android26-clang"

cargo build \
  --locked \
  --manifest-path "$project_dir/platform/android/Cargo.toml" \
  --target aarch64-linux-android \
  --release \
  --lib

native_lib="$target_dir/aarch64-linux-android/release/libflectar_mail_android.so"
jni_dir="$target_dir/android/gradle-jni/arm64-v8a"
mkdir -p "$jni_dir"
cp "$native_lib" "$jni_dir/libflectar_mail_android.so"
python3 "$project_dir/scripts/stage-pdfium.py" android-arm64 "$jni_dir" \
  --cache "$target_dir/pdfium-downloads"

"$gradle_command" --no-daemon \
  --project-cache-dir "$target_dir/android/gradle-cache" \
  -p "$project_dir/platform/android/gradle" \
  :app:assembleRelease :app:bundleRelease

artifact_dir="$target_dir/android/release"
mkdir -p "$artifact_dir"
cp "$target_dir/android/gradle/app/outputs/apk/release/app-release.apk" \
  "$artifact_dir/flectar-mail.apk"
cp "$target_dir/android/gradle/app/outputs/bundle/release/app-release.aab" \
  "$artifact_dir/flectar-mail.aab"

if ! command -v jarsigner >/dev/null 2>&1 \
  || ! jarsigner -verify "$artifact_dir/flectar-mail.aab" >/dev/null; then
  printf 'The production App Bundle signature could not be verified.\n' >&2
  exit 1
fi

if [[ -n "${FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI:-}" ]]; then
  actual_redirect="$(ANDROID_HOME="$android_sdk" "$project_dir/scripts/android-signing-identities.sh" \
    "$artifact_dir/flectar-mail.apk" | sed -n 's/^Microsoft redirect URI: //p')"
  if [[ "$actual_redirect" != "$FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI" ]]; then
    printf 'The Entra redirect does not match the APK signing certificate.\n' >&2
    printf 'Configured: %s\nActual:     %s\n' \
      "$FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI" "$actual_redirect" >&2
    exit 1
  fi
fi

ANDROID_HOME="$android_sdk" "$project_dir/scripts/verify-android-apk.sh" \
  "$artifact_dir/flectar-mail.apk" production
printf 'Signed release artifacts:\n  %s\n  %s\n' \
  "$artifact_dir/flectar-mail.apk" "$artifact_dir/flectar-mail.aab"
