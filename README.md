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
    <p>Built from the ground up for speed. Flectar Mail delivers native performance, instant startup, and about 20 MB of private memory at idle.</p>
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

| Benefit | Details |
| --- | --- |
| **Fast from the first click** | A native interface and local-first data path get you to your inbox without waiting on a browser runtime. |
| **About 20 MB of private memory at idle** | About 85 MB counting its code and libraries, and a twelfth of Thunderbird's private memory with the same mailbox. See [Performance](#-performance). |
| **Everything in one place** | Move between mail, calendars, and contacts without stitching together separate apps. |
| **Offline by design** | Your mailbox and calendar are stored locally, so your synced data stays useful without a connection. |
| **Works with your accounts** | Gmail, Outlook and Microsoft 365, or standards-based IMAP, POP3, SMTP, JMAP, CalDAV, and CardDAV services. |
| **Privacy-conscious defaults** | Remote images are blocked until you allow them, so tracking pixels can't report when you read a message. |
| **Security by architecture** | Email is never opened in a WebView. A Rust-native pipeline renders its HTML and CSS without running email scripts, avoiding the embedded-browser attack surface. |
| **An experimental Rust renderer** | Rendering email without a browser engine is new territory. Complex messages may not render correctly yet while compatibility improves. |
| **Made for every screen** | Spacious and minimal desktop layouts share the same experience as the touch-friendly compact interface. |
| **Native and open source** | Built from the ground up with Rust, not a browser wrapped in a window, and released under the AGPLv3. |

## ⚡ Performance

| Idle, same mailbox | Private memory | Including code and libraries (PSS) | Renders email with |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17.2–17.4 MiB** | **79.1–80.0 MiB** | Blitz |
| Geary 46 | 126.1–126.2 MiB | 309.6–311.4 MiB | WebKitGTK |
| Evolution 3.56 | 191.0–192.2 MiB | 395.0–395.9 MiB | WebKitGTK |
| Thunderbird 159 | 244.1–248.1 MiB | 424.4–427.1 MiB | Gecko |
| Betterbird 153 | 243.9–247.8 MiB | 411.9–419.7 MiB | Gecko |
| Mailspring 1.26 | 311.4–314.9 MiB | 551.1–551.7 MiB | Chromium |
| Aerion 0.3.6 | 421.9–437.3 MiB | 660.4–678.0 MiB | WebKitGTK |

Measured on Linux with one IMAP account (860 messages), the inbox open and an
HTML newsletter displayed, 60 seconds after launch, with the default CPU
renderer. Each range covers two runs, and every process an app starts is
counted. Private memory is what Windows Task Manager lists as **Memory**. On
Windows, with 7 real accounts and about 69,000 unread messages, Task Manager
showed 37.8 MiB with the window open and 4.2 MiB once it was closed to the
tray. See [how we measure and how to check it yourself](docs/performance.md).

### How it compares

| Email client | Interface | Renders email with | Platforms |
| --- | --- | --- | --- |
| **Flectar Mail** | Native Rust app | Blitz, a Rust HTML/CSS renderer; no WebView | Windows, macOS, Linux, Android, iOS (sideloaded) |
| Thunderbird | Gecko-based desktop app | Gecko | Windows, macOS, Linux, Android |
| Outlook for Windows | Windows shell with a web UI | Microsoft Edge WebView2 | Windows |
| Spark | Electron app | Chromium | macOS, Windows, iOS, Android |
| Aerion | Go app with a Wails web UI | System WebView | Linux, macOS, Windows |
| Gmail in a browser tab | Web app | Chrome or another browser | Web |

## ✨ Features

