<div align="center">
  <img src="resources/app-icon/flectar-mail-masked.png" width="88" alt="Flectar Mail logo">
  <h1 align="center">
    Flectar Mail
  </h1>
  <div align="center">
    <h3>Email, made fast again</h3>
    <p>Built from the ground up for speed. Flectar Mail delivers native performance, instant startup, and as little as 20 MB of RAM.</p>
  </div>
  <p>
    <a href="https://flectar.com">Website</a> ·
    <a href="https://github.com/flectar/mail/issues">Report an issue</a> ·
    <a href="CONTRIBUTING.md">Contribute</a>
  </p>
  <p>
    <a href="LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="License: AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="Downloads across all releases"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="Latest release"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="Translation status"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="Language: Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="Platforms: Windows, macOS, Linux, Android, iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-light.png">
  <img src="resources/screenshots/desktop-light.png" alt="Flectar Mail unified inbox and message view">
</picture>

Flectar Mail is a lightweight, native home for your email, calendars, and
contacts. It is engineered to open instantly, stay responsive, and use a
fraction of the memory of a typical web-based mail client.

## ❓ Why Flectar Mail?

- **Fast from the first click.** A native interface and local-first data path
  get you to your inbox without waiting on a browser runtime.
- **As little as 20 MB of RAM.** Flectar Mail is deliberately designed to keep
  memory use low, even with a full-featured inbox at your fingertips.
- **Everything in one place.** Move between mail, calendars, and contacts
  without stitching together separate apps.
- **Offline by design.** Your mailbox and calendar are stored locally, so your
  synced data remains useful without a connection.
- **Works with your accounts.** Connect Gmail, Outlook and Microsoft 365, or
  standards-based IMAP/SMTP, JMAP, CalDAV, and CardDAV services.
- **Privacy-conscious defaults.** Remote images are blocked until you allow
  them, helping prevent tracking pixels from reporting when you read a message.
- **Security by architecture.** Email content is never opened in a WebView.
  Flectar Mail renders HTML and CSS through its own Rust-native pipeline, does
  not execute email scripts, and blocks remote images by default. This avoids
  the embedded-browser attack surface by design.
- **An experimental Rust renderer.** Building an email renderer without a
  browser engine is new territory. Rendering issues are expected, especially
  in complex messages, while compatibility continues to improve.
- **Made for every screen.** Spacious and minimal desktop layouts share the
  same experience as the touch-friendly compact interface.
- **Native and open source.** Built from the ground up with Rust. It is not a
  browser wrapped in a window, and it is released under the AGPLv3.

## 📁 Files and attachments

Browse JMAP/WebDAV storage, search mail attachments, keep files offline, and
preview PDFs, images and text.

## ✍🏻 Account signatures and OpenPGP

Settings → Accounts → Signatures & OpenPGP provides named signatures, separate
new-message and reply defaults, and a composer signature selector. Desktop
OpenPGP/MIME signing and encryption use installed GnuPG 2.x with pinentry for
private-key passphrases. Required encryption blocks delivery when recipient keys
are missing or invalid; protected drafts remain local until Send.

S/MIME and mobile OpenPGP are not currently supported.

## 🧪 Experimental HTML rendering

> [!WARNING]
> HTML email rendering is currently the most experimental part of Flectar Mail.
> Some messages, especially those with complex or unusual markup and CSS, may
> not render correctly yet.

