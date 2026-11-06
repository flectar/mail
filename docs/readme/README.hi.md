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
    <p>शुरू से गति के लिए बना। Flectar Mail नेटिव प्रदर्शन और तुरंत शुरुआत देता है, और निष्क्रिय अवस्था में लगभग 20 MB निजी मेमोरी ही इस्तेमाल करता है।</p>
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

| लाभ | विवरण |
| --- | --- |
| **पहले क्लिक से ही तेज़** | नेटिव इंटरफ़ेस और स्थानीय डेटा को प्राथमिकता देने वाला तरीका आपको ब्राउज़र रनटाइम का इंतज़ार किए बिना इनबॉक्स तक पहुँचाते हैं। |
| **निष्क्रिय अवस्था में लगभग 20 MB निजी मेमोरी** | कोड और लाइब्रेरी मिलाकर लगभग 85 MB, और उसी मेलबॉक्स के साथ Thunderbird से बारह गुना कम निजी मेमोरी। विवरण के लिए [प्रदर्शन](#-प्रदर्शन) देखें। |
| **सब कुछ एक जगह** | अलग-अलग ऐप जोड़ने की ज़रूरत के बिना ईमेल, कैलेंडर और संपर्कों के बीच जाएँ। |
| **ऑफ़लाइन काम करने के लिए बना** | आपका मेलबॉक्स और कैलेंडर स्थानीय रूप से सहेजे जाते हैं, इसलिए सिंक किया गया डेटा बिना कनेक्शन के भी काम आता है। |
| **आपके खातों के साथ काम करता है** | Gmail, Outlook और Microsoft 365 या IMAP, POP3, SMTP, JMAP, CalDAV और CardDAV मानकों पर आधारित सेवाएँ। |
| **डिफ़ॉल्ट रूप से गोपनीयता का ध्यान** | आपकी अनुमति तक दूरस्थ चित्र ब्लॉक रहते हैं, इसलिए ट्रैकिंग पिक्सेल यह नहीं बता सकते कि आपने संदेश कब पढ़ा। |
| **आर्किटेक्चर में ही सुरक्षा** | ईमेल कभी WebView में नहीं खोला जाता। Rust-नेटिव सिस्टम ईमेल की स्क्रिप्ट चलाए बिना उसका HTML और CSS रेंडर करता है, जिससे अंतर्निहित ब्राउज़र से जुड़ी हमले की संभावनाओं से बचा जाता है। |
| **Rust में एक प्रयोगात्मक रेंडरर** | ब्राउज़र इंजन के बिना ईमेल रेंडर करना एक नया क्षेत्र है। अनुकूलता में सुधार जारी रहने के दौरान जटिल संदेश अभी सही तरह से नहीं दिख सकते हैं। |
| **हर स्क्रीन के लिए** | विस्तृत और मिनिमल डेस्कटॉप लेआउट, टच के अनुकूल कॉम्पैक्ट इंटरफ़ेस जैसा ही अनुभव देते हैं। |
| **नेटिव और ओपन सोर्स** | Rust में शुरू से बनाया गया है। यह किसी विंडो में बंद ब्राउज़र नहीं है और AGPLv3 के अंतर्गत उपलब्ध है। |

## ⚡ प्रदर्शन

| निष्क्रिय अवस्था, एक ही मेलबॉक्स | निजी मेमोरी | कोड और लाइब्रेरी सहित (PSS) | ईमेल किससे रेंडर होता है |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17.2–17.4 MiB** | **79.1–80.0 MiB** | Blitz |
| Geary 46 | 126.1–126.2 MiB | 309.6–311.4 MiB | WebKitGTK |
| Evolution 3.56 | 191.0–192.2 MiB | 395.0–395.9 MiB | WebKitGTK |
| Thunderbird 159 | 244.1–248.1 MiB | 424.4–427.1 MiB | Gecko |
| Betterbird 153 | 243.9–247.8 MiB | 411.9–419.7 MiB | Gecko |
| Mailspring 1.26 | 311.4–314.9 MiB | 551.1–551.7 MiB | Chromium |
| Aerion 0.3.6 | 421.9–437.3 MiB | 660.4–678.0 MiB | WebKitGTK |

माप Linux पर एक IMAP खाते (860 संदेश) के साथ लिया गया: इनबॉक्स खुला था, एक HTML न्यूज़लेटर दिख रहा था, ऐप शुरू हुए 60 सेकंड हो चुके थे और डिफ़ॉल्ट CPU रेंडरर चुना हुआ था। हर सीमा दो मापों पर आधारित है, और हर ऐप की शुरू की गई सभी प्रक्रियाएँ गिनी गई हैं। निजी मेमोरी वही है जिसे Windows टास्क मैनेजर **मेमोरी** के रूप में दिखाता है। Windows पर 7 असली खातों और लगभग 69,000 अपठित संदेशों के साथ टास्क मैनेजर ने विंडो खुली रहने पर 37.8 MiB और सिस्टम ट्रे में बंद करने के बाद 4.2 MiB दिखाया। [हम कैसे मापते हैं और आप ख़ुद कैसे जाँच सकते हैं](../performance.md), इसके बारे में पढ़ें।

### दूसरे क्लाइंट से तुलना

| ईमेल क्लाइंट | इंटरफ़ेस | ईमेल किससे रेंडर होता है | प्लेटफ़ॉर्म |
| --- | --- | --- | --- |
| **Flectar Mail** | नेटिव Rust ऐप | Blitz, Rust में बना HTML/CSS रेंडरर; कोई WebView नहीं | Windows, macOS, Linux, Android, iOS (साइडलोड करके) |
| Thunderbird | Gecko-आधारित डेस्कटॉप ऐप | Gecko | Windows, macOS, Linux, Android |
| Windows के लिए Outlook | वेब UI वाला Windows ऐप | Microsoft Edge WebView2 | Windows |
| Spark | Electron ऐप | Chromium | macOS, Windows, iOS, Android |
| Aerion | Wails वेब UI वाला Go ऐप | सिस्टम WebView | Linux, macOS, Windows |
| ब्राउज़र टैब में Gmail | वेब ऐप | Chrome या कोई दूसरा ब्राउज़र | वेब |

## ✨ सुविधाएँ

| सुविधा | यह क्या करती है |
| --- | --- |
| [बातचीत के थ्रेड](../features.md#conversation-threads) | जवाब समय के क्रम में एक साथ रहते हैं और पूरा कीबोर्ड नेविगेशन मिलता है। इसे बंद भी किया जा सकता है। |
| [टैब](../features.md#tabs) | ब्राउज़र जैसे टैब जो अपनी जगह याद रखते हैं; डेस्कटॉप पर शॉर्टकट और फ़ोन पर टैब स्विचर। |
| [प्रोफ़ाइल और खातों के रंग](../features.md#profiles-and-account-colors) | खाते काम या निजी जैसे प्रोफ़ाइल में रखे जाते हैं और हर संदेश रंग से चिह्नित होता है। |
| [रूप-रंग](../features.md#appearance) | पूर्ण या मिनिमल लेआउट, हल्की या गहरी थीम, रंग-पैलेट, भेजने वालों के अवतार और ऐप आइकन के रंग। |
| [बिना फ़्रेम की विंडो](../features.md#frameless-window) | टूलबार में विंडो कंट्रोल, जिनमें हर प्लेटफ़ॉर्म का अपना व्यवहार बना रहता है। |
| [कैलेंडर, संपर्क और फ़ाइलें](../features.md#calendar-contacts-and-files) | CalDAV और CardDAV सिंक, JMAP और WebDAV फ़ाइलें, अटैचमेंट खोज, ऑफ़लाइन फ़ाइलें और पूर्वावलोकन। |
| [हस्ताक्षर और OpenPGP](../features.md#signatures-and-openpgp) | नाम वाले हस्ताक्षर, और डेस्कटॉप पर GnuPG के ज़रिए OpenPGP/MIME हस्ताक्षर व एन्क्रिप्शन। |
| [स्टोरेज और बैकअप](../features.md#storage-and-backups) | डेटा के प्रकार के अनुसार इस्तेमाल हुई जगह, डेटाबेस स्नैपशॉट, खाता बैकअप और POP3 के लिए सर्वर से मेल हटाने के नियम। |
| [खातों और मेल का आयात](../features.md#importing-accounts-and-mail) | Thunderbird, K-9 Mail, mbox, `.eml`, maildir, Apple Mail और ZIP आर्काइव। |
| [HTML रेंडरिंग](../features.md#html-rendering) | ईमेल HTML को Rust में Blitz से रेंडर किया जाता है, CPU या GPU पर। |

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

### टैब, बातचीत और प्रोफ़ाइल

<table>
  <thead>
    <tr>
      <th width="33.33%">टैब</th>
      <th width="33.33%">बातचीत</th>
      <th width="33.33%">प्रोफ़ाइल के रंग</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
          <img src="../../resources/screenshots/desktop-tabs-light.png" alt="कई बातचीत, कैलेंडर, संपर्क और फ़ाइलों के टैब वाला Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
          <img src="../../resources/screenshots/desktop-thread-light.png" alt="प्राप्त संदेशों और जवाबों वाली Flectar Mail बातचीत" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
          <img src="../../resources/screenshots/desktop-profiles-light.png" alt="बैंगनी और नारंगी प्रोफ़ाइल संकेतों वाला Flectar Mail का संयुक्त इनबॉक्स" width="100%">
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

### बिना फ़्रेम की विंडो

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

### कैलेंडर, संपर्क और फ़ाइलें

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

### छोटी स्क्रीन के लिए

| हल्का | गहरा |
| --- | --- |
| ![हल्के मोड में Flectar Mail का मोबाइल इनबॉक्स](../../resources/screenshots/mobile-light.png) | ![गहरे मोड में Flectar Mail का मोबाइल इनबॉक्स](../../resources/screenshots/mobile-dark.png) |

## 🧪 प्रयोगात्मक HTML रेंडरिंग

> [!WARNING]
> HTML ईमेल रेंडरिंग इस समय Flectar Mail का सबसे प्रयोगात्मक हिस्सा है। कुछ संदेश, खासकर जटिल या असामान्य मार्कअप और CSS वाले, अभी सही तरह से नहीं दिख सकते हैं।

Flectar Mail ब्राउज़र या WebView जोड़ने के बजाय [Blitz](https://github.com/DioxusLabs/blitz) से ईमेल रेंडर करता है। Blitz, Dioxus टीम का Rust में बना HTML/CSS रेंडरर है। अगर कोई संदेश गलत दिखता है, तो कृपया [उसकी रिपोर्ट करें](https://github.com/flectar/mail/issues)। [रेंडरर और CPU व GPU विकल्पों](../features.md#html-rendering) के बारे में और पढ़ें।

## 📥 Flectar Mail पाएँ

Flectar Mail अभी विकास में है और स्थिर नहीं है। प्रीव्यू बिल्ड [GitHub Releases](https://github.com/flectar/mail/releases) पर उपलब्ध हैं।

> [!WARNING]
> **Google OAuth सत्यापन के लिए हर साल CASA सुरक्षा मूल्यांकन ज़रूरी है।** Flectar एक सामुदायिक ओपन-सोर्स परियोजना है, इसलिए यह मूल्यांकन प्रायोजकों के सहयोग पर निर्भर करता है। जब तक आवश्यक सत्यापन के लिए धन नहीं मिलता और वह पूरा नहीं होता, डिफ़ॉल्ट रूप से कॉन्फ़िगर किए गए Gmail OAuth वाली स्थिर रिलीज़ संभव नहीं होगी। अंतर्निहित Gmail साइन-इन उपलब्ध कराने में मदद के लिए [Flectar को प्रायोजित करें](https://github.com/sponsors/flectar)।

| प्लेटफ़ॉर्म | डाउनलोड | टिप्पणी |
| --- | --- | --- |
| Linux x64 और arm64 | AppImage, Debian/Ubuntu के लिए `.deb`, Fedora 43/44 के लिए `.rpm` या Flatpak पैकेज | Secret Service कीरिंग ज़रूरी है। Flatpak मैन्युअल रूप से इंस्टॉल होने वाला प्रीव्यू है और अभी Flathub पर नहीं है। |
| Windows x64 और arm64 | `.exe` इंस्टॉलर या पोर्टेबल ZIP | प्रीव्यू हस्ताक्षरित नहीं हैं |
| macOS 14+ (Apple silicon या Intel) | DMG या ऐप का ZIP | ad hoc हस्ताक्षरित, नोटराइज़ नहीं |
| Android 8.0+ (arm64) | हस्ताक्षरित APK | प्रीव्यू और स्थिर रिलीज़ की ऐप पहचान एक ही है। |
| iOS 17+ (iPhone और iPad) | बिना हस्ताक्षर वाली `.ipa` | AltStore, SideStore या Sideloadly और अपनी Apple ID से साइडलोड करें। |
| HarmonyOS और OpenHarmony | [springmin](https://github.com/springmin) का [सामुदायिक पोर्ट](../../platform/ohos/README.md) | GitHub रिलीज़ में शामिल नहीं है; इसे स्रोत कोड से बनाएँ। |

OAuth ऐप कुंजियों के बिना बने बिल्ड में Gmail और Outlook साइन-इन बंद रहता है। स्वागत स्क्रीन पर **साइन-इन सेटिंग्स** के गियर आइकन से अपना Google या Microsoft ऐप पंजीकरण जोड़ें, या IMAP, POP3 अथवा JMAP खाता कनेक्ट करें।

[इंस्टॉलेशन गाइड](../install.md) में इंस्टॉल करने के कमांड, Linux कीरिंग और डेस्कटॉप पोर्टल, Flatpak और RPM से जुड़ी बातें, डाउनलोड का सत्यापन, Android हस्ताक्षर, iOS पर साइडलोडिंग और `cargo run --bin flectar-mail` के ज़रिए स्रोत कोड से बिल्ड करना शामिल हैं।

## 🤝 शामिल हों

| विषय | विवरण |
| --- | --- |
| 🌍 **अनुवाद करें** | [Weblate](https://translate.flectar.com/) पर ऐप का अनुवाद करें, मौजूदा अनुवाद सुधारें या अनूदित टेक्स्ट की समीक्षा करें; प्रोग्रामिंग या GitHub के अनुभव की ज़रूरत नहीं है। अनुवाद पुल अनुरोध के रूप में इस रिपॉज़िटरी तक पहुँचते हैं और योगदानकर्ताओं को GitHub पर उनके काम का श्रेय मिलता है। README के अनुवाद यहीं रखे जाते हैं; [अनुवाद में योगदान](../../CONTRIBUTING.md#translations) देखें। |
| ♿ **सुगम्यता** | कीबोर्ड नेविगेशन, हल्की और गहरी थीम तथा लचीले लेआउट; सुगम्यता पर काम जारी है। वर्तमान सुविधाओं, परीक्षण की सीमाओं और बाधा की रिपोर्ट करने के तरीके के लिए [ACCESSIBILITY.md](../../ACCESSIBILITY.md) देखें। |
| 🔒 **सुरक्षा** | सुरक्षा कमज़ोरियों की रिपोर्ट [SECURITY.md](../../SECURITY.md) में बताए गए तरीके से निजी रूप से करें, और उनका विवरण सार्वजनिक इश्यू में न दें। |
| 🌐 **ओपन सोर्स** | Flectar Mail एक ही ओपन-सोर्स ऐप है, जिसका कोई अलग कम्युनिटी संस्करण नहीं है, और यह [GNU Affero General Public License v3](../../LICENSE) के अंतर्गत है। व्यावहारिक जानकारी के लिए [लाइसेंस का अवलोकन](../../LICENSING.md) पढ़ें। |
| 🛠️ **योगदान दें** | परियोजना को आकार देने में कैसे मदद करें, यह [CONTRIBUTING.md](../../CONTRIBUTING.md) में बताया गया है। |

## 🫡 आभार

Flectar Mail इन परियोजनाओं और उनके योगदानकर्ताओं के काम से संभव हुआ है:

- [Slint](https://slint.dev/), वह नेटिव UI टूलकिट जो Flectar Mail के इंटरफ़ेस को चलाता है।
- [Blitz](https://github.com/DioxusLabs/blitz), Dioxus टीम का Rust HTML/CSS रेंडरर जो ईमेल पढ़ने का अनुभव उपलब्ध कराता है।

---
