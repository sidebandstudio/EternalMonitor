<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · **日本語** · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*このページと [英語の README](../README.md) に相違がある場合は、英語版が最新版です。*

<img src="../docs/assets/icon.png" width="88" height="88" alt="EternalMonitor のロゴ">

# EternalMonitor

Windows デスクトップを Wi-Fi または USB 経由で iPad にミラーリングまたは拡張し、<br>
タッチ、キーボード、Apple Pencil で PC を操作できます。

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#ダウンロード)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[ダウンロード](#ダウンロード) · [セットアップガイド](../SETUP.md) · [既知の問題](https://eternalmonitor.dev/known-issues.html) · [ウェブサイト](https://eternalmonitor.dev) · [リリースノート](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="Windows の EternalMonitor アプリが iPad に配信し、iPad に PC の画面が表示されている様子">
</p>

EternalMonitor は Windows デスクトップを iPad に配信し、入力を PC に送り返します。小さな Windows アプリが画面をキャプチャし、グラフィックスカードでエンコードします。iPad アプリは動画をハードウェアでデコードして Metal で描画し、タップ、キー入力、Pencil のストロークを PC に送ります。無料で、MIT License のオープンソースです。アカウントは必要ありません。

## 機能

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="接続、QRコードをスキャン、PCを検索が表示された iPad アプリの接続画面">

- **拡張またはミラーリング。** iPad をモニターの隣に追加ディスプレイとして使うことも、既存の画面をミラーリングすることもできます。追加ディスプレイは iPad の切断から 10 秒後に消えるため、すぐに再接続すれば再利用でき、Windows に不要なモニターが残りません。
- **Wi-Fi または USB。** 自宅のネットワーク経由で配信するか、ケーブルを接続してより安定した映像を受信しながら iPad を充電できます。実行中の Wi-Fi セッションはケーブル接続に切り替わり、ケーブルを抜くと Wi-Fi に戻ります。
- **タッチ、キーボード、ポインター。** タップでクリック、ドラッグ、2 本指でスクロール、長押しで右クリックできます。ハードウェアキーボード、トラックパッド、マウスも使え、修飾キーの固定や中クリックにも対応します。
- **Apple Pencil をペンとして使用。** Windows は Windows Ink の筆圧と傾きを受け取り、どのモニター配置でもペン先の位置に描画します。**描画モード** (Drawing mode) はキャンバス上の手のひらを無視します。Clip Studio Paint では Tablet PC 設定で動作します。
- **全体を表示または画面いっぱい。** **全体を表示** (Fit) は PC の画面全体を表示し、縦横比の違いによる余白に帯を表示します。**画面いっぱい** (Fill) は iPad の画面全体を使い、収まらない端を切り取ります。
- **PC の音声。** 音声は Opus として iPad で再生され、ジッターバッファと損失補間を使用します。映像を止めずに消音できます。
- **Wi-Fi の損失を修復。** パケットが失われると iPad がすぐにその断片を再要求するため、1 パケットの損失でフレームが失われることはまれです。ビットレートは 4〜50 Mbps の間で調整されます。
- **GPU でエンコード。** NVIDIA NVENC、AMD AMF、Intel Quick Sync が、標準では H.264、選択すれば HEVC で、30、60、90、120 fps のいずれかでエンコードします。
- **ペアリング。** 最初の Wi-Fi 接続時に 6 桁のコードか QR スキャンが必要です。その後は iPad が PC を記憶します。USB ではコードは不要です。
- **自動復帰と問題の説明。** ロック画面、ディスプレイの変更、配信の再起動後に映像が自動で戻ります。問題が起きた場合は両アプリが状況と対処法を表示し、**フィードバックを送信** (Send feedback) で環境情報を入力済みのメールをスタジオ宛てに作成します。
- **ワンクリックで更新。** 新しいバージョンが出るとポップアップで通知します。Windows では **今すぐアップデート** (Update now) でダウンロードを検証し、Windows の確認画面 1 回でインストールします。iPad アプリでは TestFlight が開きます。Windows アプリはコンソールウィンドウなしで実行されます。

<br clear="right">

## ダウンロード

| | 動作要件 | 入手先 |
| --- | --- | --- |
| **Windows** | Windows 10 バージョン 1809 以降または Windows 11、64 ビット、NVIDIA、AMD、Intel のいずれかのグラフィックス | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3)（v0.3.3 が現在の安定版） |
| **iPad** | iPadOS 17 以降 | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc)。公開ベータに参加できます。TestFlight をインストールし、iPad でリンクを開いてください。 |

eternalmonitor.dev/download.html に掲載されている Windows 版と iPad 版をインストールしてください。バージョン番号が異なっていても、この組み合わせで動作します。古い v0.1 リリースは別のプロトコルを使うため、v0.3 の iPad アプリには接続できません。

