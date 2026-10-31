<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · **日本語** · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*このページと [英語の README](../README.md) に相違がある場合は、英語版が最新版です。*

<img src="../docs/assets/icon.png" width="88" height="88" alt="EternalMonitor のロゴ">

# EternalMonitor

Windows 用の無料オープンソースのセカンドモニター。<br>
iPad を Wi-Fi または USB で接続し、タッチと Apple Pencil を使えます。

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[ダウンロード](#download) · [セットアップガイド](../SETUP.md) · [ウェブサイト](https://eternalmonitor.dev) · [ロードマップ](../ROADMAP.md) · [リリースノート](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="Windows PC と iPad。PC の EternalMonitor ウィンドウで iPad をセカンドスクリーンに切り替え、ウィンドウをモニターから iPad にドラッグします。次に Apple Pencil で iPad の絵に草を描きます。押すと線が太くなり、浮かせると細くなります。">
</p>

EternalMonitor は iPad を Windows PC のモニターにします。無料で、MIT ライセンスのオープンソースです。アカウントは必要ありません。PC は最大 120 fps で配信し、対応するグラフィックスカードでハードウェアエンコード、iPad でハードウェアデコードを行います。タップ、キー入力、Pencil のストロークはすべて PC に戻ります。

<a id="download"></a>

## クイックスタート

[![Windows 版をダウンロード](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![TestFlight で iPad アプリを入手](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **Windows アプリをインストール。** NVIDIA、AMD、Intel のグラフィックスを搭載した 64 ビットの Windows 10（バージョン 1809 以降）または Windows 11 で [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe) を実行します。Windows SmartScreen が表示されたら、**詳細情報**、**実行**の順に選びます。ファイルの SHA-256 は[リリースページ](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3)に記載されています。
2. **iPad アプリをインストール。** iPadOS 17 以降の iPad に App Store から **TestFlight** をインストールします。iPad で [EternalMonitor の TestFlight リンク](https://testflight.apple.com/join/ppc9QfXc)を開き、**インストール**をタップします。
3. **接続。** 両アプリを同じネットワークで開き、iPad で PC をタップするか、PC に表示された QR コードをスキャンします。Windows 10 22H2 または Windows 11 で USB を使う場合はケーブルを接続し、Apple の無料アプリ [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) を PC で開いたままにします。

[セットアップガイド](../SETUP.md)では正確なボタン名を使って各手順を説明しています。[ダウンロードページ](https://eternalmonitor.dev/download.html)には組み合わせて使う Windows 版と iPad 版が掲載されています。

EternalMonitor が役に立ったら、スターを付けると他の人が見つけやすくなります。

## 機能

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="接続、QRコードをスキャン、PCを検索が表示された iPad アプリの接続画面">

- **拡張またはミラーリング。** iPad をモニターの隣にセカンドディスプレイとして使うことも、既存の画面をミラーリングすることもできます。セカンドディスプレイは iPad の切断後 10 秒間残るため、すぐに再接続すれば再利用でき、Windows に不要なモニターが残りません。
- **Wi-Fi または USB。** 自宅のネットワーク経由で配信するか、ケーブルを接続してより安定した映像を受信できます。USB-C ポートから iPad を充電することもできます。実行中の Wi-Fi セッションはケーブル接続に切り替わり、ケーブルを抜くと Wi-Fi に戻ります。
- **Apple Pencil をペンとして使用。** Windows は Windows Ink の筆圧と傾きを受け取り、どのモニター配置でもペン先の位置に描画します。**描画モード** (Drawing mode) はキャンバス上の手のひらを無視します。Clip Studio Paint では Tablet PC 設定で動作します。
- **タッチ、キーボード、ポインター。** タップでクリック、ドラッグ、2 本指でスクロール、長押しで右クリックできます。ハードウェアキーボード、トラックパッド、マウスも使え、修飾キーの固定や中クリックにも対応します。
- **ハードウェアエンコード。** 対応する NVIDIA、AMD、Intel のグラフィックスが 30、60、90、120 fps で配信をエンコードします。
- **Wi-Fi の損失を修復。** パケットが失われると iPad がすぐに再要求するため、1 パケットの損失でフレームが失われることはまれです。ビットレートはネットワークに合わせて調整されます。
- **PC の音声。** 音声は iPad で再生され、映像を止めずに消音できます。
- **全体を表示または画面いっぱい。** **全体を表示** (Fit) は PC の画面全体を表示し、縦横比の違いによる余白に帯を表示します。**画面いっぱい** (Fill) は iPad の画面全体を使い、収まらない端を切り取ります。
- **ペアリング。** 最初の Wi-Fi 接続時に 6 桁のコードか QR スキャンが必要です。その後は iPad が PC を記憶します。USB ではコードは不要です。
- **自動復帰と問題の説明。** ロック画面、ディスプレイの変更、配信の再起動後に映像が自動で戻ります。両アプリが状況と対処法を表示し、**フィードバックを送信** (Send feedback) で環境情報を入力済みのメールをスタジオ宛てに作成します。
- **ワンクリックで更新。** 新しいバージョンが出るとポップアップで通知します。Windows では **今すぐアップデート** (Update now) でダウンロードを検証し、Windows の確認画面 1 回でインストールします。iPad では TestFlight が開きます。Windows アプリはコンソールウィンドウなしで実行されます。

この README と[ウェブサイト](https://eternalmonitor.dev)は 11 言語で利用できます。

<br clear="right">

## 詳しくはこちら

- [iPad を Windows のセカンドモニターとして使う](https://eternalmonitor.dev/ipad-second-monitor-windows.html): 完全ガイド。
- [互換性](https://eternalmonitor.dev/compatibility.html): EternalMonitor が使われている PC と iPad。
- [代替手段](https://eternalmonitor.dev/alternatives.html): iPad を画面として使う他の方法との比較。
- [ロードマップ](../ROADMAP.md): 今後の予定。

**環境を共有。** お使いの PC と iPad を[互換性レポート](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml)で知らせるか、[Sideband Discord](https://discord.gg/Xc7NykBZ6z) に投稿してください。許可をいただければ、互換性ページに掲載します。

## 状況

現在のリリースは Windows 版 v0.3.3 と iPad 版 v0.3.3 (build 364) です。Intel と NVIDIA のグラフィックスを搭載したより多くのノート PC で **セカンドスクリーン** (Second screen) が使えます。iPad はまず PC のメイン画面を表示し、Windows がセカンドスクリーンを追加すると切り替わります。[RELEASE_NOTES.md](../RELEASE_NOTES.md) は各バージョンを、[ROADMAP.md](../ROADMAP.md) は今後の予定を説明します。

v0.3.3 のリリースチェックは Windows、macOS、iPad シミュレーターで合格しました。[docs/hardware-verification.md](../docs/hardware-verification.md) には v0.3.0 からの実際の PC と iPad での検証結果を記録しています。

両アプリは問題を発見し修正するため、デバイスの詳細、アプリのエラー、配信統計などの診断情報を共有します。PC と iPad の名前、オーディオデバイス名、ネットワークアドレスも含まれます。画面の内容、音声、ペアリングの認証情報は含まれません。共有は設定で無効にできます。[自動診断](../SETUP.md#automatic-diagnostics)と[プライバシー通知](https://eternalmonitor.dev/privacy.html)には送信内容と保存期間が詳しく記載されています。iPad では設定 > サポート > 診断を開き、レポートをコピーまたは書き出せます。

## 仕組み

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Windows PC がデスクトップをキャプチャし、GPU で映像をエンコードして Wi-Fi または USB で送信します。iPad は映像を再構成し、Wi-Fi では欠けた断片を要求して、VideoToolbox でデコードし Metal で表示します。PC の音声は WASAPI でキャプチャされ、Opus にエンコードされて iPad で再生されます。タッチ、Pencil、キーボード、トラックパッドの入力は PC に送り返され、PC に入力されます。">
</picture>

Windows ホストは Rust で書かれています。DXGI でデスクトップをキャプチャし、対応するグラフィックスハードウェア上で FFmpeg と NVIDIA NVENC、AMD AMF または Intel Quick Sync を使ってエンコードし（標準では H.264、または HEVC）、各フレームを番号付きの UDP 断片として送信します。USB では同じメッセージを Apple のデバイスサービスが提供するフレーム化されたトンネルで送ります。iPad アプリはネイティブの Swift 製です。断片を再構成し、VideoToolbox でデコードして Metal で描画します。Wi-Fi では欠けた断片も要求します。PC の音声は WASAPI でキャプチャして Opus で送信し、iPad はジッターバッファを通して再生し、失われた音声を再要求せずに補間します。入力と受信側のレポートは同じセッションで送り返され、ホストはレポートを使ってビットレートを 4〜50 Mbps の間で調整します。

[ARCHITECTURE.md](../ARCHITECTURE.md) ではパイプラインとプロトコル v2 を説明し、[docs/decisions.md](../docs/decisions.md) では各部分の仕組みを選んだ理由を説明しています。

## ソースからビルド

ワークスペースには Rust ホスト (`host/`)、プロトコルクレート `eternal-wire` (`proto/`)、Swift の iPad アプリ (`ios/`) が含まれます。

<details>
<summary><b>Windows ホスト</b></summary>

要件: Windows 10 バージョン 1809 以降、Rust 1.98.0（`rust-toolchain.toml` で固定、MSVC）、FFmpeg **7.1 shared** SDK、bindgen 用の LLVM/libclang。

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

ホストは Windows GUI プログラムなので、コンソールウィンドウは開きません。ターミナルから起動するとログはそこに出力されますが、プロンプトはすぐに戻ります。`cargo run -p eternal-host` は終了を待ちます。`ETERNAL_HEADLESS=1` で出力をリダイレクトした場合（`> host.log`）は専用のコンソールが開くため、ターミナルの Ctrl+C は届きません。そのコンソールウィンドウで Ctrl+C を押すか、タスクマネージャーを使用してください。`scripts\win\Stop-Host.ps1` は `remote.sh run-host` で起動したホストだけを停止します。

`scripts\build-installer.ps1` は Inno Setup と同じ `FFMPEG_DIR` を使って完全な Setup.exe をビルドします。`scripts\package.ps1` は最小構成の zip をビルドします。

</details>

<details>
<summary><b>macOS での開発</b>（Windows は不要）</summary>

macOS ではホストは画面キャプチャの代わりに合成テストパターンを使用します。プロトコル、エンコーダー、転送、監視機能はすべて実際に動作します。

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

Xcode のコマンドラインツールが開発者ディレクトリとして選択されている場合は、Xcode コマンドの前に `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` を付けてください。

</details>

<details>
<summary><b>iPad アプリ</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

実機の iPad で実行するには、生成されたプロジェクトを Xcode で開き、自分の署名チームを選択してください。

</details>

<details>
<summary><b>エンドツーエンドテスト</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

各行でデコード後のフレームレート（最低 55 fps）、ドロップ、修復、描画されたピクセルを確認します。`e2e_matrix.sh --real` は `scripts/win/` のランナーを通して Windows のハードウェアテストを実行します。先に [scripts/win/README.md](../scripts/win/README.md) を読んでください。CI はプルリクエストごとに Linux、macOS、Windows で Rust のテストスイート、iPad のテスト、配信システムテストを実行します。

</details>

<details>
<summary><b>ホストの環境変数</b></summary>

| Variable | Effect |
| --- | --- |
| `ETERNAL_ENCODER` | エンコーダーを指定します（`h264_nvenc`、`h264_amf`、`h264_qsv`、`libx264`） |
| `ETERNAL_HEVC` | `1` または `0` で HEVC の設定を上書きします |
| `ETERNAL_FPS` | ホストのフレームレートを変更します。iPad の設定が上限になります |
| `ETERNAL_INPUT` | エンコーダー入力: `yuv420`（既定）、`auto`、`bgra` |
| `ETERNAL_MAX_DGRAM` | メディアデータグラムのサイズ、576〜1400 バイト |
| `ETERNAL_CAPTURE` | `synthetic` で DXGI の代わりに生成されたテストパターンを使用します |
| `ETERNAL_AUDIO` | `synthetic` で再現可能な音声テストソースを選択します |
| `ETERNAL_HEADLESS` | `1` でウィンドウなしで実行します。Ctrl+C で正常に終了します |
| `ETERNAL_ABR` | `0` で適応ビットレートを無効にします |
| `ETERNAL_VDD_TIMEOUT_SECS` | 仮想ディスプレイの接続を待つ時間（既定は 30） |
| `ETERNAL_AMF_DIAG` | `1` で AMF ビットストリームの診断情報を出力します |
| `ETERNAL_LEGACY_PTS` | `1` で旧来のフレームカウンターによるタイムスタンプに戻します |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | テスト専用: 損失、順序変更、ジッターを発生させます |
| `ETERNAL_USB_DIRECT` | テスト専用: USB トンネルの代わりに TCP エンドポイントを使用します |

</details>

## ドキュメント

| ドキュメント | 内容 |
| --- | --- |
| [SETUP.md](../SETUP.md) | USB と描画を含む、誰でも使える手順ごとのセットアップ |
| [既知の問題](https://eternalmonitor.dev/known-issues.html) | 把握している問題とそれぞれの対処法（`docs/known-issues.html`） |
| [プライバシー](https://eternalmonitor.dev/privacy.html) | アプリが送信する情報、その理由、保存期間、無効にする方法（`docs/privacy.html`） |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | パイプライン、プロトコル v2、各コンポーネント |
| [docs/decisions.md](../docs/decisions.md) | 各設計を選んだ理由と測定結果 |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil、Windows Ink、Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | 機能の詳細 |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | リリーステストの記録と実機確認リスト |
| [リリーステスト](../docs/beta-testing.md) | TestFlight でビルドを公開する方法: 署名とテスト内容 |
| [docs/design.md](../docs/design.md) | アプリとウェブサイトに共通するデザイン |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | 各バージョンの変更内容 |
| [ROADMAP.md](../ROADMAP.md) | 今後の予定 |

## 貢献

転送、エンコーダー、描画、ドキュメントなどへの貢献を歓迎します。プルリクエストを開く前に [CONTRIBUTING.md](../CONTRIBUTING.md) を読んでください。最初に取り組みやすい貢献が記載されており、[ROADMAP.md](../ROADMAP.md) には今後の大きな作業が示されています。まずアイデアについて相談したい場合は、[Sideband Discord](https://discord.gg/Xc7NykBZ6z) でお尋ねください。

## お問い合わせ

質問、セットアップの相談、アイデアは [Sideband Discord](https://discord.gg/Xc7NykBZ6z) へお寄せください。[hello@sideband.studio](mailto:hello@sideband.studio) にメールを送ることもできます。

不具合を見つけた場合は、両アプリのバージョンとホストのログを添えて [issue を作成](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)するか、スタジオにメールを送る[ダウンロードページのフィードバックフォーム](https://eternalmonitor.dev/download.html#feedback)を使ってください。セキュリティの問題は Discord や issue ではなく、[SECURITY.md](../SECURITY.md) の説明に従って非公開で報告してください。

## ライセンス

EternalMonitor は [MIT License](../LICENSE) の下で公開されています。© 2026 Sideband Studio。Windows インストーラーには、[Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) や FFmpeg など、それぞれのライセンスに基づくサードパーティ製コンポーネントも含まれます。

[Sideband Studio](https://sideband.studio) が制作しました。
