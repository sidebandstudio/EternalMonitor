<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · **Deutsch** · [한국어](README.ko.md)

*Bei Abweichungen von der [englischen README](../README.md) gilt die englische Version.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="EternalMonitor-Logo">

# EternalMonitor

Ein kostenloser, quelloffener zweiter Monitor für Windows.<br>
Verbinde dein iPad über Wi-Fi oder USB, mit Unterstützung für Touch und Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Herunterladen](#download) · [Einrichtungsanleitung](../SETUP.md) · [Website](https://eternalmonitor.dev) · [Roadmap](../ROADMAP.md) · [Versionshinweise](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="Ein Windows-PC und ein iPad. Auf dem PC schaltet das EternalMonitor-Fenster das iPad auf Zweiter Bildschirm um. Ein Fenster wird vom Monitor auf das iPad gezogen. Anschließend zeichnet ein Apple Pencil Grashalme auf einem Bild auf dem iPad. Die Striche sind beim Aufdrücken dick und beim Anheben dünn.">
</p>

EternalMonitor macht ein iPad zum Monitor für deinen Windows-PC. Die Software ist kostenlos, unter der MIT-Lizenz quelloffen und benötigt kein Konto. Der PC streamt mit bis zu 120 fps, mit Hardware-Kodierung auf unterstützten Grafikkarten und Hardware-Dekodierung auf dem iPad. Jede Berührung, jeder Tastendruck und jeder Pencil-Strich geht zurück an den PC.

<a id="download"></a>

## Schnellstart

[![Für Windows herunterladen](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![iPad-App über TestFlight laden](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **Installiere die Windows-App.** Starte [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe) auf Windows 10 (Version 1809 oder neuer) oder Windows 11, 64 Bit, mit NVIDIA-, AMD- oder Intel-Grafik. Falls Windows SmartScreen nachfragt, wähle **Weitere Informationen** und dann **Trotzdem ausführen**. Die [Release-Seite](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) nennt den SHA-256-Wert der Datei.
2. **Installiere die iPad-App.** Installiere auf einem iPad mit iPadOS 17 oder neuer **TestFlight** aus dem App Store. Öffne dann den [EternalMonitor-TestFlight-Link](https://testflight.apple.com/join/ppc9QfXc) auf dem iPad und tippe auf **Installieren**.
3. **Verbinde die Geräte.** Öffne beide Apps im selben Netzwerk und tippe auf dem iPad auf deinen PC oder scanne den QR-Code, den der PC anzeigt. Für USB unter Windows 10 22H2 oder Windows 11 schließe das Kabel an und lasse Apples kostenlose App [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) auf dem PC geöffnet.

Die [Einrichtungsanleitung](../SETUP.md) erklärt jeden Schritt mit den genauen Namen der Schaltflächen. Die [Downloadseite](https://eternalmonitor.dev/download.html) nennt die zusammengehörigen Versionen für Windows und iPad.

Wenn EternalMonitor für dich nützlich ist, hilft ein Stern anderen, es zu finden.

## Funktionen

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="Der Verbindungsbildschirm der iPad-App mit Verbinden, QR-Code scannen und PCs suchen">

- **Erweitern oder spiegeln.** Nutze das iPad als zweiten Bildschirm neben deinem Monitor oder spiegle einen beliebigen Bildschirm. Der zweite Bildschirm bleibt nach der Trennung des iPad 10 Sekunden bestehen. Bei einer schnellen Neuverbindung wird er wiederverwendet und Windows behält keinen Geistermonitor.
- **Wi-Fi oder USB.** Streame über dein Heimnetz oder schließe ein Kabel für ein stabileres Bild an, das das iPad auch über einen USB-C-Anschluss laden kann. Ein Kabel übernimmt eine laufende Wi-Fi-Sitzung. Beim Abziehen wird wieder Wi-Fi verwendet.
- **Apple Pencil als echter Stift.** Windows empfängt Druck und Neigung über Windows Ink, und der Strich liegt bei jeder Monitoranordnung unter der Spitze. Der **Zeichenmodus** (Drawing mode) ignoriert deine Handfläche auf der Zeichenfläche und Clip Studio Paint funktioniert mit seiner Einstellung Tablet PC.
- **Touch, Tastatur und Zeiger.** Tippe zum Klicken, ziehe Elemente, scrolle mit zwei Fingern und halte für einen Rechtsklick gedrückt. Auch Hardware-Tastaturen, Trackpads und Mäuse funktionieren, mit einrastenden Sondertasten und Mittelklick.
- **Hardware-Kodierung.** Unterstützte NVIDIA-, AMD- und Intel-Grafik kodiert den Stream mit 30, 60, 90 oder 120 fps.
- **Verlustkorrektur für Wi-Fi.** Fehlt ein Paket, fordert das iPad es sofort erneut an. Ein verlorenes Paket kostet daher selten ein Bild. Die Bitrate passt sich deinem Netzwerk an.
- **PC-Ton.** Der Ton wird auf dem iPad wiedergegeben. Du kannst ihn stummschalten, ohne das Video anzuhalten.
- **Einpassen oder füllen.** **Einpassen** (Fit) zeigt das ganze PC-Bild und bei unterschiedlichen Seitenverhältnissen Balken. **Füllen** (Fill) nutzt den gesamten iPad-Bildschirm und schneidet überstehende Ränder ab.
- **Kopplung.** Bei der ersten Wi-Fi-Verbindung wird ein sechsstelliger Code oder ein QR-Scan verlangt. Danach merkt sich das iPad den PC. USB benötigt keinen Code.
- **Wiederherstellung und Hinweise.** Das Bild kehrt nach dem Sperrbildschirm, einer Anzeigeänderung oder einem Neustart des Streams selbst zurück. Beide Apps erklären, was passiert ist und was du tun kannst. **Feedback senden** (Send feedback) erstellt eine E-Mail an das Studio mit vorausgefüllten Angaben zu deiner Einrichtung.
- **Updates mit einem Klick.** Ein Hinweis meldet eine neue Version. Unter Windows prüft **Jetzt aktualisieren** (Update now) den Download und installiert ihn mit einer einzigen Windows-Abfrage. Auf dem iPad öffnet es TestFlight. Die Windows-App läuft ohne Konsolenfenster.

Diese README und die [Website](https://eternalmonitor.dev) sind in 11 Sprachen verfügbar.

<br clear="right">

## Mehr erfahren

- [Ein iPad als zweiten Monitor für Windows verwenden](https://eternalmonitor.dev/ipad-second-monitor-windows.html): die vollständige Anleitung.
- [Kompatibilität](https://eternalmonitor.dev/compatibility.html): PCs und iPads, auf denen EternalMonitor verwendet wird.
- [Alternativen](https://eternalmonitor.dev/alternatives.html): EternalMonitor im Vergleich mit anderen Möglichkeiten, ein iPad als Bildschirm zu nutzen.
- [Roadmap](../ROADMAP.md): was als Nächstes kommt.

**Teile deine Einrichtung.** Teile uns über den [Kompatibilitätsbericht](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml) mit, welchen PC und welches iPad du verwendest, oder poste es auf dem [Sideband Discord](https://discord.gg/Xc7NykBZ6z). Mit deiner Erlaubnis erscheint es auf der Kompatibilitätsseite.

## Status

Die aktuelle Version ist v0.3.3 für Windows und v0.3.3 (build 364) für das iPad. Sie bringt **Zweiter Bildschirm** (Second screen) auf mehr Laptops mit Intel- und NVIDIA-Grafik: Das iPad zeigt sofort den Hauptbildschirm des PCs und wechselt zum zweiten Bildschirm, sobald Windows ihn hinzufügt. [RELEASE_NOTES.md](../RELEASE_NOTES.md) beschreibt jede Version und [ROADMAP.md](../ROADMAP.md), was als Nächstes kommt.

Die Release-Prüfungen für v0.3.3 waren auf Windows, macOS und iPad-Simulatoren erfolgreich. [docs/hardware-verification.md](../docs/hardware-verification.md) dokumentiert praktische Ergebnisse auf einem echten PC und iPad seit v0.3.0.

Die Diagnosedaten von Windows und iPad enthalten Gerätedetails, App-Fehler und Streaming-Statistiken. Berichte enthalten Namen von Grafikkarten und Bildschirmen sowie Netzwerkadressen, aber keine Bildschirminhalte, Audiodaten oder Kopplungsdaten. Ab Version 0.3.4 werden aus den von Windows gesendeten Daten der Name des PCs, der Benutzername, der Benutzerordner, der Name des iPad und die Namen der Audiogeräte entfernt, wobei nur die Art jedes Audiogeräts erhalten bleibt; aus den vom iPad gesendeten Daten wird der Name des PCs entfernt. Deaktiviere die automatische Freigabe in **Einstellungen** (Settings). Diagnosedaten werden bis zu 30 Tage gespeichert und können früher gelöscht werden. Gelöschte oder abgelaufene Diagnosedaten können bis zu weitere 7 Tage in täglichen Backups verbleiben. Siehe [Automatische Diagnose](../SETUP.md#automatic-diagnostics) und den [Datenschutzhinweis](https://eternalmonitor.dev/privacy.html). Öffne auf dem iPad **Einstellungen** (Settings) > **Support** > **Diagnose** (Diagnostics), um einen Bericht zu kopieren oder zu exportieren.

## Funktionsweise

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Der Windows-PC erfasst den Desktop, kodiert das Video auf der GPU und sendet es über Wi-Fi oder USB. Das iPad setzt das Video zusammen, fordert fehlende Fragmente über Wi-Fi an, dekodiert es mit VideoToolbox und zeigt es mit Metal. PC-Ton wird mit WASAPI erfasst, als Opus kodiert und auf dem iPad abgespielt. Eingaben per Touch, Pencil, Tastatur und Trackpad gehen zurück an den PC, der sie einspeist.">
</picture>

Der Windows-Host ist in Rust geschrieben. Er erfasst den Desktop mit DXGI, kodiert über FFmpeg mit NVIDIA NVENC, AMD AMF oder Intel Quick Sync auf unterstützter Grafikhardware (standardmäßig H.264 oder HEVC) und sendet jedes Bild als nummerierte UDP-Fragmente. Über USB laufen dieselben Nachrichten durch einen Tunnel mit Nachrichtenrahmen, den Apples Gerätedienst bereitstellt. Die iPad-App ist nativ in Swift geschrieben. Sie setzt Fragmente zusammen, dekodiert mit VideoToolbox und rendert mit Metal. Über Wi-Fi fordert sie auch fehlende Fragmente erneut an. PC-Ton wird mit WASAPI erfasst und als Opus gesendet. Das iPad spielt ihn über einen Jitter-Puffer ab und verdeckt Audioverluste, statt Daten erneut anzufordern. Eingaben und Empfängerberichte gehen über dieselbe Sitzung zurück; der Host passt anhand der Berichte die Bitrate zwischen 4 und 50 Mbps an.

[ARCHITECTURE.md](../ARCHITECTURE.md) beschreibt die Verarbeitungskette und Protokoll v2, [docs/decisions.md](../docs/decisions.md) erklärt die Entscheidungen für die einzelnen Teile.

## Aus dem Quellcode bauen

Der Workspace enthält den Rust-Host (`host/`), die Protokoll-Crate `eternal-wire` (`proto/`) und die Swift-iPad-App (`ios/`).

<details>
<summary><b>Windows-Host</b></summary>

Voraussetzungen: Windows 10 Version 1809 oder neuer, Rust 1.98.0 (durch `rust-toolchain.toml` festgelegt, MSVC), ein FFmpeg **7.1 shared** SDK und LLVM/libclang für bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

Der Host ist ein grafisches Windows-Programm und öffnet daher kein Konsolenfenster. Aus einem Terminal gestartet, schreibt er sein Log dorthin, gibt aber die Eingabeaufforderung sofort frei; `cargo run -p eternal-host` wartet auf ihn. Mit `ETERNAL_HEADLESS=1` und umgeleiteter Ausgabe (`> host.log`) erhält er eine eigene Konsole. Ctrl+C im Terminal erreicht ihn dann nicht: Drücke Ctrl+C in seinem Konsolenfenster oder verwende den Task-Manager. `scripts\win\Stop-Host.ps1` stoppt nur Hosts, die `remote.sh run-host` gestartet hat.

`scripts\build-installer.ps1` baut die vollständige Setup.exe mit Inno Setup und demselben `FFMPEG_DIR`. `scripts\package.ps1` baut ein einfaches ZIP.

</details>

<details>
<summary><b>Entwicklung auf macOS</b> (ohne Windows)</summary>

Auf macOS ersetzt der Host die Bildschirmaufnahme durch ein synthetisches Testbild. Protokoll, Encoder, Transport und Supervisor laufen tatsächlich.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

Wenn die Kommandozeilenwerkzeuge von Xcode als Entwicklerverzeichnis ausgewählt sind, stelle Xcode-Befehlen `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` voran.

</details>

<details>
<summary><b>iPad-App</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

Für die Ausführung auf einem physischen iPad öffne das erzeugte Projekt in Xcode und wähle dein eigenes Signierungsteam.

</details>

<details>
<summary><b>End-to-End-Tests</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

Jede Zeile prüft die dekodierte Bildrate (mindestens 55 fps), Verluste, Reparaturen und gerenderte Pixel. `e2e_matrix.sh --real` führt die Windows-Hardwarezeilen über den Runner in `scripts/win/` aus. Lies zuerst [scripts/win/README.md](../scripts/win/README.md). CI führt bei jedem Pull Request die Rust-Tests auf Linux, macOS und Windows, die iPad-Tests und einen Streaming-Systemtest aus.

</details>

<details>
<summary><b>Umgebungsvariablen des Hosts</b></summary>

| Variable | Wirkung |
| --- | --- |
| `ETERNAL_ENCODER` | Encoder erzwingen (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` oder `0` überschreibt die HEVC-Einstellung |
| `ETERNAL_FPS` | Bildrate des Hosts überschreiben; die iPad-Einstellung begrenzt sie weiterhin |
| `ETERNAL_INPUT` | Encoder-Eingabe: `yuv420` (Standard), `auto` oder `bgra` |
| `ETERNAL_MAX_DGRAM` | Größe der Mediendatagramme, 576 bis 1400 Byte |
| `ETERNAL_CAPTURE` | `synthetic` ersetzt DXGI durch ein erzeugtes Testbild |
| `ETERNAL_AUDIO` | `synthetic` wählt die deterministische Audio-Testquelle |
| `ETERNAL_HEADLESS` | `1` startet ohne Fenster; Ctrl+C beendet sauber |
| `ETERNAL_ABR` | `0` deaktiviert die adaptive Bitrate |
| `ETERNAL_VDD_TIMEOUT_SECS` | Wartezeit auf die Verbindung des virtuellen Bildschirms (Standard 30) |
| `ETERNAL_AMF_DIAG` | `1` schreibt AMF-Bitstromdiagnosen |
| `ETERNAL_LEGACY_PTS` | `1` stellt die alten Zeitstempel anhand des Bildzählers wieder her |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | Nur für Tests: Verluste, Umordnung oder Jitter einschleusen |
| `ETERNAL_USB_DIRECT` | Nur für Tests: TCP-Endpunkt statt USB-Tunnel |

</details>

## Dokumentation

| Dokument | Inhalt |
| --- | --- |
| [SETUP.md](../SETUP.md) | Schrittweise Einrichtung für alle, einschließlich USB und Zeichnen |
| [Bekannte Probleme](https://eternalmonitor.dev/known-issues.html) | Bekannte Probleme und jeweilige Abhilfen (`docs/known-issues.html`) |
| [Datenschutz](https://eternalmonitor.dev/privacy.html) | Was die Apps senden, warum, wie lange es gespeichert wird und wie du es abschaltest (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | Verarbeitungskette, Protokoll v2 und Komponenten |
| [docs/decisions.md](../docs/decisions.md) | Gründe für die Entscheidungen, mit Messwerten |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil, Windows Ink und Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | Einzelheiten zu Funktionen |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | Release-Testprotokoll und Checkliste für praktische Tests |
| [Release-Tests](../docs/beta-testing.md) | Einen Build über TestFlight veröffentlichen: Signierung und Testumfang |
| [docs/design.md](../docs/design.md) | Gemeinsames Erscheinungsbild der Apps und Website |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | Änderungen in jeder Version |
| [ROADMAP.md](../ROADMAP.md) | Was als Nächstes kommt |

## Mitwirken

Beiträge jeder Art sind willkommen: Issues, Ideen, Fehlerbehebungen, Dokumentation, Übersetzungen und neue Funktionen. Pull Requests sind willkommen, ob groß oder klein. [CONTRIBUTING.md](../CONTRIBUTING.md) erklärt, wie du einen einreichst, und nennt gute erste Beiträge. [ROADMAP.md](../ROADMAP.md) zeigt die größeren anstehenden Arbeiten. Fragen und Ideen sind auf dem [Sideband Discord](https://discord.gg/Xc7NykBZ6z) willkommen.

## Kontakt

Der [Sideband Discord](https://discord.gg/Xc7NykBZ6z) ist für Fragen, Hilfe bei der Einrichtung und Ideen da. Du kannst auch an [hello@sideband.studio](mailto:hello@sideband.studio) schreiben.

Einen Fehler gefunden? [Öffne ein Issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) mit den Versionen beider Apps und dem Host-Log oder nutze das [Feedbackformular auf der Downloadseite](https://eternalmonitor.dev/download.html#feedback), das eine E-Mail an das Studio sendet. Melde Sicherheitsprobleme privat wie in [SECURITY.md](../SECURITY.md) beschrieben, nicht auf Discord oder in einem Issue.

## Lizenz

EternalMonitor wird unter der [MIT License](../LICENSE) veröffentlicht, © 2026 Sideband Studio. Das Windows-Installationsprogramm enthält auch Komponenten Dritter unter deren eigenen Lizenzen, darunter [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) und FFmpeg.

Entwickelt von [Sideband Studio](https://sideband.studio).
