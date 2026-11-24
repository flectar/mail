# Android signing and updates

Every published APK uses `com.flectar.mail` and the certificate pinned in
`platform/android/app-signing-certificate.sha256`. This includes alpha, beta,
release-candidate, and stable versions, starting with beta 1. Keep the original
private key **and certificate** for the lifetime of this update path. Alpha 6
and alpha 7 used different identities and cannot update to this one.

The certificate's historical subject (`Flectar Mail Test`) and alias
(`androiddebugkey`) are names, not a measure of key security. The pin identifies
the actual certificate. Re-encrypting the existing keystore with a strong
password keeps the certificate and private key unchanged.

## One-time setup from the existing alpha key

Requirements: Python 3.11+, a JDK with `keytool` (Java 21 recommended), and
authenticated GitHub CLI access with permission to manage repository
environments and secrets. Keep keystores outside the checkout.

From the repository root:

```bash
python3 scripts/configure-android-signing.py \
  "$HOME/.local/share/flectar-mail/android/alpha-signing.keystore" \
  --output "$HOME/.local/share/flectar-mail/android/app-signing.p12" \
  --upload
```

Use the path to your **existing backup** if it is elsewhere. The current
legacy keystore password is `android`; press Enter for the same key password.
Choose and save a new password of at least 16 characters in your password
manager. The script does not display or save passwords in plaintext.

The script verifies the original certificate and access to its private key,
imports that same entry into a new PKCS12 file with the new password, and
verifies it again. It refuses to overwrite an existing destination. The
original file is retained until you have verified your secure backup.

With `--upload`, it creates the `android-release` environment if absent,
restricts that environment to the repository's default branch and `v*` tags,
and sets these four **environment secrets**:

| Secret | Value |
| --- | --- |
| `FLECTAR_ANDROID_KEYSTORE_BASE64` | Base64 encoding of the re-encrypted keystore |
| `FLECTAR_ANDROID_KEYSTORE_PASSWORD` | New keystore password |
| `FLECTAR_ANDROID_KEY_ALIAS` | Existing alias, normally `androiddebugkey` |
| `FLECTAR_ANDROID_KEY_PASSWORD` | New private-key password; same as the store password for this PKCS12 file |

An existing environment's protection rules are preserved. An environment with
no branch restrictions is rejected. Review the environment's allowed branches
and tags when configuring a different repository. An empty edit field on
GitHub does not show whether a stored secret has a value: saved secret values
are not returned for inspection. Base64 is encoding, not encryption.

The old repository secret `FLECTAR_ANDROID_TEST_KEYSTORE_BASE64` belongs to
older workflow revisions. After the four environment secrets are configured
and the new workflows are merged, remove the old repository secret so the
same private key is no longer available outside the protected environment:

```bash
gh secret delete FLECTAR_ANDROID_TEST_KEYSTORE_BASE64 --repo flectar/mail
```

Do not dispatch releases from older workflow revisions after this migration;
some of those revisions could generate a replacement key when a secret was
missing. The setup script leaves the old secret in place so it can be run
before merging without breaking the workflow currently on `main`.

If upload fails after the new file was created, retry without re-encrypting:

```bash
python3 scripts/configure-android-signing.py \
  "$HOME/.local/share/flectar-mail/android/app-signing.p12" --upload
```

Enter the **new** password this time. To update the secrets manually after
configuring the environment:

```bash
set -o pipefail
base64 < "$HOME/.local/share/flectar-mail/android/app-signing.p12" |
  gh secret set FLECTAR_ANDROID_KEYSTORE_BASE64 --repo flectar/mail --env android-release
gh secret set FLECTAR_ANDROID_KEYSTORE_PASSWORD --repo flectar/mail --env android-release
gh secret set FLECTAR_ANDROID_KEY_ALIAS --repo flectar/mail --env android-release
gh secret set FLECTAR_ANDROID_KEY_PASSWORD --repo flectar/mail --env android-release
```

The final three commands prompt for the corresponding value. Do not pass
passwords on the command line or paste them into issues, chat, or logs.

## Release behavior

Both workflows restore and validate the shared key before compilation, use
`scripts/build-android-release.sh`, verify the finished APK against the same
pin, and remove the runner's keystore before emulator tests. Signing passwords
are supplied only to the restore and build steps. Pull request checks use
temporary fixture keys and never receive the release environment's secrets.

The native artifact workflow publishes the signed APK for both prereleases
and stable tags. `--preview` permits missing bundled OAuth registrations only
for prerelease versions; it never changes the signing key. The production
candidate workflow requires complete OAuth registrations. Both emit an APK
and an AAB into `target/android/release/`; the native release publishes the APK.

Version codes are derived by `scripts/android-version-code.py`:

```text
major × 100,000,000 + minor × 1,000,000 + patch × 10,000 + channel
channel: alpha.N = N; beta.N = 1,000 + N; rc.N = 2,000 + N; stable = 9,999
```

For example, `0.1.0-beta.1` is `1001001` and `0.1.0` is `1009999`.
The supported ranges are major 0–20, minor and patch 0–99, and prerelease
number 1–999; values outside these ranges fail rather than collide. Every
released version must be greater than the previous release on the update path.
Rebuilding the same version produces the same code; use a new version for a
new Play upload. Do not supply unrelated workflow run numbers or reset codes
when moving to stable.

Local `scripts/build-android-test-apk.sh` builds may still use the conventional
`android` password for a developer-only key. CI redirects that entry point to
the pinned release builder, and no published workflow uses the developer key.

## Backups, recovery, and Google Play

Store the re-encrypted keystore in a secure backup outside this machine and
GitHub, and store its password in your password manager. Test the backup with
the verification/setup command before retiring the legacy copy. GitHub
secrets are not a downloadable backup. If a secret is deleted, restore the
same keystore and credentials. Never regenerate the key or change the pin to
make a failed release pass.

When enrolling in Play App Signing, supply this existing **app signing key**
so that Play and GitHub APKs have the same signing identity. Having Google
generate a different app key would break this update path. Configure a
separate **upload key** for later AAB submissions; that key authorizes uploads
and must not replace the key signing GitHub APKs. The current AAB is signed
with the app key for initial enrollment; separate upload-key automation must
be configured when Play is introduced.

Before shipping, verify an actual update on an emulator or test device:
install a first signed APK, create test app data, and install the newer APK
without uninstalling (`adb install -r newer.apk`). Confirm that the data and
account settings survive and that both APK certificate fingerprints match
the committed pin. The keystore regression tests alone do not exercise the
Android package manager or application data migrations.

References: [Android app signing](https://developer.android.com/studio/publish/app-signing),
[version codes](https://developer.android.com/studio/publish/versioning), and
[GitHub Actions secrets](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets).