テスト中に見つけた不具合やフィードバックは、hello@sideband.studio にメールが届く[ダウンロードページのフォーム](https://eternalmonitor.dev/download.html#feedback)、[GitHub issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)、または [Sideband Discord](https://discord.gg/Xc7NykBZ6z) からお知らせください。

インストーラーはまだコード署名されていないため、実行前に Windows SmartScreen の確認が表示されます。ファイルの SHA-256 が[リリースページ](https://github.com/sidebandstudio/EternalMonitor/releases)の値と一致することを確認したうえで、**詳細情報** (More info)、**実行** (Run anyway) の順に選択してください。

USB には Apple の無料アプリ [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) も必要です。配信中はこのアプリを開いてください。Windows がケーブル経由で iPad に接続するにはこのアプリが必要で、閉じているかインストールされていない場合は EternalMonitor が通知します。Apple Devices には Windows 10 バージョン 22H2 (build 19045) または Windows 11 が必要です。それより古い Windows 10 では Wi-Fi で接続してください。

**次に、[セットアップガイド](../SETUP.md)に従ってください。** 両アプリのインストール、ペアリング、USB、拡張ディスプレイ、描画について、実際に表示されるボタン名を使って説明しています。

## 状況

v0.3.3 では、**セカンドスクリーン** (Second screen) が起動しなかった PC、特に Intel と NVIDIA のグラフィックスを搭載したノート PC での問題を修正しました。セカンドスクリーンの起動中は iPad に PC のメイン画面が表示され、PC の画面が繰り返し点滅することもなくなりました。Windows 0.3.3 に合わせて iPad を TestFlight v0.3.3, build 364 に更新してください。

Windows と iPad の診断情報には、デバイスの詳細、アプリのエラー、配信の統計が含まれます。レポートにはグラフィックカードとディスプレイの名前、ネットワークアドレスが含まれますが、画面の内容、音声、ペアリングの認証情報は含まれません。0.3.4 から、Windows の送信データから PC の名前、ユーザー名、ユーザーフォルダー、iPad の名前、オーディオデバイス名が取り除かれ、各オーディオデバイスの種類だけが残ります。iPad の送信データからは PC の名前が取り除かれます。自動共有は **設定** (Settings) で無効にできます。診断情報は最大 30 日間保存され、それより早く削除される場合もあります。削除済みまたは期限切れの診断情報は、日次バックアップにさらに最大 7 日間残る場合があります。[自動診断](../SETUP.md#automatic-diagnostics)と[プライバシー通知](https://eternalmonitor.dev/privacy.html)を参照してください。iPad では **設定** (Settings) > **サポート** (Support) > **診断** (Diagnostics) を開き、レポートをコピーまたは書き出せます。

リリースチェックは Windows、macOS、iPad シミュレーターで合格しました。把握している問題は[既知の問題](https://eternalmonitor.dev/known-issues.html)に掲載しています。以前の実機での結果は [docs/hardware-verification.md](../docs/hardware-verification.md) に残っています。

既知の制限:

- 配信は暗号化されません。ペアリングによって他のデバイスの接続は防がれますが、自宅の Wi-Fi など信頼できるネットワークを使用してください。
- ホストとして使えるのは Windows アプリだけです。ホストは開発用に macOS でもビルドできますが、画面の代わりにテストパターンを配信します。
- Apple Pencil (USB-C) には筆圧センサーがないため、ストロークの幅は一定です。

[既知の問題](https://eternalmonitor.dev/known-issues.html)には、把握しているすべての問題と、それぞれの対処法を掲載しています。

## 仕組み

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Windows PC がデスクトップをキャプチャし、GPU で映像をエンコードして Wi-Fi または USB で送信します。iPad は映像を再構成し、Wi-Fi では欠けた断片を要求して、VideoToolbox でデコードし Metal で表示します。PC の音声は WASAPI でキャプチャされ、Opus にエンコードされて iPad で再生されます。タッチ、Pencil、キーボード、トラックパッドの入力は PC に送り返され、PC に入力されます。">
</picture>

Windows ホストは Rust で書かれています。DXGI でデスクトップをキャプチャし、FFmpeg を通して GPU でエンコードして、各フレームを番号付きの UDP 断片として送信します。USB では同じメッセージを Apple のデバイスサービスが提供するフレーム化されたトンネルで送ります。iPad アプリはネイティブの Swift 製です。断片を再構成し、VideoToolbox でデコードして Metal で描画します。Wi-Fi では欠けた断片も要求します。PC の音声は WASAPI でキャプチャして Opus で送信し、iPad は失われた音声を再要求せずに補間します。入力と受信側のレポートは同じセッションで送り返され、ホストはレポートを使ってビットレートを調整します。

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

以下のドキュメントは英語です。

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
| [docs/beta-testing.md](../docs/beta-testing.md) | ベータテストの実施方法: TestFlight、署名、テスト内容 |
| [docs/design.md](../docs/design.md) | アプリとウェブサイトに共通するデザイン |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | 各バージョンの変更内容 |

## 貢献

転送、エンコーダー、描画、ドキュメントなどへの貢献を歓迎します。プルリクエストを開く前に [CONTRIBUTING.md](../CONTRIBUTING.md) を読んでください。まずアイデアについて相談したい場合は、[Sideband Discord](https://discord.gg/Xc7NykBZ6z) でお尋ねください。

## お問い合わせ

質問、セットアップの相談、アイデアは [Sideband Discord](https://discord.gg/Xc7NykBZ6z) へお寄せください。[hello@sideband.studio](mailto:hello@sideband.studio) にメールを送ることもできます。

不具合を見つけた場合は、両アプリのバージョンとホストのログを添えて [issue を作成](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)してください。セキュリティの問題は Discord や issue ではなく、[SECURITY.md](../SECURITY.md) の説明に従って非公開で報告してください。

## ライセンス

EternalMonitor は [MIT License](../LICENSE) の下で公開されています。© 2026 Sideband Studio。Windows インストーラーには、[Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) や FFmpeg など、それぞれのライセンスに基づくサードパーティ製コンポーネントも含まれます。

[Sideband Studio](https://sideband.studio) が制作しました。
