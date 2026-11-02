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
    <p>Создано с нуля ради скорости. Flectar Mail предлагает нативную производительность, мгновенный запуск и потребление оперативной памяти всего от 80 МБ.</p>
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

- **Быстро с первого клика.** Нативный интерфейс и работа прежде всего с локальными данными позволяют открыть почту, не дожидаясь запуска браузерной среды.
- **Всего от 80 МБ оперативной памяти.** Flectar Mail специально спроектирован для низкого потребления памяти, сохраняя все возможности почтового клиента.
- **Всё в одном месте.** Переключайтесь между почтой, календарями и контактами без необходимости объединять отдельные приложения.
- **Работа без интернета заложена в основу.** Почта и календарь хранятся локально, поэтому синхронизированные данные остаются доступны без подключения.
- **Поддержка ваших аккаунтов.** Подключайте Gmail, Outlook и Microsoft 365 или службы на основе стандартов IMAP, POP3, SMTP, JMAP, CalDAV и CardDAV.
- **Приватность по умолчанию.** Удалённые изображения заблокированы до вашего разрешения: это помогает не дать пикселям отслеживания сообщить отправителю, когда вы прочитали письмо.
- **Безопасность на уровне архитектуры.** Содержимое писем никогда не открывается в WebView. Flectar Mail отображает HTML и CSS собственным нативным конвейером на Rust, не выполняет скрипты из писем и по умолчанию блокирует удалённые изображения. Таким образом исключается поверхность атаки встроенного браузера.
- **Экспериментальный движок на Rust.** Отображение писем без браузерного движка — новая область. Пока совместимость улучшается, возможны ошибки отображения, особенно в сложных письмах.
- **Для любого экрана.** Просторный и минималистичный интерфейсы рабочего стола предлагают те же возможности, что и компактный интерфейс для сенсорного управления.
- **Нативное приложение с открытым кодом.** Создано с нуля на Rust. Это не браузер в отдельном окне; приложение распространяется по лицензии AGPLv3.

## 📁 Файлы и вложения

Просматривайте хранилища JMAP/WebDAV, ищите вложения, сохраняйте файлы для работы без интернета и открывайте предпросмотр PDF, изображений и текста.

## ✍🏻 Подписи аккаунтов и OpenPGP

В разделе Настройки → Аккаунты → Подписи и OpenPGP можно создавать именованные подписи, выбирать разные подписи по умолчанию для новых писем и ответов, а также менять подпись при написании письма. Подписание и шифрование OpenPGP/MIME на компьютере используют установленный GnuPG 2.x и pinentry для ввода парольных фраз закрытых ключей. Обязательное шифрование блокирует отправку, если ключи получателей отсутствуют или недействительны; защищённые черновики остаются локальными до отправки.

S/MIME и OpenPGP на мобильных устройствах пока не поддерживаются.

## 🧪 Экспериментальное отображение HTML

> [!WARNING]
> Отображение HTML-писем — пока самая экспериментальная часть Flectar Mail. Некоторые письма, особенно со сложной или необычной разметкой и CSS, могут отображаться некорректно.

