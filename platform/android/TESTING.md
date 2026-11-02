# Android JNI regression checks

Issue [#64](https://github.com/flectar/mail/issues/64) reported an
`UnsatisfiedLinkError` when Java delivered a Google authorization result.
`NativeActivity` opens the native library and calls `ANativeActivity_onCreate`,
but this does not register the library with ART's Java native-method lookup.
`FlectarActivity` must also call `System.loadLibrary("flectar_mail_android")`
in its static initializer, before any native callback can run.

References:

- [Android JNI loading guidance](https://developer.android.com/ndk/guides/jni-tips#native_libraries)
- [NativeActivity library loading](https://android.googlesource.com/platform/frameworks/base/+/master/core/jni/android_app_NativeActivity.cpp)
- [ART native-method lookup](https://android.googlesource.com/platform/art/+/master/runtime/jni/java_vm_ext.cc)
- [Android instrumentation tests](https://developer.android.com/training/testing/instrumented-tests)

## Offline and packaged-library checks

```sh
python3 scripts/test-android-jni.py
python3 scripts/test-pdf-packaging.py
python3 scripts/verify-android-native.py path/to/flectar-mail.apk
```

The offline tests require a C compiler (`cc`) and GNU `readelf` or
`llvm-readelf`. Set `READELF` to an NDK `llvm-readelf` path if needed.
APK verification checks the dynamic symbol table for the native entry point
and every Java callback, in every packaged ABI, after release stripping.
Both Android release workflows already invoke this verifier through
`verify-android-apk.sh`. Symbol checks alone cannot verify Java library loading.

## Device JNI tests

Build and stage the release Rust library and PDFium with the normal test APK
script, then run the instrumentation suite on a connected ARM64 test device:

```sh
./scripts/build-android-test-apk.sh
./platform/android/gradle/gradlew -p platform/android/gradle \
  :app:connectedDebugAndroidTest
```

This installs a debug application and a separate test APK. Use a test device
or Android user profile without an existing Flectar installation signed by a
different key. The Rust library is still the stripped release library staged
by the build script. An emulator needs a matching native ABI staged under
`target/android/gradle-jni`, or a Google APIs image with ARM64 translation.
Both Android build workflows run these tests on the API 30 Google APIs x86_64
image, which supports ARM64 translation, and retain the instrumentation reports.

`NativeCallbacksTest` invokes all five native callbacks through the real
`FlectarActivity` class and packaged Rust library. It intentionally does not
load the library itself or launch the Activity. No Google account, network
access, OAuth registration, or mailbox data is needed. Removing the static
initializer must make the invocation tests fail with `UnsatisfiedLinkError`.
These tests verify linkage, not OAuth authorization semantics.

`LauncherIconsTest` switches the launcher entry through every app icon offered
in Settings and checks that exactly one entry stays visible, then restores the
default orange icon. The app switches entries only after it leaves the
foreground; check that by choosing an icon in Settings and returning to the
home screen.

## Sign-in acceptance check

On the reported Pixel/GrapheneOS configuration and a supported stock Android
device, use an APK whose signing certificate and package are registered for
Google authorization. Exercise successful sign-in, consent cancellation,
and an authorization failure. Check that a result reaches the app without
`UnsatisfiedLinkError`; cancellation/failure must leave the app usable.
Also background/resume the app and open/cancel the document picker, since
those callbacks use the same library-loading path.

Record the APK version, Android version and test results before closing #64.
If Google authorization still fails, preserve the provider's error separately
from JNI linkage failures; never include access tokens or mailbox data in logs.

## OAuth configuration and issue #65

Preview builds may omit either `FLECTAR_GOOGLE_ANDROID_CLIENT_ID` or
`FLECTAR_MICROSOFT_ANDROID_CLIENT_ID`. No placeholder is substituted: that
provider stays unavailable until a valid custom registration is saved through
Sign-in settings. A supplied invalid ID fails the build before compilation.
Production builds require both IDs and verify that the configured Microsoft
redirect matches the APK signing certificate.

Microsoft client IDs must be the nonzero Application (client) ID from Entra app
registration, in hyphenated GUID form. The app validates bundled, custom and
runtime configuration before starting authorization. Existing invalid saved
IDs remain editable, and unrelated settings remain usable. Invalid custom
input is rejected without changing either provider's saved registration.

For working Outlook sign-in, register the Android package `com.flectar.mail`
and the signing certificate of the APK being installed. Obtain the certificate
hash and exact redirect with `scripts/android-signing-identities.sh <apk>`.
The running app derives its redirect from its installed signing certificate;
setting a build variable alone does not register that redirect with Microsoft.
The app uses the `common` authority for work/school and personal Microsoft
accounts, so choose the corresponding supported account types in Entra. This
is a public client using authorization code with PKCE; do not add a client secret.

References: [Microsoft app registration](https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app),
[Android client configuration](https://learn.microsoft.com/en-us/entra/msal/android/msal-configuration),
and [Android signing redirects](https://learn.microsoft.com/en-us/entra/msal/android/frequently-asked-questions).

Run `python3 scripts/test-android-oauth.py` for the build configuration regression
tests. Run `cargo test -p flectar-mail-core --locked --test oauth_settings` for
saved configuration, atomic rejection, and the no-browser-on-invalid-ID check.
Local syntax validation cannot establish that an ID is registered, that its
redirect is registered, or that a tenant permits consent. Check real sign-in
using the release registration and signing certificate before promising
built-in Outlook availability.
