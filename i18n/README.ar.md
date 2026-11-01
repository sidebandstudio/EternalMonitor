<div align="center">

[English](../README.md) · [Español](README.es.md) · **العربية** · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*إذا اختلفت هذه الصفحة عن [ملف README الإنجليزي](../README.md)، فالنسخة الإنجليزية هي الأحدث.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="شعار EternalMonitor">

# EternalMonitor

شاشة ثانية مجانية ومفتوحة المصدر لنظام Windows.<br>
وصّل iPad عبر Wi-Fi أو USB، مع دعم اللمس وApple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[تنزيل](#download) · [دليل الإعداد](../SETUP.md) · [الموقع](https://eternalmonitor.dev) · [خارطة الطريق](../ROADMAP.md) · [ملاحظات الإصدار](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="كمبيوتر يعمل بنظام Windows وجهاز iPad. في الكمبيوتر، تحول نافذة EternalMonitor جهاز iPad إلى شاشة ثانية، ثم تُسحب نافذة من الشاشة إلى iPad. بعد ذلك يرسم Apple Pencil أعشابًا على لوحة في iPad، بخطوط سميكة عند الضغط ورفيعة عند رفع القلم.">
</p>

يحوّل EternalMonitor جهاز iPad إلى شاشة لكمبيوتر Windows. التطبيق مجاني ومفتوح المصدر بموجب ترخيص MIT ولا يحتاج إلى حساب. يبث الكمبيوتر بمعدل يصل إلى 120 fps، مع ترميز عتادي على بطاقات الرسومات المدعومة وفك ترميز عتادي على iPad. تعود كل لمسة وضغطة مفتاح وخط بالقلم إلى الكمبيوتر.

<a id="download"></a>

## البدء السريع

[![تنزيل لنظام Windows](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![الحصول على تطبيق iPad عبر TestFlight](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **ثبّت تطبيق Windows.** شغّل [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe) على Windows 10 (الإصدار 1809 أو أحدث) أو Windows 11 بمعمارية 64 بت وبطاقة رسومات NVIDIA أو AMD أو Intel. إذا سأل Windows SmartScreen، اختر **مزيد من المعلومات** ثم **التشغيل على أي حال**. تعرض [صفحة الإصدار](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) قيمة SHA-256 للملف.
2. **ثبّت تطبيق iPad.** على iPad يعمل بنظام iPadOS 17 أو أحدث، ثبّت **TestFlight** من App Store، ثم افتح [رابط EternalMonitor على TestFlight](https://testflight.apple.com/join/ppc9QfXc) على iPad واضغط **تثبيت**.
3. **اتصل.** افتح التطبيقين على الشبكة نفسها، ثم اضغط على جهاز الكمبيوتر في iPad أو امسح رمز QR الذي يعرضه الكمبيوتر. لاستخدام USB على Windows 10 22H2 أو Windows 11، وصّل الكابل وأبقِ تطبيق [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) المجاني من Apple مفتوحًا على الكمبيوتر.

يشرح [دليل الإعداد](../SETUP.md) كل خطوة مع أسماء الأزرار الدقيقة. تعرض [صفحة التنزيل](https://eternalmonitor.dev/download.html) إصدارات Windows وiPad التي تعمل معًا.

إذا كان EternalMonitor مفيدًا لك، فإن إضافة نجمة تساعد الآخرين على العثور عليه.

## الميزات

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="شاشة الاتصال في تطبيق iPad وبها اتصال ومسح رمز QR والبحث عن أجهزة الكمبيوتر">

- **التوسيع أو انعكاس الشاشة.** أضف iPad شاشة ثانية بجانب شاشتك، أو اعكس أي شاشة لديك. تبقى الشاشة الثانية لمدة 10 ثوانٍ بعد قطع اتصال iPad، فتُستخدم مجددًا عند إعادة الاتصال سريعًا ولا يحتفظ Windows بشاشة وهمية.
- **عبر Wi-Fi أو USB.** ابث عبر شبكتك المنزلية أو وصّل كابلًا لصورة أكثر استقرارًا، ويمكنه أيضًا شحن iPad من منفذ USB-C. يتولى الكابل جلسة Wi-Fi الجارية، ويعود الاتصال إلى Wi-Fi عند فصله.
- **قلم حقيقي مع Apple Pencil.** يتلقى Windows الضغط والميل عبر Windows Ink، ويظهر القلم تحت رأسه مهما كان ترتيب الشاشات. يتجاهل **وضع الرسم** (Drawing mode) راحة يدك على مساحة الرسم، ويعمل Clip Studio Paint مع إعداد Tablet PC.
- **اللمس ولوحة المفاتيح والمؤشر.** اضغط للنقر، واسحب، ومرر بإصبعين، واضغط مطولًا للنقر بزر الفأرة الأيمن. تعمل أيضًا لوحات المفاتيح الفعلية ولوحات اللمس والفأرات، مع تثبيت مفاتيح التعديل والنقر الأوسط.
- **الترميز العتادي.** ترمّز بطاقات NVIDIA وAMD وIntel المدعومة البث بمعدل 30 أو 60 أو 90 أو 120 fps.
- **إصلاح الفقد على Wi-Fi.** عندما تفقد حزمة، يطلبها iPad مجددًا فورًا، لذلك نادرًا ما يؤدي فقد حزمة إلى فقد إطار. يتكيف معدل البت مع شبكتك.
- **صوت الكمبيوتر.** يُشغّل الصوت على iPad ويمكنك كتمه دون إيقاف الفيديو.
- **ملاءمة أو ملء.** يعرض خيار **ملاءمة** (Fit) صورة الكمبيوتر كاملة مع أشرطة عند اختلاف الأبعاد. يستخدم خيار **ملء** (Fill) شاشة iPad كلها ويقص الحواف التي لا تتسع لها.
- **الاقتران.** يطلب أول اتصال عبر Wi-Fi رمزًا من ستة أرقام أو مسح رمز QR. بعد ذلك يتذكر iPad الكمبيوتر. لا يحتاج USB إلى رمز.
- **الاستعادة والتوضيح.** تعود الصورة تلقائيًا بعد شاشة القفل أو تغيير الشاشة أو إعادة تشغيل البث. يوضح التطبيقان ما حدث وما يجب فعله، ويُعد خيار **إرسال ملاحظات** (Send feedback) رسالة بريد إلى الاستوديو تتضمن تفاصيل إعدادك.
- **التحديث بنقرة واحدة.** تنبهك نافذة منبثقة عند صدور نسخة جديدة. في Windows يتحقق خيار **التحديث الآن** (Update now) من التنزيل ويثبته بمطالبة واحدة من Windows؛ وعلى iPad يفتح TestFlight. يعمل تطبيق Windows دون نافذة وحدة تحكم.

يتوفر ملف README هذا و[الموقع](https://eternalmonitor.dev) بـ11 لغة.

<br clear="right">

## اعرف المزيد

- [استخدام iPad شاشة ثانية لنظام Windows](https://eternalmonitor.dev/ipad-second-monitor-windows.html): الدليل الكامل.
- [التوافق](https://eternalmonitor.dev/compatibility.html): أجهزة الكمبيوتر وiPad التي يستخدم الأشخاص EternalMonitor عليها.
- [البدائل](https://eternalmonitor.dev/alternatives.html): مقارنة EternalMonitor بالطرق الأخرى لاستخدام iPad شاشةً.
- [خارطة الطريق](../ROADMAP.md): ما سيأتي لاحقًا.

**شارك إعدادك.** أخبرنا عن جهاز الكمبيوتر وiPad اللذين تستخدمهما عبر [تقرير التوافق](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml)، أو انشره على [خادم Sideband في Discord](https://discord.gg/Xc7NykBZ6z). بإذنك، سيظهر على صفحة التوافق.

## الحالة

الإصدار الحالي هو v0.3.3 لنظام Windows وv0.3.3 (build 364) لجهاز iPad. يتيح **شاشة ثانية** (Second screen) على مزيد من الحواسيب المحمولة ذات بطاقات Intel وNVIDIA: يعرض iPad شاشة الكمبيوتر الرئيسية فورًا، ثم ينتقل إلى الشاشة الثانية عندما يضيفها Windows. يشرح [RELEASE_NOTES.md](../RELEASE_NOTES.md) كل إصدار، ويعرض [ROADMAP.md](../ROADMAP.md) ما سيأتي لاحقًا.

اجتازت اختبارات الإصدار v0.3.3 على Windows وmacOS ومحاكيات iPad. يسجل [docs/hardware-verification.md](../docs/hardware-verification.md) نتائج الاختبارات العملية على كمبيوتر وiPad فعليين منذ v0.3.0.

تتضمن تشخيصات Windows وiPad تفاصيل الجهاز وأخطاء التطبيق وإحصاءات البث. تشمل التقارير أسماء بطاقات الرسوميات وشاشات العرض وعناوين الشبكة، لكنها تستثني محتوى الشاشة والصوت وبيانات الاقتران. بدءًا من الإصدار 0.3.4، تزيل البيانات المرسلة من Windows اسم الكمبيوتر واسم المستخدم ومجلد المستخدم واسم iPad وأسماء أجهزة الصوت، مع الاحتفاظ بنوع كل جهاز صوت فقط، بينما تزيل البيانات المرسلة من iPad اسم الكمبيوتر. يمكنك تعطيل المشاركة التلقائية في الإعدادات. تُحتفظ التشخيصات لمدة تصل إلى 30 يومًا وقد تُحذف قبل ذلك. قد تبقى التشخيصات المحذوفة أو المنتهية في النسخ الاحتياطية اليومية لمدة تصل إلى 7 أيام إضافية. راجع [التشخيصات التلقائية](../SETUP.md#automatic-diagnostics) و[إشعار الخصوصية](https://eternalmonitor.dev/privacy.html). على iPad، افتح **الإعدادات** (Settings) > **الدعم** (Support) > **التشخيصات** (Diagnostics) لنسخ تقرير أو تصديره.

## كيف يعمل

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="يلتقط كمبيوتر Windows سطح المكتب ويرمّز الفيديو ببطاقة الرسومات ويرسله عبر Wi-Fi أو USB. يعيد iPad تجميع الفيديو ويطلب الأجزاء المفقودة عبر Wi-Fi، ثم يفك ترميزه باستخدام VideoToolbox ويعرضه باستخدام Metal. يُلتقط صوت الكمبيوتر باستخدام WASAPI ويُرمّز بصيغة Opus ويُشغّل على iPad. تعود إدخالات اللمس والقلم ولوحة المفاتيح ولوحة اللمس إلى الكمبيوتر ليحقنها.">
</picture>

كُتب مضيف Windows بلغة Rust. يلتقط سطح المكتب باستخدام DXGI، ويرمّز الفيديو عبر FFmpeg باستخدام NVIDIA NVENC أو AMD AMF أو Intel Quick Sync على بطاقات الرسومات المدعومة (H.264 افتراضيًا أو HEVC)، ويرسل كل إطار في أجزاء UDP مرقمة. عبر USB تنتقل الرسائل نفسها في نفق مؤطر توفره خدمة أجهزة Apple. تطبيق iPad أصلي ومكتوب بلغة Swift. يعيد تجميع الأجزاء ويفك ترميزها باستخدام VideoToolbox ويعرضها باستخدام Metal. وعبر Wi-Fi يطلب أيضًا الأجزاء المفقودة. يُلتقط صوت الكمبيوتر باستخدام WASAPI ويُرسل بصيغة Opus، ويشغّله iPad عبر مخزن لتفاوت وصول الحزم ويخفي الصوت المفقود بدلًا من طلبه مجددًا. تعود الإدخالات وتقارير المستقبِل عبر الجلسة نفسها، ويستخدم المضيف التقارير لتكييف معدل البت بين 4 و50 Mbps.

يصف [ARCHITECTURE.md](../ARCHITECTURE.md) مسار المعالجة والبروتوكول v2، ويوضح [docs/decisions.md](../docs/decisions.md) سبب عمل كل جزء بهذه الطريقة.

## البناء من المصدر

تحتوي مساحة العمل على مضيف Rust (`host/`) وحزمة البروتوكول `eternal-wire` (`proto/`) وتطبيق iPad المكتوب بلغة Swift (`ios/`).

<details>
<summary><b>مضيف Windows</b></summary>

المتطلبات: Windows 10 الإصدار 1809 أو أحدث، وRust 1.98.0 (المثبت بواسطة `rust-toolchain.toml`، مع MSVC)، وحزمة تطوير FFmpeg **7.1 shared**، وLLVM/libclang من أجل bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

المضيف برنامج Windows بواجهة رسومية، لذا لا يفتح نافذة وحدة تحكم. عند تشغيله من الطرفية يطبع سجله فيها، لكن يعود موجّه الأوامر فورًا؛ أما `cargo run -p eternal-host` فينتظره. عند استخدام `ETERNAL_HEADLESS=1` وإعادة توجيه المخرجات (`> host.log`) يحصل على وحدة تحكم خاصة به، فلا يصل إليه Ctrl+C في الطرفية: اضغط Ctrl+C في نافذة وحدة التحكم الخاصة به أو استخدم إدارة المهام. يوقف `scripts\win\Stop-Host.ps1` المضيفين الذين شغّلهم `remote.sh run-host` فقط.

يبني `scripts\build-installer.ps1` ملف Setup.exe الكامل باستخدام Inno Setup و`FFMPEG_DIR` نفسه. ويبني `scripts\package.ps1` ملف zip مجردًا.

</details>

<details>
<summary><b>دورة التطوير على macOS</b> (دون الحاجة إلى Windows)</summary>

على macOS يستبدل المضيف التقاط الشاشة بنمط اختبار مولّد. تعمل مكونات البروتوكول والترميز والنقل والمراقبة فعليًا.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

إذا كانت أدوات سطر الأوامر في Xcode هي مجلد المطور المحدد، فاسبق أوامر Xcode بـ `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

</details>

<details>
<summary><b>تطبيق iPad</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

للتشغيل على iPad فعلي، افتح المشروع المُنشأ في Xcode واختر فريق التوقيع الخاص بك.

</details>

<details>
<summary><b>اختبارات شاملة</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

يتحقق كل صف من معدل الإطارات المفكوك ترميزها (55 fps على الأقل) والإطارات المفقودة والإصلاحات والبكسلات المعروضة. يشغّل `e2e_matrix.sh --real` صفوف أجهزة Windows عبر المشغّل في `scripts/win/`؛ اقرأ [scripts/win/README.md](../scripts/win/README.md) أولًا. يشغّل CI مجموعات Rust على Linux وmacOS وWindows، واختبارات iPad واختبار نظام البث عند كل طلب سحب.

</details>

<details>
<summary><b>متغيرات بيئة المضيف</b></summary>

| المتغير | التأثير |
| --- | --- |
| `ETERNAL_ENCODER` | فرض مرمّز (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | تتجاوز `1` أو `0` تفضيل HEVC |
| `ETERNAL_FPS` | تجاوز معدل إطارات المضيف؛ يظل تفضيل iPad حدًا أقصى له |
| `ETERNAL_INPUT` | مدخل المرمّز: `yuv420` (الافتراضي) أو `auto` أو `bgra` |
| `ETERNAL_MAX_DGRAM` | حجم مخطط بيانات الوسائط، من 576 إلى 1400 بايت |
| `ETERNAL_CAPTURE` | يستبدل `synthetic` ‏DXGI بنمط اختبار مولّد |
| `ETERNAL_AUDIO` | يختار `synthetic` مصدر اختبار صوت حتميًا |
| `ETERNAL_HEADLESS` | تشغّل `1` التطبيق دون نافذة؛ ويوقفه Ctrl+C بشكل سليم |
| `ETERNAL_ABR` | تعطّل `0` معدل البت التكيفي |
| `ETERNAL_VDD_TIMEOUT_SECS` | مدة انتظار إرفاق الشاشة الافتراضية (الافتراضي 30) |
| `ETERNAL_AMF_DIAG` | تكتب `1` تشخيصات تدفق بت AMF |
| `ETERNAL_LEGACY_PTS` | تعيد `1` الطوابع الزمنية القديمة لعداد الإطارات |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | للاختبار فقط: إدخال فقد أو إعادة ترتيب أو تفاوت في الوصول |
| `ETERNAL_USB_DIRECT` | للاختبار فقط: نقطة اتصال TCP بدلًا من نفق USB |

</details>

## الوثائق

| المستند | المحتوى |
| --- | --- |
| [SETUP.md](../SETUP.md) | إعداد خطوة بخطوة للجميع، بما في ذلك USB والرسم |
| [المشكلات المعروفة](https://eternalmonitor.dev/known-issues.html) | المشكلات المعروفة وما يمكن فعله حيال كل منها (`docs/known-issues.html`) |
| [الخصوصية](https://eternalmonitor.dev/privacy.html) | ما ترسله التطبيقات وسببه ومدة الاحتفاظ به وكيفية إيقافه (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | مسار المعالجة والبروتوكول v2 وكل مكوّن |
| [docs/decisions.md](../docs/decisions.md) | أسباب عمل المكونات بهذه الطريقة، مع القياسات |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | معلومات عن Apple Pencil وWindows Ink وClip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | تفاصيل الميزات |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | سجل اختبارات الإصدار وقائمة التحقق العملية |
| [اختبار الإصدار](../docs/beta-testing.md) | نشر بناء عبر TestFlight: التوقيع وما يجب اختباره |
| [docs/design.md](../docs/design.md) | المظهر المشترك للتطبيقات والموقع |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | التغييرات في كل إصدار |
| [ROADMAP.md](../ROADMAP.md) | ما سيأتي لاحقًا |

## المساهمة

نرحب بكل أنواع المساهمات: بلاغات المشكلات والأفكار والإصلاحات والوثائق والترجمات والميزات الجديدة. نرحب بطلبات السحب، كبيرة كانت أم صغيرة. يشرح [CONTRIBUTING.md](../CONTRIBUTING.md) كيفية إرسال طلب ويسرد مساهمات مناسبة للبدء، ويوضح [ROADMAP.md](../ROADMAP.md) الأعمال الأكبر القادمة. نرحب بالأسئلة والأفكار على [خادم Sideband في Discord](https://discord.gg/Xc7NykBZ6z).

## الاتصال

يتيح [خادم Sideband في Discord](https://discord.gg/Xc7NykBZ6z) طرح الأسئلة وطلب المساعدة في الإعداد ومشاركة الأفكار. يمكنك أيضًا مراسلتنا على [hello@sideband.studio](mailto:hello@sideband.studio).

هل وجدت خطأ؟ [افتح بلاغًا](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) يتضمن إصداري التطبيقين وسجل المضيف، أو استخدم [نموذج الملاحظات في صفحة التنزيل](https://eternalmonitor.dev/download.html#feedback) الذي يرسل بريدًا إلى الاستوديو. أبلغ عن المشكلات الأمنية بصفة خاصة كما هو موضح في [SECURITY.md](../SECURITY.md)، وليس على Discord أو في بلاغ.

## الترخيص

يُصدر EternalMonitor بموجب [MIT License](../LICENSE)، © 2026 Sideband Studio. يتضمن برنامج تثبيت Windows أيضًا مكونات خارجية تخضع لتراخيصها الخاصة، بما فيها [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) وFFmpeg.

من تطوير [Sideband Studio](https://sideband.studio).
