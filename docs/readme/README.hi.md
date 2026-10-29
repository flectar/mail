<div align="center">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="Flectar Mail का लोगो">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <a href="../../README.md">English</a> · <a href="README.es.md">Español</a> · <a href="README.de.md">Deutsch</a> ·
    <a href="README.fr.md">Français</a> · <a href="README.pt_BR.md">Português (Brasil)</a> · <a href="README.ru.md">Русский</a> ·
    <strong>हिन्दी</strong> · <a href="README.ar.md">العربية</a> · <a href="README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>ईमेल फिर से तेज़</h3>
    <p>शुरू से गति के लिए बना। Flectar Mail नेटिव प्रदर्शन, तुरंत शुरुआत और सिर्फ़ 80 MB से RAM उपयोग देता है।</p>
  </div>
  <p>
    <a href="https://flectar.com">वेबसाइट</a> ·
    <a href="https://github.com/flectar/mail/issues">समस्या की रिपोर्ट करें</a> ·
    <a href="../../CONTRIBUTING.md">योगदान दें</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="लाइसेंस: AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="सभी रिलीज़ के डाउनलोड"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="नवीनतम रिलीज़"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="अनुवाद की स्थिति"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="भाषा: Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="प्लेटफ़ॉर्म: Windows, macOS, Linux, Android, iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="macOS पर संयुक्त इनबॉक्स और संदेश दृश्य दिखाता Flectar Mail">
</picture>

Flectar Mail आपके ईमेल, कैलेंडर और संपर्कों के लिए एक हल्का, नेटिव ऐप है। इसे तुरंत खुलने, तेज़ी से प्रतिक्रिया देने और आम वेब-आधारित मेल क्लाइंट की तुलना में बहुत कम मेमोरी इस्तेमाल करने के लिए बनाया गया है।

## ❓ Flectar Mail क्यों?

- **पहले क्लिक से ही तेज़।** नेटिव इंटरफ़ेस और स्थानीय डेटा को प्राथमिकता देने वाला तरीका आपको ब्राउज़र रनटाइम का इंतज़ार किए बिना इनबॉक्स तक पहुँचाते हैं।
- **सिर्फ़ 80 MB RAM से शुरुआत।** Flectar Mail को जानबूझकर कम मेमोरी इस्तेमाल करने के लिए बनाया गया है, जबकि इनबॉक्स की सभी सुविधाएँ आपके पास रहती हैं।
- **सब कुछ एक जगह।** अलग-अलग ऐप जोड़ने की ज़रूरत के बिना ईमेल, कैलेंडर और संपर्कों के बीच जाएँ।
- **ऑफ़लाइन काम करने के लिए बना।** आपका मेलबॉक्स और कैलेंडर स्थानीय रूप से सहेजे जाते हैं, इसलिए सिंक किया गया डेटा बिना कनेक्शन के भी काम आता है।
- **आपके खातों के साथ काम करता है।** Gmail, Outlook और Microsoft 365 या IMAP, POP3, SMTP, JMAP, CalDAV और CardDAV मानकों पर आधारित सेवाएँ जोड़ें।
- **डिफ़ॉल्ट रूप से गोपनीयता का ध्यान।** आपकी अनुमति तक दूरस्थ चित्र ब्लॉक रहते हैं। इससे ट्रैकिंग पिक्सेल को यह बताने से रोकने में मदद मिलती है कि आपने संदेश कब पढ़ा।
- **आर्किटेक्चर में ही सुरक्षा।** ईमेल की सामग्री कभी WebView में नहीं खोली जाती। Flectar Mail अपने Rust-नेटिव सिस्टम से HTML और CSS दिखाता है, ईमेल की स्क्रिप्ट नहीं चलाता और दूरस्थ चित्रों को डिफ़ॉल्ट रूप से ब्लॉक करता है। इस तरह अंतर्निहित ब्राउज़र से जुड़ी हमले की संभावनाओं से बचा जाता है।
- **Rust में एक प्रयोगात्मक रेंडरर।** ब्राउज़र इंजन के बिना ईमेल रेंडरर बनाना एक नया क्षेत्र है। अनुकूलता में सुधार जारी रहने के दौरान, खासकर जटिल संदेशों में, प्रदर्शन संबंधी समस्याएँ अपेक्षित हैं।
- **हर स्क्रीन के लिए।** विस्तृत और मिनिमल डेस्कटॉप लेआउट, टच के अनुकूल कॉम्पैक्ट इंटरफ़ेस जैसा ही अनुभव देते हैं।
- **नेटिव और ओपन सोर्स।** Rust में शुरू से बनाया गया है। यह किसी विंडो में बंद ब्राउज़र नहीं है और AGPLv3 के अंतर्गत उपलब्ध है।

