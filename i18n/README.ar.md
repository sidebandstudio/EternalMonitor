<div align="center">

[English](../README.md) · [Español](README.es.md) · **العربية** · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*إذا اختلفت هذه الصفحة عن [ملف README الإنجليزي](../README.md)، فالنسخة الإنجليزية هي الأحدث.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="شعار EternalMonitor">

# EternalMonitor

اعكس سطح مكتب Windows أو وسّعه على iPad عبر Wi-Fi أو USB،<br>
وتحكم في الكمبيوتر باللمس أو لوحة المفاتيح أو Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#تنزيل)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[تنزيل](#تنزيل) · [دليل الإعداد](../SETUP.md) · [المشكلات المعروفة](https://eternalmonitor.dev/known-issues.html) · [الموقع](https://eternalmonitor.dev) · [ملاحظات الإصدار](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="تطبيق EternalMonitor على Windows يبث إلى iPad يعرض صورة الكمبيوتر">
</p>

يبث EternalMonitor سطح مكتب Windows إلى iPad ويرسل إدخالاتك إلى الكمبيوتر. يلتقط تطبيق Windows صغير الشاشة ويرمّزها باستخدام بطاقة الرسومات. يفك تطبيق iPad ترميز الفيديو عتاديًا ويرسمه باستخدام Metal، وتعود كل لمسة وضغطة مفتاح وخط بالقلم إلى الكمبيوتر. التطبيق مجاني ومفتوح المصدر بموجب MIT License ولا يحتاج إلى حساب.

## الميزات

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="شاشة الاتصال في تطبيق iPad وبها اتصال ومسح رمز QR والبحث عن أجهزة الكمبيوتر">

- **التوسيع أو انعكاس الشاشة.** أضف iPad شاشة إضافية بجانب شاشتك، أو اعكس أي شاشة لديك. تختفي الشاشة الإضافية بعد 10 ثوانٍ من قطع اتصال iPad، فتُستخدم مجددًا عند إعادة الاتصال سريعًا ولا يحتفظ Windows بشاشة وهمية.
- **عبر Wi-Fi أو USB.** ابث عبر شبكتك المنزلية أو وصّل كابلًا لصورة أكثر استقرارًا وشحن iPad في الوقت نفسه. يتولى الكابل جلسة Wi-Fi الجارية، ويعود الاتصال إلى Wi-Fi عند فصله.
- **اللمس ولوحة المفاتيح والمؤشر.** اضغط للنقر، واسحب، ومرر بإصبعين، واضغط مطولًا للنقر بزر الفأرة الأيمن. تعمل أيضًا لوحات المفاتيح الفعلية ولوحات اللمس والفأرات، مع تثبيت مفاتيح التعديل والنقر الأوسط.
- **قلم حقيقي مع Apple Pencil.** يتلقى Windows الضغط والميل عبر Windows Ink، ويظهر القلم تحت رأسه مهما كان ترتيب الشاشات. يتجاهل **وضع الرسم** (Drawing mode) راحة يدك على مساحة الرسم، ويعمل Clip Studio Paint مع إعداد Tablet PC.
- **ملاءمة أو ملء.** يعرض خيار **ملاءمة** (Fit) صورة الكمبيوتر كاملة مع أشرطة عند اختلاف الأبعاد. يستخدم خيار **ملء** (Fill) شاشة iPad كلها ويقص الحواف التي لا تتسع لها.
- **صوت الكمبيوتر.** يُشغّل الصوت على iPad بصيغة Opus مع مخزن لتفاوت وصول الحزم وإخفاء الفقد. يمكنك كتمه دون إيقاف الفيديو.
- **إصلاح الفقد على Wi-Fi.** عندما تفقد حزمة، يطلب iPad الجزء المفقود فورًا، لذلك نادرًا ما يؤدي فقد حزمة إلى فقد إطار. يتكيف معدل البت بين 4 و50 Mbps.
- **معالجة بواسطة بطاقة الرسومات.** يرمّز NVIDIA NVENC أو AMD AMF أو Intel Quick Sync الفيديو بصيغة H.264 افتراضيًا، أو HEVC إن فضلت، بمعدل 30 أو 60 أو 90 أو 120 fps.
- **الاقتران.** يطلب أول اتصال عبر Wi-Fi رمزًا من ستة أرقام أو مسح رمز QR. بعد ذلك يتذكر iPad الكمبيوتر. لا يحتاج USB إلى رمز.
- **الاستعادة والتوضيح.** تعود الصورة تلقائيًا بعد شاشة القفل أو تغيير الشاشة أو إعادة تشغيل البث. وعند حدوث مشكلة يوضح التطبيقان ما حدث وما يجب فعله، ويُعد خيار **إرسال ملاحظات** (Send feedback) رسالة بريد إلى الاستوديو تتضمن تفاصيل إعدادك.
- **التحديث بنقرة واحدة.** تنبهك نافذة منبثقة عند صدور نسخة جديدة. في Windows يتحقق خيار **التحديث الآن** (Update now) من التنزيل ويثبته بمطالبة واحدة من Windows؛ ويفتح تطبيق iPad تطبيق TestFlight. يعمل تطبيق Windows دون نافذة وحدة تحكم.

