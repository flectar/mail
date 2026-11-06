<div align="center">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="Flectar Mail 徽标">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <a href="../../README.md">English</a> · <a href="README.es.md">Español</a> · <a href="README.de.md">Deutsch</a> ·
    <a href="README.fr.md">Français</a> · <a href="README.pt_BR.md">Português (Brasil)</a> · <a href="README.ru.md">Русский</a> ·
    <a href="README.hi.md">हिन्दी</a> · <a href="README.ar.md">العربية</a> · <strong>简体中文</strong>
  </p>
  <div align="center">
    <h3>邮件，重归迅捷</h3>
    <p>从底层开始，为速度而构建。Flectar Mail 提供原生性能、瞬时启动，空闲时私有内存占用约 20 MB。</p>
  </div>
  <p>
    <a href="https://flectar.com">官网</a> ·
    <a href="https://github.com/flectar/mail/issues">报告问题</a> ·
    <a href="../../CONTRIBUTING.md">参与贡献</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="许可证：AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="各版本累计下载量"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="最新版本"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="翻译进度"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="开发语言：Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="支持平台：Windows、macOS、Linux、Android、iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="macOS 上的 Flectar Mail 统一收件箱与邮件视图">
</picture>

Flectar Mail 是一个轻量、原生的客户端，用来承载你的邮件、日历和联系人。它经过专门设计，能够瞬时打开、始终保持流畅，内存占用仅为常见网页版邮件客户端的一小部分。

## ❓ 为什么选择 Flectar Mail？

