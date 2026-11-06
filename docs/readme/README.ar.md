<div align="center" dir="rtl">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="شعار Flectar Mail">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p dir="ltr">
    <a href="../../README.md">English</a> · <a href="README.es.md">Español</a> · <a href="README.de.md">Deutsch</a> ·
    <a href="README.fr.md">Français</a> · <a href="README.pt_BR.md">Português (Brasil)</a> · <a href="README.ru.md">Русский</a> ·
    <a href="README.hi.md">हिन्दी</a> · <strong>العربية</strong> · <a href="README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>البريد يستعيد سرعته</h3>
    <p>بُني من الصفر للسرعة. يقدم Flectar Mail أداءً أصيلًا وتشغيلًا فوريًا واستهلاكًا للذاكرة الخاصة يقارب 20 MB في وضع الخمول.</p>
  </div>
  <p>
    <a href="https://flectar.com">الموقع</a> ·
    <a href="https://github.com/flectar/mail/issues">الإبلاغ عن مشكلة</a> ·
    <a href="../../CONTRIBUTING.md">المساهمة</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="الترخيص: AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="التنزيلات عبر جميع الإصدارات"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="أحدث إصدار"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="حالة الترجمة"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="اللغة: Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="المنصات: Windows وmacOS وLinux وAndroid وiOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail على macOS يعرض صندوق الوارد الموحد وواجهة الرسالة">
</picture>

يُعد Flectar Mail تطبيقًا أصيلًا وخفيفًا لبريدك الإلكتروني وتقويماتك وجهات اتصالك. صُمم ليفتح فورًا ويحافظ على سرعة الاستجابة، مع استهلاك جزء صغير من الذاكرة التي يحتاج إليها عميل بريد تقليدي مبني على تقنيات الويب.

## ❓ لماذا Flectar Mail؟

<div dir="rtl">