<br clear="right">

## تنزيل

| | المتطلبات | الحصول عليه |
| --- | --- | --- |
| **Windows** | Windows 10 الإصدار 1809 أو أحدث، أو Windows 11، بمعمارية 64 بت وبطاقة رسومات NVIDIA أو AMD أو Intel | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) (الإصدار المستقر الحالي v0.3.3) |
| **iPad** | iPadOS 17 أو أحدث | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc). النسخة التجريبية العامة متاحة: ثبّت TestFlight ثم افتح الرابط على iPad. |

ثبّت نسختي Windows وiPad المذكورتين على eternalmonitor.dev/download.html. تعمل النسختان معًا حتى لو اختلف رقما إصدارهما. تستخدم إصدارات v0.1 الأقدم بروتوكولًا مختلفًا ولا يمكنها الاتصال بتطبيق iPad الإصدار v0.3.

هل تختبر التطبيق؟ أرسل الأخطاء والملاحظات عبر [النموذج في صفحة التنزيل](https://eternalmonitor.dev/download.html#feedback)، الذي يرسل بريدًا إلى hello@sideband.studio، أو افتح [بلاغًا على GitHub](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)، أو أخبرنا على [خادم Sideband في Discord](https://discord.gg/Xc7NykBZ6z).

لم يُوقّع برنامج التثبيت برمجيًا بعد، لذا يطلب Windows SmartScreen تأكيدًا قبل تشغيله. اختر **مزيد من المعلومات** (More info)، ثم **التشغيل على أي حال** (Run anyway)، بعد التحقق من تطابق SHA-256 للملف مع القيمة على [صفحة الإصدار](https://github.com/sidebandstudio/EternalMonitor/releases).

تحتاج أيضًا لاستخدام USB إلى تطبيق [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) المجاني من Apple، مع إبقائه مفتوحًا أثناء البث. لا يستطيع Windows الوصول إلى iPad عبر الكابل إلا من خلاله، ويخبرك EternalMonitor إذا كان مغلقًا أو غير موجود. يحتاج Apple Devices إلى Windows 10 الإصدار 22H2 (build 19045) أو Windows 11، لذا استخدم Wi-Fi مع إصدارات Windows 10 الأقدم.

**بعد ذلك، اتبع [دليل الإعداد](../SETUP.md).** يشرح تثبيت التطبيقين والاقتران وUSB والشاشة الممتدة والرسم، مع أسماء الأزرار التي ستراها بالضبط.

## الحالة

يصلح الإصدار v0.3.3 مشكلة عدم بدء **شاشة ثانية** (Second screen) على بعض أجهزة الكمبيوتر، وغالبًا على الحواسيب المحمولة ذات بطاقات Intel وNVIDIA. يعرض iPad شاشة الكمبيوتر الرئيسية أثناء بدء الشاشة الثانية، ولم تعد شاشة الكمبيوتر تومض باستمرار. حدّث iPad إلى TestFlight v0.3.3, build 364 لاستخدامه مع Windows 0.3.3.

تتضمن تشخيصات Windows وiPad تفاصيل الجهاز وأخطاء التطبيق وإحصاءات البث. تشمل التقارير أسماء الكمبيوتر وiPad وأسماء أجهزة الصوت وعناوين الشبكة، لكنها تستثني محتوى الشاشة والصوت وبيانات الاقتران. يمكنك تعطيل المشاركة التلقائية في الإعدادات. تُحتفظ التشخيصات لمدة تصل إلى 30 يومًا وقد تُحذف قبل ذلك. قد تبقى التشخيصات المحذوفة أو المنتهية في النسخ الاحتياطية اليومية لمدة تصل إلى 7 أيام إضافية. راجع [التشخيصات التلقائية](../SETUP.md#automatic-diagnostics) و[إشعار الخصوصية](https://eternalmonitor.dev/privacy.html). على iPad، افتح **الإعدادات** (Settings) > **الدعم** (Support) > **التشخيصات** (Diagnostics) لنسخ تقرير أو تصديره.

اجتازت اختبارات الإصدار على Windows وmacOS ومحاكيات iPad. تجد المشكلات المعروفة في صفحة [المشكلات المعروفة](https://eternalmonitor.dev/known-issues.html)، ونتائج الاختبارات السابقة على الأجهزة الفعلية في [docs/hardware-verification.md](../docs/hardware-verification.md).

القيود المعروفة:

- البث غير مشفّر. يمنع الاقتران الأجهزة الأخرى من الاتصال، لذا استخدم شبكة تثق بها مثل Wi-Fi المنزلية.
- تطبيق Windows هو المضيف الوحيد. يمكن بناء المضيف على macOS للتطوير، لكنه يبث نمط اختبار بدلًا من شاشتك.
- قلم Apple Pencil (USB-C) لا يحتوي على مستشعر ضغط، لذلك تبقى خطوطه بعرض واحد.

تسرد صفحة [المشكلات المعروفة](https://eternalmonitor.dev/known-issues.html) كل مشكلة نعرفها وما يمكن فعله حيالها.

## كيف يعمل

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="يلتقط كمبيوتر Windows سطح المكتب ويرمّز الفيديو ببطاقة الرسومات ويرسله عبر Wi-Fi أو USB. يعيد iPad تجميع الفيديو ويطلب الأجزاء المفقودة عبر Wi-Fi، ثم يفك ترميزه باستخدام VideoToolbox ويعرضه باستخدام Metal. يُلتقط صوت الكمبيوتر باستخدام WASAPI ويُرمّز بصيغة Opus ويُشغّل على iPad. تعود إدخالات اللمس والقلم ولوحة المفاتيح ولوحة اللمس إلى الكمبيوتر ليحقنها.">
</picture>

كُتب مضيف Windows بلغة Rust. يلتقط سطح المكتب باستخدام DXGI، ويرمّز الفيديو على بطاقة الرسومات عبر FFmpeg، ويرسل كل إطار في أجزاء UDP مرقمة. عبر USB تنتقل الرسائل نفسها في نفق مؤطر توفره خدمة أجهزة Apple. تطبيق iPad أصلي ومكتوب بلغة Swift. يعيد تجميع الأجزاء ويفك ترميزها باستخدام VideoToolbox ويعرضها باستخدام Metal. وعبر Wi-Fi يطلب أيضًا الأجزاء المفقودة. يُلتقط صوت الكمبيوتر باستخدام WASAPI ويُرسل بصيغة Opus، ويخفي iPad الصوت المفقود بدلًا من طلبه مجددًا. تعود الإدخالات وتقارير المستقبِل عبر الجلسة نفسها، ويستخدم المضيف التقارير لتكييف معدل البت.

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

هذه الوثائق باللغة الإنجليزية.

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
| [docs/beta-testing.md](../docs/beta-testing.md) | تشغيل نسخة تجريبية: TestFlight والتوقيع وما يجب اختباره |
| [docs/design.md](../docs/design.md) | المظهر المشترك للتطبيقات والموقع |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | التغييرات في كل إصدار |

## المساهمة

نرحب بالمساهمات، من النقل والمرمّزات إلى العرض والوثائق. اقرأ [CONTRIBUTING.md](../CONTRIBUTING.md) قبل فتح طلب سحب. لمناقشة فكرة أولًا، اسأل على [خادم Sideband في Discord](https://discord.gg/Xc7NykBZ6z).

## الاتصال

يتيح [خادم Sideband في Discord](https://discord.gg/Xc7NykBZ6z) طرح الأسئلة وطلب المساعدة في الإعداد ومشاركة الأفكار. يمكنك أيضًا مراسلتنا على [hello@sideband.studio](mailto:hello@sideband.studio).

هل وجدت خطأ؟ [افتح بلاغًا](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) يتضمن إصداري التطبيقين وسجل المضيف. أبلغ عن المشكلات الأمنية بصفة خاصة كما هو موضح في [SECURITY.md](../SECURITY.md)، وليس على Discord أو في بلاغ.

## الترخيص

يُصدر EternalMonitor بموجب [MIT License](../LICENSE)، © 2026 Sideband Studio. يتضمن برنامج تثبيت Windows أيضًا مكونات خارجية تخضع لتراخيصها الخاصة، بما فيها [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) وFFmpeg.

من تطوير [Sideband Studio](https://sideband.studio).
