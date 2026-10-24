<div align="center">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="Logo de Flectar Mail">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <a href="../../README.md">English</a> · <a href="README.es.md">Español</a> · <a href="README.de.md">Deutsch</a> ·
    <strong>Français</strong> · <a href="README.pt_BR.md">Português (Brasil)</a> · <a href="README.ru.md">Русский</a> ·
    <a href="README.hi.md">हिन्दी</a> · <a href="README.ar.md">العربية</a> · <a href="README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>L’e-mail retrouve sa rapidité</h3>
    <p>Conçu de zéro pour la vitesse. Flectar Mail offre des performances natives, un démarrage instantané et une consommation de RAM à partir de seulement 80 Mo.</p>
  </div>
  <p>
    <a href="https://flectar.com">Site web</a> ·
    <a href="https://github.com/flectar/mail/issues">Signaler un problème</a> ·
    <a href="../../CONTRIBUTING.md">Contribuer</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="Licence : AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="Téléchargements de toutes les versions"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="Dernière version"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="État de la traduction"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="Langage : Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="Plateformes : Windows, macOS, Linux, Android, iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail sur macOS avec la boîte de réception unifiée et la vue du message">
</picture>

Flectar Mail est une application native et légère pour vos e-mails, calendriers et contacts. Elle est conçue pour s’ouvrir instantanément, rester réactive et n’utiliser qu’une fraction de la mémoire d’un client de messagerie classique basé sur des technologies web.

## ❓ Pourquoi Flectar Mail ?

- **Rapide dès le premier clic.** Une interface native et un accès aux données privilégiant le stockage local vous conduisent à votre boîte de réception sans attendre le démarrage d’un environnement de navigateur.
- **À partir de seulement 80 Mo de RAM.** Flectar Mail est délibérément conçu pour limiter la consommation de mémoire, tout en proposant une boîte de réception riche en fonctionnalités.
- **Tout au même endroit.** Passez de la messagerie aux calendriers et aux contacts sans devoir assembler plusieurs applications.
- **Pensé pour fonctionner hors ligne.** Votre messagerie et votre calendrier sont stockés localement : vos données synchronisées restent donc utiles sans connexion.
- **Compatible avec vos comptes.** Connectez Gmail, Outlook et Microsoft 365, ou des services reposant sur IMAP, POP3, SMTP, JMAP, CalDAV et CardDAV.
- **La confidentialité par défaut.** Les images distantes sont bloquées jusqu’à votre autorisation, ce qui aide à empêcher les pixels de suivi de signaler quand vous lisez un message.
- **La sécurité dès l’architecture.** Le contenu des e-mails n’est jamais ouvert dans une WebView. Flectar Mail affiche le HTML et le CSS au moyen de son propre système natif en Rust, n’exécute pas les scripts des messages et bloque les images distantes par défaut. Cela évite par conception la surface d’attaque d’un navigateur intégré.
- **Un moteur de rendu expérimental en Rust.** Construire un moteur d’affichage des e-mails sans navigateur est un terrain nouveau. Des problèmes de rendu sont à prévoir, notamment pour les messages complexes, à mesure que la compatibilité s’améliore.
- **Pour tous les écrans.** Les interfaces de bureau, complète et minimaliste, partagent la même expérience que l’interface compacte adaptée au tactile.
- **Natif et libre.** Créé de zéro en Rust. Il ne s’agit pas d’un navigateur dans une fenêtre ; l’application est distribuée sous AGPLv3.

## 📁 Fichiers et pièces jointes

Parcourez le stockage JMAP/WebDAV, recherchez des pièces jointes, gardez des fichiers hors ligne et prévisualisez des PDF, des images et du texte.

## ✍🏻 Signatures de compte et OpenPGP

