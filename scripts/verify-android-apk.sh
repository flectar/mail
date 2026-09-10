#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
apk="${1:-$project_dir/target/android/release/apk/flectar-mail.apk}"
signing_mode="${2:-test}"
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"

if [[ ! -f "$apk" ]]; then
  printf 'APK not found: %s\n' "$apk" >&2
  exit 1
fi
if [[ -z "$android_sdk" || ! -d "$android_sdk/build-tools" ]]; then
  printf 'Android SDK build-tools not found. Set ANDROID_HOME.\n' >&2
  exit 1
fi

build_tools="$(find "$android_sdk/build-tools" -mindepth 1 -maxdepth 1 -type d -print | sort -V | tail -n 1)"
aapt="$build_tools/aapt"
apksigner="$build_tools/apksigner"
if [[ ! -x "$aapt" || ! -x "$apksigner" ]]; then
  printf 'aapt/apksigner not found under %s\n' "$build_tools" >&2
  exit 1
fi

badging="$($aapt dump badging "$apk")"
xmltree="$($aapt dump xmltree "$apk" AndroidManifest.xml)"

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local description="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    printf 'APK verification failed: %s (missing %s)\n' "$description" "$needle" >&2
    exit 1
  fi
}

assert_contains "$badging" "package: name='com.flectar.mail'" 'package identity'
if [[ -n "${FLECTAR_ANDROID_VERSION_NAME:-}" ]]; then
  assert_contains "$badging" "versionName='$FLECTAR_ANDROID_VERSION_NAME'" 'release version name'
fi
if [[ -n "${FLECTAR_ANDROID_VERSION_CODE:-}" ]]; then
  assert_contains "$badging" "versionCode='$FLECTAR_ANDROID_VERSION_CODE'" 'release version code'
fi
assert_contains "$badging" "sdkVersion:'26'" 'minimum SDK'
assert_contains "$badging" "targetSdkVersion:'36'" 'target SDK'
assert_contains "$badging" "uses-permission: name='android.permission.INTERNET'" 'network permission'
assert_contains "$badging" "uses-permission: name='android.permission.ACCESS_NETWORK_STATE'" 'network-state permission'
assert_contains "$badging" "uses-permission: name='android.permission.POST_NOTIFICATIONS'" 'notification permission declaration'
assert_contains "$badging" "native-code: 'arm64-v8a'" 'arm64 ABI'
assert_contains "$badging" 'application-icon-' 'launcher icon'
assert_contains "$xmltree" 'android:usesCleartextTraffic(0x010104ec)=(type 0x12)0x0' 'cleartext traffic disabled'
assert_contains "$xmltree" 'android:scheme(0x01010027)="msauth"' 'Microsoft OAuth callback scheme'
assert_contains "$xmltree" 'android:host(0x01010028)="com.flectar.mail"' 'Microsoft OAuth callback host'
assert_contains "$xmltree" 'android:exported(0x01010010)=(type 0x12)0xffffffff' 'explicitly exported callback/launcher Activity'
assert_contains "$xmltree" 'com.flectar.mail.FlectarActivity' 'Google AuthorizationClient Activity host'
assert_contains "$xmltree" 'android:allowBackup(0x01010280)=(type 0x12)0x0' 'Android backup disabled'

if [[ "$badging" == *"application-debuggable"* ]]; then
  printf 'APK verification failed: release application is debuggable\n' >&2
  exit 1
fi
if [[ "$badging" == *"'x86'"* || "$badging" == *"'x86_64'"* ]]; then
  printf 'APK verification failed: release artifact contains an emulator ABI\n' >&2
  exit 1
fi
archive_listing="$(unzip -l "$apk")"
if [[ "$archive_listing" != *'classes.dex'* ]]; then
  printf 'APK verification failed: Google Play Services bridge classes are absent\n' >&2
  exit 1
fi

python3 "$project_dir/scripts/verify-android-native.py" "$apk"

signing="$($apksigner verify --verbose --print-certs "$apk")"
printf '%s\n' "$signing"
signing_args=("$signing_mode")
if [[ "$signing_mode" == test ]]; then
  data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
  keystore="${FLECTAR_ANDROID_TEST_KEYSTORE:-$data_home/flectar-mail/android/test-signing.keystore}"
  if [[ ! -f "$keystore" ]]; then
    printf 'Expected test keystore not found: %s\n' "$keystore" >&2
    exit 1
  fi
  # Export only the public certificate. A subject string is not a signing
  # identity, and SDK/JDK versions format that string differently.
  expected_sha256="$(keytool -exportcert -keystore "$keystore" -alias androiddebugkey -storepass android | sha256sum)"
  signing_args+=(--expected-sha256 "${expected_sha256%% *}")
fi
printf '%s\n' "$signing" | python3 "$project_dir/scripts/verify-android-signing.py" "${signing_args[@]}"
printf 'Verified Android package metadata, arm64 ABI, TLS policy, and signing: %s\n' "$apk"
