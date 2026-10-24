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
    <p>بُني من الصفر للسرعة. يقدم Flectar Mail أداءً أصيلًا وتشغيلًا فوريًا واستهلاك RAM يبدأ من 80 MB فقط.</p>
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

- **سريع من النقرة الأولى.** توصلك الواجهة الأصيلة والوصول إلى البيانات الذي يعطي الأولوية للتخزين المحلي إلى صندوق الوارد دون انتظار بيئة تشغيل متصفح.
- **ذاكرة RAM تبدأ من 80 MB فقط.** صُمم Flectar Mail خصيصًا لتقليل استهلاك الذاكرة مع إبقاء مزايا صندوق الوارد الكاملة في متناولك.
- **كل شيء في مكان واحد.** انتقل بين البريد والتقويمات وجهات الاتصال دون الحاجة إلى تجميع تطبيقات منفصلة.
- **مصمم للعمل دون اتصال.** يُخزّن صندوق البريد والتقويم محليًا، فتبقى بياناتك المتزامنة مفيدة دون اتصال بالإنترنت.
- **يعمل مع حساباتك.** اربط Gmail وOutlook وMicrosoft 365، أو خدمات تعتمد معايير IMAP وPOP3 وSMTP وJMAP وCalDAV وCardDAV.
- **إعدادات افتراضية تراعي الخصوصية.** تُحجب الصور البعيدة حتى تسمح بها، مما يساعد على منع بكسلات التتبع من الإبلاغ عن وقت قراءتك للرسالة.
- **الأمان في البنية نفسها.** لا يُفتح محتوى البريد أبدًا داخل WebView. يعرض Flectar Mail شيفرة HTML وCSS عبر نظامه الأصيل المكتوب بلغة Rust، ولا ينفذ نصوص الرسائل البرمجية، ويحجب الصور البعيدة افتراضيًا. وهكذا يتجنب مساحة الهجوم الخاصة بالمتصفح المضمّن منذ التصميم.
- **محرّك عرض تجريبي بلغة Rust.** بناء محرّك لعرض البريد دون محرّك متصفح مجال جديد. من المتوقع حدوث مشكلات في العرض، خاصة في الرسائل المعقدة، بينما يستمر تحسين التوافق.
- **لكل الشاشات.** تقدم تخطيطات سطح المكتب الواسعة والمبسطة التجربة نفسها التي تقدمها الواجهة المدمجة الملائمة للمس.
- **أصيل ومفتوح المصدر.** بُني من الصفر بلغة Rust. ليس متصفحًا داخل نافذة، ويُوزّع بترخيص AGPLv3.

## 📁 الملفات والمرفقات

تصفح تخزين JMAP/WebDAV، وابحث في مرفقات البريد، واحتفظ بالملفات دون اتصال، وعاين ملفات PDF والصور والنصوص.

## ✍🏻 توقيعات الحسابات وOpenPGP

يوفر قسم الإعدادات ← الحسابات ← التوقيعات وOpenPGP توقيعات مسماة، وإعدادات افتراضية منفصلة للرسائل الجديدة والردود، وخيارًا لتحديد التوقيع عند كتابة رسالة. يستخدم توقيع OpenPGP/MIME وتشفيره على سطح المكتب نسخة GnuPG 2.x المثبتة مع pinentry لعبارات مرور المفاتيح الخاصة. يمنع التشفير الإلزامي الإرسال عند غياب مفاتيح المستلمين أو عدم صلاحيتها؛ وتبقى المسودات المحمية محلية حتى الإرسال.

لا يتوفر حاليًا دعم S/MIME أو OpenPGP على الأجهزة المحمولة.

## 🧪 عرض HTML تجريبي

> [!WARNING]
> عرض رسائل HTML هو حاليًا الجزء الأكثر تجريبية في Flectar Mail. قد لا تُعرض بعض الرسائل بصورة صحيحة بعد، خاصة التي تتضمن ترميزًا وتنسيقات CSS معقدة أو غير معتادة.

