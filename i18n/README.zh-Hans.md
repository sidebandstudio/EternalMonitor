<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · **简体中文** · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*如果本页与 [英文 README](../README.md) 有差异，请以英文版为准。*

<img src="../docs/assets/icon.png" width="88" height="88" alt="EternalMonitor 标志">

# EternalMonitor

适用于 Windows 的免费开源第二显示器。<br>
通过 Wi-Fi 或 USB 连接 iPad，支持触控和 Apple Pencil。

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[下载](#download) · [设置指南](../SETUP.md) · [网站](https://eternalmonitor.dev) · [路线图](../ROADMAP.md) · [发行说明](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="一台 Windows 电脑和一台 iPad。电脑上的 EternalMonitor 窗口将 iPad 切换到第二屏幕，然后一个窗口从显示器拖到 iPad。接着 Apple Pencil 在 iPad 的画作上绘制草叶，按下时笔画较粗，抬起时较细。">
</p>

EternalMonitor 将 iPad 变成 Windows 电脑的显示器。它免费、基于 MIT 许可证开源，无需账号。电脑以最高 120 fps 串流，在支持的显卡上进行硬件编码，在 iPad 上进行硬件解码。每次触摸、按键和 Pencil 笔画都会传回电脑。

<a id="download"></a>

## 快速开始

[![下载 Windows 版](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![通过 TestFlight 获取 iPad 应用](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **安装 Windows 应用。** 在配备 NVIDIA、AMD 或 Intel 显卡的 64 位 Windows 10（版本 1809 或更高）或 Windows 11 上运行 [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)。如果 Windows SmartScreen 提示，请选择**更多信息**，然后选择**仍要运行**。[发行页面](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3)列出了文件的 SHA-256。
2. **安装 iPad 应用。** 在运行 iPadOS 17 或更高版本的 iPad 上，从 App Store 安装 **TestFlight**，然后在 iPad 上打开 [EternalMonitor TestFlight 链接](https://testflight.apple.com/join/ppc9QfXc)并轻点**安装**。
3. **连接。** 在同一网络上打开两个应用，在 iPad 上轻点你的电脑，或扫描电脑显示的 QR 码。在 Windows 10 22H2 或 Windows 11 上使用 USB 时，请连接线缆，并在电脑上保持 Apple 的免费应用 [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) 打开。

[设置指南](../SETUP.md)使用准确的按钮名称介绍每个步骤。[下载页面](https://eternalmonitor.dev/download.html)列出了配合使用的 Windows 和 iPad 版本。

如果 EternalMonitor 对你有帮助，点一颗星可以帮助其他人找到它。

## 功能

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="iPad 应用的连接界面，显示连接、扫描二维码和查找电脑">

- **扩展或镜像。** 将 iPad 作为显示器旁的第二屏幕，或镜像任意现有屏幕。iPad 断开连接后第二屏幕会保留 10 秒，因此快速重连时可以复用，Windows 也不会留下不存在的显示器。
- **Wi-Fi 或 USB。** 通过家庭网络串流，或连接线缆以获得更稳定的画面，也可以通过 USB-C 端口为 iPad 充电。插入线缆会接管正在运行的 Wi-Fi 会话，拔掉后则切回 Wi-Fi。
- **将 Apple Pencil 当作真正的笔使用。** Windows 接收 Windows Ink 压力和倾斜数据，无论显示器如何排列，落笔位置都在笔尖下方。**绘图模式** (Drawing mode) 会忽略画布上的手掌触碰，Clip Studio Paint 可通过其 Tablet PC 设置使用。
- **触控、键盘和指针。** 轻点即可单击，也可拖动、双指滚动和长按右键单击。还支持实体键盘、触控板和鼠标，以及粘滞修饰键和鼠标中键单击。
- **硬件编码。** 支持的 NVIDIA、AMD 和 Intel 显卡以 30、60、90 或 120 fps 编码串流。
- **修复 Wi-Fi 丢包。** 数据包丢失时，iPad 会立即请求重传，因此单个丢包很少导致丢帧。码率会根据网络调整。
- **电脑音频。** 音频在 iPad 上播放，你可以静音而不停止视频。
- **适应或填充。** **适应** (Fit) 显示完整的电脑画面，宽高比不同时会留边。**填充** (Fill) 使用 iPad 的整个屏幕，并裁剪放不下的边缘。
- **配对。** 首次通过 Wi-Fi 连接时需要输入六位数代码或扫描 QR 码。之后 iPad 会记住电脑。USB 无需代码。
- **恢复并说明问题。** 锁屏、显示器变更或串流重启后，画面会自动恢复。两个应用都会说明发生了什么以及如何处理，**发送反馈** (Send feedback) 会生成一封填好设备配置信息的邮件发给工作室。
- **一键更新。** 新版本发布时会弹窗提示。在 Windows 上，**立即更新** (Update now) 会检查下载并安装，只需确认一次 Windows 提示；在 iPad 上则会打开 TestFlight。Windows 应用运行时不会打开控制台窗口。

本 README 和[网站](https://eternalmonitor.dev)提供 11 种语言。

<br clear="right">

## 了解更多

- [将 iPad 用作 Windows 的第二显示器](https://eternalmonitor.dev/ipad-second-monitor-windows.html)：完整指南。
- [兼容性](https://eternalmonitor.dev/compatibility.html)：人们运行 EternalMonitor 所用的电脑和 iPad。
- [替代方案](https://eternalmonitor.dev/alternatives.html)：EternalMonitor 与其他将 iPad 用作屏幕的方法的比较。
- [路线图](../ROADMAP.md)：接下来的计划。

**分享你的设备配置。** 通过[兼容性报告](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml)告诉我们你使用的电脑和 iPad，或发布到 [Sideband Discord](https://discord.gg/Xc7NykBZ6z)。经你同意后，我们会将其放到兼容性页面。

## 状态

当前版本为 Windows v0.3.3 和 iPad v0.3.3 (build 364)。它让更多配备 Intel 和 NVIDIA 显卡的笔记本电脑支持**第二屏幕** (Second screen)：iPad 会立即显示电脑主屏幕，等 Windows 添加第二屏幕后切换过去。[RELEASE_NOTES.md](../RELEASE_NOTES.md) 介绍各版本，[ROADMAP.md](../ROADMAP.md) 介绍后续计划。

v0.3.3 的发行检查已在 Windows、macOS 和 iPad 模拟器上通过。[docs/hardware-verification.md](../docs/hardware-verification.md) 记录了从 v0.3.0 起在真实电脑和 iPad 上进行的实测结果。

Windows 和 iPad 的诊断信息包括设备详情、应用错误和串流统计数据。报告包含显卡和显示器名称及网络地址，但不包含屏幕内容、音频或配对凭据。从 0.3.4 起，Windows 上传的数据会移除电脑名称、用户名、用户文件夹、iPad 名称和音频设备名称，仅保留每个音频设备的类型；iPad 上传的数据会移除电脑名称。可在**设置** (Settings) 中关闭自动共享。诊断信息最多保留 30 天，也可能提前删除。已删除或过期的诊断信息可能在每日备份中再保留最多 7 天。参见[自动诊断](../SETUP.md#automatic-diagnostics)和[隐私声明](https://eternalmonitor.dev/privacy.html)。在 iPad 上，打开**设置** (Settings) > **支持** (Support) > **诊断** (Diagnostics) 以复制或导出报告。

## 工作原理

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Windows 电脑捕获桌面，在 GPU 上编码视频并通过 Wi-Fi 或 USB 发送。iPad 重新组装视频，在 Wi-Fi 上请求缺失片段，用 VideoToolbox 解码并用 Metal 显示。电脑音频由 WASAPI 捕获，编码为 Opus 并在 iPad 上播放。触控、Pencil、键盘和触控板输入传回电脑，由电脑注入。">
</picture>

Windows 主机使用 Rust 编写。它通过 DXGI 捕获桌面，在支持的图形硬件上通过 FFmpeg 使用 NVIDIA NVENC、AMD AMF 或 Intel Quick Sync 编码（默认为 H.264，也可使用 HEVC），并将每帧作为带编号的 UDP 片段发送。通过 USB 时，相同的消息经过 Apple 设备服务提供的分帧隧道传输。iPad 应用使用原生 Swift 编写。它重新组装片段，用 VideoToolbox 解码并用 Metal 渲染。在 Wi-Fi 上，它还会请求缺失的片段。电脑音频由 WASAPI 捕获并以 Opus 发送，iPad 通过抖动缓冲播放，并掩盖丢失的音频，而不是请求重传。输入和接收端报告通过同一会话传回，主机根据报告将码率调整在 4 到 50 Mbps 之间。

[ARCHITECTURE.md](../ARCHITECTURE.md) 介绍了处理流程和协议 v2，[docs/decisions.md](../docs/decisions.md) 解释了各部分采用当前设计的原因。

## 从源码构建

工作区包含 Rust 主机（`host/`）、协议 crate `eternal-wire`（`proto/`）和 Swift iPad 应用（`ios/`）。

<details>
<summary><b>Windows 主机</b></summary>

要求：Windows 10 版本 1809 或更高版本、Rust 1.98.0（由 `rust-toolchain.toml` 固定，MSVC）、FFmpeg **7.1 shared** SDK，以及供 bindgen 使用的 LLVM/libclang。

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

主机是 Windows 图形界面程序，因此不会打开控制台窗口。从终端启动时，它会在终端输出日志，但命令提示符会立即返回；`cargo run -p eternal-host` 会等待它退出。使用 `ETERNAL_HEADLESS=1` 并重定向输出（`> host.log`）时，它会获得自己的控制台，因此终端中的 Ctrl+C 无法传递给它：请在该控制台窗口中按 Ctrl+C，或使用任务管理器。`scripts\win\Stop-Host.ps1` 只停止由 `remote.sh run-host` 启动的主机。

`scripts\build-installer.ps1` 使用 Inno Setup 和相同的 `FFMPEG_DIR` 构建完整的 Setup.exe。`scripts\package.ps1` 构建不含安装程序的 zip 包。

</details>

<details>
<summary><b>macOS 开发流程</b>（无需 Windows）</summary>

在 macOS 上，主机使用合成测试图案代替屏幕捕获。协议、编码器、传输和监督程序均实际运行。

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

如果当前选择的开发者目录是 Xcode 命令行工具，请在 Xcode 命令前加上 `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`。

</details>

<details>
<summary><b>iPad 应用</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

要在实体 iPad 上运行，请在 Xcode 中打开生成的项目并选择你自己的签名团队。

</details>

<details>
<summary><b>端到端测试</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

每一行都会检查解码帧率（至少 55 fps）、丢帧、修复情况和渲染后的像素。`e2e_matrix.sh --real` 通过 `scripts/win/` 中的运行器执行 Windows 硬件测试行；请先阅读 [scripts/win/README.md](../scripts/win/README.md)。每个 pull request 都会触发 CI 在 Linux、macOS 和 Windows 上运行 Rust 测试套件、iPad 测试和串流系统测试。

</details>

<details>
<summary><b>主机环境变量</b></summary>

| Variable | Effect |
| --- | --- |
| `ETERNAL_ENCODER` | 强制使用指定编码器（`h264_nvenc`、`h264_amf`、`h264_qsv`、`libx264`） |
| `ETERNAL_HEVC` | `1` 或 `0` 覆盖 HEVC 首选项 |
| `ETERNAL_FPS` | 覆盖主机帧率；iPad 的首选项仍限制其上限 |
| `ETERNAL_INPUT` | 编码器输入：`yuv420`（默认）、`auto` 或 `bgra` |
| `ETERNAL_MAX_DGRAM` | 媒体数据报大小，576 到 1400 字节 |
| `ETERNAL_CAPTURE` | `synthetic` 用生成的测试图案代替 DXGI |
| `ETERNAL_AUDIO` | `synthetic` 选择确定性音频测试源 |
| `ETERNAL_HEADLESS` | `1` 无窗口运行；Ctrl+C 可正常关闭 |
| `ETERNAL_ABR` | `0` 禁用自适应码率 |
| `ETERNAL_VDD_TIMEOUT_SECS` | 等待虚拟显示器接入的时间（默认 30） |
| `ETERNAL_AMF_DIAG` | `1` 写入 AMF 码流诊断信息 |
| `ETERNAL_LEGACY_PTS` | `1` 恢复旧的帧计数器时间戳 |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | 仅供测试：注入丢包、乱序或抖动 |
| `ETERNAL_USB_DIRECT` | 仅供测试：使用 TCP 端点代替 USB 隧道 |

</details>

## 文档

| 文档 | 内容 |
| --- | --- |
| [SETUP.md](../SETUP.md) | 面向所有人的分步设置指南，包括 USB 和绘图 |
| [已知问题](https://eternalmonitor.dev/known-issues.html) | 我们已知的问题及各自的处理方法（`docs/known-issues.html`） |
| [隐私](https://eternalmonitor.dev/privacy.html) | 应用发送的数据、原因、保留时间以及如何关闭（`docs/privacy.html`） |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | 处理流程、协议 v2 和各个组件 |
| [docs/decisions.md](../docs/decisions.md) | 各项设计的原因及测量数据 |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil、Windows Ink 和 Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md)、[docs/keyboard-input.md](../docs/keyboard-input.md)、[docs/host-settings.md](../docs/host-settings.md)、[docs/audio-codec.md](../docs/audio-codec.md) | 功能详情 |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | 发行测试记录和实机测试清单 |
| [发行测试](../docs/beta-testing.md) | 通过 TestFlight 发布构建：签名和测试内容 |
| [docs/design.md](../docs/design.md) | 应用和网站的统一外观 |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | 各版本的变更 |
| [ROADMAP.md](../ROADMAP.md) | 接下来的计划 |

## 参与贡献

欢迎各种贡献：issue、想法、修复、文档、翻译和新功能。无论大小，都欢迎提交 pull request。[CONTRIBUTING.md](../CONTRIBUTING.md) 介绍了如何提交，并列出了适合初次贡献的任务；[ROADMAP.md](../ROADMAP.md) 展示了更大的后续工作。也欢迎在 [Sideband Discord](https://discord.gg/Xc7NykBZ6z) 提问和分享想法。

## 联系方式

你可以在 [Sideband Discord](https://discord.gg/Xc7NykBZ6z) 提问、寻求设置帮助或分享想法。也可以发送邮件至 [hello@sideband.studio](mailto:hello@sideband.studio)。

发现错误？请[提交 issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)，附上两个应用的版本和主机日志，或使用会向工作室发送邮件的[下载页面反馈表单](https://eternalmonitor.dev/download.html#feedback)。安全问题请按照 [SECURITY.md](../SECURITY.md) 中的说明私下报告，不要在 Discord 或 issue 中公开。

## 许可证

EternalMonitor 基于 [MIT License](../LICENSE) 发布，© 2026 Sideband Studio。Windows 安装程序还附带使用各自许可证的第三方组件，包括 [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) 和 FFmpeg。

由 [Sideband Studio](https://sideband.studio) 构建。