## 📁 फ़ाइलें और अटैचमेंट

JMAP/WebDAV स्टोरेज देखें, ईमेल अटैचमेंट खोजें, फ़ाइलें ऑफ़लाइन रखें और PDF, चित्रों तथा टेक्स्ट का पूर्वावलोकन करें।

## ✍🏻 खाते के हस्ताक्षर और OpenPGP

सेटिंग्स → खाते → हस्ताक्षर और OpenPGP में नाम वाले हस्ताक्षर, नए संदेशों और जवाबों के लिए अलग डिफ़ॉल्ट हस्ताक्षर और संदेश लिखते समय हस्ताक्षर चुनने का विकल्प मिलता है। डेस्कटॉप पर OpenPGP/MIME हस्ताक्षर और एन्क्रिप्शन के लिए इंस्टॉल किया गया GnuPG 2.x इस्तेमाल होता है, और निजी कुंजियों के पासफ़्रेज़ के लिए pinentry। अनिवार्य एन्क्रिप्शन चालू होने पर प्राप्तकर्ताओं की कुंजियाँ गायब या अमान्य हों तो भेजना रुक जाता है; सुरक्षित ड्राफ़्ट भेजे जाने तक स्थानीय ही रहते हैं।

फ़िलहाल S/MIME और मोबाइल पर OpenPGP समर्थित नहीं हैं।

## 🧪 प्रयोगात्मक HTML रेंडरिंग

> [!WARNING]
> HTML ईमेल रेंडरिंग इस समय Flectar Mail का सबसे प्रयोगात्मक हिस्सा है। कुछ संदेश, खासकर जटिल या असामान्य मार्कअप और CSS वाले, अभी सही तरह से नहीं दिख सकते हैं।