Чтобы клиент оставался полностью нативным и потреблял около 80 МБ памяти, Flectar Mail отображает HTML писем с помощью [Blitz](https://github.com/DioxusLabs/blitz) — движка HTML/CSS на Rust от команды Dioxus, — вместо встроенного браузера или WebView.

В настольных сборках в разделе Настройки → Общие → Рендерер доступны **CPU — Low Memory** и **GPU — WGPU**. Изначально выбран CPU, при этом WGPU не инициализируется. Режим GPU использует Slint и Vello на общем устройстве WGPU 30; изменение применяется после перезапуска приложения. Если запуск с GPU не удался, приложение автоматически возвращается к CPU.

Насколько нам известно, Flectar Mail — один из первых проектов, использующих Blitz для произвольного HTML из реальной почты. В письмах встречается много необычного HTML и CSS, что предъявляет серьёзные требования к движку. Сейчас мы поддерживаем ряд собственных исправлений Blitz и надеемся со временем передать как можно больше из них в основной проект.

Если письмо отображается неправильно, [сообщите об этом](https://github.com/flectar/mail/issues). Этот подход всё ещё экспериментальный, но именно он во многом позволяет Flectar Mail оставаться таким лёгким по сравнению с клиентами на WebView, созданными с помощью Tauri или Wails.

## 🎨 Настройте под себя

Выберите рабочее пространство под свой способ работы с почтой: подробный интерфейс с тремя панелями или минималистичный вид, светлую или тёмную тему, встроенную или собственную цветовую палитру. Аватары отправителей можно показать или скрыть. Можно также выбрать другой цвет значка приложения: на панели задач или в Dock, в системном трее, в меню приложений Linux и на главном экране Android и iOS.

Оформление сообщений может следовать теме приложения или независимо оставаться светлым либо тёмным. Выбранная палитра применяется к HTML, обычному тексту и режиму чтения. Чтобы временно увидеть исходные цвета отправителя, нажмите значок контраста между поиском и режимом чтения на панели письма в настольном интерфейсе или выберите **Показать исходные цвета** в меню с тремя точками на экране любого размера.

В полном рабочем пространстве одновременно видны папки, список сообщений и выбранное письмо. Минималистичный вид уменьшает визуальный шум и при необходимости даёт больше места каждому разделу почты.

### 👥 Профили и цвета аккаунтов

Объединяйте связанные аккаунты в профили, например «Работа» и «Личное», и задавайте каждому свой цвет. Создайте профиль в меню профилей в начале панели вкладок или в Настройки → Аккаунты → Профили и отметьте входящие в него аккаунты. Выбор профиля ограничивает почтовый интерфейс его аккаунтами: общая папка входящих, поиск, счётчики и боковая панель показывают только их почту, а вкладки остаются прежними. **Все аккаунты** снова показывает всё.

Можно также переопределить цвет отдельного аккаунта и показывать итоговый цвет у левого края каждого сообщения. Эти метки помогают различать аккаунты в общей папке входящих; в примере ниже для работы используется фиолетовый, а для поддержки — оранжевый.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
  <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Общая папка входящих Flectar Mail с фиолетовыми и оранжевыми метками профилей">
</picture>

### 🧵 Цепочки писем

Ответы группируются в хронологическом порядке, а активное сообщение разворачивается в панели чтения.

Используйте **Вверх/Вниз** для перемещения по сообщениям, **Home/End** для перехода к первому или последнему загруженному письму и **Page Up/Page Down** для постраничного перемещения. **Enter** открывает текущее сообщение и переводит фокус на его содержимое; **Пробел** отмечает или снимает отметку для групповых действий. **Tab/Shift+Tab** переключают элементы управления. В полном интерфейсе **F6** переносит фокус между списком писем и областью чтения. В минималистичном виде стрелки выделяют письма, **Enter** открывает их, а **Escape** или **F6** возвращает к списку.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
  <img src="../../resources/screenshots/desktop-thread-light.png" alt="Переписка Flectar Mail с входящими письмами и ответами">
</picture>

### 🗂️ Вкладки

Открывайте несколько разделов одновременно во вкладках, как в браузере. Каждая вкладка помнит своё место: почтовую папку, поиск, открытую переписку, календарь, контакты, файлы или настройки, а также положение прокрутки и читаемое сообщение. Щёлкните по письму средней кнопкой или с Ctrl/⌘, чтобы открыть его в фоновой вкладке (добавьте Shift для перехода в неё), либо выберите **Открыть в новой вкладке** в меню. Перетаскивайте вкладки для изменения порядка, закрывайте средней кнопкой и используйте правую кнопку, чтобы закрыть остальные или все справа. При следующем запуске Flectar Mail работа продолжится с того же места.

**Ctrl/⌘+T** открывает вкладку, **Ctrl/⌘+W** закрывает текущую, а **Ctrl/⌘+Shift+T** восстанавливает последнюю закрытую. **Ctrl+Tab** и **Ctrl+Shift+Tab** переключают вкладки, **Ctrl/⌘+1–8** переходят к конкретной вкладке, а **Ctrl/⌘+9** — к последней.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
  <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail с вкладками переписок, календаря, контактов и файлов">
</picture>

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

### 🪟 Окно без рамки

Включите **Окно без рамки** в Настройки → Общие → Интерфейс, чтобы заменить системную строку заголовка кнопками окна на панели инструментов. Каждая платформа сохраняет привычное поведение: macOS — стандартные цветные кнопки, Windows — Snap Layouts и системное меню, Linux — порядок кнопок и действия заголовка, настроенные в GNOME или KDE.

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

### 📅 Календарь, контакты и файлы

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

### 💾 Хранилище и резервные копии

Настройки → Хранилище показывает, сколько места занимают почта, вложения, офлайн-файлы и базы данных. Там же можно экспортировать проверенные снимки баз данных или перенести настройки подключённых аккаунтов и предпочтения через резервную копию. Пароли и токены OAuth остаются в системном хранилище ключей, поэтому на восстановленном устройстве потребуется войти снова.

Аккаунты POP3 скачивают почту на это устройство и оставляют её на сервере, если вы не выберете другое: удаление через 7, 14 или 30 дней, после удаления здесь либо сразу после скачивания, но никогда до сохранения на устройстве. Их папки существуют только на этом устройстве, поэтому снимки включают эту почту. При удалении аккаунта POP3 или всех данных приложение сначала предлагает экспортировать её в файлы mbox, по одному на папку, которые можно импортировать в другие почтовые приложения.

Эта же функция импортирует аккаунты из Thunderbird (папка профиля, архив из Инструменты › Экспорт или `prefs.js`), Thunderbird для Android и K-9 Mail (экспорт настроек `.k9s`). Перед добавлением вы проверяете каждый аккаунт: настройки, которые Flectar Mail не может воспроизвести, например сервер Exchange, объясняются, а не меняются незаметно. Аккаунты POP3 сохраняют правило удаления почты с сервера. Существующие аккаунты никогда не изменяются, пароли не импортируются.

Так же импортируется и сохранённая почта: локальные папки Thunderbird и почта, скачанная его аккаунтами POP3, файлы mbox, например экспорт Google Takeout или экспорт самого Flectar Mail, письма `.eml`, папки maildir, почтовые ящики Apple Mail и ZIP-архивы с ними. Вы выбираете файлы, можно сразу несколько, а затем папки (корзина и спам пропускаются, если вы их не отметите), а структура папок и отметки «прочитано» и «помечено» сохраняются. Письма, которые там уже есть, пропускаются, поэтому прерванный импорт можно просто запустить снова, а долгий импорт может продолжаться в фоне. Почта, не принадлежащая ни одному аккаунту, попадает в раздел **На этом устройстве**: папки, которые существуют только здесь и которые можно экспортировать или удалить в Настройки → Хранилище. Её также можно импортировать в папки аккаунта POP3 или загрузить на сервер аккаунта IMAP, JMAP или Gmail, в отдельную папку.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-storage-light.png">
  <img src="../../resources/screenshots/desktop-storage-light.png" alt="Настройки хранилища Flectar Mail с объёмом локальных данных, почтой на этом устройстве, снимками базы данных и резервными копиями аккаунтов">
</picture>

### 📱 Для небольших экранов

Компактный интерфейс держит важные действия под рукой и при необходимости отдаёт сообщениям, событиям и контактам весь экран.

| Светлая тема | Тёмная тема |
| --- | --- |
| ![Мобильная папка входящих Flectar Mail в светлой теме](../../resources/screenshots/mobile-light.png) | ![Мобильная папка входящих Flectar Mail в тёмной теме](../../resources/screenshots/mobile-dark.png) |

## 📥 Скачать Flectar Mail

Flectar Mail находится в разработке и пока не является стабильным.

> [!WARNING]
> **Верификация Google OAuth требует ежегодной оценки безопасности CASA.** Flectar — открытый общественный проект, поэтому прохождение оценки зависит от спонсорской поддержки. Пока необходимая верификация не профинансирована и не завершена, стабильный выпуск с Gmail OAuth, настроенным по умолчанию, невозможен. Если хотите помочь сделать встроенный вход в Gmail доступным, [поддержите Flectar](https://github.com/sponsors/flectar).
>
> Верификация OAuth в Google и Microsoft ещё продолжается. Более ранние предварительные выпуски на GitHub предназначены для тестировщиков с собственными регистрациями OAuth или аккаунтами IMAP, POP3 либо JMAP.

В сборках без ключей приложений OAuth вход в Gmail и Outlook отключён. Используйте значок шестерёнки **Настройки входа** на экране приветствия, чтобы сохранить собственную регистрацию приложения Google или Microsoft, либо подключите аккаунт IMAP, POP3 или JMAP. Пользовательские ключи общие с разделом настроек и имеют приоритет над встроенными; удаление пользовательского ID клиента возвращает значения по умолчанию, если они доступны. Каждый провайдер включается отдельно после сохранения его конфигурации.

Когда появятся предварительные сборки, скачивайте их из [GitHub Releases](https://github.com/flectar/mail/releases) и обращайте внимание на отметку **Pre-release**:

- **Linux x64:** AppImage, `.deb` для Debian/Ubuntu, `.rpm` для Fedora 43/44 или предварительный пакет Flatpak для ручной установки
- **Windows x64:** Установщик `.exe` или переносимый ZIP
- **macOS Apple silicon или Intel (macOS 14+):** DMG или ZIP приложения
- **Android arm64 (Android 8.0+):** Подписанный APK; предварительные и стабильные выпуски используют одну идентичность приложения

Предварительные сборки Windows не подписаны; сборки macOS имеют подпись ad hoc и не проходят нотарификацию, поэтому предупреждения безопасности ОС ожидаемы. Устанавливайте обновления вручную.

Предварительный Flatpak распространяется как отдельный тестовый пакет. Его пока нет во Flathub, поэтому автоматических обновлений оттуда нет. Закрытие в системный трей отключено, пока компонент трея не сможет использовать имя D-Bus, совместимое с песочницей. Автозапуск при входе выполняется через портал Background рабочего стола и открывает окно, поскольку запуститься в трее пока нельзя. Значок приложения там остаётся оранжевым, поскольку песочница не может изменить значок в меню приложений системы.

Установите скачанный пакет Linux командой `sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` или `flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak`. Запустите Flatpak командой `flatpak run com.flectar.mail`.

RPM предназначен для Fedora 43 и 44 на x86_64. Сборки используют Fedora 43 как базовый уровень совместимости; перед публикацией один и тот же RPM должен пройти проверки установки, отображения PDF, запуска в X11 и Wayland и удаления в чистых контейнерах обеих версий. Другие RPM-дистрибутивы пока не проверены. Если Discover сообщает об общем конфликте, выполните указанную выше команду `dnf install` и приложите ошибку зависимостей к отчёту.

Старый RPM `0.1.0-alpha.7` собран на Fedora 44 и не устанавливается на Fedora 43, поскольку требует glibc 2.43. Пока ожидается выпуск RPM с описанными проверками совместимости, можно использовать его пакет Flatpak.

Каждый выпуск на GitHub содержит `SHA256SUMS` и подписанные подтверждения происхождения сборки. Установив GitHub CLI, проверьте загрузку командой `gh attestation verify <download> --repo flectar/mail`.

Начиная с alpha 8, сборки Android alpha, beta, кандидаты в релиз и стабильные выпуски используют один пакет (`com.flectar.mail`) и постоянную идентичность подписи. Последующие версии могут обновлять приложение с сохранением локальных данных. Alpha 6 и alpha 7 использовали другие ключи и не могут обновиться до этой подписи.

Оба процесса выпуска Android используют окружение GitHub `android-release`. Им нужны одинаковые секреты хранилища ключей и паролей; они сверяют публичный сертификат с `platform/android/app-signing-certificate.sha256` и вычисляют коды версий Android из версии выпуска. Отсутствующий или другой ключ блокирует публикацию. **Не создавайте заменяющий ключ и не меняйте закреплённый отпечаток для обычного выпуска.** Смена пароля сохраняет идентичность, если сохранены исходные закрытый ключ и сертификат.

Команда однократной миграции, необходимые секреты, резервные копии и Play App Signing описаны в [настройке и восстановлении подписи Android](../android-signing.md).

Локальные сборки разработчика по-прежнему могут создавать локальный тестовый ключ. Они обновляют только установки, подписанные тем же локальным ключом.

Можно также собрать Flectar Mail из исходного кода с помощью [инструментов Rust](https://rustup.rs/):

```bash
cargo run --bin flectar-mail
```

OAuth на рабочем столе Linux открывает системный браузер через портал рабочего стола и сохраняет учётные данные для обновления токенов через freedesktop Secret Service. Обычной сессии нужен компонент `xdg-desktop-portal` и провайдер Secret Service, например GNOME Keyring. Если безопасное хранение недоступно, релизные сборки останавливаются до открытия страницы OAuth, чтобы авторизация не завершилась без безопасного места для сохранения данных.

Среды разработки Podman и Docker обычно не имеют доступа ни к D-Bus сессии хоста, ни к loopback-сети браузера хоста. Отладочные сборки поддерживают такой режим через явно обозначенный файл учётных данных разработки, доступный только владельцу, и раскрывающуюся панель для вставки итогового loopback URL обратного вызова. Файловое хранилище исключено из релизных сборок.

## 🌍 Помогите перевести Flectar Mail

Flectar Mail создан для всех, и мы будем рады вашей помощи с переводом на новые языки!

Для переводов сообщества мы используем [Weblate](https://translate.flectar.com/). Хотите перевести Flectar Mail на родной язык, улучшить существующий перевод или проверить строки — любой вклад приветствуется.

**[Начать перевод Flectar Mail →](https://translate.flectar.com/)**

Начать просто:

1. Создайте аккаунт на платформе переводов.
2. Выберите свой язык или начните новый перевод, если его ещё нет.
3. Переводите строки прямо в Weblate; знания программирования или опыт работы с GitHub не нужны.

Переводы синхронизируются с репозиторием GitHub и отправляются на проверку в виде запросов на слияние. Авторы переводов также могут получить указание авторства в GitHub.

Переводы README поддерживаются в этом репозитории. Чтобы добавить или обновить перевод, см. [руководство для переводчиков](../../CONTRIBUTING.md#translations).

## ♿ Доступность

Flectar Mail поддерживает управление с клавиатуры, светлую и тёмную темы и гибкие варианты интерфейса. Работа над доступностью продолжается. Текущие возможности, ограничения тестирования и порядок сообщения о препятствиях описаны в [ACCESSIBILITY.md](../../ACCESSIBILITY.md).

## 🔒 Безопасность

Для конфиденциального сообщения об уязвимости следуйте [SECURITY.md](../../SECURITY.md). Не публикуйте сведения об уязвимостях в открытых обсуждениях проблем.

## 🌐 Открытый код для всех

Flectar Mail — единое приложение с открытым кодом, без отдельной общественной редакции. Клиент лицензирован по [GNU Affero General Public License v3](../../LICENSE). Практические подробности приведены в [обзоре лицензирования](../../LICENSING.md); чтобы участвовать в развитии проекта, см. [CONTRIBUTING.md](../../CONTRIBUTING.md).

## 🫡 Благодарности

Flectar Mail существует благодаря работе этих проектов и их участников:

- [Slint](https://slint.dev/) — инструментарий нативного интерфейса, на котором построен Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz) — движок HTML/CSS на Rust от команды Dioxus, обеспечивающий отображение писем.

---
