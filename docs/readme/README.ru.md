<div align="center">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="Логотип Flectar Mail">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <a href="../../README.md">English</a> · <a href="README.es.md">Español</a> · <a href="README.de.md">Deutsch</a> ·
    <a href="README.fr.md">Français</a> · <a href="README.pt_BR.md">Português (Brasil)</a> · <strong>Русский</strong> ·
    <a href="README.hi.md">हिन्दी</a> · <a href="README.ar.md">العربية</a> · <a href="README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>Почта снова стала быстрой</h3>
    <p>Создано с нуля ради скорости. Flectar Mail предлагает нативную производительность, мгновенный запуск и потребление приватной памяти около 20 МБ в простое.</p>
  </div>
  <p>
    <a href="https://flectar.com">Сайт</a> ·
    <a href="https://github.com/flectar/mail/issues">Сообщить о проблеме</a> ·
    <a href="../../CONTRIBUTING.md">Участвовать</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="Лицензия: AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="Загрузки всех выпусков"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="Последний выпуск"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="Состояние перевода"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="Язык: Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="Платформы: Windows, macOS, Linux, Android, iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail в macOS с общей папкой входящих и просмотром письма">
</picture>

Flectar Mail — лёгкое нативное приложение для почты, календарей и контактов. Оно мгновенно запускается, быстро реагирует на действия и использует лишь небольшую часть памяти, необходимой обычному почтовому клиенту на веб-технологиях.

## ❓ Почему Flectar Mail?

