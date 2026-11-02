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
    <p>Von Grund auf für Geschwindigkeit entwickelt. Flectar Mail bietet native Leistung, sofortigen Start und einen RAM-Verbrauch ab nur 80 MB.</p>
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

- **Schnell vom ersten Klick an.** Eine native Oberfläche und der bevorzugte Zugriff auf lokale Daten bringen dich zum Posteingang, ohne auf eine Browserlaufzeit warten zu müssen.
- **Schon ab 80 MB RAM.** Flectar Mail ist gezielt auf einen niedrigen Speicherverbrauch ausgelegt und bietet trotzdem einen voll ausgestatteten Posteingang.
- **Alles an einem Ort.** Wechsle zwischen E-Mails, Kalendern und Kontakten, ohne verschiedene Anwendungen zusammenstellen zu müssen.
- **Für die Offline-Nutzung entwickelt.** Postfach und Kalender werden lokal gespeichert. Synchronisierte Daten bleiben so auch ohne Verbindung nutzbar.
- **Für deine Konten.** Verbinde Gmail, Outlook und Microsoft 365 oder Dienste auf Basis von IMAP, POP3, SMTP, JMAP, CalDAV und CardDAV.
- **Datenschutz als Voreinstellung.** Externe Bilder bleiben bis zu deiner Freigabe gesperrt. Das hilft zu verhindern, dass Tracking-Pixel melden, wann du eine Nachricht liest.
- **Sicherheit durch die Architektur.** E-Mail-Inhalte werden niemals in einer WebView geöffnet. Flectar Mail rendert HTML und CSS mit einer eigenen nativen Rust-Pipeline, führt keine Skripte aus E-Mails aus und blockiert externe Bilder standardmäßig. Die Angriffsfläche eines eingebetteten Browsers wird so konstruktiv vermieden.
- **Ein experimenteller Rust-Renderer.** E-Mails ohne Browser-Engine darzustellen ist Neuland. Während sich die Kompatibilität verbessert, sind Darstellungsfehler zu erwarten, besonders bei komplexen Nachrichten.
- **Für jeden Bildschirm.** Die großzügige und die minimalistische Desktop-Ansicht bieten dieselbe Erfahrung wie die kompakte, für Touch-Bedienung optimierte Oberfläche.
- **Nativ und quelloffen.** Von Grund auf mit Rust entwickelt. Kein Browser in einem Fenster, sondern eine unter AGPLv3 veröffentlichte Anwendung.

## 📁 Dateien und Anhänge

Durchsuche JMAP/WebDAV-Speicher und E-Mail-Anhänge, halte Dateien offline verfügbar und zeige Vorschauen von PDFs, Bildern und Text an.

## ✍🏻 Kontosignaturen und OpenPGP

Unter Einstellungen → Konten → Signaturen und OpenPGP findest du benannte Signaturen, getrennte Standardwerte für neue Nachrichten und Antworten sowie eine Signaturauswahl beim Verfassen. Signieren und Verschlüsseln mit OpenPGP/MIME auf dem Desktop verwenden ein installiertes GnuPG 2.x mit pinentry für die Passphrasen privater Schlüssel. Ist Verschlüsselung vorgeschrieben, wird der Versand bei fehlenden oder ungültigen Empfängerschlüsseln blockiert; geschützte Entwürfe bleiben bis zum Senden lokal.

S/MIME und OpenPGP auf Mobilgeräten werden derzeit nicht unterstützt.

## 🧪 Experimentelle HTML-Darstellung

> [!WARNING]
> Die Darstellung von HTML-E-Mails ist derzeit der experimentellste Teil von Flectar Mail. Manche Nachrichten, insbesondere mit komplexem oder ungewöhnlichem Markup und CSS, werden möglicherweise noch nicht korrekt angezeigt.