Paramètres → Comptes → Signatures et OpenPGP permet de nommer vos signatures, de choisir des valeurs par défaut distinctes pour les nouveaux messages et les réponses, et de sélectionner une signature lors de la rédaction. La signature et le chiffrement OpenPGP/MIME sur ordinateur utilisent GnuPG 2.x installé, avec pinentry pour les phrases secrètes des clés privées. Le chiffrement obligatoire bloque l’envoi si les clés des destinataires sont absentes ou invalides ; les brouillons protégés restent locaux jusqu’à l’envoi.

S/MIME et OpenPGP sur mobile ne sont pas encore pris en charge.

## 🧪 Rendu HTML expérimental

> [!WARNING]
> Le rendu des e-mails HTML est actuellement la partie la plus expérimentale de Flectar Mail. Certains messages, notamment ceux dont le balisage et le CSS sont complexes ou inhabituels, peuvent encore s’afficher incorrectement.

Pour conserver un client entièrement natif et une consommation de mémoire d’environ 80 Mo, Flectar Mail affiche le HTML des e-mails avec [Blitz](https://github.com/DioxusLabs/blitz), un moteur HTML/CSS en Rust de l’équipe Dioxus, plutôt qu’en intégrant un navigateur ou une WebView.

Les versions de bureau proposent **CPU — Low Memory** et **GPU — WGPU** dans Paramètres → Général → Moteur de rendu. Le CPU est sélectionné au départ et n’initialise pas WGPU. L’option GPU utilise Slint et Vello sur un périphérique WGPU 30 partagé ; le changement s’applique après le redémarrage de l’application. Si le démarrage avec le GPU échoue, l’application revient automatiquement au CPU.

À notre connaissance, Flectar Mail est l’un des premiers projets à utiliser Blitz pour du HTML provenant d’e-mails réels de toute origine. Le balisage des messages comporte beaucoup de HTML et de CSS inhabituels, ce qui met le moteur à l’épreuve. Nous maintenons actuellement plusieurs correctifs pour Blitz et espérons en intégrer autant que possible au projet d’origine au fil du temps.

Si un e-mail s’affiche incorrectement, [signalez-le](https://github.com/flectar/mail/issues). Cette approche reste expérimentale, mais c’est aussi l’une des principales raisons pour lesquelles Flectar Mail peut être si léger par rapport aux clients basés sur une WebView et créés avec des outils comme Tauri ou Wails.

## 🎨 À votre image

Choisissez l’espace de travail adapté à votre façon de gérer vos e-mails. Conservez la disposition détaillée à trois volets, passez à une vue minimaliste, choisissez un thème clair ou sombre, sélectionnez une palette intégrée ou créez la vôtre, et affichez ou masquez les avatars des expéditeurs. Vous pouvez aussi changer la couleur de l’icône de l’application : dans la barre des tâches ou le Dock, dans la zone de notification, dans le lanceur d’applications Linux et sur l’écran d’accueil d’Android et d’iOS.

L’apparence des messages peut suivre le thème de l’application ou rester claire ou sombre indépendamment. La palette choisie s’applique au HTML, au texte brut et au mode lecture. Pour afficher temporairement les couleurs d’origine de l’expéditeur, utilisez l’icône de contraste entre la recherche et le mode lecture dans la barre d’outils du message sur ordinateur, ou choisissez **Afficher les couleurs d’origine** dans le menu à trois points du message, quelle que soit la taille de l’écran.

L’espace complet affiche simultanément les dossiers, la liste des messages et l’e-mail sélectionné. La disposition minimaliste réduit les distractions visuelles et donne davantage de place à chaque partie de la boîte de réception quand vous en avez besoin.

### 👥 Profils et couleurs des comptes

Regroupez les comptes liés dans des profils nommés, comme Travail ou Personnel, et attribuez une couleur à chaque profil. Créez un profil depuis le menu au début de la barre d’onglets ou dans Paramètres → Comptes → Profils, puis cochez les comptes qui en font partie. Choisir un profil limite la vue de messagerie à ses comptes : la boîte de réception unifiée, la recherche, les compteurs et la barre latérale n’affichent que leurs messages, tandis que vos onglets restent inchangés. **Tous les comptes** réaffiche l’ensemble.

Vous pouvez aussi remplacer la couleur d’un compte particulier et afficher la couleur effective sur le bord gauche de chaque message. Ces repères facilitent la distinction des comptes dans la boîte unifiée ; l’exemple ci-dessous utilise le violet pour Travail et l’orange pour Assistance.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
  <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Boîte de réception unifiée de Flectar Mail avec des repères de profil violets et orange">
</picture>

### 🧵 Fils de discussion

Les réponses restent regroupées par ordre chronologique, avec le message actif déplié dans le volet de lecture.

Utilisez **Haut/Bas** pour parcourir les messages, **Début/Fin** pour atteindre le premier ou le dernier message chargé, et **Page précédente/Page suivante** pour avancer d’une page. **Entrée** ouvre le message courant et place le focus sur son contenu ; **Espace** le coche ou le décoche pour les actions groupées. **Tab/Maj+Tab** passent d’un contrôle à l’autre. Dans l’espace complet, **F6** déplace le focus entre la liste et le lecteur. Dans la disposition minimaliste, les flèches mettent les messages en surbrillance et **Entrée** les ouvre ; **Échap** ou **F6** revient à la liste.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
  <img src="../../resources/screenshots/desktop-thread-light.png" alt="Conversation Flectar Mail avec les messages reçus et les réponses">
</picture>

### 🗂️ Onglets

Gardez plusieurs espaces ouverts avec des onglets comme dans un navigateur. Chaque onglet mémorise son emplacement : une boîte, une recherche ou une conversation ouverte, le calendrier, les contacts, les fichiers ou les paramètres, ainsi que la position de défilement et le message en cours de lecture. Un clic du milieu ou Ctrl/⌘+clic ouvre un message dans un onglet en arrière-plan (ajoutez Maj pour y accéder), ou choisissez **Ouvrir dans un nouvel onglet** dans son menu. Faites glisser les onglets pour les réorganiser, fermez-les avec le bouton du milieu et utilisez le clic droit pour fermer les autres ou ceux à droite. Au prochain lancement de Flectar Mail, reprenez là où vous en étiez.

**Ctrl/⌘+T** ouvre un onglet, **Ctrl/⌘+W** ferme l’onglet courant et **Ctrl/⌘+Maj+T** rouvre le dernier onglet fermé. **Ctrl+Tab** et **Ctrl+Maj+Tab** parcourent les onglets, **Ctrl/⌘+1–8** accèdent à un onglet précis et **Ctrl/⌘+9** au dernier.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
  <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail avec des onglets pour plusieurs conversations, le calendrier, les contacts et les fichiers">
</picture>

### Espaces de travail complet et minimaliste

<table>
  <thead>
    <tr>
      <th width="50%">Espace complet</th>
      <th width="50%">Espace minimaliste</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Espace de travail complet de Flectar Mail sur ordinateur" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="Espace de travail minimaliste de Flectar Mail sur ordinateur" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Palettes de couleurs

<table>
  <thead>
    <tr>
      <th width="25%">Bleu-vert</th>
      <th width="25%">Vert</th>
      <th width="25%">Violet</th>
      <th width="25%">Personnalisée</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="Palette bleu-vert de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="Palette verte de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="Palette violette de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Palette personnalisée de Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 🪟 Fenêtre sans bordure

Activez **Fenêtre sans bordure** dans Paramètres → Général → Interface pour remplacer la barre de titre du système par des contrôles dans la barre d’outils. Chaque plateforme conserve son comportement : macOS garde ses boutons colorés natifs, Windows conserve Snap Layouts et le menu système, et Linux suit l’ordre des boutons et les actions de la barre de titre configurés dans GNOME ou KDE.

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
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="Fenêtre sans bordure de Flectar Mail sur macOS" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="Fenêtre sans bordure de Flectar Mail sur Windows" width="100%">
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
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="Fenêtre sans bordure de Flectar Mail sur GNOME" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="Fenêtre sans bordure de Flectar Mail sur KDE Plasma" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 📅 Calendrier, contacts et fichiers

<table>
  <thead>
    <tr>
      <th width="33.33%">Calendrier</th>
      <th width="33.33%">Contacts</th>
      <th width="33.33%">Fichiers (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="Calendrier de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="Contacts de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="Fichiers de Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 💾 Stockage et sauvegardes

Paramètres → Stockage indique l’espace utilisé sur l’appareil par les e-mails, pièces jointes, fichiers hors ligne et bases de données. Depuis cette page, vous pouvez exporter des instantanés vérifiés des bases ou transférer les réglages des comptes connectés et vos préférences au moyen d’une sauvegarde. Les mots de passe et jetons OAuth restent dans le trousseau du système : un appareil restauré vous demande donc de vous reconnecter.

Les comptes POP3 téléchargent les e-mails sur cet appareil et les laissent sur le serveur sauf choix contraire : chaque compte peut supprimer les messages du serveur après 7, 14 ou 30 jours, lorsque vous les supprimez ici ou dès leur téléchargement, mais jamais avant leur stockage sur l’appareil. Leurs dossiers n’existent que sur cet appareil, donc les instantanés incluent ces messages. Lors de la suppression d’un compte POP3 ou de toutes les données, l’application propose d’abord de les exporter en fichiers mbox, un par dossier, importables dans d’autres logiciels de messagerie.

La même fonction importe les comptes de Thunderbird (dossier de profil, archive Outils › Exporter ou `prefs.js`), de Thunderbird pour Android ou de K-9 Mail (exports de paramètres `.k9s`). Vous vérifiez chaque compte avant tout ajout : les réglages que Flectar Mail ne peut pas reproduire, comme un serveur Exchange, sont expliqués plutôt que modifiés sans avertissement, et les comptes POP3 conservent leur règle de suppression des messages du serveur. Les comptes existants ne sont jamais modifiés et les mots de passe ne sont jamais importés.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-storage-light.png">
  <img src="../../resources/screenshots/desktop-storage-light.png" alt="Paramètres de stockage de Flectar Mail avec l’utilisation locale, les instantanés de bases de données et les sauvegardes des comptes">
</picture>

### 📱 Pour les petits écrans

L’interface compacte garde les actions importantes à portée de main tout en laissant tout l’écran aux messages, événements et contacts quand ils en ont besoin.

| Clair | Sombre |
| --- | --- |
| ![Boîte de réception mobile de Flectar Mail en mode clair](../../resources/screenshots/mobile-light.png) | ![Boîte de réception mobile de Flectar Mail en mode sombre](../../resources/screenshots/mobile-dark.png) |

## 📥 Obtenir Flectar Mail

Flectar Mail est en cours de développement et n’est pas encore stable.

> [!WARNING]
> **La vérification OAuth de Google exige une évaluation annuelle de sécurité CASA.** Flectar est un projet libre communautaire ; cette évaluation dépend donc du soutien des sponsors. Tant que la vérification requise n’est pas financée et terminée, une version stable avec OAuth Gmail configuré par défaut ne sera pas possible. Pour aider à proposer la connexion Gmail intégrée, [sponsorisez Flectar](https://github.com/sponsors/flectar).
>
> La vérification OAuth de Google et Microsoft est toujours en cours. Les préversions antérieures sur GitHub s’adressent aux personnes qui testent l’application avec leurs propres inscriptions OAuth ou des comptes IMAP, POP3 ou JMAP.

Dans les compilations sans clés d’application OAuth, la connexion Gmail et Outlook reste désactivée. Utilisez la roue dentée **Paramètres de connexion** de l’écran d’accueil pour enregistrer votre propre inscription d’application Google ou Microsoft, ou connectez un compte IMAP, POP3 ou JMAP. Les clés personnalisées sont partagées avec les paramètres et prennent le pas sur les clés intégrées ; effacer un ID client personnalisé rétablit les valeurs par défaut lorsqu’elles existent. Chaque fournisseur s’active séparément après l’enregistrement de sa configuration.

Lorsque des versions de test sont publiées, téléchargez-les depuis [GitHub Releases](https://github.com/flectar/mail/releases) et repérez la mention **Pre-release** :

- **Linux x64 :** AppImage, `.deb` pour Debian/Ubuntu, `.rpm` pour Fedora 43/44 ou paquet Flatpak de test à installer manuellement
- **Windows x64 :** Installateur `.exe` ou ZIP portable
- **macOS Apple silicon ou Intel (macOS 14+) :** DMG ou ZIP de l’application
- **Android arm64 (Android 8.0+) :** APK signé ; préversions et versions stables partagent une même identité d’application

Les préversions Windows ne sont pas signées ; celles de macOS portent une signature ad hoc et ne sont pas notariées. Les avertissements de sécurité du système sont donc attendus. Installez les mises à jour manuellement.

La préversion Flatpak est fournie comme paquet de test indépendant. Elle n’est pas encore disponible sur Flathub et ne reçoit donc pas ses mises à jour automatiques. L’option de fermeture dans la zone de notification est désactivée tant que son composant ne peut pas utiliser un nom D-Bus compatible avec le bac à sable. Le démarrage automatique à l’ouverture de session passe par le portail Background du bureau et ouvre la fenêtre, puisqu’il n’y a pas de zone de notification où démarrer. L’icône de l’application y reste orange, car le bac à sable ne peut pas modifier l’icône du lanceur sur le système hôte.

Installez le paquet Linux téléchargé avec `sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` ou `flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak`. Lancez la version Flatpak avec `flatpak run com.flectar.mail`.

Le RPM cible Fedora 43 et 44 sur x86_64. Les compilations de publication prennent Fedora 43 comme base de compatibilité ; le même RPM doit passer les contrôles d’installation, de rendu PDF, de démarrage sous X11 et Wayland et de désinstallation dans des conteneurs propres des deux versions avant publication. Les autres distributions basées sur RPM ne sont pas encore testées. Si Discover signale un conflit générique, exécutez la commande `dnf install` ci-dessus et joignez l’erreur de dépendances à votre rapport.

L’ancien RPM `0.1.0-alpha.7` a été compilé sur Fedora 44 et ne peut pas être installé sur Fedora 43, car il exige glibc 2.43. Son paquet Flatpak constitue une alternative en attendant un RPM construit avec les contrôles de compatibilité ci-dessus.

Chaque publication GitHub inclut `SHA256SUMS` et des attestations signées de provenance de compilation. Avec GitHub CLI installé, vérifiez un téléchargement avec `gh attestation verify <download> --repo flectar/mail`.

Depuis alpha 8, les versions alpha, bêta, candidates et stables d’Android utilisent un seul paquet (`com.flectar.mail`) et une identité de signature persistante. Les versions suivantes peuvent mettre à jour l’application en conservant les données locales. Alpha 6 et alpha 7 utilisaient des clés différentes et ne peuvent pas être mises à jour vers cette identité de signature.

Les deux processus de publication Android utilisent l’environnement GitHub `android-release`. Ils exigent les mêmes secrets de keystore et de mots de passe, vérifient le certificat public avec `platform/android/app-signing-certificate.sha256` et déduisent les codes de version Android du numéro de version publié. Une clé absente ou différente bloque la publication. **Ne générez pas de clé de remplacement et ne modifiez pas l’empreinte fixée pour une publication courante.** Un changement de mot de passe préserve l’identité si la clé privée et le certificat d’origine sont conservés.

Consultez la [configuration et récupération de la signature Android](../android-signing.md) pour la commande de migration à exécuter une seule fois, les secrets nécessaires, les sauvegardes et Play App Signing.

Les compilations locales de développement peuvent toujours générer une clé de test locale. Elles ne mettent à jour que les installations signées avec cette même clé.

Vous pouvez aussi compiler Flectar Mail à partir des sources avec la [chaîne d’outils Rust](https://rustup.rs/) :

```bash
cargo run --bin flectar-mail
```

Sur Linux, OAuth utilise le navigateur système via le portail du bureau et stocke les identifiants de renouvellement avec freedesktop Secret Service. Une session normale nécessite donc un composant `xdg-desktop-portal` et un fournisseur Secret Service tel que GNOME Keyring. Si le stockage sécurisé est indisponible, les compilations de publication s’arrêtent avant d’ouvrir la page OAuth, afin qu’une autorisation ne soit jamais terminée sans emplacement sûr où la conserver.

Les environnements de développement Podman et Docker n’ont généralement accès ni au D-Bus de la session hôte ni au réseau de boucle locale du navigateur hôte. Les compilations de débogage prennent en charge cette configuration avec un fichier d’identifiants de développement clairement identifié, accessible uniquement à son propriétaire, et un panneau dépliable pour coller l’URL finale de retour en boucle locale. Le stockage par fichier est exclu des compilations de publication.

## 🌍 Aidez à traduire Flectar Mail

Flectar Mail est conçu pour tout le monde, et votre aide pour le rendre disponible dans davantage de langues serait la bienvenue !

Nous utilisons [Weblate](https://translate.flectar.com/) pour gérer les traductions communautaires. Que vous souhaitiez traduire Flectar Mail dans votre langue maternelle, améliorer une traduction existante ou relire des textes traduits, toute contribution est bienvenue.

**[Commencer à traduire Flectar Mail →](https://translate.flectar.com/)**

Pour commencer :

1. Créez un compte sur notre plateforme de traduction.
2. Sélectionnez votre langue ou lancez une nouvelle traduction si elle n’existe pas encore.
3. Traduisez directement dans Weblate : aucune connaissance en programmation ou expérience de GitHub n’est nécessaire.

Les traductions sont synchronisées avec notre dépôt GitHub et soumises à relecture sous forme de pull requests. Les personnes qui contribuent peuvent aussi être créditées pour leur travail sur GitHub.

Les traductions du README sont maintenues dans ce dépôt. Consultez [Contribuer aux traductions](../../CONTRIBUTING.md#translations) pour en ajouter ou en mettre une à jour.

## ♿ Accessibilité

Flectar Mail propose la navigation au clavier, des thèmes clairs et sombres et des dispositions flexibles. Le travail sur l’accessibilité se poursuit. Consultez [ACCESSIBILITY.md](../../ACCESSIBILITY.md) pour les fonctions actuelles, les limites des tests et la manière de signaler un obstacle.

## 🔒 Sécurité

Pour signaler une vulnérabilité en privé, suivez [SECURITY.md](../../SECURITY.md). Ne publiez pas de détails sur les vulnérabilités dans les signalements publics.

## 🌐 Un logiciel libre pour tous

Flectar Mail est une seule application libre, sans édition communautaire séparée. Le client est distribué sous [GNU Affero General Public License v3](../../LICENSE). Consultez la [présentation des licences](../../LICENSING.md) pour les détails pratiques ou [CONTRIBUTING.md](../../CONTRIBUTING.md) pour participer au projet.

## 🫡 Remerciements

Flectar Mail existe grâce au travail de ces projets et de leurs contributeurs :

- [Slint](https://slint.dev/), la boîte à outils d’interface native qui fait fonctionner l’interface de Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz), le moteur HTML/CSS en Rust de l’équipe Dioxus qui assure l’affichage des e-mails.

---
