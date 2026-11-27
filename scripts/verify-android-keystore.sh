#!/usr/bin/env bash
# Check a pinned Android signing identity: the app signing key that every
# published APK carries (the default), or the upload key that signs bundles for
# Google Play (FLECTAR_ANDROID_SIGNING_KEY=upload).
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
case "${FLECTAR_ANDROID_SIGNING_KEY:-app}" in
  app)
    key_name='app signing'
    pin_file="$project_dir/platform/android/app-signing-certificate.sha256"
    replaced='Alpha, beta, and stable must use the original key; do not replace the fingerprint.'
    ;;
  upload)
    key_name='Google Play upload'
    pin_file="$project_dir/platform/android/play-upload-certificate.sha256"
    replaced='Restore the upload key from the google-play environment, or reset it in Play Console and pin the new certificate.'
    ;;
  *)
    printf 'Unknown Android signing key: %s\n' "$FLECTAR_ANDROID_SIGNING_KEY" >&2
    exit 1
    ;;
esac
for name in FLECTAR_ANDROID_KEYSTORE FLECTAR_ANDROID_KEYSTORE_PASSWORD FLECTAR_ANDROID_KEY_ALIAS FLECTAR_ANDROID_KEY_PASSWORD; do
  if [[ -z "${!name:-}" ]]; then
    printf 'Android signing requires %s. Restore the original %s key and its credentials; never generate a replacement release key.\n' "$name" "$key_name" >&2
    exit 1
  fi
done
if [[ ! -f "$FLECTAR_ANDROID_KEYSTORE" ]]; then
  printf 'The configured Android %s keystore does not exist.\n' "$key_name" >&2
  exit 1
fi

# keytool silently ignores a different -keypass for PKCS12, while Gradle's
# KeyStore.getKey does not. Reject inconsistent credentials before compilation.
if [[ "$FLECTAR_ANDROID_KEYSTORE_PASSWORD" != "$FLECTAR_ANDROID_KEY_PASSWORD" ]]; then
  details="$(keytool -list -J-Duser.language=en -J-Duser.country=US \
    -keystore "$FLECTAR_ANDROID_KEYSTORE" \
    -storepass:env FLECTAR_ANDROID_KEYSTORE_PASSWORD)"
  if [[ "$details" == *"Keystore type: PKCS12"* ]]; then
    printf 'PKCS12 requires matching keystore and private-key passwords.\n' >&2
    exit 1
  fi
fi

expected="$(cat "$pin_file")"
if [[ ! "$expected" =~ ^[0-9a-f]{64}$ ]]; then
  printf 'The pinned Android %s certificate SHA-256 is invalid.\n' "$key_name" >&2
  exit 1
fi
actual="$(keytool -exportcert -keystore "$FLECTAR_ANDROID_KEYSTORE" \
  -alias "$FLECTAR_ANDROID_KEY_ALIAS" \
  -storepass:env FLECTAR_ANDROID_KEYSTORE_PASSWORD | sha256sum)"
if [[ "${actual%% *}" != "$expected" ]]; then
  printf 'Android signing key differs from the pinned %s identity. %s\n' "$key_name" "$replaced" >&2
  exit 1
fi
# Exporting a certificate alone also succeeds for certificate-only entries.
# A discarded CSR proves that the private key and its password are usable.
keytool -certreq -keystore "$FLECTAR_ANDROID_KEYSTORE" \
  -alias "$FLECTAR_ANDROID_KEY_ALIAS" \
  -storepass:env FLECTAR_ANDROID_KEYSTORE_PASSWORD \
  -keypass:env FLECTAR_ANDROID_KEY_PASSWORD >/dev/null
printf '%s\n' "$expected"