لإبقاء العميل أصيلًا بالكامل واستهلاك الذاكرة في حدود 80 MB، يعرض Flectar Mail شيفرة HTML للبريد باستخدام [Blitz](https://github.com/DioxusLabs/blitz)، وهو محرّك HTML/CSS بلغة Rust من فريق Dioxus، بدلًا من تضمين متصفح أو WebView.

تتضمن إصدارات سطح المكتب الخيارين **CPU — Low Memory** و**GPU — WGPU** ضمن الإعدادات ← عام ← محرّك العرض. يُختار CPU في البداية ولا تجري تهيئة WGPU. يستخدم خيار GPU مكتبتَي Slint وVello على جهاز WGPU 30 مشترك؛ ويسري تغيير الإعداد بعد إعادة تشغيل التطبيق. إذا فشل التشغيل باستخدام GPU، يعود التطبيق إلى CPU تلقائيًا.

بحسب علمنا، يُعد Flectar Mail من أوائل المشاريع التي تستخدم Blitz لعرض HTML من رسائل بريد فعلية ومتنوعة. تحتوي الرسائل على كثير من HTML وCSS غير المعتاد، ما يضع متطلبات صعبة أمام المحرّك. نحتفظ حاليًا بعدة تعديلات على Blitz، ونأمل إدخال أكبر قدر ممكن منها إلى المشروع الأصلي مع الوقت.

إذا وجدت رسالة لا تُعرض بصورة صحيحة، يرجى [الإبلاغ عنها](https://github.com/flectar/mail/issues). ما زال هذا الأسلوب تجريبيًا، لكنه أيضًا سبب رئيسي لخفة Flectar Mail مقارنة بعملاء البريد المعتمدين على WebView والمبنيين بأطر مثل Tauri أو Wails.

## 🎨 حسب ذوقك

اختر مساحة العمل التي تناسب طريقة تعاملك مع البريد. احتفظ بالتخطيط المفصل ذي اللوحات الثلاث، أو انتقل إلى عرض مبسط، واختر سمة فاتحة أو داكنة، وحدد لوحة ألوان مدمجة أو أنشئ لوحتك الخاصة، وأظهر صور المرسلين أو أخفها. يمكنك أيضًا اختيار لون آخر لأيقونة التطبيق: في شريط المهام أو Dock، وفي علبة النظام، وفي مشغّل تطبيقات Linux، وعلى الشاشة الرئيسية في Android وiOS.

يمكن أن يتبع مظهر الرسائل سمة التطبيق أو يظل فاتحًا أو داكنًا بصورة مستقلة. تُستخدم لوحة الألوان المختارة مع HTML والنص العادي ووضع القراءة. لعرض ألوان المرسل الأصلية مؤقتًا، استخدم أيقونة التباين بين البحث ووضع القراءة في شريط أدوات الرسالة على سطح المكتب، أو اختر **إظهار الألوان الأصلية** من قائمة النقاط الثلاث للرسالة على أي حجم شاشة.

تُبقي مساحة العمل الكاملة المجلدات وقائمة الرسائل والبريد المحدد ظاهرة معًا. يقلل التخطيط المبسط التشتيت البصري ويمنح كل جزء من صندوق الوارد مساحة أكبر عند الحاجة.

### 👥 الملفات الشخصية وألوان الحسابات

اجمع الحسابات المرتبطة ضمن ملفات شخصية مسماة مثل العمل أو الشخصي، وامنح كل ملف لونًا خاصًا. أنشئ ملفًا من قائمة الملفات الشخصية في بداية شريط علامات التبويب، أو من الإعدادات ← الحسابات ← الملفات الشخصية، وحدد الحسابات التابعة له. عند اختيار ملف من القائمة، يتركز عرض البريد على حساباته: يعرض صندوق الوارد الموحد والبحث والعدادات والشريط الجانبي بريدها فقط، بينما تبقى علامات التبويب كما هي. يعيد خيار **كل الحسابات** إظهار الجميع.

يمكنك أيضًا تخصيص لون حساب بعينه وعرض اللون الفعلي على الحافة اليسرى لكل رسالة. تساعد العلامات على تمييز الحسابات أثناء العمل في صندوق الوارد الموحد؛ ويستخدم المثال أدناه البنفسجي للعمل والبرتقالي للدعم.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
  <img src="../../resources/screenshots/desktop-profiles-light.png" alt="صندوق الوارد الموحد في Flectar Mail بعلامات ملفات شخصية بنفسجية وبرتقالية">
</picture>

### 🧵 سلاسل المحادثات

تبقى الردود مجمعة بترتيب زمني، مع توسيع الرسالة النشطة داخل جزء القراءة.

استخدم **أعلى/أسفل** للتنقل بين الرسائل، و**Home/End** للانتقال إلى أول رسالة محملة أو آخرها، و**Page Up/Page Down** للتحرك صفحة في كل مرة. يفتح **Enter** الرسالة الحالية وينقل التركيز إلى محتواها؛ ويحدد **Space** الرسالة أو يلغي تحديدها للإجراءات الجماعية. ينقل **Tab/Shift+Tab** التركيز بين عناصر التحكم. في مساحة العمل الكاملة، ينقل **F6** التركيز بين القائمة والقارئ. في التخطيط المبسط، تبرز مفاتيح الأسهم الرسائل ويفتحها **Enter**؛ ويعيد **Escape** أو **F6** إلى القائمة.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
  <img src="../../resources/screenshots/desktop-thread-light.png" alt="محادثة Flectar Mail تتضمن الرسائل الواردة والردود">
</picture>

### 🗂️ علامات التبويب

أبقِ عدة أماكن مفتوحة في وقت واحد باستخدام علامات تبويب تشبه المتصفح. تتذكر كل علامة موقعها: صندوق بريد، أو بحث، أو محادثة مفتوحة، أو التقويم، أو جهات الاتصال، أو الملفات، أو الإعدادات، إضافة إلى موضع التمرير والرسالة التي كنت تقرؤها. انقر بزر الفأرة الأوسط أو مع Ctrl/⌘ لفتح رسالة في علامة خلفية (أضف Shift للانتقال إليها)، أو اختر **فتح في علامة تبويب جديدة** من قائمتها. اسحب العلامات لإعادة ترتيبها، وأغلقها بزر الفأرة الأوسط، واستخدم الزر الأيمن لإغلاق البقية أو العلامات التي على يمينها. استأنف من حيث توقفت عند فتح Flectar Mail في المرة التالية.

يفتح **Ctrl/⌘+T** علامة جديدة، ويغلق **Ctrl/⌘+W** الحالية، ويعيد **Ctrl/⌘+Shift+T** فتح آخر علامة أغلقتها. يتنقل **Ctrl+Tab** و**Ctrl+Shift+Tab** بين العلامات، وينتقل **Ctrl/⌘+1–8** إلى علامة محددة، و**Ctrl/⌘+9** إلى الأخيرة.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
  <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail بعلامات تبويب لعدة محادثات والتقويم وجهات الاتصال والملفات">
</picture>

### مساحتا العمل الكاملة والمبسطة

<table dir="rtl">
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

### لوحات الألوان

<table dir="rtl">
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

### 🪟 نافذة بلا إطار

فعّل **نافذة بلا إطار** في الإعدادات ← عام ← الواجهة لاستبدال شريط عنوان النظام بعناصر التحكم في النافذة داخل شريط الأدوات. تحافظ كل منصة على سلوكها: يحتفظ macOS بأزراره الملونة الأصلية، وWindows بميزات Snap Layouts وقائمة النظام، ويتبع Linux ترتيب الأزرار وإجراءات شريط العنوان المضبوطة في GNOME أو KDE.

<table dir="rtl">
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

### 📅 التقويم وجهات الاتصال والملفات

<table dir="rtl">
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

### 💾 التخزين والنسخ الاحتياطية

يعرض قسم الإعدادات ← التخزين المساحة التي تستخدمها الرسائل والمرفقات والملفات غير المتصلة وقواعد البيانات على الجهاز. من الصفحة نفسها يمكنك تصدير لقطات قواعد بيانات تم التحقق منها أو نقل إعدادات الحسابات المتصلة والتفضيلات عبر نسخة احتياطية. تبقى كلمات المرور ورموز OAuth في مخزن مفاتيح النظام، لذلك يطلب الجهاز المستعاد تسجيل الدخول مجددًا.

تنزّل حسابات POP3 البريد إلى هذا الجهاز وتتركه على الخادم ما لم تختر خلاف ذلك: يمكن لكل حساب إزالة البريد من الخادم بعد 7 أو 14 أو 30 يومًا، أو عند حذفه هنا، أو فور تنزيله، ولكن ليس قبل تخزينه على الجهاز. توجد مجلداتها على هذا الجهاز فقط، لذلك تشمل اللقطات هذا البريد. وعند إزالة حساب POP3 أو حذف كل البيانات، يُعرض أولًا تصدير البريد كملفات mbox، ملف لكل مجلد، يمكن لتطبيقات بريد أخرى استيرادها.

تجلب وظيفة الاستيراد نفسها الحسابات من Thunderbird (مجلد ملف شخصي، أو أرشيف أدوات › تصدير، أو `prefs.js`) ومن Thunderbird لأندرويد أو K-9 Mail (تصدير إعدادات `.k9s`). تراجع كل حساب قبل إضافة أي شيء: تُشرح الإعدادات التي لا يستطيع Flectar Mail تمثيلها، مثل خادم Exchange، بدلًا من تغييرها بصمت، وتحتفظ حسابات POP3 بقاعدة إزالة البريد من الخادم. لا تُعدّل الحسابات الموجودة أبدًا ولا تُستورد كلمات المرور.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-storage-light.png">
  <img src="../../resources/screenshots/desktop-storage-light.png" alt="إعدادات تخزين Flectar Mail مع الاستخدام المحلي ولقطات قواعد البيانات وخيارات النسخ الاحتياطي للحسابات">
</picture>

### 📱 للشاشات الصغيرة

تُبقي الواجهة المدمجة الإجراءات المهمة في متناول اليد، وتمنح الرسائل والأحداث وجهات الاتصال الشاشة كاملة عند الحاجة.

| فاتح | داكن |
| --- | --- |
| ![صندوق الوارد المحمول في Flectar Mail بالوضع الفاتح](../../resources/screenshots/mobile-light.png) | ![صندوق الوارد المحمول في Flectar Mail بالوضع الداكن](../../resources/screenshots/mobile-dark.png) |

## 📥 احصل على Flectar Mail

لا يزال Flectar Mail قيد التطوير ولم يصل إلى الاستقرار بعد.

> [!WARNING]
> **يتطلب تحقق Google OAuth تقييمًا أمنيًا سنويًا من CASA.** Flectar مشروع مجتمعي مفتوح المصدر، لذا يعتمد إجراء هذا التقييم على دعم الرعاة. لن يكون إصدار مستقر مع Gmail OAuth مهيأ افتراضيًا ممكنًا قبل تمويل التحقق المطلوب وإتمامه. إذا أردت المساعدة في توفير تسجيل الدخول المدمج إلى Gmail، يرجى [رعاية Flectar](https://github.com/sponsors/flectar).
>
> لا يزال تحقق OAuth لدى Google وMicrosoft جاريًا. الإصدارات التمهيدية السابقة على GitHub مخصصة للمختبرين الذين يستخدمون تسجيلات OAuth الخاصة بهم أو حسابات IMAP أو POP3 أو JMAP.

تُبقي النسخ المبنية دون مفاتيح تطبيق OAuth تسجيل الدخول إلى Gmail وOutlook معطلًا. استخدم ترس **إعدادات تسجيل الدخول** في شاشة الترحيب لحفظ تسجيل تطبيق Google أو Microsoft الخاص بك، أو اربط حساب IMAP أو POP3 أو JMAP. تُشارك المفاتيح المخصصة مع الإعدادات وتكون لها الأولوية على المفاتيح المدمجة؛ ويعيد مسح معرّف العميل المخصص القيم الافتراضية إن توفرت. يُفعّل كل مزوّد على حدة بعد حفظ تهيئته.

عند نشر نسخ المعاينة، نزّلها من [GitHub Releases](https://github.com/flectar/mail/releases) وابحث عن شارة **Pre-release**:

- **Linux x64:** ‏AppImage، أو `.deb` لـ Debian/Ubuntu، أو `.rpm` لـ Fedora 43/44، أو حزمة معاينة Flatpak للتثبيت اليدوي
- **Windows x64:** مثبّت `.exe` أو ملف ZIP محمول
- **macOS بمعالج Apple silicon أو Intel ‏(macOS 14+):** ملف DMG أو ZIP للتطبيق
- **Android arm64 ‏(Android 8.0+):** ملف APK موقّع؛ تشترك نسخ المعاينة والإصدارات المستقرة في هوية تطبيق واحدة

نسخ معاينة Windows غير موقعة؛ ونسخ macOS تحمل توقيعًا مؤقتًا ad hoc ولم تخضع للتوثيق، لذا فمن المتوقع ظهور تنبيهات أمان من نظام التشغيل. ثبّت التحديثات يدويًا.

تتوفر معاينة Flatpak كحزمة اختبار مستقلة. ليست حزمة Flathub بعد، ولذلك لا تتلقى تحديثاته التلقائية. خيار الإغلاق إلى علبة النظام معطل حتى يستطيع مكوّن العلبة استخدام اسم D-Bus متوافق مع البيئة المعزولة. يمر التشغيل التلقائي عند تسجيل الدخول عبر بوابة Background لسطح المكتب ويفتح النافذة، لعدم وجود علبة نظام يمكن البدء فيها. تبقى أيقونة التطبيق برتقالية هناك، لأن البيئة المعزولة لا تستطيع تغيير أيقونة المشغّل في النظام.

ثبّت حزمة Linux المنزّلة باستخدام `sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` أو `flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak`. شغّل نسخة Flatpak باستخدام `flatpak run com.flectar.mail`.

تستهدف حزمة RPM نسختَي Fedora 43 و44 على x86_64. تعتمد نسخ النشر Fedora 43 أساسًا للتوافق؛ ويجب أن تجتاز حزمة RPM نفسها اختبارات التثبيت وعرض PDF والتشغيل عبر X11 وWayland والإزالة في حاويات نظيفة لكلا الإصدارين قبل النشر. لم تُختبر توزيعات RPM الأخرى بعد. إذا أبلغ Discover عن تعارض عام، شغّل الأمر `dnf install` أعلاه وأرفق خطأ الاعتماديات في تقرير المشكلة.

بُنيت حزمة RPM القديمة `0.1.0-alpha.7` على Fedora 44، ولا يمكن تثبيتها على Fedora 43 لأنها تتطلب glibc 2.43. يمكن استخدام حزمة Flatpak التابعة لها بديلًا إلى حين صدور RPM مبني باختبارات التوافق المذكورة.

يتضمن كل إصدار على GitHub ملف `SHA256SUMS` وإفادات موقعة بمصدر البناء. بعد تثبيت GitHub CLI، تحقق من التنزيل باستخدام `gh attestation verify <download> --repo flectar/mail`.

بدءًا من alpha 8، تستخدم نسخ Android من مراحل alpha وbeta والنسخ المرشحة والإصدارات المستقرة حزمة واحدة (`com.flectar.mail`) وهوية توقيع دائمة. تستطيع الإصدارات اللاحقة تحديث التطبيق مع الاحتفاظ بالبيانات المحلية. استخدمت alpha 6 وalpha 7 مفاتيح مختلفة، ولا يمكن تحديثهما إلى هوية التوقيع هذه.

يستخدم مسارا نشر Android بيئة GitHub المسماة `android-release`. ويحتاجان إلى الأسرار نفسها لمخزن المفاتيح وكلمات المرور، ويتحققان من الشهادة العامة مقابل `platform/android/app-signing-certificate.sha256`، ويستخرجان رموز إصدار Android من رقم الإصدار المنشور. يمنع المفتاح المفقود أو المختلف النشر. **لا تنشئ مفتاحًا بديلًا ولا تغيّر البصمة المثبتة لإصدار اعتيادي.** يحافظ تغيير كلمة المرور على الهوية ما دام المفتاح الخاص والشهادة الأصليان محفوظين.

راجع [إعداد توقيع Android واستعادته](../android-signing.md) لأمر الترحيل الذي يُنفذ مرة واحدة، والأسرار المطلوبة، والنسخ الاحتياطية، وPlay App Signing.

ما زال بإمكان نسخ التطوير المحلية إنشاء مفتاح اختبار محلي. وهي لا تحدّث إلا التثبيتات الموقعة بالمفتاح المحلي نفسه.

يمكنك أيضًا بناء Flectar Mail من المصدر باستخدام [أدوات Rust](https://rustup.rs/):

<div dir="ltr">

```bash
cargo run --bin flectar-mail
```

</div>

يستخدم OAuth على سطح مكتب Linux متصفح النظام عبر بوابة سطح المكتب، ويخزن بيانات اعتماد التجديد عبر freedesktop Secret Service. لذلك تحتاج الجلسة العادية إلى مكوّن `xdg-desktop-portal` ومزوّد Secret Service مثل GNOME Keyring. تتوقف نسخ النشر قبل فتح صفحة OAuth عند تعذر التخزين الآمن لبيانات الاعتماد، فلا يكتمل التفويض دون مكان آمن لحفظه.

غالبًا لا تتصل بيئات تطوير Podman وDocker بناقل D-Bus لجلسة المضيف أو بشبكة loopback الخاصة بمتصفحه. تدعم نسخ التصحيح هذه الحالة بملف بيانات اعتماد للتطوير واضح التسمية ومتاح للمالك وحده، ولوحة قابلة للتوسيع للصق رابط استدعاء loopback النهائي. يُستبعد التخزين القائم على الملفات من نسخ النشر.

## 🌍 ساعد في ترجمة Flectar Mail

صُمم Flectar Mail للجميع، ويسعدنا دعمك لإتاحته بلغات أكثر!

نستخدم [Weblate](https://translate.flectar.com/) لإدارة ترجمات المجتمع. سواء أردت ترجمة Flectar Mail إلى لغتك الأم، أو تحسين ترجمة موجودة، أو مراجعة النصوص المترجمة، فكل مساهمة مرحب بها.

**[ابدأ ترجمة Flectar Mail ←](https://translate.flectar.com/)**

البدء سهل:

1. أنشئ حسابًا على منصة الترجمة.
2. اختر لغتك، أو ابدأ ترجمة جديدة إن لم تكن متاحة بعد.
3. ترجم النصوص مباشرة عبر Weblate؛ لا تحتاج إلى خبرة بالبرمجة أو GitHub.

تُزامن الترجمات مع مستودع GitHub وتُرسل كطلبات سحب للمراجعة. ويمكن أيضًا نسب المساهمات إلى أصحابها على GitHub.

تُصان ترجمات README في هذا المستودع. راجع [المساهمة في الترجمات](../../CONTRIBUTING.md#translations) لإضافة ترجمة أو تحديثها.

## ♿ إمكانية الوصول

يوفر Flectar Mail التنقل بلوحة المفاتيح والسمات الفاتحة والداكنة والتخطيطات المرنة. يستمر العمل على إمكانية الوصول. راجع [ACCESSIBILITY.md](../../ACCESSIBILITY.md) للاطلاع على الميزات الحالية وحدود الاختبار وكيفية الإبلاغ عن عائق.

## 🔒 الأمان

للإبلاغ عن ثغرة بصورة خاصة، اتبع [SECURITY.md](../../SECURITY.md). يرجى عدم نشر تفاصيل الثغرات في البلاغات العامة.

## 🌐 مفتوح المصدر للجميع

Flectar Mail تطبيق واحد مفتوح المصدر، ولا توجد نسخة مجتمعية منفصلة. العميل مرخّص بموجب [GNU Affero General Public License v3](../../LICENSE). اقرأ [نظرة عامة على الترخيص](../../LICENSING.md) للتفاصيل العملية، أو راجع [CONTRIBUTING.md](../../CONTRIBUTING.md) للمساعدة في تطوير المشروع.

## 🫡 شكر وتقدير

أصبح Flectar Mail ممكنًا بفضل عمل هذه المشاريع ومساهميها:

- [Slint](https://slint.dev/)، أدوات الواجهة الأصيلة التي تشغّل واجهة Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz)، محرّك HTML/CSS بلغة Rust من فريق Dioxus الذي يتيح عرض رسائل البريد.

---