| Преимущество | Подробности |
| --- | --- |
| **Быстро с первого клика** | Нативный интерфейс и работа прежде всего с локальными данными позволяют открыть почту, не дожидаясь запуска браузерной среды. |
| **Около 20 МБ приватной памяти в простое** | Около 85 МБ с учётом кода и библиотек и в двенадцать раз меньше приватной памяти, чем у Thunderbird с тем же почтовым ящиком. Подробнее — в разделе [«Производительность»](#-производительность). |
| **Всё в одном месте** | Переключайтесь между почтой, календарями и контактами без необходимости объединять отдельные приложения. |
| **Работа без интернета заложена в основу** | Почта и календарь хранятся локально, поэтому синхронизированные данные остаются доступны без подключения. |
| **Поддержка ваших аккаунтов** | Gmail, Outlook и Microsoft 365 или службы на основе стандартов IMAP, POP3, SMTP, JMAP, CalDAV и CardDAV. |
| **Приватность по умолчанию** | Удалённые изображения заблокированы до вашего разрешения, поэтому пиксели отслеживания не могут сообщить отправителю, когда вы прочитали письмо. |
| **Безопасность на уровне архитектуры** | Письма никогда не открываются в WebView. Нативный конвейер на Rust отображает их HTML и CSS, не выполняя скрипты из писем, и тем самым исключает поверхность атаки встроенного браузера. |
| **Экспериментальный движок на Rust** | Отображение писем без браузерного движка — новая область. Пока совместимость улучшается, сложные письма могут отображаться некорректно. |
| **Для любого экрана** | Просторный и минималистичный интерфейсы рабочего стола предлагают те же возможности, что и компактный интерфейс для сенсорного управления. |
| **Нативное приложение с открытым кодом** | Создано с нуля на Rust. Это не браузер в отдельном окне; приложение распространяется по лицензии AGPLv3. |

## ⚡ Производительность

| В простое, один и тот же почтовый ящик | Приватная память | С учётом кода и библиотек (PSS) | Отображение писем |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17,2–17,4 МиБ** | **79,1–80,0 МиБ** | Blitz |
| Geary 46 | 126,1–126,2 МиБ | 309,6–311,4 МиБ | WebKitGTK |
| Evolution 3.56 | 191,0–192,2 МиБ | 395,0–395,9 МиБ | WebKitGTK |
| Thunderbird 159 | 244,1–248,1 МиБ | 424,4–427,1 МиБ | Gecko |
| Betterbird 153 | 243,9–247,8 МиБ | 411,9–419,7 МиБ | Gecko |
| Mailspring 1.26 | 311,4–314,9 МиБ | 551,1–551,7 МиБ | Chromium |
| Aerion 0.3.6 | 421,9–437,3 МиБ | 660,4–678,0 МиБ | WebKitGTK |

Измерения проведены в Linux через 60 секунд после запуска: один аккаунт IMAP (860 писем), открыты папка входящих и HTML-рассылка, используется рендерер CPU по умолчанию. Каждый диапазон охватывает два замера; учтены все процессы, которые запускает каждое приложение. Приватная память — это значение, которое диспетчер задач Windows показывает в столбце **Память**. В Windows с 7 реальными аккаунтами и примерно 69 000 непрочитанных писем диспетчер задач показал 37,8 МиБ при открытом окне и 4,2 МиБ после закрытия в системный трей. Подробнее о том, [как мы измеряем память и как проверить результаты самостоятельно](../performance.md).

### Сравнение с другими клиентами

| Почтовый клиент | Интерфейс | Отображение писем | Платформы |
| --- | --- | --- | --- |
| **Flectar Mail** | Нативное приложение на Rust | Blitz, движок HTML/CSS на Rust; без WebView | Windows, macOS, Linux, Android, iOS (сторонняя установка) |
| Thunderbird | Настольное приложение на основе Gecko | Gecko | Windows, macOS, Linux, Android |
| Outlook для Windows | Приложение Windows с веб-интерфейсом | Microsoft Edge WebView2 | Windows |
| Spark | Приложение на Electron | Chromium | macOS, Windows, iOS, Android |
| Aerion | Приложение на Go с веб-интерфейсом на Wails | Системный WebView | Linux, macOS, Windows |
| Gmail во вкладке браузера | Веб-приложение | Chrome или другой браузер | Веб |

## ✨ Возможности

| Возможность | Описание |
| --- | --- |
| [Цепочки писем](../features.md#conversation-threads) | Ответы группируются в хронологическом порядке, поддерживается полная навигация с клавиатуры. Группировку можно отключить. |
| [Вкладки](../features.md#tabs) | Вкладки как в браузере, которые помнят своё место; на компьютере — сочетания клавиш, на телефоне — переключатель вкладок. |
| [Профили и цвета аккаунтов](../features.md#profiles-and-account-colors) | Аккаунты объединяются в профили, например «Работа» или «Личное», а каждое письмо помечается цветом. |
| [Оформление](../features.md#appearance) | Полный или минималистичный интерфейс, светлая или тёмная тема, цветовые палитры, аватары отправителей и цвет значка приложения. |
| [Окно без рамки](../features.md#frameless-window) | Кнопки окна на панели инструментов с привычным поведением каждой платформы. |
| [Календарь, контакты и файлы](../features.md#calendar-contacts-and-files) | Синхронизация CalDAV и CardDAV, файлы JMAP и WebDAV, поиск по вложениям, офлайн-файлы и предпросмотр. |
| [Подписи и OpenPGP](../features.md#signatures-and-openpgp) | Именованные подписи, а также подписание и шифрование OpenPGP/MIME на компьютере через GnuPG. |
| [Хранилище и резервные копии](../features.md#storage-and-backups) | Занятое место по типам данных, снимки баз данных, резервные копии аккаунтов и правила удаления почты POP3 с сервера. |
| [Импорт аккаунтов и почты](../features.md#importing-accounts-and-mail) | Thunderbird, K-9 Mail, mbox, `.eml`, maildir, Apple Mail и ZIP-архивы. |
| [Отображение HTML](../features.md#html-rendering) | HTML писем отображается движком Blitz на Rust — на CPU или GPU. |

### Полное и минималистичное рабочие пространства

<table>
  <thead>
    <tr>
      <th width="50%">Полный интерфейс</th>
      <th width="50%">Минималистичный интерфейс</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Полный настольный интерфейс Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="Минималистичный настольный интерфейс Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Вкладки, переписки и профили

<table>
  <thead>
    <tr>
      <th width="33.33%">Вкладки</th>
      <th width="33.33%">Переписка</th>
      <th width="33.33%">Цвета профилей</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
          <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail с вкладками переписок, календаря, контактов и файлов" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
          <img src="../../resources/screenshots/desktop-thread-light.png" alt="Переписка Flectar Mail с входящими письмами и ответами" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
          <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Общая папка входящих Flectar Mail с фиолетовыми и оранжевыми метками профилей" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Цветовые палитры

<table>
  <thead>
    <tr>
      <th width="25%">Бирюзовая</th>
      <th width="25%">Зелёная</th>
      <th width="25%">Фиолетовая</th>
      <th width="25%">Своя</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="Бирюзовая палитра Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="Зелёная палитра Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="Фиолетовая палитра Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Пользовательская палитра Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Окно без рамки

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
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="Окно Flectar Mail без рамки в macOS" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="Окно Flectar Mail без рамки в Windows" width="100%">
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
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="Окно Flectar Mail без рамки в GNOME" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="Окно Flectar Mail без рамки в KDE Plasma" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Календарь, контакты и файлы

<table>
  <thead>
    <tr>
      <th width="33.33%">Календарь</th>
      <th width="33.33%">Контакты</th>
      <th width="33.33%">Файлы (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="Календарь Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="Контакты Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="Файлы Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Для небольших экранов

| Светлая тема | Тёмная тема |
| --- | --- |
| ![Мобильная папка входящих Flectar Mail в светлой теме](../../resources/screenshots/mobile-light.png) | ![Мобильная папка входящих Flectar Mail в тёмной теме](../../resources/screenshots/mobile-dark.png) |

## 🧪 Экспериментальное отображение HTML

> [!WARNING]
> Отображение HTML-писем — пока самая экспериментальная часть Flectar Mail. Некоторые письма, особенно со сложной или необычной разметкой и CSS, могут отображаться некорректно.

Flectar Mail отображает письма с помощью [Blitz](https://github.com/DioxusLabs/blitz) — движка HTML/CSS на Rust от команды Dioxus — вместо встроенного браузера или WebView. Если письмо отображается неправильно, [сообщите об этом](https://github.com/flectar/mail/issues). Подробнее о [движке и режимах CPU и GPU](../features.md#html-rendering).

## 📥 Скачать Flectar Mail

Flectar Mail находится в разработке и пока не является стабильным. Предварительные сборки публикуются в [GitHub Releases](https://github.com/flectar/mail/releases).

> [!WARNING]
> **Верификация Google OAuth требует ежегодной оценки безопасности CASA.** Flectar — открытый общественный проект, поэтому прохождение оценки зависит от спонсорской поддержки. Пока необходимая верификация не профинансирована и не завершена, стабильный выпуск с Gmail OAuth, настроенным по умолчанию, невозможен. Если хотите помочь сделать встроенный вход в Gmail доступным, [поддержите Flectar](https://github.com/sponsors/flectar).

| Платформа | Загрузка | Примечания |
| --- | --- | --- |
| Linux x64 и arm64 | AppImage, `.deb` для Debian/Ubuntu, `.rpm` для Fedora 43/44 или пакет Flatpak | Нужно хранилище ключей с поддержкой Secret Service. Пакет Flatpak — предварительная версия для ручной установки; во Flathub его пока нет. |
| Windows x64 и arm64 | Установщик `.exe` или переносимый ZIP | Предварительные сборки без подписи |
| macOS 14+ (Apple silicon или Intel) | DMG или ZIP приложения | Подпись ad hoc, без нотарификации |
| Android 8.0+ (arm64) | Подписанный APK | Предварительные и стабильные выпуски используют одну идентичность приложения. |
| iOS 17+ (iPhone и iPad) | Неподписанный `.ipa` | Устанавливается через AltStore, SideStore или Sideloadly с вашим Apple ID. |
| HarmonyOS и OpenHarmony | [Порт сообщества](../../platform/ohos/README.md) от [springmin](https://github.com/springmin) | Не входит в выпуски на GitHub; соберите его из исходного кода. |

В сборках без ключей приложений OAuth вход в Gmail и Outlook отключён. Используйте значок шестерёнки **Настройки входа** на экране приветствия, чтобы добавить собственную регистрацию приложения Google или Microsoft, либо подключите аккаунт IMAP, POP3 или JMAP.

В [руководстве по установке](../install.md) описаны команды установки, хранилище ключей и портал рабочего стола в Linux, особенности Flatpak и RPM, проверка загрузок, подпись сборок Android, установка на iOS и сборка из исходного кода командой `cargo run --bin flectar-mail`.

## 🤝 Как принять участие

| Тема | Подробности |
| --- | --- |
| 🌍 **Перевод** | Переводите приложение, улучшайте существующие переводы или проверяйте строки в [Weblate](https://translate.flectar.com/); знания программирования или опыт работы с GitHub не нужны. Переводы поступают в этот репозиторий в виде запросов на слияние, а их авторы получают указание авторства в GitHub. Переводы README хранятся здесь же; см. [руководство для переводчиков](../../CONTRIBUTING.md#translations). |
| ♿ **Доступность** | Управление с клавиатуры, светлая и тёмная темы и гибкие варианты интерфейса; работа над доступностью продолжается. Текущие возможности, ограничения тестирования и порядок сообщения о препятствиях описаны в [ACCESSIBILITY.md](../../ACCESSIBILITY.md). |
| 🔒 **Безопасность** | Сообщайте об уязвимостях конфиденциально, как описано в [SECURITY.md](../../SECURITY.md), и не публикуйте сведения о них в открытых обсуждениях проблем. |
| 🌐 **Открытый код** | Flectar Mail — единое приложение с открытым кодом, без отдельной общественной редакции, распространяемое по лицензии [GNU Affero General Public License v3](../../LICENSE). Практические подробности приведены в [обзоре лицензирования](../../LICENSING.md). |
| 🛠️ **Участие** | Как участвовать в развитии проекта, описано в [CONTRIBUTING.md](../../CONTRIBUTING.md). |

## 🫡 Благодарности

Flectar Mail существует благодаря работе этих проектов и их участников:

- [Slint](https://slint.dev/) — инструментарий нативного интерфейса, на котором построен Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz) — движок HTML/CSS на Rust от команды Dioxus, обеспечивающий отображение писем.

---
