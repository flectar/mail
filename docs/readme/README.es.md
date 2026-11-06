<div align="center">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="Logotipo de Flectar Mail">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <a href="../../README.md">English</a> · <strong>Español</strong> · <a href="README.de.md">Deutsch</a> ·
    <a href="README.fr.md">Français</a> · <a href="README.pt_BR.md">Português (Brasil)</a> · <a href="README.ru.md">Русский</a> ·
    <a href="README.hi.md">हिन्दी</a> · <a href="README.ar.md">العربية</a> · <a href="README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>El correo vuelve a ser rápido</h3>
    <p>Diseñado desde cero para la velocidad. Flectar Mail ofrece rendimiento nativo, inicio instantáneo y unos 20 MB de memoria privada en reposo.</p>
  </div>
  <p>
    <a href="https://flectar.com">Sitio web</a> ·
    <a href="https://github.com/flectar/mail/issues">Informar de un problema</a> ·
    <a href="../../CONTRIBUTING.md">Contribuir</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="Licencia: AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="Descargas de todas las versiones"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="Última versión"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="Estado de la traducción"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="Lenguaje: Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="Plataformas: Windows, macOS, Linux, Android, iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail en macOS con la bandeja de entrada unificada y la vista de mensajes">
</picture>

Flectar Mail es una aplicación nativa y ligera para tu correo, calendarios y
contactos. Está diseñada para abrirse al instante, responder con fluidez y usar
una fracción de la memoria de un cliente de correo típico basado en tecnologías web.

## ❓ ¿Por qué Flectar Mail?

