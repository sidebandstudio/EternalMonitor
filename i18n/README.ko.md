<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · **한국어**

*이 문서와 [영어 README](../README.md)가 다르면 영어 문서가 최신입니다.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="EternalMonitor 로고">

# EternalMonitor

Wi-Fi 또는 USB로 Windows 데스크톱을 iPad에 미러링하거나 확장하고,<br>
터치, 키보드 또는 Apple Pencil로 PC를 제어하세요.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#다운로드)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[다운로드](#다운로드) · [설정 가이드](../SETUP.md) · [알려진 문제](https://eternalmonitor.dev/known-issues.html) · [웹사이트](https://eternalmonitor.dev) · [릴리스 노트](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="PC 화면을 표시하는 iPad로 스트리밍하는 EternalMonitor Windows 앱">
</p>

EternalMonitor는 Windows 데스크톱을 iPad로 스트리밍하고 입력을 PC로 돌려보냅니다. 작은 Windows 앱이 화면을 캡처하고 그래픽 카드에서 인코딩합니다. iPad 앱은 하드웨어로 비디오를 디코딩하고 Metal로 그립니다. 탭, 키 입력, Pencil 스트로크도 모두 PC로 전송됩니다. MIT License로 제공되는 무료 오픈 소스이며 계정이 필요하지 않습니다.

## 기능

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="연결, QR 코드 스캔, PC 찾기가 있는 iPad 앱의 연결 화면">

- **확장 또는 미러링.** iPad를 모니터 옆의 추가 디스플레이로 사용하거나 기존 화면을 미러링하세요. 추가 디스플레이는 iPad 연결이 끊어진 지 10초 후에 사라집니다. 따라서 빠르게 다시 연결하면 이를 재사용하며 Windows에 유령 모니터가 남지 않습니다.
- **Wi-Fi 또는 USB.** 홈 네트워크로 스트리밍하거나 케이블을 연결해 더 안정적인 화면을 보면서 iPad도 충전하세요. 케이블을 연결하면 진행 중인 Wi-Fi 세션을 이어받고, 분리하면 Wi-Fi로 돌아갑니다.
- **터치, 키보드 및 포인터.** 탭하여 클릭하고, 드래그하고, 두 손가락으로 스크롤하고, 길게 눌러 오른쪽 클릭하세요. 하드웨어 키보드, 트랙패드, 마우스도 사용하며 고정 보조 키와 가운데 클릭을 지원합니다.
- **실제 펜처럼 쓰는 Apple Pencil.** Windows가 Windows Ink의 필압과 기울기를 수신하며, 어떤 모니터 배치에서도 펜 끝 위치에 입력됩니다. **그리기 모드** (Drawing mode)는 캔버스의 손바닥 입력을 무시하며, Clip Studio Paint는 Tablet PC 설정으로 작동합니다.
- **맞춤 또는 채우기.** **맞춤** (Fit)은 PC 화면 전체를 표시하고 화면 비율이 다른 곳에 여백을 둡니다. **채우기** (Fill)는 iPad 화면 전체를 사용하고 맞지 않는 가장자리를 잘라냅니다.
- **PC 오디오.** 사운드를 Opus로 iPad에서 재생하며 지터 버퍼와 손실 은폐를 사용합니다. 비디오를 멈추지 않고 음소거할 수 있습니다.
- **Wi-Fi 손실 복구.** 패킷이 누락되면 iPad가 해당 조각을 즉시 다시 요청하므로 패킷 하나가 빠져도 프레임이 손실되는 경우가 드뭅니다. 비트레이트는 4~50 Mbps 사이에서 조정됩니다.
- **GPU 인코딩.** NVIDIA NVENC, AMD AMF 또는 Intel Quick Sync가 기본적으로 H.264를, 원하면 HEVC를 30, 60, 90 또는 120 fps로 인코딩합니다.
- **페어링.** 처음 Wi-Fi로 연결할 때 6자리 코드 또는 QR 스캔을 요청합니다. 이후 iPad가 PC를 기억합니다. USB에는 코드가 필요하지 않습니다.
- **복구 및 안내.** 잠금 화면, 디스플레이 변경 또는 스트림 재시작 후 화면이 자동으로 돌아옵니다. 문제가 발생하면 두 앱이 상황과 조치 방법을 안내하며, **피드백 보내기** (Send feedback)는 설정 정보가 채워진 이메일을 스튜디오에 작성합니다.
- **한 번의 클릭으로 업데이트.** 새 버전이 나오면 팝업으로 알려줍니다. Windows에서 **지금 업데이트** (Update now)는 다운로드를 확인하고 Windows 확인 창 한 번으로 설치합니다. iPad 앱은 TestFlight를 엽니다. Windows 앱은 콘솔 창 없이 실행됩니다.

