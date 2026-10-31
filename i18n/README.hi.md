<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · **हिन्दी** · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*इस पृष्ठ और [अंग्रेज़ी README](../README.md) में अंतर होने पर अंग्रेज़ी संस्करण नवीनतम है।*

<img src="../docs/assets/icon.png" width="88" height="88" alt="EternalMonitor का लोगो">

# EternalMonitor

Windows के लिए मुफ़्त और ओपन सोर्स दूसरा मॉनिटर।<br>
अपने iPad को Wi-Fi या USB से कनेक्ट करें, टच और Apple Pencil के समर्थन के साथ।

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[डाउनलोड करें](#download) · [सेटअप गाइड](../SETUP.md) · [वेबसाइट](https://eternalmonitor.dev) · [रोडमैप](../ROADMAP.md) · [रिलीज़ नोट्स](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="एक Windows PC और एक iPad। PC पर EternalMonitor विंडो iPad को दूसरी स्क्रीन पर बदलती है और एक विंडो मॉनिटर से iPad पर खींची जाती है। फिर Apple Pencil iPad पर एक चित्र में घास बनाता है, दबाने पर मोटे और उठाने पर पतले स्ट्रोक के साथ।">
</p>

EternalMonitor iPad को आपके Windows PC का मॉनिटर बनाता है। यह मुफ़्त है, MIT लाइसेंस के तहत ओपन सोर्स है और इसके लिए खाते की ज़रूरत नहीं है। PC अधिकतम 120 fps पर स्ट्रीम करता है, समर्थित ग्राफ़िक्स कार्ड पर हार्डवेयर एनकोडिंग और iPad पर हार्डवेयर डिकोडिंग के साथ। हर टैप, कीस्ट्रोक और Pencil स्ट्रोक वापस PC पर जाता है।

<a id="download"></a>

## तुरंत शुरू करें

[![Windows के लिए डाउनलोड करें](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![TestFlight पर iPad ऐप पाएँ](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **Windows ऐप इंस्टॉल करें।** NVIDIA, AMD या Intel ग्राफ़िक्स वाले 64-bit Windows 10 (संस्करण 1809 या नया) या Windows 11 पर [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe) चलाएँ। Windows SmartScreen पूछे तो **अधिक जानकारी**, फिर **फिर भी चलाएँ** चुनें। फ़ाइल का SHA-256 [रिलीज़ पृष्ठ](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) पर है।
2. **iPad ऐप इंस्टॉल करें।** iPadOS 17 या नए संस्करण वाले iPad पर App Store से **TestFlight** इंस्टॉल करें। फिर iPad पर [EternalMonitor TestFlight लिंक](https://testflight.apple.com/join/ppc9QfXc) खोलें और **इंस्टॉल करें** पर टैप करें।
3. **कनेक्ट करें।** दोनों ऐप एक ही नेटवर्क पर खोलें। iPad पर अपना PC टैप करें या PC पर दिखाया QR कोड स्कैन करें। Windows 10 22H2 या Windows 11 पर USB के लिए केबल लगाएँ और Apple का मुफ़्त [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) ऐप PC पर खुला रखें।

[सेटअप गाइड](../SETUP.md) हर चरण में बटन के सटीक नाम बताती है। [डाउनलोड पृष्ठ](https://eternalmonitor.dev/download.html) साथ काम करने वाले Windows और iPad संस्करण बताता है।

अगर EternalMonitor आपके काम आता है, तो एक स्टार दूसरों को इसे खोजने में मदद करता है।

## सुविधाएँ

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="iPad ऐप की कनेक्ट स्क्रीन पर कनेक्ट करें, QR कोड स्कैन करें और PC खोजें">

- **बढ़ाएँ या मिरर करें।** iPad को अपने मॉनिटर के बगल में दूसरी स्क्रीन बनाएँ या किसी भी स्क्रीन को मिरर करें। iPad के डिस्कनेक्ट होने के बाद दूसरी स्क्रीन 10 सेकंड रहती है। जल्दी दोबारा कनेक्ट करने पर वही डिस्प्ले इस्तेमाल होता है और Windows में बेकार मॉनिटर नहीं रह जाता।
- **Wi-Fi या USB।** अपने घरेलू नेटवर्क पर स्ट्रीम करें या अधिक स्थिर तस्वीर के लिए केबल लगाएँ, जिससे USB-C पोर्ट से iPad चार्ज भी हो सकता है। केबल चालू Wi-Fi सत्र संभाल लेती है और उसे निकालने पर Wi-Fi फिर इस्तेमाल होता है।
- **Apple Pencil असली पेन की तरह।** Windows को Windows Ink का दबाव और झुकाव मिलता है, और किसी भी मॉनिटर व्यवस्था में पेन का निशान नोक के नीचे पड़ता है। **ड्रॉइंग मोड** (Drawing mode) कैनवास पर हथेली को अनदेखा करता है और Clip Studio Paint अपनी "Tablet PC" सेटिंग के साथ काम करता है।
- **टच, कीबोर्ड और पॉइंटर।** क्लिक करने के लिए टैप करें, ड्रैग करें, दो उँगलियों से स्क्रॉल करें और राइट-क्लिक के लिए दबाकर रखें। हार्डवेयर कीबोर्ड, ट्रैकपैड और माउस भी काम करते हैं, साथ में स्टिकी मॉडिफ़ायर और मिडिल-क्लिक भी।
- **हार्डवेयर एनकोडिंग।** समर्थित NVIDIA, AMD और Intel ग्राफ़िक्स स्ट्रीम को 30, 60, 90 या 120 fps पर एनकोड करते हैं।
- **Wi-Fi पर खोए डेटा की मरम्मत।** पैकेट खोने पर iPad तुरंत उसे फिर माँगता है, इसलिए एक पैकेट खोने से शायद ही कोई फ़्रेम छूटता है। बिटरेट नेटवर्क के अनुसार बदलता है।
- **PC ऑडियो।** आवाज़ iPad पर चलती है और वीडियो रोके बिना आप इसे म्यूट कर सकते हैं।
- **फ़िट या फ़िल।** **फ़िट** (Fit) पूरी PC तस्वीर दिखाता है, आकार अलग होने पर खाली पट्टियों के साथ। **फ़िल** (Fill) iPad की पूरी स्क्रीन इस्तेमाल करता है और बाहर के किनारे काट देता है।
- **पेयरिंग।** पहली Wi-Fi कनेक्शन पर छह अंकों का कोड या QR स्कैन चाहिए। उसके बाद iPad PC को याद रखता है। USB के लिए कोड नहीं चाहिए।
- **फिर चालू होता है और समस्या बताता है।** लॉक स्क्रीन, डिस्प्ले बदलने या स्ट्रीम दोबारा शुरू होने के बाद तस्वीर अपने आप लौटती है। दोनों ऐप बताते हैं कि क्या हुआ और क्या करें, और **फ़ीडबैक भेजें** (Send feedback) आपके सेटअप की जानकारी भरकर स्टूडियो को ईमेल लिखता है।
- **एक क्लिक में अपडेट।** नया संस्करण आने पर पॉपअप बताता है। Windows पर **अभी अपडेट करें** (Update now) डाउनलोड जाँचता है और एक Windows प्रॉम्प्ट के साथ इंस्टॉल करता है; iPad पर यह TestFlight खोलता है। Windows ऐप बिना कंसोल विंडो के चलता है।

यह README और [वेबसाइट](https://eternalmonitor.dev) 11 भाषाओं में उपलब्ध हैं।

<br clear="right">

## और जानें

- [Windows के लिए iPad को दूसरे मॉनिटर की तरह इस्तेमाल करें](https://eternalmonitor.dev/ipad-second-monitor-windows.html): पूरी गाइड।
- [संगतता](https://eternalmonitor.dev/compatibility.html): वे PC और iPad जिन पर लोग EternalMonitor चलाते हैं।
- [विकल्प](https://eternalmonitor.dev/alternatives.html): iPad को स्क्रीन की तरह इस्तेमाल करने के दूसरे तरीकों से EternalMonitor की तुलना।
- [रोडमैप](../ROADMAP.md): आगे क्या आने वाला है।

**अपना सेटअप साझा करें।** [संगतता रिपोर्ट](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml) में बताएँ कि आप कौन से PC और iPad इस्तेमाल करते हैं, या [Sideband Discord](https://discord.gg/Xc7NykBZ6z) पर पोस्ट करें। आपकी अनुमति से वह संगतता पृष्ठ पर दिखाई देगा।

## स्थिति

मौजूदा रिलीज़ Windows के लिए v0.3.3 और iPad के लिए v0.3.3 (build 364) है। यह Intel और NVIDIA ग्राफ़िक्स वाले अधिक लैपटॉप पर **दूसरी स्क्रीन** (Second screen) लाता है: iPad तुरंत PC की मुख्य स्क्रीन दिखाता है और Windows के दूसरी स्क्रीन जोड़ने पर उस पर बदल जाता है। [RELEASE_NOTES.md](../RELEASE_NOTES.md) हर संस्करण और [ROADMAP.md](../ROADMAP.md) आगे की योजना बताता है।

v0.3.3 रिलीज़ जाँच Windows, macOS और iPad सिमुलेटर पर पास हुई। [docs/hardware-verification.md](../docs/hardware-verification.md) में v0.3.0 से वास्तविक PC और iPad पर हुए परीक्षणों के परिणाम हैं।

समस्याएँ खोजने और ठीक करने के लिए दोनों ऐप डायग्नोस्टिक्स साझा करते हैं: डिवाइस विवरण, ऐप की त्रुटियाँ और स्ट्रीम आँकड़े, जिनमें PC और iPad के नाम, ऑडियो डिवाइस के नाम और नेटवर्क पते शामिल हैं। डायग्नोस्टिक्स में स्क्रीन सामग्री, ऑडियो या पेयरिंग क्रेडेंशियल कभी नहीं होते। सेटिंग्स में साझाकरण बंद करें। [स्वचालित डायग्नोस्टिक्स](../SETUP.md#automatic-diagnostics) और [गोपनीयता सूचना](https://eternalmonitor.dev/privacy.html) में बताया है कि क्या भेजा जाता है और कितने समय रखा जाता है। iPad पर रिपोर्ट कॉपी या एक्सपोर्ट करने के लिए सेटिंग्स > सहायता > डायग्नोस्टिक्स खोलें।

## यह कैसे काम करता है

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Windows PC डेस्कटॉप कैप्चर करता है, GPU पर वीडियो एनकोड करके Wi-Fi या USB से भेजता है। iPad वीडियो के टुकड़े जोड़ता है, Wi-Fi पर गायब टुकड़े माँगता है, VideoToolbox से डिकोड करता है और Metal से दिखाता है। PC ऑडियो WASAPI से कैप्चर होता है, Opus में एनकोड होता है और iPad पर चलता है। टच, Pencil, कीबोर्ड और ट्रैकपैड का इनपुट PC पर वापस जाता है, जो उसे लागू करता है।">
</picture>

Windows होस्ट Rust में लिखा है। यह DXGI से डेस्कटॉप कैप्चर करता है, समर्थित ग्राफ़िक्स हार्डवेयर पर FFmpeg के ज़रिए NVIDIA NVENC, AMD AMF या Intel Quick Sync से एनकोड करता है (डिफ़ॉल्ट रूप से H.264 या HEVC) और हर फ़्रेम को क्रमांकित UDP टुकड़ों में भेजता है। USB पर वही संदेश Apple की डिवाइस सेवा की फ़्रेम वाली टनल से जाते हैं। iPad ऐप नेटिव Swift में है। यह टुकड़े जोड़ता है, VideoToolbox से डिकोड करता है और Metal से रेंडर करता है। Wi-Fi पर यह गायब टुकड़े भी माँगता है। PC ऑडियो WASAPI से कैप्चर होकर Opus में भेजा जाता है और iPad उसे जिटर बफ़र से चलाता है और खोए ऑडियो को दोबारा माँगने के बजाय उसकी कमी छिपाता है। इनपुट और रिसीवर रिपोर्ट उसी सत्र में वापस जाते हैं और होस्ट उन रिपोर्ट से बिटरेट 4 से 50 Mbps के बीच बदलता है।

[ARCHITECTURE.md](../ARCHITECTURE.md) पाइपलाइन और प्रोटोकॉल v2 का वर्णन करता है और [docs/decisions.md](../docs/decisions.md) हर हिस्से के काम करने के तरीके का कारण बताता है।

## सोर्स से बिल्ड करें

वर्कस्पेस में Rust होस्ट (`host/`), प्रोटोकॉल crate `eternal-wire` (`proto/`) और Swift iPad ऐप (`ios/`) हैं।

<details>
<summary><b>Windows होस्ट</b></summary>

ज़रूरतें: Windows 10 संस्करण 1809 या नया, Rust 1.98.0 (`rust-toolchain.toml` में निर्धारित, MSVC), FFmpeg **7.1 shared** SDK और bindgen के लिए LLVM/libclang।

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

होस्ट Windows GUI प्रोग्राम है, इसलिए कंसोल विंडो नहीं खोलता। टर्मिनल से शुरू करने पर वह वहाँ लॉग लिखता है, लेकिन प्रॉम्प्ट तुरंत लौट आता है; `cargo run -p eternal-host` उसके लिए इंतज़ार करता है। `ETERNAL_HEADLESS=1` और रीडायरेक्टेड आउटपुट (`> host.log`) के साथ उसे अपना कंसोल मिलता है, इसलिए टर्मिनल का Ctrl+C उस तक नहीं पहुँचता: उस कंसोल की विंडो में Ctrl+C दबाएँ या Task Manager इस्तेमाल करें। `scripts\win\Stop-Host.ps1` केवल `remote.sh run-host` से शुरू हुए होस्ट रोकता है।

`scripts\build-installer.ps1` Inno Setup और उसी `FFMPEG_DIR` से पूरा Setup.exe बनाता है। `scripts\package.ps1` एक साधारण zip बनाता है।

</details>

<details>
<summary><b>macOS डेवलपमेंट प्रक्रिया</b> (Windows की ज़रूरत नहीं)</summary>

macOS पर होस्ट स्क्रीन कैप्चर की जगह कृत्रिम परीक्षण पैटर्न इस्तेमाल करता है। प्रोटोकॉल, एनकोडर, ट्रांसपोर्ट और सुपरवाइज़र वास्तव में चलते हैं।

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

अगर Xcode के कमांड लाइन टूल चयनित डेवलपर डायरेक्टरी हैं, तो Xcode कमांड से पहले `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` लगाएँ।

</details>

<details>
<summary><b>iPad ऐप</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

वास्तविक iPad पर चलाने के लिए Xcode में बना हुआ प्रोजेक्ट खोलें और अपनी signing team चुनें।

</details>

<details>
<summary><b>एंड-टू-एंड परीक्षण</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

हर पंक्ति डिकोडेड फ़्रेम रेट (कम से कम 55 fps), ड्रॉप, मरम्मत और रेंडर किए पिक्सेल जाँचती है। `e2e_matrix.sh --real` Windows हार्डवेयर पंक्तियाँ `scripts/win/` के रनर से चलाता है; पहले [scripts/win/README.md](../scripts/win/README.md) पढ़ें। CI हर pull request पर Linux, macOS और Windows में Rust suites, iPad परीक्षण और स्ट्रीमिंग सिस्टम परीक्षण चलाता है।

</details>

<details>
<summary><b>होस्ट के environment variables</b></summary>

| Variable | Effect |
| --- | --- |
| `ETERNAL_ENCODER` | एनकोडर तय करें (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` या `0` HEVC प्राथमिकता बदलता है |
| `ETERNAL_FPS` | होस्ट फ़्रेम रेट बदलें; iPad की प्राथमिकता अब भी अधिकतम सीमा तय करती है |
| `ETERNAL_INPUT` | एनकोडर इनपुट: `yuv420` (डिफ़ॉल्ट), `auto` या `bgra` |
| `ETERNAL_MAX_DGRAM` | मीडिया datagram का आकार, 576 से 1400 bytes |
| `ETERNAL_CAPTURE` | `synthetic` DXGI की जगह बनाया हुआ परीक्षण पैटर्न इस्तेमाल करता है |
| `ETERNAL_AUDIO` | `synthetic` निश्चित ऑडियो परीक्षण स्रोत चुनता है |
| `ETERNAL_HEADLESS` | `1` बिना विंडो के चलाता है; Ctrl+C साफ़ तरीके से बंद करता है |
| `ETERNAL_ABR` | `0` अनुकूली बिटरेट बंद करता है |
| `ETERNAL_VDD_TIMEOUT_SECS` | वर्चुअल डिस्प्ले जुड़ने का इंतज़ार (डिफ़ॉल्ट 30) |
| `ETERNAL_AMF_DIAG` | `1` AMF बिटस्ट्रीम डायग्नोस्टिक्स लिखता है |
| `ETERNAL_LEGACY_PTS` | `1` पुराने फ़्रेम काउंटर टाइमस्टैम्प लौटाता है |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | केवल परीक्षण: लॉस, क्रम परिवर्तन या जिटर डालें |
| `ETERNAL_USB_DIRECT` | केवल परीक्षण: USB टनल की जगह TCP endpoint |

</details>

## दस्तावेज़

| दस्तावेज़ | विषय |
| --- | --- |
| [SETUP.md](../SETUP.md) | सभी के लिए चरण-दर-चरण सेटअप, USB और ड्रॉइंग सहित |
| [ज्ञात समस्याएँ](https://eternalmonitor.dev/known-issues.html) | ज्ञात समस्याएँ और उनके समाधान (`docs/known-issues.html`) |
| [गोपनीयता](https://eternalmonitor.dev/privacy.html) | ऐप क्या और क्यों भेजते हैं, कितने समय रखा जाता है और कैसे बंद करें (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | पाइपलाइन, प्रोटोकॉल v2 और हर घटक |
| [docs/decisions.md](../docs/decisions.md) | माप के साथ डिज़ाइन के कारण |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil, Windows Ink और Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | सुविधाओं का विवरण |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | रिलीज़ परीक्षण का रिकॉर्ड और व्यावहारिक जाँच सूची |
| [रिलीज़ परीक्षण](../docs/beta-testing.md) | TestFlight से बिल्ड जारी करना: signing और क्या जाँचना है |
| [docs/design.md](../docs/design.md) | ऐप और वेबसाइट का साझा रूप |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | हर संस्करण में बदलाव |
| [ROADMAP.md](../ROADMAP.md) | आगे क्या आने वाला है |

## योगदान

हर तरह के योगदान का स्वागत है: issue, विचार, सुधार, दस्तावेज़, अनुवाद और नए फ़ीचर। छोटे हों या बड़े, pull request का स्वागत है। [CONTRIBUTING.md](../CONTRIBUTING.md) बताता है कि pull request कैसे भेजें और शुरुआत के लिए अच्छे योगदान कौन से हैं। [ROADMAP.md](../ROADMAP.md) आगे के बड़े काम दिखाता है। [Sideband Discord](https://discord.gg/Xc7NykBZ6z) पर सवालों और विचारों का स्वागत है।

## संपर्क

सवाल, सेटअप में मदद और विचारों के लिए [Sideband Discord](https://discord.gg/Xc7NykBZ6z) पर आएँ। आप [hello@sideband.studio](mailto:hello@sideband.studio) पर ईमेल भी कर सकते हैं।

बग मिला? दोनों ऐप के संस्करण और होस्ट लॉग के साथ [issue खोलें](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) या [डाउनलोड पृष्ठ के फ़ीडबैक फ़ॉर्म](https://eternalmonitor.dev/download.html#feedback) का इस्तेमाल करें, जो स्टूडियो को ईमेल भेजता है। सुरक्षा समस्याएँ Discord या issue में नहीं, बल्कि [SECURITY.md](../SECURITY.md) में बताए अनुसार निजी तौर पर रिपोर्ट करें।

## लाइसेंस

EternalMonitor [MIT License](../LICENSE) के तहत जारी है, © 2026 Sideband Studio। Windows इंस्टॉलर में दूसरे पक्षों के घटक भी उनके अपने लाइसेंस के तहत शामिल हैं, जिनमें [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) और FFmpeg हैं।

[Sideband Studio](https://sideband.studio) द्वारा बनाया गया।
