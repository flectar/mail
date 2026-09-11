#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
target_dir="$project_dir/target"
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
android_ndk="${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-}}"
gradle_command="${GRADLE_CMD:-}"

required=(
  FLECTAR_GOOGLE_ANDROID_CLIENT_ID
  FLECTAR_MICROSOFT_ANDROID_CLIENT_ID
  FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI
  FLECTAR_ANDROID_VERSION_CODE
  FLECTAR_ANDROID_VERSION_NAME
  FLECTAR_ANDROID_KEYSTORE
  FLECTAR_ANDROID_KEYSTORE_PASSWORD
  FLECTAR_ANDROID_KEY_ALIAS
  FLECTAR_ANDROID_KEY_PASSWORD
)
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    printf 'Production Android build requires %s.\n' "$name" >&2
    exit 1
  fi
done

if [[ ! -f "$FLECTAR_ANDROID_KEYSTORE" ]]; then
  printf 'Production keystore not found: %s\n' "$FLECTAR_ANDROID_KEYSTORE" >&2
  exit 1
fi
if [[ "$FLECTAR_GOOGLE_ANDROID_CLIENT_ID" != *.apps.googleusercontent.com || "$FLECTAR_GOOGLE_ANDROID_CLIENT_ID" == test.* ]]; then
  printf 'FLECTAR_GOOGLE_ANDROID_CLIENT_ID is not a production Android OAuth client ID.\n' >&2
  exit 1
fi
if [[ ! "$FLECTAR_MICROSOFT_ANDROID_CLIENT_ID" =~ ^[0-9A-Fa-f-]{36}$ || "$FLECTAR_MICROSOFT_ANDROID_CLIENT_ID" == 00000000-0000-0000-0000-000000000000 ]]; then
  printf 'FLECTAR_MICROSOFT_ANDROID_CLIENT_ID is not a production Entra application ID.\n' >&2
  exit 1
fi
if [[ ! "$FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI" =~ ^msauth://com\.flectar\.mail/.+ ]]; then
  printf 'Microsoft redirect must be msauth://com.flectar.mail/<signature-hash>.\n' >&2
  exit 1
fi
if [[ ! "$FLECTAR_ANDROID_VERSION_CODE" =~ ^[1-9][0-9]*$ ]]; then
  printf 'FLECTAR_ANDROID_VERSION_CODE must be a positive Play version code.\n' >&2
  exit 1
fi
if [[ ! "$FLECTAR_ANDROID_VERSION_NAME" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  printf 'FLECTAR_ANDROID_VERSION_NAME must be a release version such as 1.2.3.\n' >&2
  exit 1
fi
if [[ -z "$android_sdk" || ! -d "$android_sdk" ]]; then
  printf 'Android SDK not found. Set ANDROID_HOME.\n' >&2
  exit 1
fi
if [[ -z "$android_ndk" || ! -d "$android_ndk/toolchains/llvm/prebuilt" ]]; then
  printf 'Android NDK not found. Set ANDROID_NDK_ROOT or ANDROID_NDK_HOME.\n' >&2
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

artifact_dir="$target_dir/android/production"
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

actual_redirect="$(ANDROID_HOME="$android_sdk" "$project_dir/scripts/android-signing-identities.sh" \
  "$artifact_dir/flectar-mail.apk" | sed -n 's/^Microsoft redirect URI: //p')"
if [[ "$actual_redirect" != "$FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI" ]]; then
  printf 'The Entra redirect does not match the APK signing certificate.\n' >&2
  printf 'Configured: %s\nActual:     %s\n' \
    "$FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI" "$actual_redirect" >&2
  exit 1
fi

ANDROID_HOME="$android_sdk" "$project_dir/scripts/verify-android-apk.sh" \
  "$artifact_dir/flectar-mail.apk" production
printf 'Production artifacts:\n  %s\n  %s\n' \
  "$artifact_dir/flectar-mail.apk" "$artifact_dir/flectar-mail.aab"
