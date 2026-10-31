<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · **Русский** · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*Если эта страница отличается от [английского README](../README.md), актуальна английская версия.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="Логотип EternalMonitor">

# EternalMonitor

Повторяйте или расширяйте рабочий стол Windows на iPad по Wi-Fi или USB,<br>
управляя ПК касаниями, с клавиатуры или с помощью Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#скачать)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Скачать](#скачать) · [Руководство по настройке](../SETUP.md) · [Известные проблемы](https://eternalmonitor.dev/known-issues.html) · [Сайт](https://eternalmonitor.dev) · [Примечания к выпуску](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="Приложение EternalMonitor для Windows передаёт изображение ПК на iPad">
</p>

EternalMonitor передаёт рабочий стол Windows на iPad и отправляет действия пользователя обратно на ПК. Небольшое приложение Windows захватывает экран и кодирует видео на видеокарте. Приложение iPad аппаратно декодирует видео и отображает его через Metal, а касания, нажатия клавиш и движения Apple Pencil передаются на ПК. Программа бесплатна, распространяется с открытым исходным кодом под MIT License и не требует учётной записи.

## Возможности

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="Экран подключения приложения iPad с кнопками Подключить, Сканировать QR-код и Найти ПК">

- **Расширение или повтор экрана.** Добавьте iPad как дополнительный дисплей рядом с монитором или повторите изображение любого экрана. Дополнительный дисплей исчезает через 10 секунд после отключения iPad, поэтому при быстром повторном подключении он используется снова, а в Windows не остаётся фантомного монитора.
- **Wi-Fi или USB.** Передавайте изображение по домашней сети или подключите кабель для более стабильной картинки и одновременной зарядки iPad. Кабель перехватывает активное соединение Wi-Fi, а при отключении кабеля соединение возвращается к Wi-Fi.
- **Касания, клавиатура и указатель.** Нажимайте для щелчка, перетаскивайте, прокручивайте двумя пальцами и удерживайте для щелчка правой кнопкой. Также поддерживаются физические клавиатуры, трекпады и мыши, включая залипание клавиш-модификаторов и щелчок средней кнопкой.
- **Apple Pencil как настоящее перо.** Windows получает данные Windows Ink о давлении и наклоне, а касание пера попадает под его наконечник при любом расположении мониторов. **Режим рисования** (Drawing mode) игнорирует ладонь на холсте, а Clip Studio Paint работает с настройкой Tablet PC.
- **Вписать или заполнить.** Режим **Вписать** (Fit) показывает изображение ПК целиком, добавляя полосы при несовпадении пропорций. Режим **Заполнить** (Fill) занимает весь экран iPad и обрезает края, которые не помещаются.
- **Звук ПК.** Звук воспроизводится на iPad в формате Opus с буфером джиттера и компенсацией потерь. Его можно отключить, не останавливая видео.
- **Восстановление потерь в Wi-Fi.** При потере пакета iPad сразу повторно запрашивает нужный фрагмент, поэтому потеря одного пакета редко приводит к потере кадра. Битрейт меняется от 4 до 50 Mbps.
- **Кодирование на видеокарте.** NVIDIA NVENC, AMD AMF или Intel Quick Sync кодируют H.264 по умолчанию или HEVC по вашему выбору с частотой 30, 60, 90 или 120 fps.
- **Сопряжение.** При первом подключении по Wi-Fi потребуется шестизначный код или сканирование QR. После этого iPad запоминает ПК. Для USB код не нужен.
- **Восстановление и объяснение ошибок.** Изображение возвращается автоматически после экрана блокировки, смены дисплея или перезапуска трансляции. При ошибке оба приложения объясняют, что произошло и что делать, а кнопка **Отправить отзыв** (Send feedback) создаёт письмо студии с данными вашей конфигурации.
- **Обновление одним нажатием.** Всплывающее окно сообщает о новой версии. В Windows кнопка **Обновить сейчас** (Update now) проверяет загрузку и устанавливает её с одним запросом Windows; приложение iPad открывает TestFlight. Приложение Windows работает без окна консоли.

<br clear="right">

## Скачать

| | Требования | Где получить |
| --- | --- | --- |
| **Windows** | Windows 10 версии 1809 или новее либо Windows 11, 64-разрядная версия, с графикой NVIDIA, AMD или Intel | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) (v0.3.3, текущая стабильная версия) |
| **iPad** | iPadOS 17 или новее | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc). Публичная бета открыта: установите TestFlight, затем откройте ссылку на iPad. |

Установите версии для Windows и iPad, указанные на eternalmonitor.dev/download.html. Они работают вместе, даже если номера версий отличаются. Старые выпуски v0.1 используют другой протокол и не могут подключиться к приложению iPad версии v0.3.

