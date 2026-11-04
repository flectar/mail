# Installing Flectar Mail

Flectar Mail is in development and isn't stable yet. Preview builds are
published on [GitHub Releases](https://github.com/flectar/mail/releases). The
[README](../README.md#-get-flectar-mail) lists the files for each platform.

## Signing in to Gmail and Outlook

Google OAuth verification requires an annual CASA security assessment, which
depends on sponsor support for a community project like Flectar. Until that
verification is funded and complete, a stable release with Gmail OAuth
configured by default isn't possible. You can help by
[sponsoring Flectar](https://github.com/sponsors/flectar). Google and Microsoft
OAuth verification is still in progress, so earlier GitHub prereleases are
intended for testers using their own OAuth registrations or IMAP, POP3, or JMAP
accounts.

Builds without OAuth app keys keep Gmail and Outlook sign-in disabled. Use the
**Sign-in settings** cog on the welcome screen to save your own Google or
Microsoft app registration, or connect an IMAP, POP3, or JMAP account. Custom
keys are shared with Settings and take precedence over bundled keys; clearing a
custom client ID restores the defaults when available. Each provider enables
separately once its configuration is saved.

## Windows and macOS

Windows previews are unsigned; macOS previews are ad-hoc signed and not
notarized, so operating-system security prompts are expected. Install updates
manually.

Windows downloads come in x64 and arm64 builds. A Windows on Arm PC can run
the x64 build through emulation, but the arm64 build runs natively. The
[Microsoft Store package](microsoft-store.md) is built with every release and
is not published in the Store yet.

## Linux

### Installing a package

Each Linux package has an x64 build and an arm64 build for 64-bit Arm
computers. Run `uname -m` to see which one you need (`x86_64` or `aarch64`).
The commands below install the x64 files, and the arm64 files install the same
way.

```bash
sudo dnf install ./flectar-mail-<version>-linux-x64.rpm
flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak
flatpak run com.flectar.mail
```

### Raspberry Pi

Use the arm64 packages with the 64-bit Raspberry Pi OS. Every release checks
that they load with the 16 KiB memory pages of the Raspberry Pi 5 kernel.
Raspberry Pi OS kernels don't include Landlock, which lets the sandboxed PDF
preview read the system fonts and nothing else. There the preview can't open
any file, so PDFs that don't embed their fonts are drawn with built-in fonts
instead.

### Fedora RPM

The RPMs target Fedora 43 and 44 on x86_64 and aarch64. Release builds use
Fedora 43 as their compatibility baseline; each RPM must pass installation, PDF
rendering, X11 and Wayland startup, and removal checks in clean containers for
both versions before publication. Other RPM-based distributions are not yet
tested. If Discover reports a generic conflict, run the `dnf install` command
above and include its dependency error in your bug report.

The older `0.1.0-alpha.7` RPM was built on Fedora 44 and cannot install on
Fedora 43 because it requires glibc 2.43. Its Flatpak bundle is an alternative
while waiting for an RPM release built with the compatibility checks above.

### Flatpak preview

The Flatpak preview is provided as a standalone test bundle. It is not yet a
Flathub package and therefore does not receive automatic Flathub updates. Its
close-to-tray option is disabled until the tray backend can use a sandbox-safe
D-Bus name. Starting automatically at sign-in goes through the desktop's
Background portal and opens the window, because there is no tray to start into.
The app icon stays orange there, because the sandbox cannot change the launcher
icon on the host.

### Keyring

On Linux, passwords, OAuth tokens, and API keys are kept in the system keyring
through the freedesktop Secret Service, never in Flectar Mail's database. Any
Secret Service provider works, on any desktop or window manager, sway included:
GNOME Keyring, KWallet, or KeePassXC with its Secret Service integration turned
on. The Flatpak uses the same keyring as the rest of your session. If the
session has no default keyring yet, adding the first account asks the provider
to create one, and the provider asks you to choose its password. Without a
provider, adding an account stops with an error that says what is missing,
because Flectar Mail never stores secrets anywhere less safe.

### Desktop portal

Linux desktop OAuth also opens the system browser through the desktop portal,
so the session needs an `xdg-desktop-portal` backend as well as the keyring
described above. Release builds stop before opening the OAuth page when secure
credential storage is unavailable, so an authorization grant can never be
completed without a safe place to persist it.

## Verifying a download

Each GitHub release includes `SHA256SUMS` and signed build-provenance
attestations. With GitHub CLI installed, verify a download with
`gh attestation verify <download> --repo flectar/mail`.

## Android

Starting with alpha 8, Android alpha, beta, release-candidate, and stable builds
use one package (`com.flectar.mail`) and one persistent signing identity. Later
versions can update the app in place and retain local data. Alpha 6 and alpha 7
used different keys and cannot update to this signing identity.

Both Android release workflows use the `android-release` GitHub environment.
They require the same keystore and password secrets, verify the public
certificate against `platform/android/app-signing-certificate.sha256`, and
derive Android version codes from the release version. A missing or different
key blocks publication. **Do not generate a replacement key or change the
pinned fingerprint for a routine release.** Password changes preserve the
identity when the original private key and certificate are retained.

See [Android signing setup and recovery](android-signing.md) for the one-time
migration command, required secrets, backups, and Play App Signing.

Local developer builds can still generate a local test key. They only update
installations signed with that same local key.

## iOS

The iOS app runs on iPhone and iPad with iOS 17 or later. Apple only installs
signed apps and Flectar Mail isn't on the App Store yet, so each release
includes an unsigned `.ipa` that a sideloading tool signs with your own Apple
ID:

- [AltStore](https://altstore.io) or [SideStore](https://sidestore.io): add
  the source
  `https://github.com/flectar/mail/releases/latest/download/flectar-mail-altstore-source.json`
  in the app's **Sources** and install Flectar Mail from it. Both offer each
  new release as an update and check the download's SHA-256 before installing
  it.
- [Sideloadly](https://sideloadly.io) on Windows or macOS: install
  `flectar-mail-<version>-ios-arm64.ipa` from the
  [release](https://github.com/flectar/mail/releases) with your Apple ID.

After the first install, turn on **Settings > Privacy & Security > Developer
Mode**. With a free Apple ID, sideloaded apps stop opening after 7 days unless
AltStore or SideStore refreshes them or you install them again, and only three
can be installed at once, counting AltStore or SideStore. A paid Apple
Developer Program membership extends this to a year.

Mail syncs while the app is open; it doesn't check in the background or show
notifications on iOS yet.

Releases built with Flectar's iOS registrations sign in to Gmail and Outlook
directly. Your own keys in **Sign-in settings** must be iOS registrations: a
Google OAuth client of type iOS, or a Microsoft app registration with the
iOS / macOS platform and the redirect `msauth.com.flectar.mail://auth`.

## HarmonyOS and OpenHarmony

HarmonyOS and OpenHarmony have a community port maintained by
[springmin](https://github.com/springmin). It isn't part of GitHub releases;
see [platform/ohos](../platform/ohos/README.md) to build it.

## Building from source

Install the [Rust toolchain](https://rustup.rs/), then run:

```bash
cargo run --bin flectar-mail
```

On Linux, the app needs the keyring and desktop portal described above. The
iOS app builds with Xcode, as [platform/ios](../platform/ios/README.md)
describes.

Podman and Docker development shells commonly have neither the host session
D-Bus nor the host browser's loopback network. Debug builds support that setup
with a clearly marked, owner-only development credential file and an accordion
for pasting the final loopback callback URL. The file backend is excluded from
release builds.
