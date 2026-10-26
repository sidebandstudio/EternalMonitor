import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var connectionManager: ConnectionManager
    @Environment(\.dismiss) private var dismiss
    @State private var feedbackRequest: FeedbackRequest?

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "–"
        let build = info?["CFBundleVersion"] as? String ?? "–"
        return "\(version) (\(build))"
    }

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
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

                        // A menu, like Frame rate: a segmented control in a
                        // scrolling list takes a flick that starts on it as a tap.
                        Picker(selection: $settings.pictureFit) {
                            Text("Fit").tag(PictureFit.fit)
                            Text("Fill").tag(PictureFit.fill)
                        } label: {
                            rowLabel("Picture", "aspectratio")
                        }
                        .accessibilityIdentifier("settings.pictureFit")
                        .accessibilityValue(settings.pictureFit == .fit ? "Fit" : "Fill")
                    } header: {
                        sectionHeader("Streaming")
                    } footer: {
                        footnote("Frame rate applies from the next connection. Tap the picture to show or hide the controls. "
                            + "Fill uses the whole iPad screen and trims the edges of the PC picture that don't fit.")
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
                            infoRow("Pencil", connectionManager.sessionHasPen ? "Ready to draw" : "Not available from this PC", "pencil")
                            infoRow("Drawing mode", connectionManager.sessionDrawingMode ? "On" : "Off", "hand.draw")
                            if settings.drawingMode != connectionManager.sessionDrawingMode {
                                Button {
                                    connectionManager.reconnectToApply()
                                } label: {
                                    rowLabel("Reconnect now to apply", "arrow.triangle.2.circlepath")
                                }
                                .accessibilityIdentifier("settings.reconnectToApply")
                            }
                        }
                    } header: {
                        sectionHeader("Apple Pencil")
                    } footer: {
                        footnote("Draw with Apple Pencil in Windows apps, with pressure and tilt; fingers on the picture are ignored and PC sound is muted while it's on. "
                            + "In Clip Studio Paint, choose Preferences > Tablet > Tablet PC.")
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
                                "Video format",
                                config.codec == StreamConfig.codecHEVC ? "HEVC (H.265)" : "H.264",
                                "cpu"
                            )
                            infoRow(
                                "Data rate",
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
                        footnote("These are set in EternalMonitor on your PC.")
                    }
                    .listRowBackground(Theme.surfaceRaised)

                    // After Connected PC: the audio status there stays close to the
                    // Play PC audio switch, and this explains what the PC decides.
                    Section {
                        Label {
                            Text("Your PC chooses whether this iPad mirrors its screen or works as a second screen. "
                                + "In EternalMonitor on the PC, pick Mirror my screen or Second screen at the top of the Stream page. "
                                + "This iPad reconnects by itself.")
                                .font(.app(15, relativeTo: .callout))
                                .foregroundStyle(Theme.text)
                                .fixedSize(horizontal: false, vertical: true)
                        } icon: {
                            Image(systemName: "rectangle.on.rectangle")
                                .foregroundStyle(Theme.textMuted)
                        }
                        .accessibilityIdentifier("settings.screenMode")
                    } header: {
                        sectionHeader("Second screen or mirror")
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
                        NavigationLink {
                            DiagnosticsView()
                        } label: {
                            rowLabel("Diagnostics", "doc.text.magnifyingglass")
                        }
                        .accessibilityIdentifier("settings.diagnostics")
                        Button {
                            let context = connectionManager.feedbackContext
                            Task { feedbackRequest = await FeedbackRequest.make(.general, context: context) }
                        } label: {
                            rowLabel("Send feedback", "envelope")
                        }
                        .accessibilityIdentifier("settings.sendFeedback")
                    } header: {
                        sectionHeader("Support")
                    } footer: {
                        footnote("We're a small studio and we read every message. Mail opens with your iPad's details and a diagnostics report attached, which you can remove.")
                    }
                    .listRowBackground(Theme.surfaceRaised)

                    UpdateSettingsSection()

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
                        creditLink("Developer", "Sideband Studio", symbol: "building.2", url: "https://sideband.studio")
                        creditLink("Source code", "EternalMonitor", symbol: "chevron.left.forwardslash.chevron.right", url: "https://github.com/sidebandstudio/EternalMonitor")
                        creditLink("Star EternalMonitor on GitHub", "GitHub", symbol: "star", url: Feedback.repoURL)
                            .accessibilityIdentifier("settings.star")
                        creditLink("Contact", "hello@sideband.studio", symbol: "envelope", url: "mailto:hello@sideband.studio")
                    } header: {
                        sectionHeader("About")
                    } footer: {
                        footnote("Built by Sideband Studio.")
                    }
                    .listRowBackground(Theme.surfaceRaised)
                    .id("about")
                }
                #if DEBUG
                .onAppear {
                    if UIPreview.scrollsToAbout {
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { proxy.scrollTo("about", anchor: .center) }
                    }
                }
                #endif
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
            .feedbackSheet($feedbackRequest)
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
