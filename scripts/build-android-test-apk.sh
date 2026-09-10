#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
target_dir="$project_dir/target"
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
android_ndk="${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-}}"

if [[ -z "$android_sdk" || ! -d "$android_sdk" ]]; then
  printf 'Android SDK not found. Set ANDROID_HOME to your Android SDK directory.\n' >&2
  exit 1
fi
if ! command -v keytool >/dev/null 2>&1; then
  printf 'keytool not found. Install a JDK (Java 17 or newer is recommended).\n' >&2
  exit 1
fi
gradle_command="${GRADLE_CMD:-}"
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

if [[ -z "$android_ndk" || ! -d "$android_ndk/toolchains/llvm/prebuilt" ]]; then
  printf 'Android NDK not found. Set ANDROID_NDK_HOME to an installed NDK directory.\n' >&2
  exit 1
fi
toolchain="$(find "$android_ndk/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d -print | head -n 1)/bin"
if [[ ! -x "$toolchain/aarch64-linux-android26-clang" ]]; then
  printf 'Android ARM64 API 26 compiler not found under %s.\n' "$toolchain" >&2
  exit 1
fi
export CC_aarch64_linux_android="$toolchain/aarch64-linux-android26-clang"
export CXX_aarch64_linux_android="$toolchain/aarch64-linux-android26-clang++"
export AR_aarch64_linux_android="$toolchain/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$toolchain/aarch64-linux-android26-clang"

# Gradle signs the final package with this app-specific test key, keeping
# production signing credentials out of the test build.
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
keystore_dir="$data_home/flectar-mail/android"
keystore="${FLECTAR_ANDROID_TEST_KEYSTORE:-$keystore_dir/test-signing.keystore}"
mkdir -p "$keystore_dir"

if [[ -n "${FLECTAR_ANDROID_TEST_KEYSTORE:-}" && ! -f "$keystore" ]]; then
  printf 'Configured Android test keystore does not exist: %s\n' "$keystore" >&2
  exit 1
fi
if [[ ! -f "$keystore" ]]; then
  keytool \
    -genkeypair \
    -noprompt \
    -keystore "$keystore" \
    -storepass android \
    -alias androiddebugkey \
    -keypass android \
    -dname 'CN=Flectar Mail Test,O=Android,C=US' \
    -keyalg RSA \
    -keysize 2048 \
    -validity 10000
fi

export ANDROID_HOME="$android_sdk"
export CARGO_TARGET_DIR="$target_dir"

# Test builds have deterministic provider placeholders so static packaging and
# device startup can be exercised without borrowing production registrations.
# Provider sign-in intentionally remains unavailable until the release-owner
# values documented in ANDROID_FUNCTIONAL_APP_TODO.md are supplied.
export FLECTAR_GOOGLE_ANDROID_CLIENT_ID="${FLECTAR_GOOGLE_ANDROID_CLIENT_ID:-test.apps.googleusercontent.com}"
export FLECTAR_MICROSOFT_ANDROID_CLIENT_ID="${FLECTAR_MICROSOFT_ANDROID_CLIENT_ID:-00000000-0000-0000-0000-000000000000}"
export FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI="${FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI:-msauth://com.flectar.mail/test-signature-hash}"

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

export FLECTAR_ANDROID_KEYSTORE="$keystore"
export FLECTAR_ANDROID_KEYSTORE_PASSWORD=android
export FLECTAR_ANDROID_KEY_ALIAS=androiddebugkey
export FLECTAR_ANDROID_KEY_PASSWORD=android
"$gradle_command" \
  --no-daemon \
  --project-cache-dir "$target_dir/android/gradle-cache" \
  -p "$project_dir/platform/android/gradle" \
  :app:assembleRelease

gradle_apk="$target_dir/android/gradle/app/outputs/apk/release/app-release.apk"
artifact_dir="$target_dir/android/release/apk"
mkdir -p "$artifact_dir"
cp "$gradle_apk" "$artifact_dir/flectar-mail.apk"
printf 'Built Google-AuthorizationClient Android package %s\n' "$artifact_dir/flectar-mail.apk"
