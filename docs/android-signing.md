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

The native artifact workflow restores the app signing key from the
`android-release` environment. The Google Play workflow restores the upload
key from the `google-play` environment, described below, and never sees the
app signing key. Both validate their key against its pin before compilation,
build with `scripts/build-android-release.sh`, check that the finished APK and
App Bundle are signed by the pinned key, and remove the runner's keystore
before emulator tests. Signing passwords are supplied only to the restore and
build steps. Pull request checks use temporary fixture keys and never receive
either environment's secrets.

The native artifact workflow publishes the signed APK for both prereleases
and stable tags. The Google Play workflow keeps only the App Bundle. Its APK
is signed with the upload key and exists only to be checked. In both,
`--preview` permits missing bundled OAuth registrations for prerelease
versions only, and it never changes the signing key. Stable versions require
complete OAuth registrations.

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

Google Play signs the copies it installs with this same **app signing key**.
A copy of it was given to Play App Signing with Google's PEPK tool when the
Play app was created, in November 2026, so Play and GitHub installs update each
other. `platform/android/app-signing-certificate.pem` is the key's public
certificate. Builds signed with the upload key take the Microsoft redirect
from it, because the copies people install carry the app signing key.

Bundles sent to Google Play are signed with a separate **upload key**, pinned
in `platform/android/play-upload-certificate.sha256`. The `google-play`
environment holds it and has the same protection as `android-release`, which
is only the default branch and `v*` tags, and an approval for every run.

| Secret | Value |
| --- | --- |
| `FLECTAR_PLAY_UPLOAD_KEYSTORE_BASE64` | Base64 encoding of the upload keystore (PKCS12, alias `upload`) |
| `FLECTAR_PLAY_UPLOAD_KEYSTORE_PASSWORD` | Its password, which is also the key's |

Keep the upload keystore and its password in the same backup as the app
signing key. Unlike the app signing key, an upload key that is lost or leaked
can be replaced. Request an upload key reset on the **App signing** page in
Play Console, then pin the new certificate and replace both secrets.

To publish on Google Play, run the **Google Play bundle** workflow from `main`
or a release tag with the release version and a testing track, and approve
it. After its checks it releases the bundle on that track through the Google
Play Developer API, with the notes in
`fastlane/metadata/android/<language>/changelogs/<version code>.txt`. The
track `none` only builds it, and the `.aab` stays in the
`flectar-mail-google-play-<version>` artifact either way. Production stays a
step in Play Console, where a release that passed testing is promoted. Play
and GitHub share the version sequence above, and Play accepts each version
code only once.

The **Google Play listing** workflow makes the store listing match
`fastlane/metadata/android`, changing only what differs. A change there on
`main` starts it, and it waits for approval like every run in the
`google-play` environment. Run it by hand with **validate only** to check
changes with Play without publishing them. Image alt text isn't part of the
API, so it's typed in Play Console. Both workflows stop rather than cancel
changes that are already in review.

### Google Play Developer API access

The workflows sign in through Workload Identity Federation, so there's no
Google key to store or leak. Google exchanges the GitHub token of a job in
this repository's `google-play` environment for a short-lived token of the
service account `play-publisher@flectar-mail.iam.gserviceaccount.com`, in the
Google Cloud project `flectar-mail`.

| Environment variable | Value |
| --- | --- |
| `FLECTAR_PLAY_WORKLOAD_IDENTITY_PROVIDER` | `projects/<project number>/locations/global/workloadIdentityPools/github/providers/flectar-mail` |
| `FLECTAR_PLAY_SERVICE_ACCOUNT` | `play-publisher@flectar-mail.iam.gserviceaccount.com` |

In Play Console, under **Manage account › Users and permissions**, the
service account has no account permissions. For Flectar Mail it may view app
information, manage the store presence, release to testing tracks and manage
testing tracks, but not release to production.

To set it up again, run this in Cloud Shell, invite the service account in
Play Console with those permissions, and set the two variables from what it
prints.

```bash
PROJECT_ID=flectar-mail
gcloud config set project "$PROJECT_ID"
gcloud services enable androidpublisher.googleapis.com iam.googleapis.com iamcredentials.googleapis.com sts.googleapis.com
gcloud iam service-accounts create play-publisher --display-name="Flectar Mail Play publisher"
gcloud iam workload-identity-pools create github --location=global --display-name="GitHub Actions"
gcloud iam workload-identity-pools providers create-oidc flectar-mail \
  --location=global \
  --workload-identity-pool=github \
  --display-name="flectar/mail" \
  --issuer-uri="https://token.actions.githubusercontent.com" \
  --attribute-mapping="google.subject=assertion.sub,attribute.repository=assertion.repository,attribute.environment=assertion.environment" \
  --attribute-condition="assertion.repository == 'flectar/mail' && assertion.environment == 'google-play'"
PROJECT_NUMBER="$(gcloud projects describe "$PROJECT_ID" --format='value(projectNumber)')"
gcloud iam service-accounts add-iam-policy-binding "play-publisher@$PROJECT_ID.iam.gserviceaccount.com" \
  --role=roles/iam.workloadIdentityUser \
  --member="principalSet://iam.googleapis.com/projects/$PROJECT_NUMBER/locations/global/workloadIdentityPools/github/attribute.repository/flectar/mail"
echo "projects/$PROJECT_NUMBER/locations/global/workloadIdentityPools/github/providers/flectar-mail"
```

Before shipping, verify an actual update on an emulator or test device:
install a first signed APK, create test app data, and install the newer APK
without uninstalling (`adb install -r newer.apk`). Confirm that the data and
account settings survive and that both APK certificate fingerprints match
the committed pin. For Google Play, also check that a GitHub APK updates the
copy Play installed. The keystore regression tests alone do not exercise the
Android package manager or application data migrations.

References: [Android app signing](https://developer.android.com/studio/publish/app-signing),
[version codes](https://developer.android.com/studio/publish/versioning),
[GitHub Actions secrets](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets),
the [Google Play Developer API](https://developers.google.com/android-publisher), and
[Workload Identity Federation for GitHub](https://github.com/google-github-actions/auth#preferred-direct-workload-identity-federation).