क्लाइंट को पूरी तरह नेटिव रखने और मेमोरी उपयोग लगभग 80 MB रखने के लिए Flectar Mail ब्राउज़र या WebView जोड़ने के बजाय [Blitz](https://github.com/DioxusLabs/blitz) से ईमेल HTML रेंडर करता है। Blitz, Dioxus टीम का Rust में बना HTML/CSS रेंडरर है।

डेस्कटॉप बिल्ड में सेटिंग्स → सामान्य → रेंडरर के अंतर्गत **CPU — Low Memory** और **GPU — WGPU** विकल्प हैं। शुरुआत में CPU चुना होता है और WGPU प्रारंभ नहीं होता। GPU विकल्प एक साझा WGPU 30 डिवाइस पर Slint और Vello इस्तेमाल करता है; सेटिंग बदलने का असर ऐप दोबारा शुरू करने पर होता है। GPU से शुरू करने में विफलता हो तो ऐप अपने आप CPU पर लौट जाता है।

जहाँ तक हमें पता है, Flectar Mail उन शुरुआती परियोजनाओं में है जो वास्तविक दुनिया के किसी भी प्रकार के ईमेल HTML के लिए Blitz इस्तेमाल कर रही हैं। ईमेल में बहुत सा असामान्य HTML और CSS होता है, जो रेंडरर के सामने कठिन चुनौतियाँ रखता है। अभी हम Blitz के ऊपर कई पैच बनाए रखते हैं और समय के साथ इनमें से जितना संभव हो मूल परियोजना में शामिल करवाना चाहते हैं।

अगर कोई ईमेल गलत दिखता है, तो कृपया [उसकी रिपोर्ट करें](https://github.com/flectar/mail/issues)। यह तरीका अभी प्रयोगात्मक है, लेकिन Tauri या Wails जैसे फ़्रेमवर्क से बने WebView-आधारित क्लाइंट की तुलना में Flectar Mail के इतना हल्का रहने की एक बड़ी वजह भी यही है।

## 🎨 अपनी पसंद के अनुसार

ईमेल सँभालने के अपने तरीके के अनुसार कार्यक्षेत्र चुनें। विस्तृत तीन-पैनल लेआउट रखें या सरल मिनिमल दृश्य अपनाएँ, हल्की या गहरी थीम चुनें, उपलब्ध रंग-पैलेट लें या अपना बनाएँ और भेजने वालों के अवतार दिखाएँ या छिपाएँ। आप ऐप आइकन का रंग भी बदल सकते हैं: टास्कबार या Dock में, सिस्टम ट्रे में, Linux ऐप लॉन्चर में और Android व iOS की होम स्क्रीन पर।

संदेशों का रूप ऐप की थीम के अनुसार बदल सकता है या स्वतंत्र रूप से हल्का या गहरा रह सकता है। चुना गया रंग-पैलेट HTML, सादे टेक्स्ट और रीडर मोड में इस्तेमाल होता है। कुछ समय के लिए भेजने वाले के मूल रंग देखने के लिए डेस्कटॉप संदेश टूलबार में खोज और रीडर मोड के बीच कंट्रास्ट आइकन दबाएँ, या किसी भी स्क्रीन आकार पर संदेश के तीन-बिंदु मेन्यू से **मूल रंग दिखाएँ** चुनें।

पूर्ण कार्यक्षेत्र में फ़ोल्डर, संदेशों की सूची और चुना गया ईमेल एक साथ दिखते हैं। मिनिमल लेआउट दृश्य अव्यवस्था कम करता है और ज़रूरत पड़ने पर इनबॉक्स के हर हिस्से को अधिक जगह देता है।

### 👥 प्रोफ़ाइल और खातों के रंग

संबंधित खातों को काम या निजी जैसे नाम वाले प्रोफ़ाइल में रखें और हर प्रोफ़ाइल को एक रंग दें। टैब बार की शुरुआत में प्रोफ़ाइल मेन्यू से या सेटिंग्स → खाते → प्रोफ़ाइल में प्रोफ़ाइल बनाएँ और उसमें शामिल खाते चुनें। इस मेन्यू में प्रोफ़ाइल चुनने पर मेल दृश्य उसके खातों तक सीमित हो जाता है: संयुक्त इनबॉक्स, खोज, गिनती और साइडबार केवल उन्हीं का मेल दिखाते हैं, जबकि टैब वैसे ही रहते हैं। **सभी खाते** चुनने पर फिर सब दिखता है।

आप किसी एक खाते का रंग अलग भी तय कर सकते हैं और प्रभावी रंग हर संदेश के बाएँ किनारे पर दिखा सकते हैं। इन संकेतों से संयुक्त इनबॉक्स में खातों को पहचानना आसान होता है; नीचे के उदाहरण में काम के लिए बैंगनी और सहायता के लिए नारंगी रंग है।

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
  <img src="../../resources/screenshots/desktop-profiles-light.png" alt="बैंगनी और नारंगी प्रोफ़ाइल संकेतों वाला Flectar Mail का संयुक्त इनबॉक्स">
</picture>

### 🧵 बातचीत के थ्रेड

जवाब समय के क्रम में एक साथ रहते हैं और सक्रिय संदेश रीडिंग पैन में खुला दिखता है।

संदेशों के बीच जाने के लिए **ऊपर/नीचे**, पहले या आख़िरी लोड हुए संदेश पर जाने के लिए **Home/End** और एक पृष्ठ आगे-पीछे जाने के लिए **Page Up/Page Down** इस्तेमाल करें। **Enter** वर्तमान संदेश खोलता है और उसके कंटेंट पर फ़ोकस करता है; **Space** सामूहिक कार्रवाइयों के लिए उसे चुनता या चयन हटाता है। **Tab/Shift+Tab** कंट्रोल के बीच जाते हैं। पूर्ण कार्यक्षेत्र में **F6** संदेश सूची और रीडर के बीच फ़ोकस बदलता है। मिनिमल लेआउट में तीर कुंजियाँ संदेश हाइलाइट करती हैं और **Enter** उन्हें खोलता है; **Escape** या **F6** सूची पर लौटाता है।

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
  <img src="../../resources/screenshots/desktop-thread-light.png" alt="प्राप्त संदेशों और जवाबों वाली Flectar Mail बातचीत">
</picture>

### 🗂️ टैब

ब्राउज़र जैसे टैब से कई जगहें एक साथ खुली रखें। हर टैब अपनी जगह याद रखता है: मेलबॉक्स, खोज, खुली बातचीत, कैलेंडर, संपर्क, फ़ाइलें या सेटिंग्स, साथ ही सूची में स्क्रॉल की स्थिति और पढ़ा जा रहा संदेश। संदेश पर बीच वाले माउस बटन या Ctrl/⌘ के साथ क्लिक करके उसे बैकग्राउंड टैब में खोलें (उस पर जाने के लिए Shift भी दबाएँ), या उसके मेन्यू से **नए टैब में खोलें** चुनें। क्रम बदलने के लिए टैब खींचें, बंद करने के लिए बीच वाला बटन दबाएँ और बाकी टैब या दाईं ओर के टैब बंद करने के लिए दायाँ क्लिक करें। अगली बार Flectar Mail खोलने पर वहीं से शुरू करें जहाँ छोड़ा था।

**Ctrl/⌘+T** नया टैब खोलता है, **Ctrl/⌘+W** वर्तमान टैब बंद करता है और **Ctrl/⌘+Shift+T** आख़िरी बंद टैब फिर खोलता है। **Ctrl+Tab** और **Ctrl+Shift+Tab** टैब के बीच जाते हैं, **Ctrl/⌘+1–8** किसी खास टैब पर और **Ctrl/⌘+9** आख़िरी टैब पर जाता है।

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
  <img src="../../resources/screenshots/desktop-tabs-light.png" alt="कई बातचीत, कैलेंडर, संपर्क और फ़ाइलों के टैब वाला Flectar Mail">
</picture>

### पूर्ण और मिनिमल कार्यक्षेत्र

<table>
  <thead>
    <tr>
      <th width="50%">पूर्ण कार्यक्षेत्र</th>
      <th width="50%">मिनिमल कार्यक्षेत्र</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail का पूर्ण डेस्कटॉप कार्यक्षेत्र" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="Flectar Mail का मिनिमल डेस्कटॉप कार्यक्षेत्र" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### रंग-पैलेट

<table>
  <thead>
    <tr>
      <th width="25%">नीलहरित</th>
      <th width="25%">हरा</th>
      <th width="25%">बैंगनी</th>
      <th width="25%">कस्टम</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="Flectar Mail का नीलहरित पैलेट" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="Flectar Mail का हरा पैलेट" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="Flectar Mail का बैंगनी पैलेट" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail का कस्टम पैलेट" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 🪟 बिना फ़्रेम की विंडो

सेटिंग्स → सामान्य → इंटरफ़ेस में **बिना फ़्रेम की विंडो** चालू करें ताकि सिस्टम टाइटल बार की जगह टूलबार में विंडो कंट्रोल आ जाएँ। हर प्लेटफ़ॉर्म का अपना व्यवहार बना रहता है: macOS के मूल रंगीन बटन, Windows के Snap Layouts और सिस्टम मेन्यू, तथा Linux में GNOME या KDE के लिए तय बटन क्रम और टाइटल बार कार्रवाइयाँ।

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
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="macOS पर Flectar Mail की बिना फ़्रेम की विंडो" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="Windows पर Flectar Mail की बिना फ़्रेम की विंडो" width="100%">
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
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="GNOME पर Flectar Mail की बिना फ़्रेम की विंडो" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="KDE Plasma पर Flectar Mail की बिना फ़्रेम की विंडो" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 📅 कैलेंडर, संपर्क और फ़ाइलें

<table>
  <thead>
    <tr>
      <th width="33.33%">कैलेंडर</th>
      <th width="33.33%">संपर्क</th>
      <th width="33.33%">फ़ाइलें (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="Flectar Mail का कैलेंडर" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="Flectar Mail के संपर्क" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="Flectar Mail की फ़ाइलें" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 💾 स्टोरेज और बैकअप

सेटिंग्स → स्टोरेज दिखाता है कि ईमेल, अटैचमेंट, ऑफ़लाइन फ़ाइलें और डेटाबेस डिवाइस पर कितनी जगह लेते हैं। इसी पृष्ठ से सत्यापित डेटाबेस स्नैपशॉट निर्यात कर सकते हैं या बैकअप के ज़रिए जुड़े खातों की सेटिंग्स और प्राथमिकताएँ ले जा सकते हैं। पासवर्ड और OAuth टोकन सिस्टम कीरिंग में रहते हैं, इसलिए बहाल किए गए डिवाइस पर दोबारा साइन इन करना होता है।

POP3 खाते ईमेल इस डिवाइस पर डाउनलोड करते हैं और सर्वर पर छोड़ देते हैं, जब तक आप दूसरा विकल्प न चुनें: हर खाता 7, 14 या 30 दिन बाद, यहाँ मिटाने पर या डाउनलोड होते ही सर्वर से ईमेल हटा सकता है, लेकिन डिवाइस पर सहेजने से पहले कभी नहीं। उनके फ़ोल्डर केवल इस डिवाइस पर होते हैं, इसलिए स्नैपशॉट में वह मेल भी शामिल होता है। POP3 खाता हटाने या सारा डेटा मिटाने से पहले ऐप उसे mbox फ़ाइलों में, हर फ़ोल्डर के लिए एक फ़ाइल, निर्यात करने का विकल्प देता है। दूसरे मेल ऐप इन्हें आयात कर सकते हैं।

यही आयात सुविधा Thunderbird से (प्रोफ़ाइल फ़ोल्डर, उसके टूल्स › एक्सपोर्ट का आर्काइव या `prefs.js`) और Thunderbird for Android या K-9 Mail से (`.k9s` सेटिंग्स निर्यात) खाते भी लाती है। कुछ भी जोड़ने से पहले आप हर खाते की समीक्षा करते हैं: Flectar Mail जिन सेटिंग्स को दोहरा नहीं सकता, जैसे Exchange सर्वर, उन्हें चुपचाप बदलने के बजाय समझाया जाता है। POP3 खाते सर्वर से मेल हटाने का अपना नियम बनाए रखते हैं। मौजूदा खाते कभी बदले नहीं जाते और पासवर्ड कभी आयात नहीं किए जाते।

सहेजा गया मेल भी इसी तरह आयात होता है: Thunderbird के लोकल फ़ोल्डर और उसके POP3 खातों द्वारा डाउनलोड किया गया मेल, mbox फ़ाइलें जैसे Google Takeout निर्यात या Flectar Mail का अपना निर्यात, `.eml` संदेश, maildir फ़ोल्डर, Apple Mail मेलबॉक्स और इन्हें रखने वाले ZIP आर्काइव। आप फ़ाइलें चुनते हैं, चाहें तो एक साथ कई, और फिर फ़ोल्डर (ट्रैश और जंक तभी शामिल होते हैं जब आप उन्हें चुनें), और फ़ोल्डर संरचना के साथ पढ़े गए और तारांकित होने की स्थिति भी साथ आती है। जो मेल पहले से वहाँ है उसे छोड़ दिया जाता है, इसलिए रुका हुआ आयात बस फिर से चलाया जा सकता है, और लंबा आयात बैकग्राउंड में चलता रह सकता है। जो मेल किसी खाते का नहीं है वह **इस डिवाइस पर** में जाता है: ऐसे फ़ोल्डर जो केवल यहीं मौजूद हैं और जिन्हें सेटिंग्स → स्टोरेज से निर्यात या हटाया जा सकता है। मेल किसी POP3 खाते के फ़ोल्डरों में भी जा सकता है, या किसी IMAP, JMAP या Gmail खाते के सर्वर पर, वहाँ एक अलग फ़ोल्डर में, अपलोड किया जा सकता है।

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-storage-light.png">
  <img src="../../resources/screenshots/desktop-storage-light.png" alt="स्थानीय उपयोग, इस डिवाइस पर मेल, डेटाबेस स्नैपशॉट और खाता बैकअप कंट्रोल वाली Flectar Mail की स्टोरेज सेटिंग्स">
</picture>

### 📱 छोटी स्क्रीन के लिए

कॉम्पैक्ट इंटरफ़ेस महत्वपूर्ण कार्रवाइयों को पहुँच में रखता है और ज़रूरत पड़ने पर संदेशों, घटनाओं और संपर्कों को पूरी स्क्रीन देता है।

| हल्का | गहरा |
| --- | --- |
| ![हल्के मोड में Flectar Mail का मोबाइल इनबॉक्स](../../resources/screenshots/mobile-light.png) | ![गहरे मोड में Flectar Mail का मोबाइल इनबॉक्स](../../resources/screenshots/mobile-dark.png) |

## 📥 Flectar Mail पाएँ

Flectar Mail अभी विकास में है और स्थिर नहीं है।

> [!WARNING]
> **Google OAuth सत्यापन के लिए हर साल CASA सुरक्षा मूल्यांकन ज़रूरी है।** Flectar एक सामुदायिक ओपन-सोर्स परियोजना है, इसलिए यह मूल्यांकन प्रायोजकों के सहयोग पर निर्भर करता है। जब तक आवश्यक सत्यापन के लिए धन नहीं मिलता और वह पूरा नहीं होता, डिफ़ॉल्ट रूप से कॉन्फ़िगर किए गए Gmail OAuth वाली स्थिर रिलीज़ संभव नहीं होगी। अंतर्निहित Gmail साइन-इन उपलब्ध कराने में मदद के लिए [Flectar को प्रायोजित करें](https://github.com/sponsors/flectar)।
>
> Google और Microsoft का OAuth सत्यापन अभी जारी है। GitHub पर पहले की प्रीरिलीज़ उन परीक्षकों के लिए हैं जो अपना OAuth पंजीकरण या IMAP, POP3 अथवा JMAP खाते इस्तेमाल करते हैं।

OAuth ऐप कुंजियों के बिना बने बिल्ड में Gmail और Outlook साइन-इन बंद रहता है। स्वागत स्क्रीन पर **साइन-इन सेटिंग्स** के गियर आइकन से अपना Google या Microsoft ऐप पंजीकरण सहेजें, या IMAP, POP3 अथवा JMAP खाता जोड़ें। कस्टम कुंजियाँ सेटिंग्स के साथ साझा होती हैं और शामिल कुंजियों पर प्राथमिकता रखती हैं; कस्टम क्लाइंट ID साफ़ करने पर उपलब्ध डिफ़ॉल्ट फिर लागू होते हैं। हर प्रदाता अपनी कॉन्फ़िगरेशन सहेजने के बाद अलग से सक्षम होता है।

प्रीव्यू बिल्ड प्रकाशित होने पर उन्हें [GitHub Releases](https://github.com/flectar/mail/releases) से डाउनलोड करें और **Pre-release** चिह्न देखें:

- **Linux x64:** AppImage, Debian/Ubuntu के लिए `.deb`, Fedora 43/44 के लिए `.rpm` या मैन्युअल इंस्टॉल करने वाला Flatpak प्रीव्यू पैकेज
- **Windows x64:** `.exe` इंस्टॉलर या पोर्टेबल ZIP
- **macOS Apple silicon या Intel (macOS 14+):** DMG या ऐप का ZIP
- **Android arm64 (Android 8.0+):** हस्ताक्षरित APK; प्रीव्यू और स्थिर रिलीज़ की ऐप पहचान एक ही है

Windows प्रीव्यू हस्ताक्षरित नहीं हैं; macOS प्रीव्यू ad hoc हस्ताक्षरित हैं और नोटराइज़ नहीं हैं, इसलिए ऑपरेटिंग सिस्टम की सुरक्षा चेतावनियाँ अपेक्षित हैं। अपडेट मैन्युअल रूप से इंस्टॉल करें।

Flatpak प्रीव्यू अलग परीक्षण पैकेज के रूप में मिलता है। यह अभी Flathub पैकेज नहीं है, इसलिए Flathub से अपने आप अपडेट नहीं मिलता। ट्रे का बैकएंड सैंडबॉक्स के अनुकूल D-Bus नाम इस्तेमाल कर सके, तब तक बंद करके ट्रे में भेजने का विकल्प निष्क्रिय है। साइन-इन पर अपने आप शुरू होना डेस्कटॉप के Background पोर्टल से होता है और विंडो खुलती है, क्योंकि ट्रे में शुरू होने की सुविधा उपलब्ध नहीं है। वहाँ ऐप आइकन नारंगी ही रहता है, क्योंकि सैंडबॉक्स होस्ट पर लॉन्चर का आइकन नहीं बदल सकता।

डाउनलोड किया Linux पैकेज `sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` या `flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak` से इंस्टॉल करें। Flatpak को `flatpak run com.flectar.mail` से चलाएँ।

RPM, x86_64 पर Fedora 43 और 44 के लिए है। रिलीज़ बिल्ड Fedora 43 को अनुकूलता का आधार मानते हैं; प्रकाशन से पहले वही RPM दोनों संस्करणों के साफ़ कंटेनर में इंस्टॉलेशन, PDF रेंडरिंग, X11 और Wayland पर शुरू होने तथा हटाए जाने की जाँच पास करता है। दूसरे RPM-आधारित वितरण अभी जाँचे नहीं गए हैं। Discover कोई सामान्य टकराव बताए तो ऊपर वाला `dnf install` कमांड चलाएँ और निर्भरता की त्रुटि अपनी बग रिपोर्ट में शामिल करें।

पुराना `0.1.0-alpha.7` RPM Fedora 44 पर बना था और glibc 2.43 की ज़रूरत के कारण Fedora 43 पर इंस्टॉल नहीं हो सकता। ऊपर दी गई अनुकूलता जाँचों के साथ बनी RPM रिलीज़ आने तक उसका Flatpak पैकेज एक विकल्प है।

हर GitHub रिलीज़ में `SHA256SUMS` और बिल्ड के स्रोत के हस्ताक्षरित प्रमाण शामिल होते हैं। GitHub CLI इंस्टॉल होने पर डाउनलोड को `gh attestation verify <download> --repo flectar/mail` से सत्यापित करें।

alpha 8 से Android के alpha, beta, रिलीज़-कैंडिडेट और स्थिर बिल्ड एक पैकेज (`com.flectar.mail`) और स्थायी हस्ताक्षर पहचान इस्तेमाल करते हैं। बाद के संस्करण ऐप को उसी जगह अपडेट करके स्थानीय डेटा बचा सकते हैं। Alpha 6 और alpha 7 में अलग कुंजियाँ थीं और उनसे इस हस्ताक्षर पहचान पर अपडेट नहीं हो सकता।

दोनों Android रिलीज़ वर्कफ़्लो GitHub का `android-release` एनवायरनमेंट इस्तेमाल करते हैं। उन्हें वही कीस्टोर और पासवर्ड सीक्रेट चाहिए; वे सार्वजनिक प्रमाणपत्र को `platform/android/app-signing-certificate.sha256` से जाँचते हैं और रिलीज़ संस्करण से Android संस्करण कोड निकालते हैं। कुंजी गायब या अलग होने पर प्रकाशन रुकता है। **सामान्य रिलीज़ के लिए नई प्रतिस्थापन कुंजी न बनाएँ और तय फ़िंगरप्रिंट न बदलें।** मूल निजी कुंजी और प्रमाणपत्र रखे जाएँ तो पासवर्ड बदलने पर पहचान बनी रहती है।

एक बार चलाने वाला माइग्रेशन कमांड, आवश्यक सीक्रेट, बैकअप और Play App Signing के लिए [Android हस्ताक्षर सेटअप और पुनर्प्राप्ति](../android-signing.md) देखें।

स्थानीय डेवलपर बिल्ड अब भी स्थानीय परीक्षण कुंजी बना सकते हैं। वे केवल उसी स्थानीय कुंजी से हस्ताक्षरित इंस्टॉलेशन अपडेट करते हैं।

आप [Rust टूलचेन](https://rustup.rs/) से Flectar Mail को स्रोत कोड से भी बना सकते हैं:

```bash
cargo run --bin flectar-mail
```

Linux डेस्कटॉप पर OAuth, डेस्कटॉप पोर्टल के ज़रिए सिस्टम ब्राउज़र इस्तेमाल करता है और नवीनीकरण क्रेडेंशियल freedesktop Secret Service में सहेजता है। इसलिए सामान्य डेस्कटॉप सत्र को `xdg-desktop-portal` बैकएंड और GNOME Keyring जैसे Secret Service प्रदाता की ज़रूरत होती है। सुरक्षित क्रेडेंशियल स्टोरेज उपलब्ध न हो तो रिलीज़ बिल्ड OAuth पृष्ठ खोलने से पहले रुक जाते हैं, ताकि सहेजने की सुरक्षित जगह के बिना अनुमति प्रक्रिया पूरी न हो।

Podman और Docker के विकास परिवेश में अक्सर होस्ट सत्र के D-Bus और होस्ट ब्राउज़र के loopback नेटवर्क, दोनों का अभिगम नहीं होता। डिबग बिल्ड इस स्थिति के लिए स्पष्ट रूप से चिह्नित, केवल मालिक के लिए उपलब्ध विकास क्रेडेंशियल फ़ाइल और अंतिम loopback कॉलबैक URL चिपकाने का खुलने वाला पैनल देते हैं। फ़ाइल-आधारित स्टोरेज रिलीज़ बिल्ड में शामिल नहीं होता।

## 🌍 Flectar Mail के अनुवाद में मदद करें

Flectar Mail सबके लिए बना है और इसे अधिक भाषाओं में उपलब्ध कराने में आपकी मदद का स्वागत है!

सामुदायिक अनुवाद सँभालने के लिए हम [Weblate](https://translate.flectar.com/) इस्तेमाल करते हैं। आप Flectar Mail का अपनी मातृभाषा में अनुवाद करना चाहें, मौजूदा अनुवाद सुधारना चाहें या अनूदित टेक्स्ट की समीक्षा करना चाहें, हर योगदान का स्वागत है।

**[Flectar Mail का अनुवाद शुरू करें →](https://translate.flectar.com/)**

शुरू करना आसान है:

1. हमारे अनुवाद प्लेटफ़ॉर्म पर खाता बनाएँ।
2. अपनी भाषा चुनें या उपलब्ध न होने पर नया अनुवाद शुरू करें।
3. सीधे Weblate में टेक्स्ट का अनुवाद करें; प्रोग्रामिंग या GitHub के अनुभव की ज़रूरत नहीं है।

अनुवाद हमारे GitHub रिपॉज़िटरी से सिंक होते हैं और समीक्षा के लिए पुल अनुरोध के रूप में भेजे जाते हैं। योगदानकर्ताओं को GitHub पर उनके काम का श्रेय भी मिल सकता है।

README के अनुवाद इस रिपॉज़िटरी में बनाए रखे जाते हैं। नया अनुवाद जोड़ने या अपडेट करने के लिए [अनुवाद में योगदान](../../CONTRIBUTING.md#translations) देखें।

## ♿ सुगम्यता

Flectar Mail में कीबोर्ड नेविगेशन, हल्की और गहरी थीम तथा लचीले लेआउट हैं। सुगम्यता पर काम जारी है। वर्तमान सुविधाओं, परीक्षण की सीमाओं और बाधा की रिपोर्ट करने के तरीके के लिए [ACCESSIBILITY.md](../../ACCESSIBILITY.md) देखें।

## 🔒 सुरक्षा

किसी सुरक्षा कमज़ोरी की निजी रूप से रिपोर्ट करने के लिए [SECURITY.md](../../SECURITY.md) का पालन करें। सुरक्षा कमज़ोरियों का विवरण सार्वजनिक इश्यू में न दें।

## 🌐 सबके लिए ओपन सोर्स

Flectar Mail एक ही ओपन-सोर्स ऐप है। इसका अलग कम्युनिटी संस्करण नहीं है। क्लाइंट [GNU Affero General Public License v3](../../LICENSE) के अंतर्गत है। व्यावहारिक जानकारी के लिए [लाइसेंस का अवलोकन](../../LICENSING.md) पढ़ें या परियोजना में योगदान के लिए [CONTRIBUTING.md](../../CONTRIBUTING.md) देखें।

## 🫡 आभार

Flectar Mail इन परियोजनाओं और उनके योगदानकर्ताओं के काम से संभव हुआ है:

- [Slint](https://slint.dev/), वह नेटिव UI टूलकिट जो Flectar Mail के इंटरफ़ेस को चलाता है।
- [Blitz](https://github.com/DioxusLabs/blitz), Dioxus टीम का Rust HTML/CSS रेंडरर जो ईमेल पढ़ने का अनुभव उपलब्ध कराता है।

---
