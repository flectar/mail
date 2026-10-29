<div align="center">
  <img src="resources/app-icon/flectar-mail-masked.png" width="88" alt="Flectar Mail logo">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <strong>English</strong> · <a href="docs/readme/README.es.md">Español</a> · <a href="docs/readme/README.de.md">Deutsch</a> ·
    <a href="docs/readme/README.fr.md">Français</a> · <a href="docs/readme/README.pt_BR.md">Português (Brasil)</a> · <a href="docs/readme/README.ru.md">Русский</a> ·
    <a href="docs/readme/README.hi.md">हिन्दी</a> · <a href="docs/readme/README.ar.md">العربية</a> · <a href="docs/readme/README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>Email, made fast again</h3>
    <p>Built from the ground up for speed. Flectar Mail delivers native performance, instant startup, and as little as 80 MB of RAM.</p>
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
  <img src="resources/screenshots/desktop-light.png" alt="Flectar Mail on macOS showing the unified inbox and message view">
</picture>

Flectar Mail is a lightweight, native home for your email, calendars, and
contacts. It is engineered to open instantly, stay responsive, and use a
fraction of the memory of a typical web-based mail client.

## ❓ Why Flectar Mail?

- **Fast from the first click.** A native interface and local-first data path
  get you to your inbox without waiting on a browser runtime.
- **As little as 80 MB of RAM.** Flectar Mail is deliberately designed to keep
  memory use low, even with a full-featured inbox at your fingertips.
- **Everything in one place.** Move between mail, calendars, and contacts
  without stitching together separate apps.
- **Offline by design.** Your mailbox and calendar are stored locally, so your
  synced data remains useful without a connection.
- **Works with your accounts.** Connect Gmail, Outlook and Microsoft 365, or
  standards-based IMAP, POP3, SMTP, JMAP, CalDAV, and CardDAV services.
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

