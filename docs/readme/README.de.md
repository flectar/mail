<div align="center">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="Flectar-Mail-Logo">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <a href="../../README.md">English</a> · <a href="README.es.md">Español</a> · <strong>Deutsch</strong> ·
    <a href="README.fr.md">Français</a> · <a href="README.pt_BR.md">Português (Brasil)</a> · <a href="README.ru.md">Русский</a> ·
    <a href="README.hi.md">हिन्दी</a> · <a href="README.ar.md">العربية</a> · <a href="README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>E-Mail wird wieder schnell</h3>
    <p>Von Grund auf für Geschwindigkeit entwickelt. Flectar Mail bietet native Leistung, startet sofort und belegt im Leerlauf nur etwa 20 MB privaten Speicher.</p>
  </div>
  <p>
    <a href="https://flectar.com">Website</a> ·
    <a href="https://github.com/flectar/mail/issues">Problem melden</a> ·
    <a href="../../CONTRIBUTING.md">Mitwirken</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="Lizenz: AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="Downloads aller Versionen"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="Neueste Version"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="Übersetzungsstand"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="Sprache: Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="Plattformen: Windows, macOS, Linux, Android, iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail unter macOS mit gemeinsamem Posteingang und Nachrichtenansicht">
</picture>

Flectar Mail ist eine schlanke, native Anwendung für E-Mails, Kalender und Kontakte. Sie startet sofort, bleibt reaktionsschnell und benötigt nur einen Bruchteil des Arbeitsspeichers eines üblichen webbasierten E-Mail-Clients.

## ❓ Warum Flectar Mail?

