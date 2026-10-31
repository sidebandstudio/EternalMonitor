<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · **Deutsch** · [한국어](README.ko.md)

*Bei Abweichungen von der [englischen README](../README.md) gilt die englische Version.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="EternalMonitor-Logo">

# EternalMonitor

Spiegle oder erweitere deinen Windows-Desktop über Wi-Fi oder USB auf ein iPad<br>
und steuere den PC per Touch, Tastatur oder Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#herunterladen)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Herunterladen](#herunterladen) · [Einrichtungsanleitung](../SETUP.md) · [Bekannte Probleme](https://eternalmonitor.dev/known-issues.html) · [Website](https://eternalmonitor.dev) · [Versionshinweise](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="Die Windows-App von EternalMonitor streamt das Bild des PCs auf ein iPad">
</p>

EternalMonitor streamt deinen Windows-Desktop auf ein iPad und sendet deine Eingaben zurück. Eine kleine Windows-App erfasst den Bildschirm und kodiert ihn auf deiner Grafikkarte. Die iPad-App dekodiert das Video per Hardware und stellt es mit Metal dar. Jede Berührung, jeder Tastendruck und jeder Pencil-Strich geht zurück an den PC. Die Software ist kostenlos, unter der MIT License quelloffen und benötigt kein Konto.

## Funktionen

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="Der Verbindungsbildschirm der iPad-App mit Verbinden, QR-Code scannen und PCs suchen">

- **Erweitern oder spiegeln.** Nutze das iPad als zusätzlichen Bildschirm neben deinem Monitor oder spiegle einen beliebigen Bildschirm. Der zusätzliche Bildschirm verschwindet 10 Sekunden nach der Trennung des iPad. Bei einer schnellen Neuverbindung wird er wiederverwendet und Windows behält keinen Geistermonitor.
- **Wi-Fi oder USB.** Streame über dein Heimnetz oder schließe ein Kabel für ein stabileres Bild an, das auch das iPad lädt. Ein Kabel übernimmt eine laufende Wi-Fi-Sitzung. Beim Abziehen wird wieder Wi-Fi verwendet.
- **Touch, Tastatur und Zeiger.** Tippe zum Klicken, ziehe Elemente, scrolle mit zwei Fingern und halte für einen Rechtsklick gedrückt. Auch Hardware-Tastaturen, Trackpads und Mäuse funktionieren, mit einrastenden Sondertasten und Mittelklick.
- **Apple Pencil als echter Stift.** Windows empfängt Druck und Neigung über Windows Ink, und der Strich liegt bei jeder Monitoranordnung unter der Spitze. Der **Zeichenmodus** (Drawing mode) ignoriert deine Handfläche auf der Zeichenfläche und Clip Studio Paint funktioniert mit seiner Einstellung Tablet PC.
- **Einpassen oder füllen.** **Einpassen** (Fit) zeigt das ganze PC-Bild und bei unterschiedlichen Seitenverhältnissen Balken. **Füllen** (Fill) nutzt den gesamten iPad-Bildschirm und schneidet überstehende Ränder ab.
- **PC-Ton.** Der Ton wird auf dem iPad als Opus mit Jitter-Puffer und Verlustverdeckung wiedergegeben. Du kannst ihn stummschalten, ohne das Video anzuhalten.
- **Verlustkorrektur für Wi-Fi.** Fehlt ein Paket, fordert das iPad das Fragment sofort erneut an. Ein verlorenes Paket kostet daher selten ein Bild. Die Bitrate passt sich zwischen 4 und 50 Mbps an.
- **Deine GPU übernimmt die Arbeit.** NVIDIA NVENC, AMD AMF oder Intel Quick Sync kodieren standardmäßig H.264 oder auf Wunsch HEVC mit 30, 60, 90 oder 120 fps.
- **Kopplung.** Bei der ersten Wi-Fi-Verbindung wird ein sechsstelliger Code oder ein QR-Scan verlangt. Danach merkt sich das iPad den PC. USB benötigt keinen Code.
- **Wiederherstellung und Hinweise.** Das Bild kehrt nach dem Sperrbildschirm, einer Anzeigeänderung oder einem Neustart des Streams selbst zurück. Bei Problemen erklären beide Apps, was passiert ist und was du tun kannst. **Feedback senden** (Send feedback) erstellt eine E-Mail an das Studio mit vorausgefüllten Angaben zu deiner Einrichtung.
- **Updates mit einem Klick.** Ein Hinweis meldet eine neue Version. Unter Windows prüft **Jetzt aktualisieren** (Update now) den Download und installiert ihn mit einer einzigen Windows-Abfrage. Die iPad-App öffnet TestFlight. Die Windows-App läuft ohne Konsolenfenster.

