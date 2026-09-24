import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var connectionManager: ConnectionManager
    @Environment(\.dismiss) private var dismiss

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "–"
        let build = info?["CFBundleVersion"] as? String ?? "–"
        return "\(version) (\(build))"
    }

    var body: some View {
        NavigationStack {
            Form {
                // Keep Frame Rate, Play PC audio and Allow USB within the first
                // screen of the sheet; UI tests toggle them without scrolling.
                Section {
                    Picker(selection: $settings.targetFPS) {
                        Text("30 fps").tag(30)
                        Text("60 fps").tag(60)
                        Text("120 fps").tag(120)
                    } label: {
                        rowLabel("Frame rate", "speedometer")
                    }
                    .accessibilityIdentifier("settings.targetFPS")
                    .accessibilityValue("\(settings.targetFPS) fps")

                    Toggle(isOn: $settings.playPCaudio) {
                        rowLabel("Play PC audio", "speaker.wave.2")
                    }
                    .accessibilityIdentifier("settings.playPCaudio")

                    Toggle(isOn: $settings.showHUD) {
                        rowLabel("Show stream stats", "gauge.with.dots.needle.33percent")
                    }

                    Toggle(isOn: $settings.keepScreenAwake) {
                        rowLabel("Keep screen awake", "sun.max")
                    }
                } header: {
                    sectionHeader("Streaming")
                } footer: {
                    footnote("Frame rate applies from the next connection. Tap the picture to show or hide the controls.")
                }
                .listRowBackground(Theme.surfaceRaised)

                Section {
                    Toggle(isOn: $settings.allowUSB) {
                        rowLabel("Allow USB connections", "cable.connector")
                    }
                    .accessibilityIdentifier("settings.allowUSB")

                    Toggle(isOn: $settings.autoReconnect) {
                        rowLabel("Reconnect automatically", "arrow.triangle.2.circlepath")
                    }

                    Toggle(isOn: $settings.autoResumeOnForeground) {
                        rowLabel("Resume after switching apps", "arrow.uturn.forward")
                    }
                } header: {
                    sectionHeader("Connection")
                }
                .listRowBackground(Theme.surfaceRaised)

                Section {
                    Toggle(isOn: $settings.controlPC) {
                        rowLabel("Control PC", "hand.point.up.left")
                    }
                    Picker(selection: $settings.commandAsControl) {
                        Text("Ctrl").tag(true)
                        Text("Win").tag(false)
                    } label: {
                        rowLabel("⌘ key acts as", "command")
                    }
                    .accessibilityIdentifier("settings.commandMapping")
                } header: {
                    sectionHeader("Control")
                } footer: {
                    footnote(
                        "Tap to click, drag to move, two fingers to scroll, hold to right-click. "
                            + "Keyboard, trackpad and Apple Pencil work too. Applies from the next connection. "
                            + "While Control PC is on, tap with three fingers to show the controls."
                    )
                }
                .listRowBackground(Theme.surfaceRaised)

                Section {
                    Toggle(isOn: $settings.drawingMode) {
                        rowLabel("Drawing mode", "pencil.tip")
                    }
                    .accessibilityIdentifier("settings.drawingMode")
                    .onChange(of: settings.drawingMode) { _, enabled in
                        if enabled { settings.allowUSB = true }
                    }
                    if connectionManager.state == .connected {
                        infoRow("Pencil", connectionManager.sessionHasPen ? "Windows Ink ready" : "Unavailable on this host", "pencil")
                        infoRow("Drawing mode", connectionManager.sessionDrawingMode ? "Active" : "Off", "hand.draw")
                    }
                } header: {
                    sectionHeader("Apple Pencil")
                } footer: {
                    footnote("Drawing mode ignores fingers on the canvas and mutes PC audio. Over USB it requests the iPad's highest frame rate, up to the PC's limit. Applies from the next connection. "
                        + "In Clip Studio Paint, choose Preferences > Tablet > Tablet PC. Pressure requires a pressure-sensitive Pencil; Apple Pencil USB-C does not support pressure.")
                }
                .listRowBackground(Theme.surfaceRaised)

                // Live values from HELLO_ACK, refreshed by heartbeats.
                Section {
                    if let host = connectionManager.hostInfo,
                       let config = connectionManager.hostStreamConfig {
                        infoRow("PC", host.hostName, "desktopcomputer")
                        infoRow(
                            "Resolution",
                            "\(config.width) × \(config.height) at \(config.fps) fps",
                            "rectangle.on.rectangle"
                        )
                        infoRow(
                            "Codec",
                            config.codec == StreamConfig.codecHEVC ? "HEVC (H.265)" : "H.264",
                            "cpu"
                        )
                        infoRow(
                            "Bitrate",
                            String(format: "%.1f Mbps", Double(config.bitrateBps) / 1_000_000.0),
                            "chart.bar"
                        )
                        infoRow("Audio", audioDescription, "speaker.wave.2")
                            .accessibilityElement(children: .ignore)
                            .accessibilityLabel("Audio")
                            .accessibilityValue(audioDescription)
                            .accessibilityIdentifier("settings.hostAudio")
                    } else {
                        infoRow("PC", "Not connected", "desktopcomputer")
                    }
                } header: {
                    sectionHeader("Connected PC")
                } footer: {
                    footnote("Resolution, codec and bitrate are set in EternalMonitor on your PC.")
                }
                .listRowBackground(Theme.surfaceRaised)

                Section {
                    Button(role: .destructive) {
                        connectionManager.forgetPairedHosts()
                    } label: {
                        Text("Forget paired PCs")
                            .font(.app(16, .medium, relativeTo: .body))
                            .foregroundStyle(Theme.danger)
                    }
                    .accessibilityIdentifier("settings.forgetPairings")
                } header: {
                    sectionHeader("Pairing")
                } footer: {
                    footnote("Each PC asks for its six-digit code again on the next Wi-Fi connection.")
                }
                .listRowBackground(Theme.surfaceRaised)

                Section {
                    HStack(spacing: 14) {
                        LogoMark(size: 40)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("EternalMonitor")
                                .font(.app(16, .semibold, relativeTo: .headline))
                                .foregroundStyle(Theme.text)
                            Text("Version \(appVersion)")
                                .font(.app(13, relativeTo: .footnote))
                                .foregroundStyle(Theme.textMuted)
                        }
                    }
                    .padding(.vertical, 4)
                    creditLink("Developer", "github.com/whoisaldo", symbol: "person", url: "https://github.com/whoisaldo")
                    creditLink("Source code", "EternalMonitor", symbol: "chevron.left.forwardslash.chevron.right", url: "https://github.com/whoisaldo/EternalMonitor")
                    creditLink("Contact", "aliyounes@eternalreverse.com", symbol: "envelope", url: "mailto:aliyounes@eternalreverse.com")
                } header: {
                    sectionHeader("About")
                } footer: {
                    footnote("Built by Ali Younes (@whoisaldo). Set in Geist.")
                }
                .listRowBackground(Theme.surfaceRaised)
            }
            .scrollContentBackground(.hidden)
            .background(Theme.canvas.ignoresSafeArea())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .font(.app(16, .semibold, relativeTo: .body))
                        .foregroundStyle(Theme.accent)
                        .accessibilityIdentifier("settings.done")
                }
            }
            .toolbarBackground(Theme.canvas, for: .navigationBar)
            .tint(Theme.accent)
        }
        .presentationBackground(Theme.canvas)
    }

    private func rowLabel(_ title: String, _ symbol: String) -> some View {
        Label {
            Text(title)
                .font(.app(16, relativeTo: .body))
                .foregroundStyle(Theme.text)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(Theme.textMuted)
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(title.uppercased())
            .font(.app(12, .medium, relativeTo: .caption))
            .tracking(1.1)
            .foregroundStyle(Theme.textFaint)
    }

    private var audioDescription: String {
        if connectionManager.sessionDrawingMode { return "Muted for drawing" }
        if !settings.playPCaudio { return "Muted" }
        if let error = connectionManager.audioStats.error { return error }
        guard connectionManager.audioStats.playing else { return "Unavailable" }
        return "Opus 128 kbps, buffer \(connectionManager.audioStats.targetMs) ms"
    }

    private func footnote(_ text: String) -> some View {
        Text(text)
            .font(.app(13, relativeTo: .footnote))
            .foregroundStyle(Theme.textFaint)
    }

    private func infoRow(_ title: String, _ value: String, _ symbol: String) -> some View {
        HStack {
            rowLabel(title, symbol)
            Spacer()
            Text(value)
                .font(.app(15, relativeTo: .callout))
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.trailing)
        }
    }

    @ViewBuilder
    private func creditLink(_ title: String, _ value: String, symbol: String, url: String) -> some View {
        if let destination = URL(string: url) {
            Link(destination: destination) {
                HStack {
                    rowLabel(title, symbol)
                    Spacer()
                    Text(value)
                        .font(.app(15, relativeTo: .callout))
                        .foregroundStyle(Theme.accent)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Image(systemName: "arrow.up.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                }
            }
        }
    }
}