| Feature | What it does |
| --- | --- |
| [Conversation threads](docs/features.md#conversation-threads) | Groups replies in chronological order, with full keyboard navigation. You can turn it off. |
| [Tabs](docs/features.md#tabs) | Browser-style tabs that remember their place, with shortcuts on desktop and a tab switcher on phones. |
| [Profiles and account colors](docs/features.md#profiles-and-account-colors) | Groups accounts into profiles such as Work or Personal and color-codes every message. |
| [Appearance](docs/features.md#appearance) | Full or minimal layout, light or dark theme, color palettes, sender avatars and app icon colors. |
| [Frameless window](docs/features.md#frameless-window) | Window controls in the toolbar that keep each platform's behavior. |
| [Calendar, contacts, and files](docs/features.md#calendar-contacts-and-files) | CalDAV and CardDAV sync, JMAP and WebDAV files, attachment search, offline files and previews. |
| [Signatures and OpenPGP](docs/features.md#signatures-and-openpgp) | Named signatures, and OpenPGP/MIME signing and encryption on desktop through GnuPG. |
| [Storage and backups](docs/features.md#storage-and-backups) | Space used per type, database snapshots, account backups and POP3 server rules. |
| [Importing accounts and mail](docs/features.md#importing-accounts-and-mail) | Thunderbird, K-9 Mail, mbox, `.eml`, maildir, Apple Mail and ZIP archives. |
| [HTML rendering](docs/features.md#html-rendering) | Email HTML rendered in Rust with Blitz, on the CPU or the GPU. |

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

### Tabs, conversations, and profiles

<table>
  <thead>
    <tr>
      <th width="33.33%">Tabs</th>
      <th width="33.33%">Conversation</th>
      <th width="33.33%">Profile colors</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-tabs-light.png">
          <img src="resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail with tabs for several conversations, the calendar, contacts, and files" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-thread-light.png">
          <img src="resources/screenshots/desktop-thread-light.png" alt="Flectar Mail conversation with incoming and replied messages" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="resources/screenshots/desktop-profiles-light.png">
          <img src="resources/screenshots/desktop-profiles-light.png" alt="Flectar Mail unified inbox with purple and orange profile color markers" width="100%">
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

### Frameless window

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

### Calendar, contacts, and files

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

### Made for smaller screens

| Light | Dark |
| --- | --- |
| ![Flectar Mail mobile inbox in light mode](resources/screenshots/mobile-light.png) | ![Flectar Mail mobile inbox in dark mode](resources/screenshots/mobile-dark.png) |

## 🧪 Experimental HTML rendering

> [!WARNING]
> HTML email rendering is currently the most experimental part of Flectar Mail.
> Some messages, especially those with complex or unusual markup and CSS, may
> not render correctly yet.

Flectar Mail renders email with [Blitz](https://github.com/DioxusLabs/blitz), a
Rust HTML/CSS renderer from the Dioxus team, instead of embedding a browser or
WebView. If a message renders incorrectly, please
[report it](https://github.com/flectar/mail/issues). Read more about
[the renderer and the CPU and GPU options](docs/features.md#html-rendering).

## 📥 Get Flectar Mail

Flectar Mail is in development and not yet stable. Preview builds are on
[GitHub Releases](https://github.com/flectar/mail/releases).

> [!WARNING]
> **Google OAuth verification requires an annual CASA security assessment.**
> Flectar is a community open-source project, so completing this assessment
> depends on sponsor support. Until the required verification is funded and
> complete, a stable release with Gmail OAuth configured by default will not be
> possible. If you want to help make built-in Gmail sign-in available, please
> [sponsor Flectar](https://github.com/sponsors/flectar).

| Platform | Download | Notes |
| --- | --- | --- |
| Linux x64 and arm64 | AppImage, Debian/Ubuntu `.deb`, Fedora 43/44 `.rpm`, or a Flatpak bundle | Needs a Secret Service keyring. The Flatpak is a sideloaded preview, not on Flathub yet. |
| Windows x64 and arm64 | Setup `.exe` or portable ZIP | Unsigned previews |
| macOS 14+ (Apple silicon or Intel) | DMG or application ZIP | Ad-hoc signed, not notarized |
| Android 8.0+ (arm64) | Signed APK | Previews and stable releases share one app identity. |
| iOS 17+ (iPhone and iPad) | Unsigned `.ipa` | Sideload it with AltStore, SideStore, or Sideloadly and your Apple ID. |
| HarmonyOS and OpenHarmony | [Community port](platform/ohos/README.md) by [springmin](https://github.com/springmin) | Not part of GitHub releases; build it from source. |

Builds without OAuth app keys keep Gmail and Outlook sign-in disabled. Use the
**Sign-in settings** cog on the welcome screen to add your own Google or
Microsoft app registration, or connect an IMAP, POP3, or JMAP account.

The [installation guide](docs/install.md) covers install commands, the Linux
keyring and desktop portal, Flatpak and RPM notes, verifying downloads, Android
signing, iOS sideloading, and building from source with
`cargo run --bin flectar-mail`.

## 🤝 Get involved

| Topic | Details |
| --- | --- |
| 🌍 **Translate** | Translate the app, improve a translation, or review strings on [Weblate](https://translate.flectar.com/), with no programming or GitHub experience required. Translations reach this repository as pull requests, with GitHub attribution for contributors. README translations live here; see [Contributing translations](CONTRIBUTING.md#translations). |
| ♿ **Accessibility** | Keyboard navigation, light and dark themes, and flexible layouts, with work ongoing. [ACCESSIBILITY.md](ACCESSIBILITY.md) covers current features, testing limitations, and how to report a barrier. |
| 🔒 **Security** | Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), and keep their details out of public issues. |
| 🌐 **Open source** | One open-source application with no separate community edition, licensed under the [GNU Affero General Public License v3](LICENSE). The [licensing overview](LICENSING.md) has the practical details. |
| 🛠️ **Contribute** | [CONTRIBUTING.md](CONTRIBUTING.md) explains how to help shape the project. |

## 🫡 Acknowledgements

Flectar Mail is made possible by the work of these projects and their
contributors:

- [Slint](https://slint.dev/), the native UI toolkit that powers the Flectar
  Mail interface.
- [Blitz](https://github.com/DioxusLabs/blitz), the Rust HTML/CSS renderer from
  the Dioxus team that powers the email reading experience.

---