Damit der Client vollständig nativ bleibt und der Speicherverbrauch bei etwa 80 MB liegt, rendert Flectar Mail E-Mail-HTML mit [Blitz](https://github.com/DioxusLabs/blitz), einem Rust-Renderer für HTML/CSS des Dioxus-Teams, statt einen Browser oder eine WebView einzubetten.

Desktop-Versionen bieten unter Einstellungen → Allgemein → Renderer die Optionen **CPU — Low Memory** und **GPU — WGPU**. Anfangs ist CPU ausgewählt; WGPU wird dabei nicht initialisiert. Die GPU-Option nutzt Slint und Vello auf einem gemeinsamen WGPU-30-Gerät. Eine Änderung wird nach einem Neustart der Anwendung wirksam. Schlägt der GPU-Start fehl, wird automatisch auf CPU zurückgegriffen.

Soweit uns bekannt, ist Flectar Mail eines der ersten Projekte, das Blitz für beliebiges HTML aus echten E-Mails verwendet. E-Mail-Markup enthält viel ungewöhnliches HTML und CSS und stellt den Renderer daher vor anspruchsvolle Aufgaben. Derzeit pflegen wir mehrere Patches für Blitz und hoffen, möglichst viele davon nach und nach in das Ursprungsprojekt einzubringen.

Falls eine E-Mail falsch dargestellt wird, [melde das Problem](https://github.com/flectar/mail/issues). Dieser Ansatz ist noch experimentell, aber zugleich ein wesentlicher Grund dafür, dass Flectar Mail im Vergleich zu WebView-Clients auf Basis von Frameworks wie Tauri oder Wails so schlank bleiben kann.

## 🎨 Nach deinem Geschmack

Wähle die Arbeitsumgebung, die zu deinem Umgang mit E-Mails passt. Behalte die ausführliche Ansicht mit drei Bereichen bei oder wechsle zur minimalistischen Ansicht, wähle ein helles oder dunkles Design, nutze eine vorhandene Farbpalette oder erstelle eine eigene und blende Absenderbilder ein oder aus. Außerdem kannst du dem App-Symbol eine andere Farbe geben: in der Taskleiste oder im Dock, im Infobereich, im Linux-Anwendungsstarter sowie auf dem Startbildschirm von Android und iOS.

Das Erscheinungsbild von Nachrichten kann dem App-Design folgen oder unabhängig davon hell oder dunkel bleiben. Die gewählte Farbpalette gilt für HTML, Klartext und den Lesemodus. Um vorübergehend die Originalfarben des Absenders zu sehen, nutze auf dem Desktop das Kontrastsymbol zwischen Suche und Lesemodus in der Nachrichtenleiste oder wähle **Originalfarben anzeigen** im Dreipunktmenü der Nachricht auf jeder Bildschirmgröße.

Die vollständige Ansicht zeigt Ordner, Nachrichtenliste und ausgewählte E-Mail gleichzeitig. Das minimalistische Layout reduziert visuelle Ablenkungen und gibt jedem Bereich deines Posteingangs bei Bedarf mehr Platz.

### 👥 Profile und Kontofarben

Fasse zusammengehörige Konten in benannten Profilen wie Arbeit oder Privat zusammen und gib jedem Profil eine eigene Farbe. Erstelle ein Profil im Profilmenü am Anfang der Tableiste oder unter Einstellungen → Konten → Profile und markiere die zugehörigen Konten. Wähle ein Profil, um die E-Mail-Ansicht auf dessen Konten zu beschränken: gemeinsamer Posteingang, Suche, Zähler und Seitenleiste zeigen nur deren Nachrichten, während deine Tabs unverändert bleiben. **Alle Konten** zeigt wieder alles an.

Du kannst auch die Farbe eines einzelnen Kontos überschreiben und die wirksame Farbe am linken Rand jeder Nachricht anzeigen. Diese Markierungen erleichtern die Unterscheidung von Konten im gemeinsamen Posteingang; im Beispiel unten steht Violett für Arbeit und Orange für Support.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
  <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Gemeinsamer Posteingang von Flectar Mail mit violetten und orangefarbenen Profilmarkierungen">
</picture>

### 🧵 Unterhaltungen

Antworten bleiben chronologisch gruppiert, wobei die aktive Nachricht im Lesebereich aufgeklappt ist.

Mit **Pfeil hoch/runter** navigierst du durch Nachrichten, mit **Pos1/Ende** zur ersten oder letzten geladenen Nachricht und mit **Bild auf/Bild ab** seitenweise. **Enter** öffnet die aktuelle Nachricht und fokussiert ihren Inhalt; die **Leertaste** markiert sie für Sammelaktionen oder hebt die Markierung auf. **Tab/Umschalt+Tab** wechseln zwischen Bedienelementen. In der vollständigen Ansicht verschiebt **F6** den Fokus zwischen Liste und Lesebereich. Im minimalistischen Layout heben Pfeiltasten Nachrichten hervor und **Enter** öffnet sie; **Escape** oder **F6** kehrt zur Liste zurück.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
  <img src="../../resources/screenshots/desktop-thread-light.png" alt="Flectar-Mail-Unterhaltung mit eingegangenen Nachrichten und Antworten">
</picture>

### 🗂️ Tabs

Halte mehrere Bereiche gleichzeitig in Tabs offen, wie in einem Browser. Jeder Tab merkt sich seinen Ort: ein Postfach, eine Suche, eine geöffnete Unterhaltung, Kalender, Kontakte, Dateien oder Einstellungen, einschließlich Scrollposition und zuletzt gelesener Nachricht. Ein Mittelklick oder Ctrl/⌘-Klick auf eine Nachricht öffnet sie im Hintergrund (mit Umschalt wechselst du direkt dorthin); alternativ wähle **In neuem Tab öffnen** im Menü. Ziehe Tabs zum Umsortieren, schließe sie per Mittelklick und nutze den Rechtsklick, um andere Tabs oder alle rechts davon zu schließen. Beim nächsten Start von Flectar Mail machst du dort weiter, wo du aufgehört hast.

**Ctrl/⌘+T** öffnet einen Tab, **Ctrl/⌘+W** schließt den aktuellen und **Ctrl/⌘+Umschalt+T** öffnet den zuletzt geschlossenen erneut. **Ctrl+Tab** und **Ctrl+Umschalt+Tab** wechseln zwischen Tabs, **Ctrl/⌘+1–8** springen zu einem bestimmten Tab und **Ctrl/⌘+9** zum letzten.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
  <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail mit Tabs für mehrere Unterhaltungen, Kalender, Kontakte und Dateien">
</picture>

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

### 🪟 Rahmenloses Fenster

Aktiviere **Rahmenloses Fenster** unter Einstellungen → Allgemein → Oberfläche, um die Systemtitelleiste durch Fenstersteuerelemente in der Werkzeugleiste zu ersetzen. Jede Plattform behält ihr Verhalten: macOS die nativen farbigen Fensterknöpfe, Windows Snap Layouts und das Systemmenü, Linux die in GNOME oder KDE konfigurierte Reihenfolge der Schaltflächen und Titelleistenaktionen.

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

### 📅 Kalender, Kontakte und Dateien

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

### 💾 Speicher und Sicherungen

Einstellungen → Speicher zeigt, wie viel Platz E-Mails, Anhänge, Offline-Dateien und Datenbanken auf dem Gerät belegen. Dort kannst du geprüfte Datenbank-Snapshots exportieren oder die Konfiguration verbundener Konten und deine Einstellungen über eine Sicherung übertragen. Passwörter und OAuth-Tokens bleiben im Schlüsselbund des Systems. Auf einem wiederhergestellten Gerät musst du dich deshalb erneut anmelden.

POP3-Konten laden E-Mails auf dieses Gerät und lassen sie auf dem Server, sofern du nichts anderes auswählst: Jedes Konto kann Nachrichten nach 7, 14 oder 30 Tagen, beim Löschen hier oder direkt nach dem Herunterladen vom Server entfernen, aber niemals vor dem Speichern auf diesem Gerät. Die Ordner existieren nur auf diesem Gerät, daher enthalten Snapshots auch diese Nachrichten. Beim Entfernen eines POP3-Kontos oder Löschen aller Daten wird zunächst der Export in mbox-Dateien angeboten, eine je Ordner, die andere E-Mail-Programme importieren können.

Die gleiche Importfunktion übernimmt Konten aus Thunderbird (Profilordner, Archiv aus Extras › Exportieren oder `prefs.js`) sowie aus Thunderbird für Android oder K-9 Mail (`.k9s`-Einstellungsexporte). Du prüfst jedes Konto, bevor etwas hinzugefügt wird: Einstellungen, die Flectar Mail nicht abbilden kann, etwa einen Exchange-Server, werden erklärt statt stillschweigend geändert. POP3-Konten behalten ihre Regel zum Löschen von Nachrichten auf dem Server. Bestehende Konten werden nie verändert, Passwörter nie importiert.

Gespeicherte E-Mails lassen sich genauso übernehmen: die Lokalen Ordner von Thunderbird und die Nachrichten, die seine POP3-Konten heruntergeladen haben, mbox-Dateien wie ein Google-Takeout-Export oder die von Flectar Mail selbst, `.eml`-Nachrichten, Maildir-Ordner, Postfächer von Apple Mail sowie ZIP-Archive davon. Du wählst die Dateien aus, auch mehrere auf einmal, und dann die Ordner – Papierkorb und Junk bleiben außen vor, sofern du sie nicht ankreuzt –, und Ordnerstruktur sowie Gelesen- und Markiert-Status werden übernommen. Bereits vorhandene Nachrichten werden übersprungen, sodass ein abgebrochener Import einfach erneut laufen kann, und ein langer Import kann im Hintergrund weiterlaufen. E-Mails ohne Konto landen unter **Auf diesem Gerät**: Ordner, die nur hier existieren und sich unter Einstellungen → Speicher exportieren oder löschen lassen. Sie können auch in die Ordner eines POP3-Kontos gehen oder auf den Server eines IMAP-, JMAP- oder Gmail-Kontos, dort in einen eigenen Ordner.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-storage-light.png">
  <img src="../../resources/screenshots/desktop-storage-light.png" alt="Speichereinstellungen von Flectar Mail mit lokaler Belegung, E-Mails auf diesem Gerät, Datenbank-Snapshots und Kontosicherungen">
</picture>

### 📱 Für kleinere Bildschirme

Die kompakte Oberfläche hält wichtige Aktionen griffbereit und gibt Nachrichten, Terminen und Kontakten bei Bedarf den ganzen Bildschirm.

| Hell | Dunkel |
| --- | --- |
| ![Mobiler Posteingang von Flectar Mail im hellen Design](../../resources/screenshots/mobile-light.png) | ![Mobiler Posteingang von Flectar Mail im dunklen Design](../../resources/screenshots/mobile-dark.png) |

## 📥 Flectar Mail herunterladen

Flectar Mail wird derzeit entwickelt und ist noch nicht stabil.

> [!WARNING]
> **Die OAuth-Verifizierung von Google erfordert eine jährliche CASA-Sicherheitsprüfung.** Flectar ist ein gemeinschaftliches Open-Source-Projekt, daher hängt diese Prüfung von Sponsoren ab. Solange die erforderliche Verifizierung nicht finanziert und abgeschlossen ist, kann es keine stabile Version mit standardmäßig eingerichtetem Gmail OAuth geben. Wenn du die integrierte Gmail-Anmeldung ermöglichen möchtest, [unterstütze Flectar](https://github.com/sponsors/flectar).
>
> Die OAuth-Verifizierung bei Google und Microsoft läuft noch. Frühere Vorabversionen auf GitHub richten sich an Tester mit eigenen OAuth-Registrierungen oder IMAP-, POP3- beziehungsweise JMAP-Konten.

In Builds ohne OAuth-App-Schlüssel bleiben Gmail- und Outlook-Anmeldung deaktiviert. Über das Zahnrad **Anmeldeeinstellungen** auf dem Begrüßungsbildschirm kannst du deine eigene Google- oder Microsoft-App-Registrierung speichern oder ein IMAP-, POP3- oder JMAP-Konto verbinden. Eigene Schlüssel werden mit den Einstellungen geteilt und haben Vorrang vor mitgelieferten Schlüsseln. Das Löschen einer eigenen Client-ID stellt vorhandene Standardwerte wieder her. Jeder Anbieter wird nach dem Speichern seiner Konfiguration einzeln aktiviert.

Sobald Vorschauversionen veröffentlicht werden, lade sie von [GitHub Releases](https://github.com/flectar/mail/releases) herunter und achte auf die Kennzeichnung **Pre-release**:

- **Linux x64:** AppImage, Debian/Ubuntu `.deb`, Fedora 43/44 `.rpm` oder ein Flatpak-Vorschaupaket zur manuellen Installation
- **Windows x64:** `.exe`-Installationsprogramm oder portables ZIP
- **macOS Apple silicon oder Intel (macOS 14+):** DMG oder Anwendungs-ZIP
- **Android arm64 (Android 8.0+):** Signiertes APK; Vorschauversionen und stabile Veröffentlichungen teilen dieselbe App-Identität

Windows-Vorschauversionen sind unsigniert; macOS-Vorschauversionen sind ad hoc signiert und nicht notarisiert. Sicherheitshinweise des Betriebssystems sind daher zu erwarten. Installiere Updates manuell.

Die Flatpak-Vorschau wird als eigenständiges Testpaket bereitgestellt. Sie ist noch kein Flathub-Paket und erhält deshalb keine automatischen Flathub-Updates. Das Schließen in den Infobereich ist deaktiviert, bis dessen Backend einen mit der Sandbox kompatiblen D-Bus-Namen verwenden kann. Der automatische Start bei der Anmeldung erfolgt über das Background-Portal des Desktops und öffnet das Fenster, da ein Start im Infobereich nicht möglich ist. Das App-Symbol bleibt dort orange, weil die Sandbox das Starter-Symbol auf dem Host nicht ändern kann.

Installiere das heruntergeladene Linux-Paket mit `sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` oder `flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak`. Starte die Flatpak-Version mit `flatpak run com.flectar.mail`.

Das RPM richtet sich an Fedora 43 und 44 auf x86_64. Veröffentlichungs-Builds nutzen Fedora 43 als Kompatibilitätsbasis. Dasselbe RPM muss vor der Veröffentlichung Installation, PDF-Darstellung, Start unter X11 und Wayland sowie Deinstallation in sauberen Containern beider Versionen bestehen. Andere RPM-Distributionen sind noch nicht getestet. Meldet Discover einen allgemeinen Konflikt, führe den obigen Befehl `dnf install` aus und füge den Abhängigkeitsfehler deinem Fehlerbericht bei.

Das ältere RPM `0.1.0-alpha.7` wurde auf Fedora 44 gebaut und lässt sich wegen seiner Anforderung an glibc 2.43 nicht auf Fedora 43 installieren. Das zugehörige Flatpak-Paket ist eine Alternative, bis ein RPM mit den genannten Kompatibilitätsprüfungen veröffentlicht wird.

Jede GitHub-Veröffentlichung enthält `SHA256SUMS` und signierte Nachweise zur Build-Herkunft. Mit installiertem GitHub CLI prüfst du einen Download über `gh attestation verify <download> --repo flectar/mail`.

Ab alpha 8 verwenden Android-Alpha-, Beta-, Release-Candidate- und stabile Builds ein Paket (`com.flectar.mail`) und eine dauerhafte Signaturidentität. Spätere Versionen können die Anwendung aktualisieren und lokale Daten erhalten. Alpha 6 und alpha 7 nutzten andere Schlüssel und können nicht auf diese Signaturidentität aktualisiert werden.

Beide Android-Veröffentlichungsabläufe verwenden die GitHub-Umgebung `android-release`. Sie benötigen dieselben Keystore- und Passwort-Secrets, prüfen das öffentliche Zertifikat gegen `platform/android/app-signing-certificate.sha256` und leiten Android-Versionscodes aus der veröffentlichten Version ab. Ein fehlender oder abweichender Schlüssel blockiert die Veröffentlichung. **Erzeuge für eine normale Veröffentlichung keinen Ersatzschlüssel und ändere nicht den festgelegten Fingerabdruck.** Passwortänderungen erhalten die Identität, solange ursprünglicher privater Schlüssel und Zertifikat erhalten bleiben.

Den einmaligen Migrationsbefehl, benötigte Secrets, Sicherungen und Play App Signing beschreibt die [Einrichtung und Wiederherstellung der Android-Signatur](../android-signing.md).

Lokale Entwickler-Builds können weiterhin einen lokalen Testschlüssel erzeugen. Sie aktualisieren nur Installationen, die mit demselben lokalen Schlüssel signiert sind.

Du kannst Flectar Mail auch mit der [Rust-Toolchain](https://rustup.rs/) aus dem Quellcode bauen:

```bash
cargo run --bin flectar-mail
```

OAuth auf dem Linux-Desktop verwendet den Systembrowser über das Desktop-Portal und speichert Zugangsdaten zur Token-Erneuerung über freedesktop Secret Service. Eine normale Sitzung benötigt daher ein `xdg-desktop-portal`-Backend und einen Secret-Service-Anbieter wie GNOME Keyring. Ist kein sicherer Zugangsdaten-Speicher verfügbar, stoppen Veröffentlichungs-Builds vor dem Öffnen der OAuth-Seite, damit keine Autorisierung ohne sicheren Speicherort abgeschlossen wird.

Podman- und Docker-Entwicklungsumgebungen haben häufig weder Zugriff auf den D-Bus der Host-Sitzung noch auf das Loopback-Netz des Host-Browsers. Debug-Builds unterstützen dies durch eine klar gekennzeichnete, nur für den Eigentümer zugängliche Entwicklungs-Zugangsdaten-Datei und einen aufklappbaren Bereich zum Einfügen der endgültigen Loopback-Rückruf-URL. Der dateibasierte Speicher ist aus Veröffentlichungs-Builds ausgeschlossen.

## 🌍 Hilf bei der Übersetzung von Flectar Mail

Flectar Mail ist für alle gedacht. Wir freuen uns über deine Hilfe, es in weiteren Sprachen verfügbar zu machen!

Wir nutzen [Weblate](https://translate.flectar.com/) für die Übersetzungen der Community. Ob du Flectar Mail in deine Muttersprache übersetzen, eine bestehende Übersetzung verbessern oder übersetzte Texte prüfen möchtest: Jeder Beitrag ist willkommen.

**[Flectar Mail übersetzen →](https://translate.flectar.com/)**

Der Einstieg ist einfach:

1. Erstelle ein Konto auf unserer Übersetzungsplattform.
2. Wähle deine Sprache oder beginne eine neue Übersetzung, falls sie noch fehlt.
3. Übersetze direkt in Weblate. Programmierkenntnisse oder Erfahrung mit GitHub sind nicht erforderlich.

Übersetzungen werden mit unserem GitHub-Repository synchronisiert und als Pull Requests zur Prüfung eingereicht. Mitwirkende können auf GitHub auch eine Zuordnung ihrer Beiträge erhalten.

README-Übersetzungen werden in diesem Repository gepflegt. Unter [Übersetzungen beitragen](../../CONTRIBUTING.md#translations) erfährst du, wie du eine hinzufügst oder aktualisierst.

## ♿ Barrierefreiheit

Flectar Mail bietet Tastaturnavigation, helle und dunkle Designs sowie flexible Layouts. Die Arbeit an der Barrierefreiheit läuft weiter. Aktuelle Funktionen, Grenzen der Tests und Hinweise zum Melden von Barrieren stehen in [ACCESSIBILITY.md](../../ACCESSIBILITY.md).

## 🔒 Sicherheit

Um eine Schwachstelle vertraulich zu melden, folge [SECURITY.md](../../SECURITY.md). Veröffentliche keine Details zu Schwachstellen in öffentlichen Issues.

## 🌐 Open Source für alle

Flectar Mail ist eine einzige Open-Source-Anwendung. Es gibt keine separate Community-Ausgabe. Der Client steht unter der [GNU Affero General Public License v3](../../LICENSE). Praktische Einzelheiten findest du in der [Lizenzübersicht](../../LICENSING.md); unter [CONTRIBUTING.md](../../CONTRIBUTING.md) erfährst du, wie du das Projekt mitgestalten kannst.

## 🫡 Danksagungen

Flectar Mail wird durch die Arbeit dieser Projekte und ihrer Mitwirkenden ermöglicht:

- [Slint](https://slint.dev/), das native UI-Toolkit hinter der Oberfläche von Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz), der Rust-Renderer für HTML/CSS des Dioxus-Teams, der die Darstellung von E-Mails ermöglicht.

---
