import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var connectionManager: ConnectionManager
    @Environment(\.dismiss) private var dismiss

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "—"
        let build = info?["CFBundleVersion"] as? String ?? "—"
        return "\(version) (\(build))"
    }

    var body: some View {
        NavigationStack {
            Form {
                // Playback (local device preferences)
                Section {
                    Picker(selection: $settings.targetFPS) {
                        Text("30 fps").tag(30)
                        Text("60 fps").tag(60)
                        Text("120 fps").tag(120)
                    } label: {
                        Label("Frame Rate", systemImage: "speedometer")
                    }

                    .accessibilityIdentifier("settings.targetFPS")
                    .accessibilityValue("\(settings.targetFPS) fps")

                    Toggle(isOn: $settings.showHUD) {
                        Label("Show stats HUD", systemImage: "gauge.with.dots.needle.33percent")
                    }

                    Toggle(isOn: $settings.playPCaudio) {
                        Label("Play PC audio", systemImage: "speaker.wave.2")
                    }
                    .accessibilityIdentifier("settings.playPCaudio")

                    Toggle(isOn: $settings.autoReconnect) {
                        Label("Auto-reconnect", systemImage: "arrow.triangle.2.circlepath")
                    }

                    Toggle(isOn: $settings.allowUSB) {
                        Label("Allow USB connections", systemImage: "cable.connector")
                    }
                    .accessibilityIdentifier("settings.allowUSB")

                    Toggle(isOn: $settings.keepScreenAwake) {
                        Label("Keep screen awake", systemImage: "sun.max")
                    }

                    Toggle(isOn: $settings.autoResumeOnForeground) {
                        Label("Resume after switching apps", systemImage: "arrow.uturn.forward")
                    }
                } header: {
                    sectionHeader("Playback")
                } footer: {
                    footnote(
                        "Frame Rate sets the host limit on the next connection. "
                            + "Tap the video to toggle the HUD while streaming — "
                            + "three fingers while Control PC is on."
                    )
                }

                // Control (input relay)
                Section {
                    Toggle(isOn: $settings.controlPC) {
                        Label("Control PC", systemImage: "hand.point.up.left")
                    }
                    Picker("⌘ key acts as", selection: $settings.commandAsControl) {
                        Text("Ctrl").tag(true)
                        Text("Win").tag(false)
                    }
                    .accessibilityIdentifier("settings.commandMapping")
                } header: {
                    sectionHeader("Control")
                } footer: {
                    footnote(
                        "Tap to click, drag to move the mouse, two fingers to scroll, "
                            + "hold for a right-click. Keyboard, trackpad and pencil hover also control the PC. "
                            + "Control PC takes effect on the next connect. iPadOS keeps Globe, ⌘H, ⌘Tab and ⌘Space. "
                            + "While control is on, tap with three fingers to toggle the HUD."
                    )
                }

                // Host — live values from HELLO_ACK, refreshed by heartbeats.
                Section {
                    if let host = connectionManager.hostInfo,
                       let config = connectionManager.hostStreamConfig {
                        infoRow("Host", host.hostName, "desktopcomputer")
                        infoRow(
                            "Resolution",
                            "\(config.width)×\(config.height) @ \(config.fps) fps",
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
                            "slider.horizontal.3"
                        )
                        infoRow("Audio", audioDescription, "speaker.wave.2")
                            .accessibilityElement(children: .ignore)
                            .accessibilityLabel("Audio")
                            .accessibilityValue(audioDescription)
                            .accessibilityIdentifier("settings.hostAudio")
                    } else {
                        infoRow("Host", "Not connected", "desktopcomputer")
                    }
                } header: {
                    sectionHeader("Host stream")
                } footer: {
                    footnote(
                        "Live values from the connected host. Resolution, codec and bitrate "
                            + "are configured in the EternalMonitor app on your Windows PC."
                    )
                }

                Section {
                    Button("Forget paired hosts", role: .destructive) {
                        connectionManager.forgetPairedHosts()
                    }
                    .accessibilityIdentifier("settings.forgetPairings")
                } header: {
                    sectionHeader("Pairing")
                } footer: {
                    footnote("Saved hosts will ask for a code on your next connection.")
                }

                // About
                Section {
                    HStack {
                        Text("Version")
                            .foregroundColor(Theme.text)
                        Spacer()
                        Text(appVersion)
                            .font(.appMonoRegular(size: 13))
                            .foregroundColor(Theme.text2)
                    }
                    HStack {
                        Text("Build")
                            .foregroundColor(Theme.text)
                        Spacer()
                        Text("SIGNAL")
                            .font(.appMonoMedium(size: 12))
                            .tracking(2)
                            .foregroundColor(Theme.amber)
                    }
                } header: {
                    sectionHeader("About")
                }

                // Credits
                Section {
                    creditLink(
                        "Developer",
                        "github.com/whoisaldo",
                        symbol: "chevron.left.forwardslash.chevron.right",
                        url: "https://github.com/whoisaldo"
                    )
                    creditLink(
                        "Repository",
                        "EternalMonitor",
                        symbol: "shippingbox",
                        url: "https://github.com/whoisaldo/EternalMonitor"
                    )
                    creditLink(
                        "Questions & concerns",
                        "aliyounes@eternalreverse.com",
                        symbol: "envelope",
                        url: "mailto:aliyounes@eternalreverse.com"
                    )
                } header: {
                    sectionHeader("Credits")
                } footer: {
                    footnote("Built by Ali Younes (@whoisaldo).")
                }
            }
            .scrollContentBackground(.hidden)
            .background(Theme.void.ignoresSafeArea())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .accessibilityIdentifier("settings.done")
                        .font(.appMonoMedium(size: 15))
                        .foregroundColor(Theme.amber)
                }
            }
            .tint(Theme.amber)
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(title.uppercased())
            .font(.appMonoMedium(size: 11))
            .tracking(1.5)
            .foregroundColor(Theme.text3)
    }

    private var audioDescription: String {
        if !settings.playPCaudio { return "Muted" }
        if let error = connectionManager.audioStats.error { return error }
        guard connectionManager.audioStats.playing else { return "Unavailable" }
        return "Opus 128 kbps, buffer \(connectionManager.audioStats.targetMs) ms"
    }

    private func footnote(_ text: String) -> some View {
        Text(text)
            .font(.appMonoRegular(size: 11))
            .foregroundColor(Theme.text3)
    }

    private func infoRow(_ title: String, _ value: String, _ symbol: String) -> some View {
        HStack {
            Label(title, systemImage: symbol)
                .foregroundColor(Theme.text)
            Spacer()
            Text(value)
                .font(.appMonoRegular(size: 13))
                .foregroundColor(Theme.text2)
        }
    }

    @ViewBuilder
    private func creditLink(_ title: String, _ value: String, symbol: String, url: String) -> some View {
        if let destination = URL(string: url) {
            Link(destination: destination) {
                HStack {
                    Label(title, systemImage: symbol)
                        .foregroundColor(Theme.text)
                    Spacer()
                    Text(value)
                        .font(.appMonoRegular(size: 13))
                        .foregroundColor(Theme.amber)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Image(systemName: "arrow.up.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundColor(Theme.text3)
                }
            }
        }
    }
}
