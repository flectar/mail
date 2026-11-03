# Microsoft Store package

Release builds package Flectar Mail for the Microsoft Store as MSIX, next to
the GitHub installer and portable ZIP. The Store signs the package and
delivers updates, so no code-signing certificate is needed for it.

## What CI builds

The **Native platform artifacts** workflow builds an MSIX for x64 and arm64
from the same files as the ZIP, with the manifest and logos in
`platform/windows/msix`. `scripts/test-msix.ps1` signs a copy with a
throwaway certificate, installs it, starts the app with its package identity,
checks that the app's data lands in the package's private folder, and removes
it again. The **Microsoft Store bundle** job then combines both packages into
`flectar-mail-<version>.msixbundle` and tests that too.

Tag pushes, and manual runs with Windows enabled, upload the bundle as the
`microsoft-store-package` artifact. The GitHub release does not include it:
outside the Store, Windows only installs signed packages.

## Before the first submission

1. In Partner Center, choose **Apps and games > New product > MSIX or PWA
   app** and reserve the name **Flectar Mail**.
2. Open **Product management > Product identity** and copy `Package/Identity/Name`,
   `Package/Identity/Publisher` and `Package/Properties/PublisherDisplayName`
   into the `Identity` and `PublisherDisplayName` elements of
   `platform/windows/msix/AppxManifest.xml.in`. Partner Center compares them
   exactly. Until then the manifest carries placeholders, and the workflow
   warns that the bundle can't be uploaded. Tag a release after the change so
   the bundle carries the real identity.

## Submitting a release

1. Download the `microsoft-store-package` artifact from the tag's workflow
   run and unzip it. Workflow artifacts expire after 90 days.
2. Upload the `.msixbundle` on the submission's **Packages** page.
3. The package declares the `runFullTrust` restricted capability, which every
   packaged desktop app needs. Explain it as: "Flectar Mail is a desktop email
   client packaged with MSIX. It runs as a full-trust desktop app, like its
   installer and portable builds."
4. Complete the pages below. Later releases only need a new package and the
   **What's new in this version** text.

Store versions must increase with every submission. The Store requires the
form `Major.Minor.Build.0` with a nonzero first part, so
`scripts/release-metadata.py` derives it from the Cargo version in the same
order as the Android version codes: `0.1.0-alpha.8` becomes `1.100.8.0`, and
`0.1.0` becomes `1.100.9999.0`.

## Store listing

The first submission needs these pages. The text matches the README.

**Pricing and availability:** free, in all markets.

**Properties:** category **Productivity**. Privacy policy
<https://flectar.com/mail/privacy/>, website <https://flectar.com/mail/>,
support <https://github.com/flectar/mail/issues>. The app stores mail, contacts
and credentials, so answer yes to accessing personal information.

**Age ratings:** the questionnaire asks about communication between users.
Email lets users exchange messages and content, so answer yes there.

**Short description:**

> A native email client for your mail, calendars, and contacts. It opens
> instantly, works offline, and uses a fraction of the memory of web-based
> mail apps.

**Description:**

> Flectar Mail is a lightweight, native home for your email, calendars, and
> contacts. It is engineered to open instantly, stay responsive, and use a
> fraction of the memory of a typical web-based mail client.
>
> It works with Gmail, Outlook and Microsoft 365, or standards-based IMAP,
> POP3, SMTP, JMAP, CalDAV, and CardDAV services. Your mailbox and calendar are
> stored locally, so your synced data stays useful without a connection.
> Remote images are blocked until you allow them, so tracking pixels can't
> report when you read a message.
>
> Email is never opened in a web view. A Rust-native pipeline renders its HTML
> and CSS without running email scripts. Rendering email without a browser
> engine is new territory, so complex messages may not render correctly yet
> while compatibility improves.
>
> Flectar Mail is free and open source under the AGPLv3.

**Product features:**

- Mail, calendars, and contacts in one app
- About 20 MB of private memory at idle
- A local copy of your mailbox that works offline
- Gmail, Outlook, Microsoft 365, IMAP, POP3, SMTP, JMAP, CalDAV, and CardDAV
- Remote images blocked until you allow them
- Email rendered without a web view or scripts
- Light and dark themes
- Free and open source

**Search terms:** email, mail client, calendar, contacts, IMAP, Outlook, Gmail.

**Screenshots:** `resources/screenshots/frameless/windows-light.png` and
`windows-dark.png` show the app with Windows window controls.

**Notes for certification:** testers need an email account to get past the
welcome screen. Create a mailbox for the review, and put its address,
password and IMAP and SMTP servers in the notes. Tell testers to add it with
**IMAP** under **Connect a mail account** on the welcome screen.

## How the Store build differs

- **Start at sign-in** uses the package's startup task, which appears under
  **Settings > Apps > Startup**. Packaged apps can't use the `Run` registry key
  that the installer build writes. If the user turns the task off there, only
  they can turn it back on, so the app opens that page for them.
- **App data** of a new install stays in
  `%LOCALAPPDATA%\Packages\<package family name>\LocalCache`, which Windows
  removes when the app is uninstalled. On a PC that already ran the installer
  or portable build, the Store build picks up that build's data instead.
- **Updates** come from the Store.