| Vorteil | Details |
| --- | --- |
| **Schnell vom ersten Klick an** | Eine native Oberfläche und der bevorzugte Zugriff auf lokale Daten bringen dich zum Posteingang, ohne auf eine Browserlaufzeit warten zu müssen. |
| **Etwa 20 MB privater Speicher im Leerlauf** | Etwa 85 MB samt Programmcode und Bibliotheken, und ein Zwölftel des privaten Speichers, den Thunderbird mit demselben Postfach belegt. Siehe [Leistung](#-leistung). |
| **Alles an einem Ort** | Wechsle zwischen E-Mails, Kalendern und Kontakten, ohne verschiedene Anwendungen zusammenstellen zu müssen. |
| **Für die Offline-Nutzung entwickelt** | Postfach und Kalender werden lokal gespeichert. Synchronisierte Daten bleiben so auch ohne Verbindung nutzbar. |
| **Für deine Konten** | Gmail, Outlook und Microsoft 365 oder Dienste auf Basis von IMAP, POP3, SMTP, JMAP, CalDAV und CardDAV. |
| **Datenschutz als Voreinstellung** | Externe Bilder bleiben bis zu deiner Freigabe gesperrt, sodass Tracking-Pixel nicht melden können, wann du eine Nachricht liest. |
| **Sicherheit durch die Architektur** | E-Mails werden niemals in einer WebView geöffnet. Eine native Rust-Pipeline rendert ihr HTML und CSS, ohne darin enthaltene Skripte auszuführen, und vermeidet so die Angriffsfläche eines eingebetteten Browsers. |
| **Ein experimenteller Rust-Renderer** | E-Mails ohne Browser-Engine darzustellen ist Neuland. Die Kompatibilität wird laufend verbessert, doch komplexe Nachrichten werden möglicherweise noch nicht korrekt angezeigt. |
| **Für jeden Bildschirm** | Die großzügige und die minimalistische Desktop-Ansicht bieten dieselbe Erfahrung wie die kompakte, für Touch-Bedienung optimierte Oberfläche. |
| **Nativ und quelloffen** | Von Grund auf mit Rust entwickelt. Kein Browser in einem Fenster, sondern eine unter AGPLv3 veröffentlichte Anwendung. |

## ⚡ Leistung

| Im Leerlauf, gleiches Postfach | Privater Speicher | Einschließlich Code und Bibliotheken (PSS) | E-Mail-Darstellung |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17,2–17,4 MiB** | **79,1–80,0 MiB** | Blitz |
| Geary 46 | 126,1–126,2 MiB | 309,6–311,4 MiB | WebKitGTK |
| Evolution 3.56 | 191,0–192,2 MiB | 395,0–395,9 MiB | WebKitGTK |
| Thunderbird 159 | 244,1–248,1 MiB | 424,4–427,1 MiB | Gecko |
| Betterbird 153 | 243,9–247,8 MiB | 411,9–419,7 MiB | Gecko |
| Mailspring 1.26 | 311,4–314,9 MiB | 551,1–551,7 MiB | Chromium |
| Aerion 0.3.6 | 421,9–437,3 MiB | 660,4–678,0 MiB | WebKitGTK |

Gemessen unter Linux mit einem IMAP-Konto (860 Nachrichten), geöffnetem Posteingang und angezeigtem HTML-Newsletter, 60 Sekunden nach dem Start und mit dem voreingestellten CPU-Renderer. Jeder Wertebereich umfasst zwei Messungen, und mitgezählt sind alle Prozesse, die eine App startet. Privater Speicher entspricht dem Wert, den der Windows-Task-Manager als **Arbeitsspeicher** anzeigt. Unter Windows mit 7 echten Konten und etwa 69.000 ungelesenen Nachrichten zeigte der Task-Manager 37,8 MiB bei geöffnetem Fenster und 4,2 MiB, sobald es in den Infobereich geschlossen war. Erfahre, [wie wir messen und wie du selbst nachmessen kannst](../performance.md).

### Im Vergleich

| E-Mail-Client | Oberfläche | E-Mail-Darstellung | Plattformen |
| --- | --- | --- | --- |
| **Flectar Mail** | Native Rust-App | Blitz, ein Rust-Renderer für HTML/CSS; keine WebView | Windows, macOS, Linux, Android, iOS (per Sideloading) |
| Thunderbird | Gecko-basierte Desktop-App | Gecko | Windows, macOS, Linux, Android |
| Outlook für Windows | Windows-App mit Web-Oberfläche | Microsoft Edge WebView2 | Windows |
| Spark | Electron-App | Chromium | macOS, Windows, iOS, Android |
| Aerion | Go-App mit Web-Oberfläche auf Wails-Basis | System-WebView | Linux, macOS, Windows |
| Gmail in einem Browser-Tab | Web-App | Chrome oder ein anderer Browser | Web |

## ✨ Funktionen

| Funktion | Was sie tut |
| --- | --- |
| [Unterhaltungen](../features.md#conversation-threads) | Gruppiert Antworten chronologisch und ist vollständig per Tastatur bedienbar. Lässt sich ausschalten. |
| [Tabs](../features.md#tabs) | Tabs wie im Browser, die sich merken, wo du warst. Auf dem Desktop mit Tastenkürzeln, auf dem Smartphone mit Tab-Übersicht. |
| [Profile und Kontofarben](../features.md#profiles-and-account-colors) | Fasst Konten in Profilen wie Arbeit oder Privat zusammen und kennzeichnet jede Nachricht farblich. |
| [Erscheinungsbild](../features.md#appearance) | Vollständiges oder minimalistisches Layout, helles oder dunkles Design, Farbpaletten, Absenderbilder und Farben für das App-Symbol. |
| [Rahmenloses Fenster](../features.md#frameless-window) | Fenstersteuerelemente in der Werkzeugleiste, die sich wie auf der jeweiligen Plattform üblich verhalten. |
| [Kalender, Kontakte und Dateien](../features.md#calendar-contacts-and-files) | Synchronisierung über CalDAV und CardDAV, Dateien über JMAP und WebDAV, Suche in Anhängen, Offline-Dateien und Vorschauen. |
| [Signaturen und OpenPGP](../features.md#signatures-and-openpgp) | Benannte Signaturen sowie Signieren und Verschlüsseln mit OpenPGP/MIME auf dem Desktop über GnuPG. |
| [Speicher und Sicherungen](../features.md#storage-and-backups) | Speicherbelegung nach Datentyp, Datenbank-Snapshots, Kontosicherungen und Löschregeln für POP3-Server. |
| [Konten und E-Mails importieren](../features.md#importing-accounts-and-mail) | Thunderbird, K-9 Mail, mbox, `.eml`, Maildir, Apple Mail und ZIP-Archive. |
| [HTML-Darstellung](../features.md#html-rendering) | Rendert E-Mail-HTML in Rust mit Blitz, wahlweise auf der CPU oder der GPU. |

### Vollständige und minimalistische Arbeitsumgebung

<table>
  <thead>
    <tr>
      <th width="50%">Vollständige Ansicht</th>
      <th width="50%">Minimalistische Ansicht</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Vollständige Desktop-Arbeitsumgebung von Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="Minimalistische Desktop-Arbeitsumgebung von Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Tabs, Unterhaltungen und Profile

<table>
  <thead>
    <tr>
      <th width="33.33%">Tabs</th>
      <th width="33.33%">Unterhaltung</th>
      <th width="33.33%">Profilfarben</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
          <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail mit Tabs für mehrere Unterhaltungen, Kalender, Kontakte und Dateien" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
          <img src="../../resources/screenshots/desktop-thread-light.png" alt="Flectar-Mail-Unterhaltung mit eingegangenen Nachrichten und Antworten" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
          <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Gemeinsamer Posteingang von Flectar Mail mit violetten und orangefarbenen Profilmarkierungen" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Farbpaletten

<table>
  <thead>
    <tr>
      <th width="25%">Türkis</th>
      <th width="25%">Grün</th>
      <th width="25%">Violett</th>
      <th width="25%">Eigene</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="Türkise Farbpalette von Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="Grüne Farbpalette von Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="Violette Farbpalette von Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Eigene Farbpalette von Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Rahmenloses Fenster

<table>
  <tbody>
    <tr>
      <th width="50%">macOS</th>
      <th width="50%">Windows</th>
    </tr>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/macos-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/macos-light.png">
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="Rahmenloses Flectar-Mail-Fenster unter macOS" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="Rahmenloses Flectar-Mail-Fenster unter Windows" width="100%">
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
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-gnome-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-gnome-light.png">
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="Rahmenloses Flectar-Mail-Fenster unter GNOME" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="Rahmenloses Flectar-Mail-Fenster unter KDE Plasma" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Kalender, Kontakte und Dateien

<table>
  <thead>
    <tr>
      <th width="33.33%">Kalender</th>
      <th width="33.33%">Kontakte</th>
      <th width="33.33%">Dateien (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="Kalender von Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="Kontakte von Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="Dateien von Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Für kleinere Bildschirme

| Hell | Dunkel |
| --- | --- |
| ![Mobiler Posteingang von Flectar Mail im hellen Design](../../resources/screenshots/mobile-light.png) | ![Mobiler Posteingang von Flectar Mail im dunklen Design](../../resources/screenshots/mobile-dark.png) |

## 🧪 Experimentelle HTML-Darstellung

> [!WARNING]
> Die Darstellung von HTML-E-Mails ist derzeit der experimentellste Teil von Flectar Mail. Manche Nachrichten, insbesondere mit komplexem oder ungewöhnlichem Markup und CSS, werden möglicherweise noch nicht korrekt angezeigt.

Flectar Mail rendert E-Mails mit [Blitz](https://github.com/DioxusLabs/blitz), einem Rust-Renderer für HTML/CSS des Dioxus-Teams, statt einen Browser oder eine WebView einzubetten. Falls eine Nachricht falsch dargestellt wird, [melde das Problem](https://github.com/flectar/mail/issues). Erfahre mehr über [den Renderer und die Optionen für CPU und GPU](../features.md#html-rendering).

## 📥 Flectar Mail herunterladen

Flectar Mail wird derzeit entwickelt und ist noch nicht stabil. Vorschauversionen findest du unter [GitHub Releases](https://github.com/flectar/mail/releases).

> [!WARNING]
> **Die OAuth-Verifizierung von Google erfordert eine jährliche CASA-Sicherheitsprüfung.** Flectar ist ein gemeinschaftliches Open-Source-Projekt, daher hängt diese Prüfung von Sponsoren ab. Solange die erforderliche Verifizierung nicht finanziert und abgeschlossen ist, kann es keine stabile Version mit standardmäßig eingerichtetem Gmail OAuth geben. Wenn du die integrierte Gmail-Anmeldung ermöglichen möchtest, [unterstütze Flectar](https://github.com/sponsors/flectar).

| Plattform | Download | Hinweise |
| --- | --- | --- |
| Linux x64 und arm64 | AppImage, Debian/Ubuntu `.deb`, Fedora 43/44 `.rpm` oder ein Flatpak-Paket | Benötigt einen Secret-Service-Schlüsselbund. Das Flatpak ist eine Vorschau zur manuellen Installation und noch nicht auf Flathub verfügbar. |
| Windows x64 und arm64 | `.exe`-Installationsprogramm oder portables ZIP | Unsignierte Vorschauversionen |
| macOS 14+ (Apple silicon oder Intel) | DMG oder Anwendungs-ZIP | Ad hoc signiert, nicht notarisiert |
| Android 8.0+ (arm64) | Signiertes APK | Vorschauversionen und stabile Veröffentlichungen teilen dieselbe App-Identität. |
| iOS 17+ (iPhone und iPad) | Unsigniertes `.ipa` | Installiere es mit AltStore, SideStore oder Sideloadly und deiner Apple-ID. |
| HarmonyOS und OpenHarmony | [Community-Portierung](../../platform/ohos/README.md) von [springmin](https://github.com/springmin) | Nicht Teil der GitHub-Veröffentlichungen; du baust sie selbst aus dem Quellcode. |

In Builds ohne OAuth-App-Schlüssel bleiben Gmail- und Outlook-Anmeldung deaktiviert. Über das Zahnrad **Anmeldeeinstellungen** auf dem Begrüßungsbildschirm kannst du deine eigene Google- oder Microsoft-App-Registrierung hinzufügen oder ein IMAP-, POP3- oder JMAP-Konto verbinden.

Die [Installationsanleitung](../install.md) behandelt Installationsbefehle, Schlüsselbund und Desktop-Portal unter Linux, Hinweise zu Flatpak und RPM, das Prüfen von Downloads, die Android-Signatur, das Sideloading unter iOS und das Bauen aus dem Quellcode mit `cargo run --bin flectar-mail`.

## 🤝 Mitmachen

| Thema | Details |
| --- | --- |
| 🌍 **Übersetzen** | Übersetze die App, verbessere eine Übersetzung oder prüfe übersetzte Texte auf [Weblate](https://translate.flectar.com/). Programmierkenntnisse oder Erfahrung mit GitHub sind nicht erforderlich. Übersetzungen gelangen als Pull Requests in dieses Repository, und Mitwirkende erhalten auf GitHub eine Zuordnung ihrer Beiträge. README-Übersetzungen werden hier gepflegt; siehe [Übersetzungen beitragen](../../CONTRIBUTING.md#translations). |
| ♿ **Barrierefreiheit** | Tastaturnavigation, helle und dunkle Designs sowie flexible Layouts; die Arbeit an der Barrierefreiheit läuft weiter. Aktuelle Funktionen, Grenzen der Tests und Hinweise zum Melden von Barrieren stehen in [ACCESSIBILITY.md](../../ACCESSIBILITY.md). |
| 🔒 **Sicherheit** | Melde Schwachstellen vertraulich, wie in [SECURITY.md](../../SECURITY.md) beschrieben, und veröffentliche keine Details dazu in öffentlichen Issues. |
| 🌐 **Open Source** | Eine einzige Open-Source-Anwendung ohne separate Community-Ausgabe, lizenziert unter der [GNU Affero General Public License v3](../../LICENSE). Praktische Einzelheiten findest du in der [Lizenzübersicht](../../LICENSING.md). |
| 🛠️ **Mitwirken** | Unter [CONTRIBUTING.md](../../CONTRIBUTING.md) erfährst du, wie du das Projekt mitgestalten kannst. |

## 🫡 Danksagungen

Flectar Mail wird durch die Arbeit dieser Projekte und ihrer Mitwirkenden ermöglicht:

- [Slint](https://slint.dev/), das native UI-Toolkit hinter der Oberfläche von Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz), der Rust-Renderer für HTML/CSS des Dioxus-Teams, der die Darstellung von E-Mails ermöglicht.

---
