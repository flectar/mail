#!/usr/bin/env bash
# Materialize a signing secret on an ephemeral Actions runner: the shared app
# signing key, or the Google Play upload key with FLECTAR_ANDROID_SIGNING_KEY=upload.
set -euo pipefail
project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

if [[ "${FLECTAR_ANDROID_SIGNING_KEY:-app}" == upload ]]; then
  environment=google-play key='Google Play upload key' prefix=flectar-play-upload
else
  environment=android-release key='original app signing key' prefix=flectar-app-signing
fi
if [[ -z "${FLECTAR_ANDROID_KEYSTORE_BASE64:-}" ]]; then
  printf 'FLECTAR_ANDROID_KEYSTORE_BASE64 is required in the %s environment. Restore the %s.\n' "$environment" "$key" >&2
  exit 1
fi
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_ENV:?GITHUB_ENV is required}"
umask 077
keystore="$(mktemp "$RUNNER_TEMP/$prefix.XXXXXX.keystore")"
trap 'rm -f "$keystore"' EXIT
printf '%s' "$FLECTAR_ANDROID_KEYSTORE_BASE64" | base64 --decode > "$keystore"
export FLECTAR_ANDROID_KEYSTORE="$keystore"
bash "$project_dir/scripts/verify-android-keystore.sh"
printf 'FLECTAR_ANDROID_KEYSTORE=%s\n' "$keystore" >> "$GITHUB_ENV"
trap - EXIT
