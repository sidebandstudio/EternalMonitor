import SwiftUI

struct DisplayView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings
    @Environment(\.horizontalSizeClass) private var sizeClass

    @State private var showHUD = true
    @State private var hudDismissTask: Task<Void, Never>?
    @State private var showQualityPopover = false
    @State private var showSettings = false
    @State private var showKeyboard = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            // Full-bleed video
            if connectionManager.sessionWantsInput {
                // Input relay active: every 1-2 finger gesture belongs to the
                // PC; a three-finger tap (via the relay layer) drives the HUD.
                MetalView()
                    .ignoresSafeArea()
                TouchRelayView(keyboardVisible: $showKeyboard, active: !showSettings && !connectionManager.signalLost, onToggleHUD: { toggleHUD() })
                    .ignoresSafeArea()
            } else {
                // One gesture, one meaning: tap toggles the HUD.
                MetalView()
                    .ignoresSafeArea()
                    .onTapGesture { toggleHUD() }
            }

            #if DEBUG
            if UIPreview.showsBackdrop {
                UIPreview.Backdrop()
                    .onTapGesture { toggleHUD() }
            }
            #endif

            if connectionManager.signalLost {
                reconnectingCard
                    .transition(.opacity.combined(with: .scale(scale: 0.96)))
                    .allowsHitTesting(false)
            }

            VStack {
                HStack {
                    Spacer()
                    if connectionManager.sessionDrawingMode {
                        Button { toggleHUD() } label: {
                            Image(systemName: "slider.horizontal.3")
                                .frame(width: 44, height: 44)
                                .foregroundStyle(Theme.text)
                                .background(glass(Circle()))
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Show or hide drawing controls")
                        .accessibilityIdentifier("display.drawingControls")
                    }
                    if showHUD && settings.showHUD {
                        hudOverlay
                            .transition(.opacity.combined(with: .move(edge: .top)))
                    }
                }
                .padding(.top, 12)
                .padding(.trailing, 16)

                if connectionManager.sessionDrawingMode && !connectionManager.sessionHasPen {
                    Text("Native Pencil input is unavailable. Check that your Windows host supports Windows Ink.")
                        .font(.app(14, relativeTo: .footnote))
                        .foregroundStyle(Theme.text)
                        .padding(12)
                        .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: 12))
                        .padding(.horizontal, 16)
                        .accessibilityIdentifier("display.penUnavailable")
                }

                Spacer()

                if showHUD {
                    bottomBar
                        .padding(.horizontal, 16)
                        .padding(.bottom, 12)
                        .transition(.opacity.combined(with: .move(edge: .bottom)))
                }
            }
        }
        .statusBarHidden(true)
        .persistentSystemOverlays(.hidden)
        .animation(.easeInOut(duration: 0.25), value: connectionManager.signalLost)
        .onAppear {
            scheduleHUDDismiss()
            #if DEBUG
            if UIPreview.opensQuality { showQualityPopover = true }
            #endif
        }
        .onDisappear { hudDismissTask?.cancel() }
        .onChange(of: showQualityPopover) { _, presented in
            if presented { hudDismissTask?.cancel() }
            else { scheduleHUDDismiss() }
        }
        .onChange(of: settings.keepScreenAwake) { _, keepAwake in
            // Apply mid-session — the whole point of flipping it is the
            // screen dimming (or not) right now.
            UIApplication.shared.isIdleTimerDisabled = keepAwake
        }
        .sheet(isPresented: $showSettings) { SettingsView() }
        .onChange(of: showKeyboard) { _, presented in
            if presented { hudDismissTask?.cancel(); showHUD = true }
            else { scheduleHUDDismiss() }
        }
        .onChange(of: showSettings) { _, presented in
            if presented { showKeyboard = false; hudDismissTask?.cancel() }
            else { scheduleHUDDismiss() }
        }
        .onChange(of: connectionManager.signalLost) { _, lost in
            if lost { hudDismissTask?.cancel(); showHUD = true }
            else { scheduleHUDDismiss() }
        }
    }

    private var transportName: String {
        connectionManager.transportMode == "WiFi" ? "Wi-Fi" : connectionManager.transportMode
    }

    private var audioPlaying: Bool {
        connectionManager.audioStats.playing && settings.playPCaudio
    }

    // MARK: - Stats pill

    private var hudOverlay: some View {
        Button { showQualityPopover.toggle() } label: {
            HStack(spacing: 12) {
                stat("\(Int(connectionManager.fps))", unit: "fps")
                separator
                stat(connectionManager.stats.e2eMs.map { String(format: "%.0f", $0) } ?? "–", unit: "ms")
                separator
                Text(transportName)
                    .font(.app(13, .medium, relativeTo: .footnote))
                    .foregroundStyle(Theme.text)
                Image(systemName: audioPlaying ? "speaker.wave.2.fill" : "speaker.slash.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(audioPlaying ? Theme.text : Theme.textFaint)
                qualityBars
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 38)
            .background(glass(Capsule()))
        }
        .buttonStyle(.plain)
        .popover(isPresented: $showQualityPopover) {
            qualityPopover
                .padding(18)
                .frame(minWidth: 300)
                .background(Theme.surface)
                .presentationCompactAdaptation(.popover)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(hudAccessibilitySummary)
        .accessibilityAddTraits(.isButton)
        .accessibilityHint("Show connection quality details")
        .accessibilityIdentifier("display.hud")
    }

    private var hudAccessibilitySummary: String {
        let latency = connectionManager.stats.e2eMs.map { String(format: "%.0f milliseconds", $0) }
            ?? "unknown latency"
        return "Stream statistics: \(Int(connectionManager.fps)) frames per second, "
            + "\(latency), \(connectionManager.transportMode), \(connectionManager.stats.bars) of 4 signal bars, "
            + (audioPlaying ? "PC audio playing" : "PC audio muted or unavailable")
    }

    private var qualityBars: some View {
        let bars = connectionManager.stats.bars
        let color = Theme.quality(bars: bars)
        return HStack(alignment: .bottom, spacing: 2) {
            ForEach(0..<4, id: \.self) { idx in
                RoundedRectangle(cornerRadius: 1)
                    .fill(idx < bars ? color : Color.white.opacity(0.18))
                    .frame(width: 3, height: CGFloat(5 + idx * 3))
            }
        }
        .frame(height: 14)
        .animation(Motion.fade, value: bars)
    }

    private var qualityPopover: some View {
        let q = connectionManager.stats
        return VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Connection quality")
                    .font(.app(17, .semibold, relativeTo: .headline))
                    .foregroundStyle(Theme.text)
                Text("\(transportName) · measured on this iPad")
                    .font(.app(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
            }
            Grid(alignment: .leading, horizontalSpacing: 28, verticalSpacing: 14) {
                GridRow {
                    metric("Loss", String(format: "%.1f", q.lossPercent), "%", color: q.lossPercent < 3 ? Theme.text : Theme.warning)
                    metric("Round trip", q.rttMs.map { String(format: "%.0f", $0) } ?? "–", "ms")
                    metric("Dropped", "\(q.framesDropped)", "")
                }
                GridRow {
                    metric("Repaired", "\(q.fragsRepaired)", "")
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel("Repaired fragments")
                        .accessibilityValue("\(q.fragsRepaired)")
                        .accessibilityIdentifier("quality.repaired")
                    metric("Jitter", String(format: "%.1f", q.jitterMs), "ms")
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel("Jitter")
                        .accessibilityValue(String(format: "%.1f milliseconds", q.jitterMs))
                        .accessibilityIdentifier("quality.jitter")
                    metric("Latency", q.e2eMs.map { String(format: "%.0f", $0) } ?? "–", "ms")
                }
            }
        }
    }

    private func metric(_ label: String, _ value: String, _ unit: String, color: Color = Theme.text) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label)
                .font(.app(12, relativeTo: .caption))
                .foregroundStyle(Theme.textMuted)
            HStack(alignment: .firstTextBaseline, spacing: 3) {
                Text(value)
                    .font(.appMono(20, medium: true, relativeTo: .title3))
                    .foregroundStyle(color)
                if !unit.isEmpty {
                    Text(unit)
                        .font(.app(12, relativeTo: .caption))
                        .foregroundStyle(Theme.textMuted)
                }
            }
        }
    }

    private func stat(_ value: String, unit: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 3) {
            Text(value)
                .font(.appMono(14, medium: true, relativeTo: .footnote))
                .foregroundStyle(Theme.text)
                .monospacedDigit()
                // Digits roll to the new value instead of snapping.
                .contentTransition(.numericText())
                .animation(Motion.fade, value: value)
            Text(unit)
                .font(.app(11, relativeTo: .caption2))
                .foregroundStyle(Theme.textMuted)
        }
        // A one-digit value leaves the pill too narrow for "fps" otherwise.
        .fixedSize()
    }

    private var separator: some View {
        Rectangle()
            .fill(Color.white.opacity(0.16))
            .frame(width: 1, height: 14)
    }

    // MARK: - Controls

    private var bottomBar: some View {
        HStack(spacing: 8) {
            HStack(spacing: 10) {
                StatusDot(color: connectionManager.signalLost ? Theme.warning : Theme.accent, size: 8, pulses: !connectionManager.signalLost)
                Text(connectionManager.signalLost ? "Reconnecting" : "Live")
                    .font(.app(14, .semibold, relativeTo: .subheadline))
                    .foregroundStyle(Theme.text)
                if sizeClass != .compact {
                    Text(transportName)
                        .font(.app(14, relativeTo: .subheadline))
                        .foregroundStyle(Theme.textMuted)
                }
            }
            .padding(.leading, 14)
            .padding(.trailing, 6)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(
                connectionManager.signalLost ? "Signal lost, reconnecting" : "On air"
            )
            .accessibilityIdentifier("display.signal")

            Spacer(minLength: 12)

            if connectionManager.sessionHasKeyboard {
                Button { showKeyboard.toggle() } label: {
                    toolbarLabel(showKeyboard ? "Hide keyboard" : "Keyboard", "keyboard")
                }
                .buttonStyle(PillButtonStyle(
                    foreground: showKeyboard ? Theme.onAccent : Theme.text,
                    fill: showKeyboard ? Theme.accent : Color.white.opacity(0.08)
                ))
                .animation(Motion.fade, value: showKeyboard)
                .accessibilityIdentifier("display.keyboard")
            }

            Button { showSettings = true } label: {
                Image(systemName: "gearshape")
                    .font(.system(size: 15, weight: .medium))
                    .frame(width: 40, height: 40)
            }
            .buttonStyle(PillButtonStyle())
            .accessibilityLabel("Stream settings")
            .accessibilityIdentifier("display.settings")

            Button {
                connectionManager.cancel()
            } label: {
                toolbarLabel("Disconnect", "xmark")
            }
            .buttonStyle(PillButtonStyle(foreground: Theme.danger, fill: Theme.danger.opacity(0.14)))
            .accessibilityLabel("Disconnect")
            .accessibilityIdentifier("display.disconnect")
        }
        .padding(6)
        .frame(maxWidth: 620)
        .background(glass(RoundedRectangle(cornerRadius: 26, style: .continuous)))
    }

    /// Icon and title, or just the icon when the app is narrow (Split View,
    /// Stage Manager). The accessibility label is the title either way.
    @ViewBuilder
    private func toolbarLabel(_ title: String, _ symbol: String) -> some View {
        if sizeClass == .compact {
            Label(title, systemImage: symbol).labelStyle(.iconOnly)
        } else {
            Label(title, systemImage: symbol)
        }
    }

    private var reconnectingCard: some View {
        VStack(spacing: 12) {
            PulseRings(color: Theme.warning, size: 56)
            Text("Reconnecting to your PC")
                .font(.app(18, .semibold, relativeTo: .headline))
                .foregroundStyle(Theme.text)
            Text("The picture comes back as soon as the PC answers.")
                .font(.app(14, relativeTo: .callout))
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 24)
        .frame(maxWidth: 360)
        .background(glass(RoundedRectangle(cornerRadius: 22, style: .continuous)))
    }

    private func glass<S: InsettableShape>(_ shape: S) -> some View {
        shape
            .fill(.ultraThinMaterial)
            .environment(\.colorScheme, .dark)
            .overlay(shape.strokeBorder(Color.white.opacity(0.1), lineWidth: 1))
            .shadow(color: .black.opacity(0.35), radius: 18, y: 8)
    }

    // MARK: - Auto-hide HUD

    /// Show the HUD (auto-hiding after a few seconds) or hide it now. Both the
    /// plain tap and the three-finger relay tap route here: calling
    /// `scheduleHUDDismiss()` to "toggle" could only ever show it, since that
    /// function unconditionally sets `showHUD = true`.
    private func toggleHUD() {
        if showHUD {
            hudDismissTask?.cancel()
            withAnimation(.easeOut(duration: 0.25)) { showHUD = false }
        } else {
            scheduleHUDDismiss()
        }
    }

    private func scheduleHUDDismiss() {
        hudDismissTask?.cancel()
        withAnimation(Motion.gentle) { showHUD = true }
        guard !showKeyboard && !showSettings && !connectionManager.signalLost else { return }
        hudDismissTask = Task {
            try? await Task.sleep(for: .seconds(5))
            if !Task.isCancelled {
                withAnimation(.easeOut(duration: 0.3)) {
                    showHUD = false
                }
            }
        }
    }
}