To keep the client fully native and memory usage around 20 MB, Flectar Mail
renders email HTML with [Blitz](https://github.com/DioxusLabs/blitz), a Rust
HTML/CSS renderer from the Dioxus team, instead of embedding a browser or
WebView.

Desktop builds include **CPU — Low Memory** and **GPU — WGPU** under
Settings → General → Renderer. CPU is selected initially and does not initialize
WGPU. The GPU option uses Slint and Vello on a shared WGPU 29 device; changing
the setting takes effect after restarting the app. A failed GPU startup falls
back to CPU automatically.

As far as we know, Flectar Mail is one of the first projects using Blitz for
arbitrary, real-world email HTML. Email markup contains plenty of unusual HTML
and CSS, so this pushes the renderer into demanding territory. We currently
carry several patches on top of Blitz and hope to upstream as much of that work
as possible over time.

If you find an email that renders incorrectly, please
[report it](https://github.com/flectar/mail/issues). This approach is still
experimental, but it is also a major reason Flectar Mail can remain so
lightweight compared with WebView-based clients built with frameworks such as
Tauri or Wails.

## 🎨 Make it yours

Choose the workspace that fits the way you handle email. Keep the detailed
three-pane layout, switch to a streamlined minimal view, choose a light or dark
theme, select a built-in color palette or create a custom one, and show or hide
sender avatars.

The full workspace keeps your folders, message list, and selected email visible
together. The minimal layout reduces visual noise and gives each part of your
inbox more room when you need it.

### 👥 Profiles and account colors

Group related accounts into named profiles such as Work or Personal, then give
each profile its own color. In Settings → Accounts → Profiles, you can assign
accounts, override an individual account's color, and show the effective color
along the left edge of every message. The markers make accounts easy to tell
apart while working in the unified inbox; the example below uses purple for Work
and orange for Support.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-profiles-light.png">
  <img src="resources/screenshots/desktop-profiles-light.png" alt="Flectar Mail unified inbox with purple and orange profile color markers">
</picture>

### 🧵 Conversation threads

Replies stay grouped in chronological order, with the active message expanded
inside the reading pane.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-thread-light.png">
  <img src="resources/screenshots/desktop-thread-light.png" alt="Flectar Mail conversation with incoming and replied messages">
</picture>

### Light

| Full workspace | Minimal workspace |
| --- | --- |
| ![Flectar Mail full desktop workspace in light mode](resources/screenshots/desktop-light.png) | ![Flectar Mail minimal desktop workspace in light mode](resources/screenshots/desktop-minimal-light.png) |

### Dark

| Full workspace | Minimal workspace |
| --- | --- |
| ![Flectar Mail full desktop workspace in dark mode](resources/screenshots/desktop-dark.png) | ![Flectar Mail minimal desktop workspace in dark mode](resources/screenshots/desktop-minimal-dark.png) |

### Color palettes

| Teal | Green | Purple | Custom |
| --- | --- | --- | --- |
| ![Flectar Mail teal palette in light mode](resources/screenshots/desktop-teal-light.png) | ![Flectar Mail green palette in light mode](resources/screenshots/desktop-green-light.png) | ![Flectar Mail purple palette in light mode](resources/screenshots/desktop-purple-light.png) | ![Flectar Mail custom palette in light mode](resources/screenshots/desktop-light.png) |
| ![Flectar Mail teal palette in dark mode](resources/screenshots/desktop-teal-dark.png) | ![Flectar Mail green palette in dark mode](resources/screenshots/desktop-green-dark.png) | ![Flectar Mail purple palette in dark mode](resources/screenshots/desktop-purple-dark.png) | ![Flectar Mail custom palette in dark mode](resources/screenshots/desktop-dark.png) |

### 📅 Calendar, contacts, and files

<table>
  <thead>
    <tr>
      <th width="33.33%">Calendar</th>
      <th width="33.33%">Contacts</th>
      <th width="33.33%">Files (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-calendar-light.png">
          <img src="resources/screenshots/desktop-calendar-light.png" alt="Flectar Mail calendar" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-contacts-light.png">
          <img src="resources/screenshots/desktop-contacts-light.png" alt="Flectar Mail contacts" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-files-light.png">
          <img src="resources/screenshots/desktop-files-light.png" alt="Flectar Mail files" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 💾 Storage and backups

Settings → Storage shows how much space mail, attachments, offline files, and
databases use on the device. From the same page, you can export verified database
snapshots or transfer connected-account setup and preferences through a backup.
Passwords and OAuth tokens stay in the system keyring, so a restored device asks
you to sign in again.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-storage-light.png">
  <img src="resources/screenshots/desktop-storage-light.png" alt="Flectar Mail Storage settings with local usage, database snapshot, and account backup controls">
</picture>

### 📱 Made for smaller screens

The compact interface keeps the important actions within reach while giving
messages, events, and contacts the full screen when they need it.

| Light | Dark |
| --- | --- |
| ![Flectar Mail mobile inbox in light mode](resources/screenshots/mobile-light.png) | ![Flectar Mail mobile inbox in dark mode](resources/screenshots/mobile-dark.png) |

## 📥 Get Flectar Mail

Flectar Mail is currently in development and is not yet stable.

> [!WARNING]
> **Google OAuth verification requires an annual CASA security assessment.**
> Flectar is a community open-source project, so completing this assessment
> depends on sponsor support. Until the required verification is funded and
> complete, a stable release with Gmail OAuth configured by default will not be
> possible. If you want to help make built-in Gmail sign-in available, please
> [sponsor Flectar](https://github.com/sponsors/flectar).
>
> Google and Microsoft OAuth verification is still in progress. Earlier GitHub
> prereleases are intended for testers using their own OAuth registrations or
> IMAP/JMAP accounts.

Builds without OAuth app keys keep Gmail and Outlook sign-in disabled. Use the
**Sign-in settings** cog on the welcome screen to save your own Google or
Microsoft app registration, or connect an IMAP/JMAP account. Custom keys are
shared with Settings and take precedence over bundled keys; clearing a custom
client ID restores the defaults when available. Each provider enables separately
once its configuration is saved.

When preview builds are published, download them from
[GitHub Releases](https://github.com/flectar/mail/releases) and look for the
**Pre-release** badge:

- **Linux x64:** AppImage, Debian/Ubuntu `.deb`, Fedora `.rpm`, or a
  sideloaded Flatpak preview bundle
- **Windows x64:** Setup `.exe` or portable ZIP
- **macOS Apple silicon or Intel (macOS 14+):** DMG or application ZIP
- **Android arm64 (Android 8.0+):** Experimental test APK in prereleases

Windows previews are unsigned; macOS previews are ad-hoc signed and not
notarized, so operating-system security prompts are expected. Install updates
manually.

The Flatpak preview is provided as a standalone test bundle. It is not yet a
Flathub package and therefore does not receive automatic Flathub updates. Its
close-to-tray option is disabled until the tray backend can use a sandbox-safe
D-Bus name.

Install the downloaded Linux package with either
`sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` or
`flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak`. Launch the
Flatpak with `flatpak run com.flectar.mail`.

Each GitHub release includes `SHA256SUMS` and signed build-provenance
attestations. With GitHub CLI installed, verify a download with
`gh attestation verify <download> --repo flectar/mail`.

Android APK updates require the same signing key; builds without a persistent
test key may require uninstalling the previous app, which deletes local app data.

You can also build Flectar Mail from source with the
[Rust toolchain](https://rustup.rs/):

```bash
cargo run --bin flectar-mail
```

Linux desktop OAuth uses the system browser through the desktop portal and
stores refresh credentials through the freedesktop Secret Service. A normal
desktop session therefore needs an `xdg-desktop-portal` backend and a Secret
Service provider such as GNOME Keyring. Release builds stop before opening the
OAuth page when secure credential storage is unavailable, so an authorization
grant can never be completed without a safe place to persist it.

Podman and Docker development shells commonly have neither the host session
D-Bus nor the host browser's loopback network. Debug builds support that setup
with a clearly marked, owner-only development credential file and an accordion
for pasting the final loopback callback URL. The file backend is excluded from
release builds.

## 🌍 Help translate Flectar Mail

Flectar Mail is built for everyone, and we'd love your help making it available in more languages!

We're using [Weblate](https://translate.flectar.com/) to manage community translations. Whether you'd like to translate Flectar Mail into your native language, improve an existing translation, or help review translated strings, every contribution is welcome.

**[Start translating Flectar Mail →](https://translate.flectar.com/)**

Getting started is easy:

1. Create an account on our translation platform.
2. Select your language, or start a new translation if it isn't available yet.
3. Translate strings directly through Weblate, no programming or GitHub experience required.

Translations are synchronized with our GitHub repository and submitted as pull requests for review. Contributors can also receive GitHub attribution for their work.

## 🌐 Open source, for everyone

Flectar Mail is one open-source application. There is no separate community
edition. The client is licensed under the
[GNU Affero General Public License v3](LICENSE). Read the
[licensing overview](LICENSING.md) for the practical details, or see
[CONTRIBUTING.md](CONTRIBUTING.md) to help shape the project.

## 🫡 Acknowledgements

Flectar Mail is made possible by the work of these projects and their
contributors:

- [Slint](https://slint.dev/), the native UI toolkit that powers the Flectar
  Mail interface.
- [Blitz](https://github.com/DioxusLabs/blitz), the Rust HTML/CSS renderer from
  the Dioxus team that powers the email reading experience.

---