| 优势 | 说明 |
| --- | --- |
| **从第一次点击起就快** | 原生界面与本地优先的数据通路，让你无需等待浏览器运行时即可进入收件箱。 |
| **空闲时私有内存约 20 MB** | 计入代码和库约为 85 MB；处理同一邮箱时，私有内存约为 Thunderbird 的十二分之一。详见[性能](#-性能)。 |
| **所有内容集中一处** | 在邮件、日历和联系人之间切换，无需拼凑多个相互独立的应用。 |
| **生来支持离线** | 邮箱和日历都存储在本地，因此已同步的数据在没有网络连接时依然可用。 |
| **兼容你的各类账户** | 可连接 Gmail、Outlook 和 Microsoft 365，也可连接基于标准的 IMAP、POP3、SMTP、JMAP、CalDAV 与 CardDAV 服务。 |
| **注重隐私的默认设置** | 远程图片在你允许之前会被拦截，因此跟踪像素无法上报你阅读邮件的时间。 |
| **以架构保障安全** | 邮件内容从不在 WebView 中打开。邮件的 HTML 与 CSS 由 Rust 原生管线渲染，不执行邮件中的脚本，由此规避了内嵌浏览器的攻击面。 |
| **实验性的 Rust 渲染器** | 不借助浏览器引擎来渲染邮件，是一片全新的领域。兼容性仍在持续改进，复杂邮件目前可能无法正确渲染。 |
| **适配各种屏幕** | 宽裕的桌面布局和极简桌面布局，与触控友好的紧凑界面提供一致的体验。 |
| **原生且开源** | 完全以 Rust 从底层构建，不是套在窗口里的浏览器，并按 AGPLv3 发布。 |

## ⚡ 性能

| 空闲时（同一邮箱） | 私有内存 | 含代码与库（PSS） | 邮件渲染方式 |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17.2–17.4 MiB** | **79.1–80.0 MiB** | Blitz |
| Geary 46 | 126.1–126.2 MiB | 309.6–311.4 MiB | WebKitGTK |
| Evolution 3.56 | 191.0–192.2 MiB | 395.0–395.9 MiB | WebKitGTK |
| Thunderbird 159 | 244.1–248.1 MiB | 424.4–427.1 MiB | Gecko |
| Betterbird 153 | 243.9–247.8 MiB | 411.9–419.7 MiB | Gecko |
| Mailspring 1.26 | 311.4–314.9 MiB | 551.1–551.7 MiB | Chromium |
| Aerion 0.3.6 | 421.9–437.3 MiB | 660.4–678.0 MiB | WebKitGTK |

以上数据在 Linux 上测得：一个 IMAP 账户（860 封邮件），打开收件箱并显示一封 HTML 新闻简报，采用默认的 CPU 渲染器，在启动 60 秒后采样。每个范围涵盖两次测量，每个应用启动的全部进程均计算在内。私有内存即 Windows 任务管理器中 **内存** 一栏显示的数值。在 Windows 上，使用 7 个真实账户、约 69,000 封未读邮件时，任务管理器在窗口打开时显示 37.8 MiB，关闭到系统托盘后为 4.2 MiB。详见[我们的测量方法及自行验证方式](../performance.md)。

### 与其他客户端对比

| 邮件客户端 | 界面 | 邮件渲染方式 | 平台 |
| --- | --- | --- | --- |
| **Flectar Mail** | 原生 Rust 应用 | Blitz（Rust HTML/CSS 渲染器），不使用 WebView | Windows、macOS、Linux、Android、iOS（侧载） |
| Thunderbird | 基于 Gecko 的桌面应用 | Gecko | Windows、macOS、Linux、Android |
| Outlook for Windows | 带网页界面的 Windows 应用 | Microsoft Edge WebView2 | Windows |
| Spark | Electron 应用 | Chromium | macOS、Windows、iOS、Android |
| Aerion | 采用 Wails 网页界面的 Go 应用 | 系统 WebView | Linux、macOS、Windows |
| 浏览器标签页中的 Gmail | 网页应用 | Chrome 或其他浏览器 | 网页端 |

## ✨ 功能

| 功能 | 说明 |
| --- | --- |
| [会话线程](../features.md#conversation-threads) | 按时间顺序归组显示回复，并支持完整的键盘导航。此功能可以关闭。 |
| [标签页](../features.md#tabs) | 浏览器式标签页，会记住各自的位置；桌面端支持快捷键，手机上提供标签页切换器。 |
| [分组与账户颜色](../features.md#profiles-and-account-colors) | 把账户归入「工作」「个人」等分组，并为每封邮件标上颜色。 |
| [外观](../features.md#appearance) | 完整或极简布局、浅色或深色主题、配色、发件人头像以及应用图标颜色。 |
| [无边框窗口](../features.md#frameless-window) | 窗口控制按钮位于工具栏中，并保留各平台的原生行为。 |
| [日历、联系人与文件](../features.md#calendar-contacts-and-files) | CalDAV 与 CardDAV 同步、JMAP 与 WebDAV 文件、附件搜索、离线文件与文件预览。 |
| [签名与 OpenPGP](../features.md#signatures-and-openpgp) | 具名签名，以及桌面端通过 GnuPG 实现的 OpenPGP/MIME 签名与加密。 |
| [存储与备份](../features.md#storage-and-backups) | 按类型显示的空间占用、数据库快照、账户备份以及 POP3 服务器保留规则。 |
| [导入账户与邮件](../features.md#importing-accounts-and-mail) | 支持 Thunderbird、K-9 Mail、mbox、`.eml`、maildir、Apple Mail 以及 ZIP 归档。 |
| [HTML 渲染](../features.md#html-rendering) | 使用以 Rust 编写的 Blitz 渲染邮件 HTML，可在 CPU 或 GPU 上运行。 |

### 完整与极简工作区

<table>
  <thead>
    <tr>
      <th width="50%">完整工作区</th>
      <th width="50%">极简工作区</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail 完整桌面工作区" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="Flectar Mail 极简桌面工作区" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 标签页、会话与分组

<table>
  <thead>
    <tr>
      <th width="33.33%">标签页</th>
      <th width="33.33%">会话</th>
      <th width="33.33%">分组颜色</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
          <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail 打开了多个会话、日历、联系人与文件标签页" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
          <img src="../../resources/screenshots/desktop-thread-light.png" alt="Flectar Mail 会话，包含收到的邮件与回复" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
          <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Flectar Mail 统一收件箱，带有紫色与橙色分组色标" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 配色

<table>
  <thead>
    <tr>
      <th width="25%">青绿</th>
      <th width="25%">绿色</th>
      <th width="25%">紫色</th>
      <th width="25%">自定义</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="Flectar Mail 青绿配色" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="Flectar Mail 绿色配色" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="Flectar Mail 紫色配色" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail 自定义配色" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 无边框窗口

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
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="macOS 上的 Flectar Mail 无边框窗口" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="Windows 上的 Flectar Mail 无边框窗口" width="100%">
        </picture>
      </td>
    </tr>
    <tr>
      <th width="50%">Linux（GNOME）</th>
      <th width="50%">Linux（KDE）</th>
    </tr>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-gnome-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-gnome-light.png">
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="GNOME 上的 Flectar Mail 无边框窗口" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="KDE Plasma 上的 Flectar Mail 无边框窗口" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 日历、联系人与文件

<table>
  <thead>
    <tr>
      <th width="33.33%">日历</th>
      <th width="33.33%">联系人</th>
      <th width="33.33%">文件（WebDAV/JMAP）</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="Flectar Mail 日历" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="Flectar Mail 联系人" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="Flectar Mail 文件" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 为小屏幕而设计

| 浅色 | 深色 |
| --- | --- |
| ![Flectar Mail 浅色模式下的移动端收件箱](../../resources/screenshots/mobile-light.png) | ![Flectar Mail 深色模式下的移动端收件箱](../../resources/screenshots/mobile-dark.png) |

## 🧪 实验性的 HTML 渲染

> [!WARNING]
> HTML 邮件渲染目前是 Flectar Mail 中最具实验性的部分。部分邮件，尤其是标记与 CSS 复杂或特殊的邮件，可能仍无法正确渲染。

Flectar Mail 使用 [Blitz](https://github.com/DioxusLabs/blitz)——Dioxus 团队开发的 Rust HTML/CSS 渲染器——来渲染邮件，而不是内嵌浏览器或 WebView。如果某封邮件渲染有误，请[提交问题](https://github.com/flectar/mail/issues)。详细了解[渲染器以及 CPU 与 GPU 选项](../features.md#html-rendering)。

## 📥 获取 Flectar Mail

Flectar Mail 目前仍在开发中，尚未稳定。预览构建发布在 [GitHub Releases](https://github.com/flectar/mail/releases) 上。

> [!WARNING]
> **Google OAuth 验证需要每年进行一次 CASA 安全评估。** Flectar 是社区开源项目，因此能否完成该项评估取决于赞助支持。在所需的验证获得资金并完成之前，无法推出默认配置 Gmail OAuth 的稳定版本。如果你希望帮助实现内置的 Gmail 登录，请[赞助 Flectar](https://github.com/sponsors/flectar)。

| 平台 | 下载 | 说明 |
| --- | --- | --- |
| Linux x64 和 arm64 | AppImage、Debian/Ubuntu `.deb`、Fedora 43/44 `.rpm` 或 Flatpak 包 | 需要支持 Secret Service 的密钥环。Flatpak 为侧载预览版，尚未上架 Flathub。 |
| Windows x64 和 arm64 | 安装程序 `.exe` 或便携版 ZIP | 预览版未签名 |
| macOS 14+（Apple 芯片或 Intel） | DMG 或应用程序 ZIP | 临时签名（ad-hoc），未经公证 |
| Android 8.0+（arm64） | 已签名的 APK | 预览版与稳定版共用同一个应用身份。 |
| iOS 17+（iPhone 与 iPad） | 未签名的 `.ipa` | 使用 AltStore、SideStore 或 Sideloadly 以你的 Apple ID 侧载安装。 |
| HarmonyOS 与 OpenHarmony | 由 [springmin](https://github.com/springmin) 维护的[社区移植版](../../platform/ohos/README.md) | 不包含在 GitHub 发布版本中，需从源码构建。 |

未内置 OAuth 应用密钥的构建版本会禁用 Gmail 和 Outlook 登录。可使用欢迎界面上的**登录设置**齿轮，添加你自己的 Google 或 Microsoft 应用注册信息，或连接 IMAP、POP3 或 JMAP 账户。

[安装指南](../install.md)介绍了安装命令、Linux 密钥环与桌面门户、Flatpak 与 RPM 注意事项、下载校验、Android 签名、iOS 侧载，以及如何使用 `cargo run --bin flectar-mail` 从源码构建。

## 🤝 参与项目

| 主题 | 说明 |
| --- | --- |
| 🌍 **翻译** | 在 [Weblate](https://translate.flectar.com/) 上翻译应用、改进现有译文或参与审校，无需编程或 GitHub 使用经验。译文会以拉取请求的形式提交到本仓库，贡献者可在 GitHub 上获得署名。README 译文在本仓库中维护，详见[翻译贡献指南](../../CONTRIBUTING.md#translations)。 |
| ♿ **无障碍** | 提供键盘导航、浅色和深色主题以及灵活的布局，无障碍支持仍在持续改进。有关现有功能、测试局限以及如何报告使用障碍，请参阅 [ACCESSIBILITY.md](../../ACCESSIBILITY.md)。 |
| 🔒 **安全** | 如需私下报告安全漏洞，请遵循 [SECURITY.md](../../SECURITY.md)。请勿在公开的问题报告中披露漏洞细节。 |
| 🌐 **开源** | Flectar Mail 只有一个开源版本，没有单独划分的社区版，采用 [GNU Affero 通用公共许可证 v3](../../LICENSE) 授权。实际细节请阅读[许可说明](../../LICENSING.md)。 |
| 🛠️ **参与贡献** | 如需参与项目建设，请参阅 [CONTRIBUTING.md](../../CONTRIBUTING.md)。 |

## 🫡 致谢

Flectar Mail 的实现离不开以下项目及其贡献者的工作：

- [Slint](https://slint.dev/)，驱动 Flectar Mail 界面的原生 UI 工具包。
- [Blitz](https://github.com/DioxusLabs/blitz)，Dioxus 团队开发的 Rust HTML/CSS 渲染器，支撑邮件阅读体验。

---