| الميزة | التفاصيل |
| --- | --- |
| **سريع من النقرة الأولى** | توصلك الواجهة الأصيلة والوصول إلى البيانات الذي يعطي الأولوية للتخزين المحلي إلى صندوق الوارد دون انتظار بيئة تشغيل متصفح. |
| **نحو 20 MB من الذاكرة الخاصة في وضع الخمول** | نحو 85 MB مع احتساب الشيفرة والمكتبات، وما يعادل جزءًا من اثني عشر من الذاكرة الخاصة التي يستهلكها Thunderbird مع صندوق البريد نفسه. راجع [الأداء](#-الأداء). |
| **كل شيء في مكان واحد** | انتقل بين البريد والتقويمات وجهات الاتصال دون الحاجة إلى تجميع تطبيقات منفصلة. |
| **مصمم للعمل دون اتصال** | يُخزّن صندوق البريد والتقويم محليًا، فتبقى بياناتك المتزامنة مفيدة دون اتصال بالإنترنت. |
| **يعمل مع حساباتك** | اربط Gmail وOutlook وMicrosoft 365، أو خدمات تعتمد معايير IMAP وPOP3 وSMTP وJMAP وCalDAV وCardDAV. |
| **إعدادات افتراضية تراعي الخصوصية** | تُحجب الصور البعيدة حتى تسمح بها، فلا تستطيع بكسلات التتبع الإبلاغ عن وقت قراءتك للرسالة. |
| **الأمان في البنية نفسها** | لا يُفتح محتوى البريد أبدًا داخل WebView. يعرض Flectar Mail شيفرة HTML وCSS عبر نظامه الأصيل المكتوب بلغة Rust دون تنفيذ نصوص الرسائل البرمجية، فيتجنب بذلك مساحة الهجوم الخاصة بالمتصفح المضمّن. |
| **محرّك عرض تجريبي بلغة Rust** | يُعد عرض البريد دون محرّك متصفح مجالًا جديدًا. قد لا تُعرض الرسائل المعقدة بصورة صحيحة بعد، والعمل جارٍ على تحسين التوافق. |
| **لكل الشاشات** | تقدم تخطيطات سطح المكتب الواسعة والمبسطة التجربة نفسها التي تقدمها الواجهة المدمجة الملائمة للمس. |
| **أصيل ومفتوح المصدر** | بُني من الصفر بلغة Rust. ليس متصفحًا داخل نافذة، ويُوزّع بترخيص AGPLv3. |

</div>

## ⚡ الأداء

<div dir="rtl">

| في وضع الخمول، بصندوق البريد نفسه | الذاكرة الخاصة | مع احتساب الشيفرة والمكتبات (PSS) | يعرض البريد عبر |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17.2–17.4 MiB** | **79.1–80.0 MiB** | Blitz |
| Geary 46 | 126.1–126.2 MiB | 309.6–311.4 MiB | WebKitGTK |
| Evolution 3.56 | 191.0–192.2 MiB | 395.0–395.9 MiB | WebKitGTK |
| Thunderbird 159 | 244.1–248.1 MiB | 424.4–427.1 MiB | Gecko |
| Betterbird 153 | 243.9–247.8 MiB | 411.9–419.7 MiB | Gecko |
| Mailspring 1.26 | 311.4–314.9 MiB | 551.1–551.7 MiB | Chromium |
| Aerion 0.3.6 | 421.9–437.3 MiB | 660.4–678.0 MiB | WebKitGTK |

</div>

قيست هذه الأرقام على Linux بحساب IMAP واحد (860 رسالة)، مع فتح صندوق الوارد وعرض نشرة بريدية بتنسيق HTML، بعد 60 ثانية من التشغيل، وباستخدام محرّك العرض الافتراضي القائم على CPU. ويغطي كل نطاق قياسين، وتُحتسب كل العمليات التي يشغّلها التطبيق. والذاكرة الخاصة هي القيمة التي تعرضها إدارة المهام في Windows ضمن عمود **الذاكرة**. وعلى Windows، مع 7 حسابات حقيقية ونحو 69,000 رسالة غير مقروءة، عرضت إدارة المهام 37.8 MiB والنافذة مفتوحة، و4.2 MiB بعد إغلاقها إلى علبة النظام. راجع [طريقة قياسنا وكيف تتحقق من النتائج بنفسك](../performance.md).

### مقارنة بعملاء البريد الآخرين

<div dir="rtl">

| عميل البريد | الواجهة | يعرض البريد عبر | المنصات |
| --- | --- | --- | --- |
| **Flectar Mail** | تطبيق Rust أصيل | Blitz، محرّك HTML/CSS بلغة Rust؛ دون WebView | Windows وmacOS وLinux وAndroid وiOS (بالتثبيت الجانبي) |
| Thunderbird | تطبيق سطح مكتب مبني على Gecko | Gecko | Windows وmacOS وLinux وAndroid |
| Outlook for Windows | تطبيق Windows بواجهة ويب | Microsoft Edge WebView2 | Windows |
| Spark | تطبيق Electron | Chromium | macOS وWindows وiOS وAndroid |
| Aerion | تطبيق Go بواجهة ويب مبنية بـ Wails | WebView الخاص بالنظام | Linux وmacOS وWindows |
| Gmail في علامة تبويب بالمتصفح | تطبيق ويب | Chrome أو متصفح آخر | الويب |

</div>

## ✨ الميزات

<div dir="rtl">

| الخاصية | ما تقدمه |
| --- | --- |
| [سلاسل المحادثات](../features.md#conversation-threads) | تجمع الردود بترتيب زمني، مع تنقل كامل بلوحة المفاتيح. ويمكنك إيقافها. |
| [علامات التبويب](../features.md#tabs) | علامات تبويب تشبه المتصفح وتتذكر كل منها موقعها، مع اختصارات على سطح المكتب ومبدّل لعلامات التبويب على الهواتف. |
| [الملفات الشخصية وألوان الحسابات](../features.md#profiles-and-account-colors) | تجمع الحسابات في ملفات شخصية مثل العمل أو الشخصي، وتميّز كل رسالة بلون. |
| [المظهر](../features.md#appearance) | تخطيط كامل أو مبسط، وسمة فاتحة أو داكنة، ولوحات ألوان، وصور المرسلين، وألوان أيقونة التطبيق. |
| [نافذة بلا إطار](../features.md#frameless-window) | عناصر التحكم في النافذة داخل شريط الأدوات، مع الحفاظ على سلوك كل منصة. |
| [التقويم وجهات الاتصال والملفات](../features.md#calendar-contacts-and-files) | مزامنة CalDAV وCardDAV، وملفات JMAP وWebDAV، والبحث في المرفقات، والاحتفاظ بالملفات دون اتصال ومعاينتها. |
| [التوقيعات وOpenPGP](../features.md#signatures-and-openpgp) | توقيعات مسماة، وتوقيع OpenPGP/MIME وتشفيره على سطح المكتب عبر GnuPG. |
| [التخزين والنسخ الاحتياطية](../features.md#storage-and-backups) | المساحة المستخدمة حسب النوع، ولقطات قواعد البيانات، والنسخ الاحتياطية للحسابات، وقواعد إزالة البريد من خادم POP3. |
| [استيراد الحسابات والبريد](../features.md#importing-accounts-and-mail) | من Thunderbird وK-9 Mail وmbox و‎`.eml` وmaildir وApple Mail وأرشيفات ZIP. |
| [عرض HTML](../features.md#html-rendering) | عرض شيفرة HTML في الرسائل بلغة Rust عبر Blitz، باستخدام CPU أو GPU. |

</div>

### مساحتا العمل الكاملة والمبسطة

<div dir="rtl">
<table>
  <thead>
    <tr>
      <th width="50%">مساحة العمل الكاملة</th>
      <th width="50%">مساحة العمل المبسطة</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="مساحة عمل Flectar Mail الكاملة على سطح المكتب" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="مساحة عمل Flectar Mail المبسطة على سطح المكتب" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>
</div>

### علامات التبويب والمحادثات والملفات الشخصية

<div dir="rtl">
<table>
  <thead>
    <tr>
      <th width="33.33%">علامات التبويب</th>
      <th width="33.33%">المحادثة</th>
      <th width="33.33%">ألوان الملفات الشخصية</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
          <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail بعلامات تبويب لعدة محادثات والتقويم وجهات الاتصال والملفات" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
          <img src="../../resources/screenshots/desktop-thread-light.png" alt="محادثة Flectar Mail تتضمن الرسائل الواردة والردود" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
          <img src="../../resources/screenshots/desktop-profiles-light.png" alt="صندوق الوارد الموحد في Flectar Mail بعلامات ملفات شخصية بنفسجية وبرتقالية" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>
</div>

### لوحات الألوان

<div dir="rtl">
<table>
  <thead>
    <tr>
      <th width="25%">أزرق مخضر</th>
      <th width="25%">أخضر</th>
      <th width="25%">بنفسجي</th>
      <th width="25%">مخصصة</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="لوحة Flectar Mail الزرقاء المخضرة" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="لوحة Flectar Mail الخضراء" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="لوحة Flectar Mail البنفسجية" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="لوحة ألوان Flectar Mail المخصصة" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>
</div>

### نافذة بلا إطار

<div dir="rtl">
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
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="نافذة Flectar Mail بلا إطار على macOS" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="نافذة Flectar Mail بلا إطار على Windows" width="100%">
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
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="نافذة Flectar Mail بلا إطار على GNOME" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="نافذة Flectar Mail بلا إطار على KDE Plasma" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>
</div>

### التقويم وجهات الاتصال والملفات

<div dir="rtl">
<table>
  <thead>
    <tr>
      <th width="33.33%">التقويم</th>
      <th width="33.33%">جهات الاتصال</th>
      <th width="33.33%">الملفات (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="تقويم Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="جهات اتصال Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="ملفات Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>
</div>

### للشاشات الصغيرة

<div dir="rtl">

| فاتح | داكن |
| --- | --- |
| ![صندوق الوارد المحمول في Flectar Mail بالوضع الفاتح](../../resources/screenshots/mobile-light.png) | ![صندوق الوارد المحمول في Flectar Mail بالوضع الداكن](../../resources/screenshots/mobile-dark.png) |

</div>

## 🧪 عرض HTML تجريبي

> [!WARNING]
> عرض رسائل HTML هو حاليًا الجزء الأكثر تجريبية في Flectar Mail. قد لا تُعرض بعض الرسائل بصورة صحيحة بعد، خاصة التي تتضمن ترميزًا وتنسيقات CSS معقدة أو غير معتادة.

يعرض Flectar Mail البريد باستخدام [Blitz](https://github.com/DioxusLabs/blitz)، وهو محرّك HTML/CSS بلغة Rust من فريق Dioxus، بدلًا من تضمين متصفح أو WebView. إذا وجدت رسالة لا تُعرض بصورة صحيحة، يرجى [الإبلاغ عنها](https://github.com/flectar/mail/issues). اقرأ المزيد عن [محرّك العرض وخيارَي CPU وGPU](../features.md#html-rendering).

## 📥 احصل على Flectar Mail

لا يزال Flectar Mail قيد التطوير ولم يصل إلى الاستقرار بعد. تتوفر نسخ المعاينة على [GitHub Releases](https://github.com/flectar/mail/releases).

> [!WARNING]
> **يتطلب تحقق Google OAuth تقييمًا أمنيًا سنويًا من CASA.** Flectar مشروع مجتمعي مفتوح المصدر، لذا يعتمد إجراء هذا التقييم على دعم الرعاة. لن يكون إصدار مستقر مع Gmail OAuth مهيأ افتراضيًا ممكنًا قبل تمويل التحقق المطلوب وإتمامه. إذا أردت المساعدة في توفير تسجيل الدخول المدمج إلى Gmail، يرجى [رعاية Flectar](https://github.com/sponsors/flectar).

<div dir="rtl">

| المنصة | التنزيل | ملاحظات |
| --- | --- | --- |
| Linux x64 و arm64 | AppImage، أو ‎`.deb` لـ Debian/Ubuntu، أو ‎`.rpm` لـ Fedora 43/44، أو حزمة Flatpak | يتطلب مخزن مفاتيح يدعم Secret Service. حزمة Flatpak نسخة معاينة للتثبيت اليدوي، وليست متاحة على Flathub بعد. |
| Windows x64 و arm64 | مثبّت ‎`.exe` أو ملف ZIP محمول | نسخ معاينة غير موقعة |
| macOS 14+‎ (بمعالج Apple silicon أو Intel) | ملف DMG أو ZIP للتطبيق | تحمل توقيعًا مؤقتًا ad hoc ولم تخضع للتوثيق |
| Android 8.0+ (arm64) | ملف APK موقّع | تشترك نسخ المعاينة والإصدارات المستقرة في هوية تطبيق واحدة. |
| iOS 17+‎ (iPhone وiPad) | ملف ‎`.ipa` غير موقّع | ثبّته تثبيتًا جانبيًا عبر AltStore أو SideStore أو Sideloadly باستخدام Apple ID الخاص بك. |
| HarmonyOS وOpenHarmony | [نسخة مجتمعية منقولة](../../platform/ohos/README.md) يطوّرها [springmin](https://github.com/springmin) | ليست ضمن إصدارات GitHub؛ ابنِها من المصدر. |

</div>

تُبقي النسخ المبنية دون مفاتيح تطبيق OAuth تسجيل الدخول إلى Gmail وOutlook معطلًا. استخدم ترس **إعدادات تسجيل الدخول** في شاشة الترحيب لإضافة تسجيل تطبيق Google أو Microsoft الخاص بك، أو اربط حساب IMAP أو POP3 أو JMAP.

يشرح [دليل التثبيت](../install.md) أوامر التثبيت، ومخزن المفاتيح وبوابة سطح المكتب على Linux، وملاحظات Flatpak وRPM، والتحقق من التنزيلات، وتوقيع Android، والتثبيت الجانبي على iOS، والبناء من المصدر باستخدام `cargo run --bin flectar-mail`.

## 🤝 شارك في المشروع

<div dir="rtl">

| المجال | التفاصيل |
| --- | --- |
| 🌍 **الترجمة** | ترجم التطبيق، أو حسّن ترجمة موجودة، أو راجع النصوص المترجمة عبر [Weblate](https://translate.flectar.com/)؛ لا تحتاج إلى خبرة بالبرمجة أو GitHub. تصل الترجمات إلى هذا المستودع كطلبات سحب، وتُنسب المساهمات إلى أصحابها على GitHub. تُصان ترجمات README في هذا المستودع؛ راجع [المساهمة في الترجمات](../../CONTRIBUTING.md#translations). |
| ♿ **إمكانية الوصول** | يوفر Flectar Mail التنقل بلوحة المفاتيح والسمات الفاتحة والداكنة والتخطيطات المرنة، ويستمر العمل على إمكانية الوصول. راجع [ACCESSIBILITY.md](../../ACCESSIBILITY.md) للاطلاع على الميزات الحالية وحدود الاختبار وكيفية الإبلاغ عن عائق. |
| 🔒 **الأمان** | للإبلاغ عن ثغرة بصورة خاصة، اتبع [SECURITY.md](../../SECURITY.md). يرجى عدم نشر تفاصيل الثغرات في البلاغات العامة. |
| 🌐 **مفتوح المصدر** | Flectar Mail تطبيق واحد مفتوح المصدر، ولا توجد نسخة مجتمعية منفصلة. وهو مرخّص بموجب [GNU Affero General Public License v3](../../LICENSE). اقرأ [نظرة عامة على الترخيص](../../LICENSING.md) للتفاصيل العملية. |
| 🛠️ **المساهمة** | راجع [CONTRIBUTING.md](../../CONTRIBUTING.md) للمساعدة في تطوير المشروع. |

</div>

## 🫡 شكر وتقدير

أصبح Flectar Mail ممكنًا بفضل عمل هذه المشاريع ومساهميها:

- ‏[Slint](https://slint.dev/)، أدوات الواجهة الأصيلة التي تشغّل واجهة Flectar Mail.
- ‏[Blitz](https://github.com/DioxusLabs/blitz)، محرّك HTML/CSS بلغة Rust من فريق Dioxus الذي يتيح عرض رسائل البريد.

---