| Ventaja | Detalles |
| --- | --- |
| **Rápido desde el primer clic** | Una interfaz nativa y un acceso a los datos que prioriza el almacenamiento local te llevan a tu bandeja de entrada sin esperar a que se cargue un entorno de navegador. |
| **Unos 20 MB de memoria privada en reposo** | Unos 85 MB contando su código y bibliotecas, y la doceava parte de la memoria privada que usa Thunderbird con el mismo buzón. Consulta [Rendimiento](#-rendimiento). |
| **Todo en un mismo lugar** | Pasa del correo a los calendarios y contactos sin tener que combinar varias aplicaciones independientes. |
| **Sin conexión por diseño** | Tu buzón y tu calendario se guardan localmente, así que los datos sincronizados siguen siendo útiles sin conexión. |
| **Compatible con tus cuentas** | Conecta Gmail, Outlook y Microsoft 365, o servicios basados en los estándares IMAP, POP3, SMTP, JMAP, CalDAV y CardDAV. |
| **Privacidad por defecto** | Las imágenes remotas se bloquean hasta que las permites, así que los píxeles de seguimiento no pueden revelar cuándo lees un mensaje. |
| **Seguridad desde la arquitectura** | El correo nunca se abre en una WebView. Un sistema nativo en Rust renderiza su HTML y CSS sin ejecutar los scripts de los mensajes, lo que evita la superficie de ataque de un navegador integrado. |
| **Un renderizador experimental en Rust** | Renderizar el correo sin un motor de navegador es un terreno nuevo. Puede que los mensajes complejos aún no se muestren correctamente mientras mejora la compatibilidad. |
| **Para todas las pantallas** | Las vistas de escritorio, amplias o minimalistas, comparten la misma experiencia que la interfaz compacta adaptada al uso táctil. |
| **Nativo y de código abierto** | Creado desde cero con Rust. No es un navegador dentro de una ventana y se distribuye bajo la licencia AGPLv3. |

## ⚡ Rendimiento

| En reposo, mismo buzón | Memoria privada | Con código y bibliotecas (PSS) | Renderiza el correo con |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17,2–17,4 MiB** | **79,1–80,0 MiB** | Blitz |
| Geary 46 | 126,1–126,2 MiB | 309,6–311,4 MiB | WebKitGTK |
| Evolution 3.56 | 191,0–192,2 MiB | 395,0–395,9 MiB | WebKitGTK |
| Thunderbird 159 | 244,1–248,1 MiB | 424,4–427,1 MiB | Gecko |
| Betterbird 153 | 243,9–247,8 MiB | 411,9–419,7 MiB | Gecko |
| Mailspring 1.26 | 311,4–314,9 MiB | 551,1–551,7 MiB | Chromium |
| Aerion 0.3.6 | 421,9–437,3 MiB | 660,4–678,0 MiB | WebKitGTK |

Mediciones realizadas en Linux con una cuenta IMAP (860 mensajes), la bandeja de
entrada abierta y un boletín HTML en pantalla, 60 segundos después de abrir la
aplicación y con el renderizador por CPU, que es el predeterminado. Cada rango
abarca dos mediciones y se cuentan todos los procesos que inicia cada
aplicación. La memoria privada es lo que el Administrador de tareas de Windows
muestra como **Memoria**. En Windows, con 7 cuentas reales y unos 69.000
mensajes sin leer, el Administrador de tareas mostró 37,8 MiB con la ventana
abierta y 4,2 MiB al cerrarla a la bandeja del sistema. Consulta [cómo lo medimos y cómo comprobarlo por tu cuenta](../performance.md).

### Comparación con otros clientes

| Cliente de correo | Interfaz | Renderiza el correo con | Plataformas |
| --- | --- | --- | --- |
| **Flectar Mail** | Aplicación nativa en Rust | Blitz, un renderizador HTML/CSS en Rust; sin WebView | Windows, macOS, Linux, Android, iOS (mediante sideloading) |
| Thunderbird | Aplicación de escritorio basada en Gecko | Gecko | Windows, macOS, Linux, Android |
| Outlook para Windows | Aplicación de Windows con interfaz web | Microsoft Edge WebView2 | Windows |
| Spark | Aplicación Electron | Chromium | macOS, Windows, iOS, Android |
| Aerion | Aplicación en Go con interfaz web de Wails | WebView del sistema | Linux, macOS, Windows |
| Gmail en una pestaña del navegador | Aplicación web | Chrome u otro navegador | Web |

## ✨ Funciones

| Función | Qué hace |
| --- | --- |
| [Conversaciones](../features.md#conversation-threads) | Agrupa las respuestas en orden cronológico, con navegación completa por teclado. Puedes desactivar esta función. |
| [Pestañas](../features.md#tabs) | Pestañas como las de un navegador que recuerdan dónde estabas, con atajos en el escritorio y un selector de pestañas en el móvil. |
| [Perfiles y colores de cuenta](../features.md#profiles-and-account-colors) | Agrupa las cuentas en perfiles, como Trabajo o Personal, e identifica cada mensaje con un color. |
| [Apariencia](../features.md#appearance) | Vista completa o minimalista, tema claro u oscuro, paletas de colores, avatares de los remitentes y colores del icono de la aplicación. |
| [Ventana sin marco](../features.md#frameless-window) | Controles de ventana en la barra de herramientas que conservan el comportamiento de cada plataforma. |
| [Calendario, contactos y archivos](../features.md#calendar-contacts-and-files) | Sincronización CalDAV y CardDAV, archivos JMAP y WebDAV, búsqueda de adjuntos, archivos sin conexión y vistas previas. |
| [Firmas y OpenPGP](../features.md#signatures-and-openpgp) | Firmas con nombre, además de firma y cifrado OpenPGP/MIME en el escritorio mediante GnuPG. |
| [Almacenamiento y copias de seguridad](../features.md#storage-and-backups) | Espacio ocupado por tipo, instantáneas de las bases de datos, copias de seguridad de cuentas y reglas para eliminar del servidor el correo POP3. |
| [Importación de cuentas y correo](../features.md#importing-accounts-and-mail) | Thunderbird, K-9 Mail, mbox, `.eml`, maildir, Apple Mail y archivos ZIP. |
| [Renderizado HTML](../features.md#html-rendering) | El HTML del correo se renderiza en Rust con Blitz, por CPU o por GPU. |

### Vistas completa y minimalista

<table>
  <thead>
    <tr>
      <th width="50%">Vista completa</th>
      <th width="50%">Vista minimalista</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Vista completa de Flectar Mail en el escritorio" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="Vista minimalista de Flectar Mail en el escritorio" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Pestañas, conversaciones y perfiles

<table>
  <thead>
    <tr>
      <th width="33.33%">Pestañas</th>
      <th width="33.33%">Conversación</th>
      <th width="33.33%">Colores de perfil</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
          <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail con pestañas para varias conversaciones, el calendario, los contactos y los archivos" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
          <img src="../../resources/screenshots/desktop-thread-light.png" alt="Conversación de Flectar Mail con mensajes recibidos y respuestas" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
          <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Bandeja de entrada unificada de Flectar Mail con indicadores de perfil morados y naranjas" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Paletas de colores

<table>
  <thead>
    <tr>
      <th width="25%">Verde azulado</th>
      <th width="25%">Verde</th>
      <th width="25%">Morado</th>
      <th width="25%">Personalizada</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="Paleta verde azulada de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="Paleta verde de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="Paleta morada de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Paleta personalizada de Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Ventana sin marco

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
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="Ventana sin marco de Flectar Mail en macOS" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="Ventana sin marco de Flectar Mail en Windows" width="100%">
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
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="Ventana sin marco de Flectar Mail en GNOME" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="Ventana sin marco de Flectar Mail en KDE Plasma" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Calendario, contactos y archivos

<table>
  <thead>
    <tr>
      <th width="33.33%">Calendario</th>
      <th width="33.33%">Contactos</th>
      <th width="33.33%">Archivos (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="Calendario de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="Contactos de Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="Archivos de Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Para pantallas pequeñas

| Claro | Oscuro |
| --- | --- |
| ![Bandeja de entrada móvil de Flectar Mail en modo claro](../../resources/screenshots/mobile-light.png) | ![Bandeja de entrada móvil de Flectar Mail en modo oscuro](../../resources/screenshots/mobile-dark.png) |

## 🧪 Renderizado HTML experimental

> [!WARNING]
> El renderizado de correo HTML es actualmente la parte más experimental de
> Flectar Mail. Algunos mensajes, especialmente los que contienen marcado y CSS
> complejos o poco habituales, pueden no mostrarse correctamente todavía.

Flectar Mail renderiza el correo con [Blitz](https://github.com/DioxusLabs/blitz),
un renderizador HTML/CSS en Rust del equipo de Dioxus, en lugar de integrar un
navegador o una WebView. Si un mensaje se muestra de forma incorrecta,
[informa del problema](https://github.com/flectar/mail/issues). Más información
sobre [el renderizador y las opciones de CPU y GPU](../features.md#html-rendering).

## 📥 Consigue Flectar Mail

Flectar Mail está en desarrollo y todavía no es estable. Las versiones de prueba
se publican en [GitHub Releases](https://github.com/flectar/mail/releases).

> [!WARNING]
> **La verificación OAuth de Google requiere una evaluación de seguridad CASA
> anual.** Flectar es un proyecto comunitario de código abierto, por lo que
> completar esta evaluación depende del apoyo de patrocinadores. Hasta que la
> verificación necesaria esté financiada y completada, no será posible publicar
> una versión estable con OAuth de Gmail configurado por defecto. Si quieres
> ayudar a ofrecer el inicio de sesión integrado de Gmail,
> [patrocina Flectar](https://github.com/sponsors/flectar).

| Plataforma | Descarga | Notas |
| --- | --- | --- |
| Linux x64 y arm64 | AppImage, `.deb` para Debian/Ubuntu, `.rpm` para Fedora 43/44 o un paquete Flatpak | Necesita un llavero compatible con Secret Service. El Flatpak es una versión de prueba para instalar manualmente y todavía no está en Flathub. |
| Windows x64 y arm64 | Instalador `.exe` o ZIP portátil | Versiones de prueba sin firmar |
| macOS 14+ (Apple silicon o Intel) | DMG o ZIP de la aplicación | Firma ad hoc, sin notarizar |
| Android 8.0+ (arm64) | APK firmado | Las versiones de prueba y las estables comparten una misma identidad de aplicación. |
| iOS 17+ (iPhone y iPad) | `.ipa` sin firmar | Se instala con AltStore, SideStore o Sideloadly usando tu ID de Apple. |
| HarmonyOS y OpenHarmony | [Adaptación comunitaria](../../platform/ohos/README.md) de [springmin](https://github.com/springmin) | No forma parte de las publicaciones de GitHub; compílala desde el código fuente. |

Las compilaciones sin claves de aplicación OAuth mantienen desactivado el
inicio de sesión en Gmail y Outlook. Usa el engranaje de **Ajustes de inicio
de sesión** de la pantalla de bienvenida para añadir tu propio registro de
aplicación de Google o Microsoft, o conecta una cuenta IMAP, POP3 o JMAP.

La [guía de instalación](../install.md) explica los comandos de instalación, el
llavero y el portal del escritorio en Linux, las notas sobre Flatpak y RPM, cómo
verificar las descargas, la firma de Android, la instalación en iOS y cómo
compilar desde el código fuente con `cargo run --bin flectar-mail`.

## 🤝 Participa

| Tema | Detalles |
| --- | --- |
| 🌍 **Traducir** | Traduce la aplicación, mejora una traducción o revisa textos traducidos en [Weblate](https://translate.flectar.com/), sin necesidad de saber programar ni de tener experiencia con GitHub. Las traducciones llegan a este repositorio como solicitudes de cambios, con reconocimiento en GitHub para quienes contribuyen. Las traducciones del README se mantienen aquí; consulta la [guía para contribuir traducciones](../../CONTRIBUTING.md#translations). |
| ♿ **Accesibilidad** | Navegación con teclado, temas claros y oscuros y diseños flexibles; seguimos trabajando en la accesibilidad. [ACCESSIBILITY.md](../../ACCESSIBILITY.md) explica las funciones actuales, las limitaciones de las pruebas y cómo informar de una barrera de accesibilidad. |
| 🔒 **Seguridad** | Informa de las vulnerabilidades de forma privada como se indica en [SECURITY.md](../../SECURITY.md) y no publiques sus detalles en informes de problemas públicos. |
| 🌐 **Código abierto** | Una única aplicación de código abierto, sin una edición comunitaria separada, distribuida bajo la [Licencia Pública General Affero de GNU v3](../../LICENSE). La [descripción de las licencias](../../LICENSING.md) recoge los detalles prácticos. |
| 🛠️ **Contribuir** | [CONTRIBUTING.md](../../CONTRIBUTING.md) explica cómo ayudar a dar forma al proyecto. |

## 🫡 Agradecimientos

Flectar Mail es posible gracias al trabajo de estos proyectos y de quienes
contribuyen a ellos:

- [Slint](https://slint.dev/), el conjunto de herramientas de interfaz nativa
  que da vida a la interfaz de Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz), el renderizador HTML/CSS en Rust
  del equipo de Dioxus que permite leer los mensajes de correo.

---