<br clear="right">

## 다운로드

| | 요구 사항 | 받기 |
| --- | --- | --- |
| **Windows** | Windows 10 버전 1809 이상 또는 Windows 11, 64비트, NVIDIA, AMD 또는 Intel 그래픽 | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) (현재 안정 버전 v0.3.3) |
| **iPad** | iPadOS 17 이상 | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc). 공개 베타에 참여할 수 있습니다. TestFlight를 설치한 후 iPad에서 링크를 여세요. |

eternalmonitor.dev/download.html에 나열된 Windows와 iPad 버전을 설치하세요. 버전 번호가 달라도 함께 작동합니다. 이전 v0.1 릴리스는 프로토콜이 달라 v0.3 iPad 앱에 연결할 수 없습니다.

테스트 중인가요? [다운로드 페이지의 양식](https://eternalmonitor.dev/download.html#feedback)으로 버그와 피드백을 보내면 hello@sideband.studio로 이메일이 전송됩니다. 또는 [GitHub 이슈](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)를 열거나 [Sideband Discord](https://discord.gg/Xc7NykBZ6z)에 알려주세요.

설치 프로그램은 아직 코드 서명되지 않아 Windows SmartScreen이 실행 전에 확인을 요청합니다. 파일의 SHA-256이 [릴리스 페이지](https://github.com/sidebandstudio/EternalMonitor/releases)의 값과 일치하는지 확인한 후 **추가 정보** (More info)를 선택하고 **실행** (Run anyway)을 선택하세요.

USB를 사용하려면 Apple의 무료 [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) 앱도 필요하며 스트리밍 중에 열어 두어야 합니다. Windows는 이 앱을 통해서만 케이블로 iPad에 연결할 수 있으며, 앱이 닫혀 있거나 없으면 EternalMonitor가 알려줍니다. Apple Devices에는 Windows 10 버전 22H2 (build 19045) 또는 Windows 11이 필요하므로 이전 Windows 10에서는 Wi-Fi로 연결하세요.

**다음으로 [설정 가이드](../SETUP.md)를 따르세요.** 두 앱 설치, 페어링, USB, 확장 디스플레이와 그리기를 실제 버튼 이름과 함께 안내합니다.

## 상태

v0.3.3은 일부 PC, 특히 Intel과 NVIDIA 그래픽을 함께 사용하는 노트북에서 **두 번째 화면** (Second screen)이 시작되지 않던 문제를 해결합니다. 두 번째 화면이 시작되는 동안 iPad에 PC의 기본 화면이 표시되며 PC 화면이 반복해서 깜박이지 않습니다. Windows 0.3.3에 맞춰 iPad를 TestFlight v0.3.3, build 364로 업데이트하세요.

Windows와 iPad 진단에는 기기 정보, 앱 오류와 스트림 통계가 포함됩니다. 보고서에는 PC와 iPad 이름, 오디오 기기 이름과 네트워크 주소가 포함되지만 화면 내용, 오디오와 페어링 자격 증명은 제외됩니다. 설정에서 자동 공유를 끌 수 있습니다. 진단 정보는 최대 30일 동안 보관되며 더 일찍 삭제될 수 있습니다. 삭제되거나 만료된 진단 정보는 일일 백업에 최대 7일 더 남을 수 있습니다. [자동 진단](../SETUP.md#automatic-diagnostics)과 [개인정보 처리방침](https://eternalmonitor.dev/privacy.html)을 참조하세요. iPad에서 **설정** (Settings) > **지원** (Support) > **진단** (Diagnostics)을 열어 보고서를 복사하거나 내보내세요.

Windows, macOS 및 iPad 시뮬레이터에서 릴리스 검사를 통과했습니다. 확인된 문제는 [알려진 문제](https://eternalmonitor.dev/known-issues.html) 페이지에 있으며, 이전 실물 기기 결과는 [docs/hardware-verification.md](../docs/hardware-verification.md)에 남아 있습니다.

알려진 제한 사항:

- 스트림은 암호화되지 않습니다. 페어링은 다른 기기의 접근을 막으므로 홈 Wi-Fi처럼 신뢰할 수 있는 네트워크를 사용하세요.
- Windows 앱만 호스트로 사용할 수 있습니다. 개발을 위해 macOS에서도 호스트를 빌드할 수 있지만 화면 대신 테스트 패턴을 스트리밍합니다.
- Apple Pencil (USB-C)에는 필압 센서가 없어 획의 너비가 일정합니다.

[알려진 문제](https://eternalmonitor.dev/known-issues.html)에 확인된 모든 문제와 각각의 대처 방법이 나와 있습니다.

## 작동 방식

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Windows PC가 데스크톱을 캡처하고 GPU에서 비디오를 인코딩해 Wi-Fi 또는 USB로 전송합니다. iPad는 비디오를 재조립하며 Wi-Fi에서 누락된 조각을 요청하고, VideoToolbox로 디코딩해 Metal로 표시합니다. PC 오디오는 WASAPI로 캡처해 Opus로 인코딩하고 iPad에서 재생합니다. 터치, Pencil, 키보드와 트랙패드 입력은 PC로 돌아가 주입됩니다.">
</picture>

Windows 호스트는 Rust로 작성되었습니다. DXGI로 데스크톱을 캡처하고 FFmpeg를 통해 GPU에서 인코딩한 다음 각 프레임을 번호가 매겨진 UDP 조각으로 전송합니다. USB에서는 Apple 기기 서비스가 제공하는 프레임 터널을 통해 같은 메시지를 전송합니다. iPad 앱은 네이티브 Swift로 작성되었습니다. 조각을 재조립하고 VideoToolbox로 디코딩해 Metal로 렌더링합니다. Wi-Fi에서는 누락된 조각도 요청합니다. PC 오디오는 WASAPI로 캡처해 Opus로 전송하며 iPad는 손실된 오디오를 다시 요청하지 않고 은폐합니다. 입력과 수신기 보고서는 같은 세션으로 돌아가며 호스트는 이 보고서를 사용해 비트레이트를 조정합니다.

[ARCHITECTURE.md](../ARCHITECTURE.md)는 파이프라인과 프로토콜 v2를 설명하고 [docs/decisions.md](../docs/decisions.md)는 각 부분의 작동 방식을 선택한 이유를 설명합니다.

## 소스에서 빌드

작업 공간에는 Rust 호스트 (`host/`), 프로토콜 크레이트 `eternal-wire` (`proto/`), Swift iPad 앱 (`ios/`)이 있습니다.

<details>
<summary><b>Windows 호스트</b></summary>

요구 사항: Windows 10 버전 1809 이상, Rust 1.98.0 (`rust-toolchain.toml`로 고정, MSVC), FFmpeg **7.1 shared** SDK 및 bindgen용 LLVM/libclang.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

호스트는 Windows GUI 프로그램이므로 콘솔 창을 열지 않습니다. 터미널에서 시작하면 로그를 그곳에 출력하지만 프롬프트는 즉시 반환됩니다. `cargo run -p eternal-host`는 종료될 때까지 기다립니다. `ETERNAL_HEADLESS=1`로 출력 리디렉션 (`> host.log`)을 사용하면 별도의 콘솔이 생겨 터미널의 Ctrl+C가 전달되지 않습니다. 해당 콘솔 창에서 Ctrl+C를 누르거나 작업 관리자를 사용하세요. `scripts\win\Stop-Host.ps1`은 `remote.sh run-host`로 시작한 호스트만 중지합니다.

`scripts\build-installer.ps1`은 Inno Setup과 동일한 `FFMPEG_DIR`을 사용해 전체 Setup.exe를 빌드합니다. `scripts\package.ps1`은 기본 zip을 빌드합니다.

</details>

<details>
<summary><b>macOS 개발 과정</b> (Windows 불필요)</summary>

macOS에서 호스트는 화면 캡처 대신 합성 테스트 패턴을 사용합니다. 프로토콜, 인코더, 전송 및 감독 프로세스는 모두 실제로 실행됩니다.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

Xcode의 명령줄 도구가 개발자 디렉터리로 선택되어 있다면 Xcode 명령 앞에 `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`를 붙이세요.

</details>

<details>
<summary><b>iPad 앱</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

실제 iPad에서 실행하려면 생성된 프로젝트를 Xcode에서 열고 본인의 서명 팀을 선택하세요.

</details>

<details>
<summary><b>종단 간 테스트</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

각 행은 디코딩된 프레임 레이트 (최소 55 fps), 프레임 드롭, 복구 및 렌더링된 픽셀을 검사합니다. `e2e_matrix.sh --real`은 `scripts/win/`의 실행기를 통해 Windows 하드웨어 행을 실행합니다. 먼저 [scripts/win/README.md](../scripts/win/README.md)를 읽으세요. CI는 모든 풀 리퀘스트에서 Linux, macOS, Windows의 Rust 테스트 모음, iPad 테스트와 스트리밍 시스템 테스트를 실행합니다.

</details>

<details>
<summary><b>호스트 환경 변수</b></summary>

| Variable | Effect |
| --- | --- |
| `ETERNAL_ENCODER` | 인코더 강제 지정 (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` 또는 `0`으로 HEVC 설정을 덮어씀 |
| `ETERNAL_FPS` | 호스트 프레임 레이트를 덮어쓰며 iPad 설정이 상한을 정함 |
| `ETERNAL_INPUT` | 인코더 입력: `yuv420` (기본값), `auto` 또는 `bgra` |
| `ETERNAL_MAX_DGRAM` | 미디어 데이터그램 크기, 576~1400바이트 |
| `ETERNAL_CAPTURE` | `synthetic`은 DXGI 대신 생성된 테스트 패턴 사용 |
| `ETERNAL_AUDIO` | `synthetic`은 결정적 오디오 테스트 소스 선택 |
| `ETERNAL_HEADLESS` | `1`은 창 없이 실행하며 Ctrl+C로 정상 종료 |
| `ETERNAL_ABR` | `0`은 적응형 비트레이트 비활성화 |
| `ETERNAL_VDD_TIMEOUT_SECS` | 가상 디스플레이 연결 대기 시간 (기본값 30) |
| `ETERNAL_AMF_DIAG` | `1`은 AMF 비트스트림 진단 정보 기록 |
| `ETERNAL_LEGACY_PTS` | `1`은 이전 프레임 카운터 타임스탬프 복원 |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | 테스트 전용: 손실, 순서 변경 또는 지터 주입 |
| `ETERNAL_USB_DIRECT` | 테스트 전용: USB 터널 대신 TCP 엔드포인트 사용 |

</details>

## 문서

이 문서들은 영어로 제공됩니다.

| 문서 | 내용 |
| --- | --- |
| [SETUP.md](../SETUP.md) | USB와 그리기를 포함한 단계별 설정 안내 |
| [알려진 문제](https://eternalmonitor.dev/known-issues.html) | 확인된 문제와 각각의 대처 방법 (`docs/known-issues.html`) |
| [개인정보 처리방침](https://eternalmonitor.dev/privacy.html) | 앱이 전송하는 정보, 이유, 보관 기간과 끄는 방법 (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | 파이프라인, 프로토콜 v2 및 각 구성 요소 |
| [docs/decisions.md](../docs/decisions.md) | 현재 방식을 선택한 이유와 측정 결과 |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil, Windows Ink 및 Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | 기능 상세 정보 |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | 릴리스 테스트 기록과 실기기 점검 목록 |
| [docs/beta-testing.md](../docs/beta-testing.md) | 베타 실행: TestFlight, 서명 및 테스트 항목 |
| [docs/design.md](../docs/design.md) | 앱과 웹사이트의 공통 디자인 |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | 버전별 변경 사항 |

## 기여

전송과 인코더부터 렌더링과 문서까지 기여를 환영합니다. 풀 리퀘스트를 열기 전에 [CONTRIBUTING.md](../CONTRIBUTING.md)를 읽어 주세요. 아이디어를 먼저 논의하려면 [Sideband Discord](https://discord.gg/Xc7NykBZ6z)에 질문하세요.

## 연락처

질문, 설정 도움말과 아이디어는 [Sideband Discord](https://discord.gg/Xc7NykBZ6z)에서 나눌 수 있습니다. [hello@sideband.studio](mailto:hello@sideband.studio)로 이메일을 보낼 수도 있습니다.

버그를 발견했다면 두 앱의 버전과 호스트 로그를 첨부해 [이슈를 열어 주세요](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose). 보안 문제는 Discord나 이슈가 아닌 [SECURITY.md](../SECURITY.md)에 설명된 방법으로 비공개로 신고하세요.

## 라이선스

EternalMonitor는 [MIT License](../LICENSE)로 배포됩니다. © 2026 Sideband Studio. Windows 설치 프로그램에는 [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)와 FFmpeg를 비롯해 각자의 라이선스가 적용되는 타사 구성 요소도 포함됩니다.

[Sideband Studio](https://sideband.studio)에서 만들었습니다.