Тестируете приложение? Сообщите об ошибках и отправьте отзыв через [форму на странице загрузки](https://eternalmonitor.dev/download.html#feedback), которая отправляет письмо на hello@sideband.studio, создайте [issue на GitHub](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) или напишите в [Sideband Discord](https://discord.gg/Xc7NykBZ6z).

Установщик пока не подписан цифровой подписью, поэтому Windows SmartScreen запрашивает подтверждение запуска. Выберите **Подробнее** (More info), затем **Выполнить в любом случае** (Run anyway), только после проверки того, что SHA-256 файла совпадает со значением на [странице выпуска](https://github.com/sidebandstudio/EternalMonitor/releases).

Для USB также потребуется бесплатное приложение Apple [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k), открытое во время трансляции. Windows может подключиться к iPad по кабелю только через него. EternalMonitor сообщит, если приложение закрыто или отсутствует. Apple Devices требует Windows 10 версии 22H2 (build 19045) или Windows 11, поэтому на более старых версиях Windows 10 подключайтесь по Wi-Fi.

**Затем следуйте [руководству по настройке](../SETUP.md).** В нём описаны установка обоих приложений, сопряжение, USB, расширенный дисплей и рисование с точными названиями кнопок.

## Состояние

В v0.3.3 исправлен запуск режима **Второй экран** (Second screen) на ПК, где он не работал, чаще всего на ноутбуках с графикой Intel и NVIDIA. Пока запускается второй экран, iPad показывает основной экран ПК, а экран ПК больше не мигает бесконечно. Для работы с Windows 0.3.3 обновите приложение iPad до TestFlight v0.3.3, build 364.

Диагностические данные Windows и iPad содержат сведения об устройствах, ошибки приложений и статистику трансляции. Отчёты включают имена ПК и iPad, названия аудиоустройств и сетевые адреса, но не содержат изображение экрана, звук и данные сопряжения. Автоматическую отправку можно отключить в настройках. Диагностические данные хранятся до 30 дней и могут быть удалены раньше. Удалённые или устаревшие данные могут оставаться в ежедневных резервных копиях ещё до 7 дней. См. [Автоматическая диагностика](../SETUP.md#automatic-diagnostics) и [уведомление о конфиденциальности](https://eternalmonitor.dev/privacy.html). На iPad откройте **Настройки** (Settings) > **Поддержка** (Support) > **Диагностика** (Diagnostics), чтобы скопировать или экспортировать отчёт.

Проверки выпуска прошли в Windows, macOS и симуляторах iPad. Известные проблемы перечислены на [странице известных проблем](https://eternalmonitor.dev/known-issues.html), а результаты предыдущих проверок на физических устройствах сохранены в [docs/hardware-verification.md](../docs/hardware-verification.md).

Известные ограничения:

- Трансляция не зашифрована. Сопряжение не допускает другие устройства, поэтому используйте доверенную сеть, например домашнюю Wi-Fi.
- Только приложение Windows может быть хостом. Хост собирается на macOS для разработки, но вместо экрана передаёт тестовое изображение.
- У Apple Pencil (USB-C) нет датчика давления, поэтому ширина его штрихов не меняется.

На [странице известных проблем](https://eternalmonitor.dev/known-issues.html) перечислены все известные проблемы и способы их решения.

## Как это работает

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="ПК с Windows захватывает рабочий стол, кодирует видео на видеокарте и отправляет его по Wi-Fi или USB. iPad собирает видео, запрашивая потерянные фрагменты по Wi-Fi, декодирует его с помощью VideoToolbox и показывает через Metal. Звук ПК захватывается через WASAPI, кодируется в Opus и воспроизводится на iPad. Касания, движения Pencil, ввод с клавиатуры и трекпада передаются обратно на ПК, который их воспроизводит.">
</picture>

Хост Windows написан на Rust. Он захватывает рабочий стол с помощью DXGI, кодирует видео на видеокарте через FFmpeg и отправляет каждый кадр в виде пронумерованных фрагментов UDP. Через USB те же сообщения передаются по туннелю с разделением на кадры, предоставляемому службой устройств Apple. Приложение iPad написано на Swift. Оно собирает фрагменты, декодирует видео с помощью VideoToolbox и отображает его через Metal. При подключении по Wi-Fi оно также запрашивает недостающие фрагменты. Звук ПК захватывается через WASAPI и передаётся как Opus, а iPad компенсирует потери звука без повторного запроса. Действия пользователя и отчёты приёмника передаются обратно в том же сеансе, а хост использует отчёты для адаптации битрейта.

В [ARCHITECTURE.md](../ARCHITECTURE.md) описаны обработка данных и протокол v2, а в [docs/decisions.md](../docs/decisions.md) объясняется устройство каждого компонента.

## Сборка из исходного кода

Рабочее пространство содержит хост на Rust (`host/`), пакет протокола `eternal-wire` (`proto/`) и приложение iPad на Swift (`ios/`).

<details>
<summary><b>Хост Windows</b></summary>

Требования: Windows 10 версии 1809 или новее, Rust 1.98.0 (версия закреплена в `rust-toolchain.toml`, MSVC), SDK FFmpeg **7.1 shared** и LLVM/libclang для bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

Хост является приложением Windows с графическим интерфейсом и не открывает окно консоли. При запуске из терминала он выводит туда журнал, но приглашение командной строки возвращается сразу; `cargo run -p eternal-host` ожидает завершения хоста. При `ETERNAL_HEADLESS=1` и перенаправлении вывода (`> host.log`) хост получает собственную консоль, поэтому Ctrl+C в терминале до него не доходит: нажмите Ctrl+C в окне его консоли или используйте диспетчер задач. `scripts\win\Stop-Host.ps1` останавливает только хосты, запущенные через `remote.sh run-host`.

`scripts\build-installer.ps1` собирает полный Setup.exe с помощью Inno Setup и того же `FFMPEG_DIR`. `scripts\package.ps1` собирает простой zip-архив.

</details>

<details>
<summary><b>Разработка на macOS</b> (без Windows)</summary>

На macOS хост заменяет захват экрана синтетическим тестовым изображением. Протокол, кодировщик, транспорт и супервизор работают в обычном режиме.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

Если в качестве каталога разработчика выбраны инструменты командной строки Xcode, добавляйте перед командами Xcode `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

</details>

<details>
<summary><b>Приложение iPad</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

Для запуска на физическом iPad откройте созданный проект в Xcode и выберите свою команду подписи.

</details>

<details>
<summary><b>Сквозные тесты</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

Каждая строка проверяет частоту декодированных кадров (не менее 55 fps), пропуски, восстановление и отображённые пиксели. `e2e_matrix.sh --real` запускает проверки оборудования Windows через средство запуска в `scripts/win/`; сначала прочитайте [scripts/win/README.md](../scripts/win/README.md). CI запускает наборы тестов Rust на Linux, macOS и Windows, тесты iPad и системный тест трансляции для каждого pull request.

</details>

<details>
<summary><b>Переменные окружения хоста</b></summary>

| Переменная | Действие |
| --- | --- |
| `ETERNAL_ENCODER` | Принудительно выбрать кодировщик (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` или `0` переопределяет настройку HEVC |
| `ETERNAL_FPS` | Переопределить частоту кадров хоста; настройка iPad по-прежнему ограничивает её |
| `ETERNAL_INPUT` | Вход кодировщика: `yuv420` (по умолчанию), `auto` или `bgra` |
| `ETERNAL_MAX_DGRAM` | Размер медиадатаграммы, от 576 до 1400 байт |
| `ETERNAL_CAPTURE` | `synthetic` заменяет DXGI сгенерированным тестовым изображением |
| `ETERNAL_AUDIO` | `synthetic` выбирает детерминированный тестовый источник звука |
| `ETERNAL_HEADLESS` | `1` запускает без окна; Ctrl+C корректно завершает работу |
| `ETERNAL_ABR` | `0` отключает адаптивный битрейт |
| `ETERNAL_VDD_TIMEOUT_SECS` | Время ожидания подключения виртуального дисплея (по умолчанию 30) |
| `ETERNAL_AMF_DIAG` | `1` записывает диагностику битового потока AMF |
| `ETERNAL_LEGACY_PTS` | `1` возвращает старые временные метки на основе счётчика кадров |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | Только для тестов: имитация потерь, изменения порядка или джиттера |
| `ETERNAL_USB_DIRECT` | Только для тестов: конечная точка TCP вместо туннеля USB |

</details>

## Документация

Эти документы доступны на английском языке.

| Документ | Содержание |
| --- | --- |
| [SETUP.md](../SETUP.md) | Пошаговая настройка, включая USB и рисование |
| [Известные проблемы](https://eternalmonitor.dev/known-issues.html) | Известные проблемы и способы их решения (`docs/known-issues.html`) |
| [Конфиденциальность](https://eternalmonitor.dev/privacy.html) | Какие данные отправляют приложения, зачем, как долго они хранятся и как отключить отправку (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | Обработка данных, протокол v2 и компоненты |
| [docs/decisions.md](../docs/decisions.md) | Причины принятых решений и результаты измерений |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil, Windows Ink и Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | Подробности о функциях |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | Результаты тестирования выпуска и список проверок на устройствах |
| [docs/beta-testing.md](../docs/beta-testing.md) | Бета-тестирование: TestFlight, подпись и проверки |
| [docs/design.md](../docs/design.md) | Общий визуальный стиль приложений и сайта |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | Изменения в каждой версии |

## Участие в разработке

Мы приветствуем вклад в разработку транспорта, кодировщиков, отображения и документации. Прочитайте [CONTRIBUTING.md](../CONTRIBUTING.md), прежде чем открывать pull request. Чтобы сначала обсудить идею, напишите в [Sideband Discord](https://discord.gg/Xc7NykBZ6z).

## Контакты

В [Sideband Discord](https://discord.gg/Xc7NykBZ6z) можно задать вопросы, получить помощь с настройкой и поделиться идеями. Также можно написать на [hello@sideband.studio](mailto:hello@sideband.studio).

Нашли ошибку? [Создайте issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose), указав версии обоих приложений и приложив журнал хоста. О проблемах безопасности сообщайте в частном порядке, как описано в [SECURITY.md](../SECURITY.md), а не в Discord или issue.

## Лицензия

EternalMonitor распространяется под [MIT License](../LICENSE), © 2026 Sideband Studio. Установщик Windows также включает сторонние компоненты под их собственными лицензиями, включая [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) и FFmpeg.

Разработано в [Sideband Studio](https://sideband.studio).
