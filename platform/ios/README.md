# iOS platform

`platform/ios` hosts the iOS app: the [XcodeGen](https://github.com/yonaskolb/XcodeGen)
project, the Objective-C bridges that `build.rs` compiles into the Rust
binary, and the Xcode build phase that runs Cargo. `src/platform/ios.rs` is
the application side of the host contract in
[platform/README.md](../README.md).

Releases include an unsigned IPA that people sideload with their own Apple ID;
[docs/install.md](../../docs/install.md#ios) explains how.

## Building

On a Mac with Xcode:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
brew install xcodegen
cd platform/ios
xcodegen generate
open FlectarMail.xcodeproj
```

Choose a simulator, or your team under **Signing & Capabilities** to run on a
device. The **Build Rust application** phase builds `flectar-mail` with Cargo
for each architecture Xcode asks for and embeds PDFium as a framework.

`scripts/package-ios.sh target/ios` builds what releases publish: an unsigned
archive packaged as `target/ios/flectar-mail-ios-arm64.ipa`, stamped with the
`Cargo.toml` version. `CFBundleShortVersionString` takes only numbers, so
prereleases share it, and `CFBundleVersion` follows the Android version
sequence (`0.1.0-alpha.8` is build `1000008`), which AltStore and SideStore
compare to offer updates.

## Accounts and sign-in

Account secrets live in the Keychain, readable once the device has been
unlocked after starting up and never restored onto another device
(`src/platform/keychain.rs`).

Gmail and Outlook sign in through `ASWebAuthenticationSession`, which shows
the provider's page in a system sheet and hands its redirect back to the app
(`src/platform/web_authentication.rs`). The providers need:

- **Google** gives every platform its own OAuth client. In the Google Cloud
  project of the desktop client, create an OAuth client of type **iOS** for
  the bundle ID `com.flectar.mail` and store its ID as the
  `FLECTAR_GOOGLE_IOS_CLIENT_ID` repository secret. The client has no secret
  and redirects to its reversed client ID,
  `com.googleusercontent.apps.<id>:/oauth2redirect`.
- **Microsoft** registrations serve several platforms, so iOS reuses the one
  Android uses. Add the **iOS / macOS** platform for the bundle ID
  `com.flectar.mail` to it, which registers the redirect
  `msauth.com.flectar.mail://auth`. The iOS job builds with
  `FLECTAR_MICROSOFT_ANDROID_CLIENT_ID`.

Local builds read `FLECTAR_GOOGLE_IOS_CLIENT_ID` and
`FLECTAR_MICROSOFT_IOS_CLIENT_ID` from the environment. Without them, people
can enter their own iOS registrations in **Sign-in settings**. Sideloading
tools may change the bundle ID when they sign the app, which neither redirect
depends on.

## Releases and the AltStore source

Tag pushes run the iOS job in `native-platforms.yml`, and the release
publishes `flectar-mail-<version>-ios-arm64.ipa` with its checksum and build
provenance like every other download.

After a release run succeeds, `ios-source.yml` publishes an
[AltStore source](https://faq.altstore.io/developers/make-a-source), which
SideStore also reads, at `https://flectar.github.io/mail/source.json`. It
offers the newest release's IPA with its size, SHA-256 and permissions, which
both apps check before installing. Run the workflow by hand to publish it
again.

The apps find updates by reading the source again from the address people
added, so it needs one that never changes. Release downloads have none: each
release has its own URL, and GitHub's latest-release link skips prereleases.
GitHub Pages serves the file at a fixed address, and a custom domain can be
added later; GitHub then redirects the old address.

The source needs GitHub Pages once: in the repository settings, open
**Pages** and set **Source** to **GitHub Actions**. Until then the workflow
fails at its first Pages step.

## Known limitations

- Mail syncs while the app is open. There is no background refresh or
  notification support yet.
- Without an Apple Developer Program membership there is no App Store or
  TestFlight build, and copies signed with a free Apple ID expire after 7
  days unless they are refreshed.
- Mobile document transfers are capped at 512 MiB, as
  [platform/README.md](../README.md#known-gaps) describes.