To keep the client fully native and memory usage around 80 MB, Flectar Mail
renders email HTML with [Blitz](https://github.com/DioxusLabs/blitz), a Rust
HTML/CSS renderer from the Dioxus team, instead of embedding a browser or
WebView.

Desktop builds include **CPU — Low Memory** and **GPU — WGPU** under
Settings → General → Renderer. CPU is selected initially and does not initialize
WGPU. The GPU option uses Slint and Vello on a shared WGPU 30 device; changing
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
sender avatars. You can also give the app icon a different color: on the
taskbar or Dock, in the system tray, in the Linux app launcher, and on Android
and iOS home screens.

Message appearance can follow the app theme or stay light or dark independently.
It uses your selected color palette for HTML, plain text, and Reader mode.
To temporarily show the sender's original colors, use the contrast icon between
Find and Reader mode in the desktop message toolbar, or choose **Show original
colors** from the message's three-dot menu on any screen size.

The full workspace keeps your folders, message list, and selected email visible
together. The minimal layout reduces visual noise and gives each part of your
inbox more room when you need it.

### 👥 Profiles and account colors

Group related accounts into named profiles such as Work or Personal, then give
each profile its own color. Create a profile from the profile menu at the start
of the tab bar, or in Settings → Accounts → Profiles, and check the accounts
that belong to it. Choose a profile in that menu to focus the mail view on its
accounts: the unified inbox, search, counts, and sidebar show only their mail,
while your tabs stay the same. **All accounts** shows everything again.

You can also override an individual account's color and show the effective
color along the left edge of every message. The markers make accounts easy to
tell apart while working in the unified inbox; the example below uses purple
for Work and orange for Support.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-profiles-light.png">
  <img src="resources/screenshots/desktop-profiles-light.png" alt="Flectar Mail unified inbox with purple and orange profile color markers">
</picture>

### 🧵 Conversation threads

Replies stay grouped in chronological order, with the active message expanded
inside the reading pane.

Use **Up/Down** to navigate messages, **Home/End** to jump to the first or last
loaded message, and **Page Up/Page Down** to move a page at a time. **Enter**
opens the current message and focuses its content; **Space** checks or unchecks
it for bulk actions. **Tab/Shift+Tab** move between controls. In the full
workspace, **F6** moves focus between the message list and reader. In the minimal
layout, arrow keys highlight messages and **Enter** opens them; **Escape** or
**F6** returns to the list.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-thread-light.png">
  <img src="resources/screenshots/desktop-thread-light.png" alt="Flectar Mail conversation with incoming and replied messages">
</picture>

### 🗂️ Tabs

Keep several places open at once with browser-style tabs. Each tab remembers
its own place: a mailbox, search, or open conversation, the calendar,
contacts, files, or Settings, along with how far you scrolled the list and the
message you were reading. Middle-click or Ctrl/⌘-click a message to open it in
a background tab (add Shift to switch to it), or choose **Open in new tab**
from its menu. Drag tabs to reorder them, middle-click a tab to close it,
right-click a tab to close the others or those to its right, and pick up where
you left off the next time you open Flectar Mail.

**Ctrl/⌘+T** opens a new tab, **Ctrl/⌘+W** closes the current one, and
**Ctrl/⌘+Shift+T** reopens the last tab you closed. **Ctrl+Tab** and
**Ctrl+Shift+Tab** cycle through tabs, **Ctrl/⌘+1–8** jump to a tab, and
**Ctrl/⌘+9** jumps to the last.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-tabs-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-tabs-light.png">
  <img src="resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail with tabs for several conversations, the calendar, contacts, and files">
</picture>

### Full and minimal workspaces

<table>
  <thead>
    <tr>
      <th width="50%">Full workspace</th>
      <th width="50%">Minimal workspace</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-light.png">
          <img src="resources/screenshots/desktop-light.png" alt="Flectar Mail full desktop workspace" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-minimal-light.png">
          <img src="resources/screenshots/desktop-minimal-light.png" alt="Flectar Mail minimal desktop workspace" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Color palettes

<table>
  <thead>
    <tr>
      <th width="25%">Teal</th>
      <th width="25%">Green</th>
      <th width="25%">Purple</th>
      <th width="25%">Custom</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-teal-light.png">
          <img src="resources/screenshots/desktop-teal-light.png" alt="Flectar Mail teal palette" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-green-light.png">
          <img src="resources/screenshots/desktop-green-light.png" alt="Flectar Mail green palette" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-purple-light.png">
          <img src="resources/screenshots/desktop-purple-light.png" alt="Flectar Mail purple palette" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-light.png">
          <img src="resources/screenshots/desktop-light.png" alt="Flectar Mail custom palette" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 🪟 Frameless window

Turn on **Frameless window** in Settings → General → Interface to replace the
system title bar with window controls in the toolbar. Each platform keeps its
own behavior: macOS keeps the native traffic lights, Windows keeps Snap Layouts
and the system menu, and Linux follows the button order and title-bar actions
configured for GNOME or KDE.

<table>
  <tbody>
    <tr>
      <th width="50%">macOS</th>
      <th width="50%">Windows</th>
    </tr>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/frameless/macos-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/frameless/macos-light.png">
          <img src="resources/screenshots/frameless/macos-light.png" alt="Flectar Mail frameless window on macOS" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/frameless/windows-light.png">
          <img src="resources/screenshots/frameless/windows-light.png" alt="Flectar Mail frameless window on Windows" width="100%">
        </picture>
      </td>
    </tr>
    <tr>
      <th width="50%">Linux (GNOME)</th>
      <th width="50%">Linux (KDE)</th>
    </tr>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/frameless/linux-gnome-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/frameless/linux-gnome-light.png">
          <img src="resources/screenshots/frameless/linux-gnome-light.png" alt="Flectar Mail frameless window on GNOME" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/frameless/linux-kde-light.png">
          <img src="resources/screenshots/frameless/linux-kde-light.png" alt="Flectar Mail frameless window on KDE Plasma" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

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

POP3 accounts download mail to this device and leave it on the server unless
you choose otherwise: each account can remove mail from the server after 7, 14,
or 30 days, once you delete it here, or as soon as it's downloaded, but never
before it's stored on this device. Their folders exist only on this device, so
snapshots include that mail, and removing a POP3 account or deleting all data
offers to export it first as mbox files, one per folder, which other mail apps
can import.

The same import also brings accounts over from Thunderbird (a profile folder,
its Tools › Export archive, or `prefs.js`) and from Thunderbird for Android or
K-9 Mail (`.k9s` settings exports). You review each account before anything is
added: settings Flectar Mail can't reproduce, such as an Exchange server, are
explained instead of changed silently, and POP3 accounts keep their rule for
removing mail from the server. Existing accounts are never modified, and
passwords are never imported.

Saved mail comes over the same way: Thunderbird's Local Folders and the mail
its POP3 accounts downloaded, mbox files such as a Google Takeout export or
Flectar Mail's own, `.eml` messages, maildir folders, Apple Mail mailboxes, and
ZIP archives of them. You pick the files, several at once if you like, then the
folders, with Trash and Junk left out unless you check them, and the folder
structure and read and starred states come along. Mail that's already there is
skipped, so a stopped import can simply run again, and a long one can carry on
in the background. Mail that belongs to no account goes to **On this device**:
folders kept only here, which Settings → Storage can export or delete. It can
also go into a POP3 account's folders, or up to the server of an IMAP, JMAP or
Gmail account, into a folder of its own there.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-storage-light.png">
  <img src="resources/screenshots/desktop-storage-light.png" alt="Flectar Mail Storage settings with local usage, mail on this device, database snapshot, and account backup controls">
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
> IMAP, POP3, or JMAP accounts.

Builds without OAuth app keys keep Gmail and Outlook sign-in disabled. Use the
**Sign-in settings** cog on the welcome screen to save your own Google or
Microsoft app registration, or connect an IMAP, POP3, or JMAP account. Custom keys are
shared with Settings and take precedence over bundled keys; clearing a custom
client ID restores the defaults when available. Each provider enables separately
once its configuration is saved.

When preview builds are published, download them from
[GitHub Releases](https://github.com/flectar/mail/releases) and look for the
**Pre-release** badge:

- **Linux x64:** AppImage, Debian/Ubuntu `.deb`, Fedora 43/44 `.rpm`, or a
  sideloaded Flatpak preview bundle
- **Windows x64:** Setup `.exe` or portable ZIP
- **macOS Apple silicon or Intel (macOS 14+):** DMG or application ZIP
- **Android arm64 (Android 8.0+):** Signed APK; previews and stable releases share one app identity

Windows previews are unsigned; macOS previews are ad-hoc signed and not
notarized, so operating-system security prompts are expected. Install updates
manually.

The Flatpak preview is provided as a standalone test bundle. It is not yet a
Flathub package and therefore does not receive automatic Flathub updates. Its
close-to-tray option is disabled until the tray backend can use a sandbox-safe
D-Bus name. Starting automatically at sign-in goes through the desktop's
Background portal and opens the window, because there is no tray to start into.
The app icon stays orange there, because the sandbox cannot change the launcher
icon on the host.

Install the downloaded Linux package with either
`sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` or
`flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak`. Launch the
Flatpak with `flatpak run com.flectar.mail`.

The RPM targets Fedora 43 and 44 on x86_64. Release builds use Fedora 43 as
their compatibility baseline; the same RPM must pass installation, PDF
rendering, X11 and Wayland startup, and removal checks in clean containers for
both versions before publication. Other RPM-based distributions are not yet
tested. If Discover reports a generic conflict, run the `dnf install` command
above and include its dependency error in your bug report.

The older `0.1.0-alpha.7` RPM was built on Fedora 44 and cannot install on
Fedora 43 because it requires glibc 2.43. Its Flatpak bundle is an alternative
while waiting for an RPM release built with the compatibility checks above.

Each GitHub release includes `SHA256SUMS` and signed build-provenance
attestations. With GitHub CLI installed, verify a download with
`gh attestation verify <download> --repo flectar/mail`.

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

See [Android signing setup and recovery](docs/android-signing.md) for the
one-time migration command, required secrets, backups, and Play App Signing.

Local developer builds can still generate a local test key. They only update
installations signed with that same local key.

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

README translations are maintained in this repository. See
[Contributing translations](CONTRIBUTING.md#translations) to add or update one.

## ♿ Accessibility

Flectar Mail includes keyboard navigation, light and dark themes, and flexible
layouts. Accessibility work is ongoing. See [ACCESSIBILITY.md](ACCESSIBILITY.md)
for current features, testing limitations, and how to report a barrier.

## 🔒 Security

To report a vulnerability privately, follow [SECURITY.md](SECURITY.md).
Please keep vulnerability details out of public issues.

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