<br clear="right">

## Herunterladen

| | Voraussetzungen | Download |
| --- | --- | --- |
| **Windows** | Windows 10 Version 1809 oder neuer oder Windows 11, 64 Bit, mit NVIDIA-, AMD- oder Intel-Grafik | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) (v0.3.3, aktuelle stabile Version) |
| **iPad** | iPadOS 17 oder neuer | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc). Die öffentliche Beta ist offen: Installiere TestFlight und öffne den Link auf deinem iPad. |

Installiere die auf eternalmonitor.dev/download.html aufgeführten Versionen für Windows und iPad. Sie funktionieren auch bei unterschiedlichen Versionsnummern zusammen. Die älteren v0.1-Versionen verwenden ein anderes Protokoll und können sich nicht mit der iPad-App v0.3 verbinden.

Du testest die App? Sende Fehler und Feedback über das [Formular auf der Downloadseite](https://eternalmonitor.dev/download.html#feedback), das eine E-Mail an hello@sideband.studio sendet, öffne ein [GitHub-Issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) oder schreibe uns auf dem [Sideband Discord](https://discord.gg/Xc7NykBZ6z).

Das Installationsprogramm ist noch nicht codesigniert, daher fragt Windows SmartScreen vor der Ausführung nach. Wähle **Weitere Informationen** (More info) und dann **Trotzdem ausführen** (Run anyway), nachdem du geprüft hast, dass der SHA-256-Wert der Datei mit dem auf der [Release-Seite](https://github.com/sidebandstudio/EternalMonitor/releases) übereinstimmt.

Für USB brauchst du auch Apples kostenlose App [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k), die während des Streamings geöffnet sein muss. Windows erreicht ein iPad per Kabel nur darüber. EternalMonitor meldet, wenn die App geschlossen ist oder fehlt. Apple Devices benötigt Windows 10 Version 22H2 (build 19045) oder Windows 11. Verbinde dich bei älterem Windows 10 über Wi-Fi.

**Folge als Nächstes der [Einrichtungsanleitung](../SETUP.md).** Sie erklärt die Installation beider Apps, Kopplung, USB, den erweiterten Bildschirm und das Zeichnen mit den genauen Namen der angezeigten Schaltflächen.

## Status

v0.3.3 behebt **Zweiter Bildschirm** (Second screen) auf PCs, auf denen dieser nicht startete, meist Laptops mit Intel- und NVIDIA-Grafik. Das iPad zeigt den Hauptbildschirm des PCs, während der zweite Bildschirm startet, und der PC-Bildschirm blinkt nicht mehr in einer Schleife. Aktualisiere das iPad auf TestFlight v0.3.3, build 364, zur Verwendung mit Windows 0.3.3.

Die Diagnosedaten von Windows und iPad enthalten Gerätedetails, App-Fehler und Streaming-Statistiken. Berichte enthalten Namen von Grafikkarten und Bildschirmen sowie Netzwerkadressen, aber keine Bildschirminhalte, Audiodaten oder Kopplungsdaten. Ab Version 0.3.4 werden aus den von Windows gesendeten Daten der Name des PCs, der Benutzername, der Benutzerordner, der Name des iPad und die Namen der Audiogeräte entfernt, wobei nur die Art jedes Audiogeräts erhalten bleibt; aus den vom iPad gesendeten Daten wird der Name des PCs entfernt. Deaktiviere die automatische Freigabe in **Einstellungen** (Settings). Diagnosedaten werden bis zu 30 Tage gespeichert und können früher gelöscht werden. Gelöschte oder abgelaufene Diagnosedaten können bis zu weitere 7 Tage in täglichen Backups verbleiben. Siehe [Automatische Diagnose](../SETUP.md#automatic-diagnostics) und den [Datenschutzhinweis](https://eternalmonitor.dev/privacy.html). Öffne auf dem iPad **Einstellungen** (Settings) > **Support** > **Diagnose** (Diagnostics), um einen Bericht zu kopieren oder zu exportieren.

Die Release-Prüfungen waren auf Windows, macOS und iPad-Simulatoren erfolgreich. Bekannte Probleme stehen auf der Seite [Bekannte Probleme](https://eternalmonitor.dev/known-issues.html). Frühere Ergebnisse auf physischen Geräten stehen weiterhin in [docs/hardware-verification.md](../docs/hardware-verification.md).

Bekannte Einschränkungen:

- Der Stream ist nicht verschlüsselt. Die Kopplung hält andere Geräte fern. Nutze daher ein vertrauenswürdiges Netzwerk wie dein Wi-Fi zu Hause.
- Nur die Windows-App dient als Host. Der Host lässt sich zur Entwicklung auf macOS bauen, streamt dort aber ein Testbild statt deines Bildschirms.
- Apple Pencil (USB-C) hat keinen Drucksensor, daher bleiben seine Striche gleich breit.

[Bekannte Probleme](https://eternalmonitor.dev/known-issues.html) führt alle bekannten Probleme und die jeweiligen Abhilfen auf.

## Funktionsweise

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Der Windows-PC erfasst den Desktop, kodiert das Video auf der GPU und sendet es über Wi-Fi oder USB. Das iPad setzt das Video zusammen, fordert fehlende Fragmente über Wi-Fi an, dekodiert es mit VideoToolbox und zeigt es mit Metal. PC-Ton wird mit WASAPI erfasst, als Opus kodiert und auf dem iPad abgespielt. Eingaben per Touch, Pencil, Tastatur und Trackpad gehen zurück an den PC, der sie einspeist.">
</picture>

Der Windows-Host ist in Rust geschrieben. Er erfasst den Desktop mit DXGI, kodiert auf der GPU über FFmpeg und sendet jedes Bild als nummerierte UDP-Fragmente. Über USB laufen dieselben Nachrichten durch einen Tunnel mit Nachrichtenrahmen, den Apples Gerätedienst bereitstellt. Die iPad-App ist nativ in Swift geschrieben. Sie setzt Fragmente zusammen, dekodiert mit VideoToolbox und rendert mit Metal. Über Wi-Fi fordert sie auch fehlende Fragmente erneut an. PC-Ton wird mit WASAPI erfasst und als Opus gesendet. Das iPad verdeckt Audioverluste, statt Daten erneut anzufordern. Eingaben und Empfängerberichte gehen über dieselbe Sitzung zurück; der Host passt anhand der Berichte die Bitrate an.

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

Diese Dokumente sind auf Englisch.

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
| [docs/beta-testing.md](../docs/beta-testing.md) | Eine Beta testen: TestFlight, Signierung und Testumfang |
| [docs/design.md](../docs/design.md) | Gemeinsames Erscheinungsbild der Apps und Website |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | Änderungen in jeder Version |

## Mitwirken

Beiträge sind willkommen, von Transport und Encodern bis zu Rendering und Dokumentation. Lies [CONTRIBUTING.md](../CONTRIBUTING.md), bevor du einen Pull Request öffnest. Um eine Idee zuerst zu besprechen, frage auf dem [Sideband Discord](https://discord.gg/Xc7NykBZ6z).

## Kontakt

Der [Sideband Discord](https://discord.gg/Xc7NykBZ6z) ist für Fragen, Hilfe bei der Einrichtung und Ideen da. Du kannst auch an [hello@sideband.studio](mailto:hello@sideband.studio) schreiben.

Einen Fehler gefunden? [Öffne ein Issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) mit den Versionen beider Apps und dem Host-Log. Melde Sicherheitsprobleme privat wie in [SECURITY.md](../SECURITY.md) beschrieben, nicht auf Discord oder in einem Issue.

## Lizenz

EternalMonitor wird unter der [MIT License](../LICENSE) veröffentlicht, © 2026 Sideband Studio. Das Windows-Installationsprogramm enthält auch Komponenten Dritter unter deren eigenen Lizenzen, darunter [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) und FFmpeg.

Entwickelt von [Sideband Studio](https://sideband.studio).
