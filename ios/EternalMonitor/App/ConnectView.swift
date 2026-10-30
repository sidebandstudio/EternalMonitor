import SwiftUI

struct ConnectView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var recentStore = RecentConnectionStore.shared
    @StateObject private var scanner = NetworkScanner()
    @ObservedObject private var pairings = PairingStore.shared
    @ObservedObject private var language = LanguageStore.shared

    @State private var hostIP: String = ""
    @State private var port: String = "9876"
    @State private var showSettings = false
    @State private var showSetup = false
    /// Run setup again was tapped in Settings; setup opens once it closes.
    @State private var setupAfterSettings = false
    @State private var setupOutcome: SetupOutcome = .finished
    // track whether we've already pre-filled from lastHost so we
    // don't clobber user edits on every re-render.
    @State private var didPrefillFromLastHost = false
    @State private var showQRScanner = false
    @State private var showDetails = false
    @State private var feedbackRequest: FeedbackRequest?
    /// Send feedback was tapped and its sheet is on the way (Mail first
    /// collects diagnostics). Nothing else may open meanwhile.
    @State private var preparingFeedback = false
    /// Setup was finished or skipped, or this is an upgrade (the name is
    /// from the tips card setup replaced).
    @AppStorage("didSeeOnboarding") private var didSeeOnboarding = false
    @FocusState private var focusedField: Field?
    @Environment(\.horizontalSizeClass) private var sizeClass
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    // Flipped on appear; the header and cards rise in one after another.
    @State private var appeared = false

    private enum Field: Hashable {
        case host, port
    }

    private var isConnecting: Bool {
        connectionManager.state == .connecting
    }

    private var normalizedHostIP: String {
        hostIP.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var parsedPort: UInt16? {
        guard let value = UInt16(port.trimmingCharacters(in: .whitespacesAndNewlines)),
              value > 0
        else { return nil }
        return value
    }

    /// The one-time feedback ask is on screen (see `body`).
    private var showsFeedbackAsk: Bool {
        connectionManager.problem == nil && connectionManager.connectionError == nil && !isConnecting
            && connectionManager.feedbackAsk.due(streaming: connectionManager.state == .connected)
    }

    private var canConnect: Bool {
        !normalizedHostIP.isEmpty && parsedPort != nil
    }

    var body: some View {
        NavigationStack {
            ZStack {
                AppBackground()

                GeometryReader { geometry in
                    ScrollView {
                        VStack(spacing: 20) {
                            header
                                .padding(.top, 24)
                                .padding(.bottom, 8)
                                .entrance(appeared)

                            UpdateBanner()

                            if let problem = connectionManager.problem {
                                problemBanner(problem)
                            } else if let error = connectionManager.connectionError {
                                errorBanner(error)
                            } else if !isConnecting
                                        && connectionManager.feedbackAsk.due(streaming: connectionManager.state == .connected) {
                                FeedbackAskCard(
                                    send: {
                                        withAnimation(Motion.fade) { connectionManager.finishFeedbackAsk() }
                                        startFeedback(.general)
                                    },
                                    notNow: { withAnimation(Motion.fade) { connectionManager.finishFeedbackAsk() } }
                                )
                            }

                            if isConnecting {
                                connectingCard
                                    .transition(.opacity.combined(with: .scale(scale: 0.97)))
                            } else {
                                connectCard
                                    .entrance(appeared, delay: 0.14)
                                    .transition(.opacity.combined(with: .scale(scale: 0.97)))
                            }

                            if !scanner.hosts.isEmpty {
                                discoveredSection
                            } else if !scanner.isScanning && !scanner.statusMessage.isEmpty {
                                scanEmptyState
                            }

                            usbCard
                                .entrance(appeared, delay: 0.2)

                            // Saved PCs stay visible while discovery runs: Bonjour can
                            // miss them (a VPN address, another subnet) and can find
                            // someone else's PC on a shared network.
                            if !unlistedRecents.isEmpty {
                                recentSection
                                    .entrance(appeared, delay: 0.26)
                            }

                            if isConnecting || connectionManager.connectionError != nil || connectionManager.problem != nil
                                || !connectionManager.diagnostics.isEmpty {
                                diagnosticsSection
                            }
                        }
                        .frame(maxWidth: 560)
                        .padding(.horizontal, 24)
                        .padding(.bottom, 40)
                        // Centre the column on tall screens; scroll when it grows.
                        .frame(maxWidth: .infinity, minHeight: geometry.size.height * 0.92, alignment: .center)
                    }
                    .scrollDismissesKeyboard(.interactively)
                }

                // One thing at a time: not while connecting or reconnecting,
                // over a problem card or an error such as a version mismatch,
                // while typing, over a sheet (Settings, QR scanner, pairing,
                // Mail or the feedback copy sheet) or next to the feedback ask.
                UpdatePopup(allowed: connectionManager.isIdleOnConnect && focusedField == nil
                    && !showSettings && !showSetup && !showQRScanner && connectionManager.pairingPrompt == nil
                    && feedbackRequest == nil && !preparingFeedback && !showsFeedbackAsk)
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showSettings = true
                    } label: {
                        Image(systemName: "gearshape")
                            .font(.system(size: 17, weight: .medium))
                            .foregroundStyle(Theme.text)
                            .frame(width: 44, height: 44)
                    }
                    .accessibilityLabel(L("Settings"))
                    .accessibilityIdentifier("settings.button")
                }
            }
            .sheet(item: $connectionManager.pairingPrompt, onDismiss: connectionManager.pairingSheetDismissed) { model in
                PairingSheet(model: model, submit: connectionManager.submitPairing, cancel: connectionManager.cancel)
                    .appLanguage()
            }
            .sheet(isPresented: $showSettings, onDismiss: {
                if setupAfterSettings {
                    setupAfterSettings = false
                    showSetup = true
                }
            }) {
                SettingsView(runSetup: {
                    setupAfterSettings = true
                    showSettings = false
                })
                .appLanguage()
            }
            .fullScreenCover(isPresented: $showSetup, onDismiss: finishSetup) {
                SetupView(scanner: scanner, outcome: $setupOutcome)
                    .appLanguage()
            }
            .feedbackSheet($feedbackRequest)
            .sheet(isPresented: $showQRScanner) {
                QRScannerView(
                    onScan: { value in
                        showQRScanner = false
                        handleScannedQR(value)
                    },
                    onCancel: { showQRScanner = false }
                )
            }
            .onAppear {
                appeared = true
                #if DEBUG
                if UIPreview.opensSettings { showSettings = true }
                if UIPreview.opensFeedback { startFeedback(.general) }
                if UIPreview.opensSetup && !didSeeOnboarding { showSetup = true; return }
                #endif
                if !didPrefillFromLastHost && hostIP.isEmpty && !settings.lastHost.isEmpty {
                    hostIP = settings.lastHost
                    if settings.lastPort != 0 {
                        port = String(settings.lastPort)
                    }
                    didPrefillFromLastHost = true
                }
                if !didSeeOnboarding {
                    if FirstRun.isFreshInstall(lastHost: settings.lastHost, hasPairings: pairings.hasPairings,
                                               hasRecents: !recentStore.connections.isEmpty) {
                        // Setup explains Local Network before the search
                        // makes iPadOS ask for it.
                        showSetup = true
                        return
                    }
                    didSeeOnboarding = true
                }
                startScanIfIdle()
            }
            // Back from Settings, where Local Network may now be on: search
            // again rather than keep showing the old denial.
            .onChange(of: scenePhase) { _, phase in
                if phase == .active && scanner.searchProblem == .localNetworkOff { scanner.startScan() }
            }
            .animation(.easeInOut(duration: 0.2), value: isConnecting)
            .animation(.easeInOut(duration: 0.2), value: connectionManager.connectionError)
            .animation(.easeInOut(duration: 0.2), value: connectionManager.problem)
        }
        .tint(Theme.accent)
    }

    // MARK: - Header

    private var header: some View {
        VStack(spacing: 14) {
            LogoMark(size: 68)
                .scaleEffect(appeared || reduceMotion ? 1 : 0.9)
                .animation(reduceMotion ? nil : Motion.gentle, value: appeared)
            VStack(spacing: 6) {
                Text("EternalMonitor")
                    .font(.app(30, .semibold, relativeTo: .largeTitle))
                    .foregroundStyle(Theme.text)
                Text(L("Use this iPad as a second screen for your Windows PC."))
                    .font(.app(16, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: - Setup

    /// Look for PCs straight away, so the usual first step is tapping your
    /// PC rather than typing an address.
    private func startScanIfIdle() {
        if connectionManager.state == .disconnected && !scanner.isScanning && scanner.hosts.isEmpty {
            scanner.startScan()
        }
    }

    /// Setup closed: carry on with the way to connect it chose.
    private func finishSetup() {
        didSeeOnboarding = true
        let outcome = setupOutcome
        setupOutcome = .finished
        switch outcome {
        case .connect(let host, let port):
            hostIP = host
            self.port = String(port)
            connectionManager.connect(host: host, port: port)
            return
        case .scanQR:
            showQRScanner = true
        case .usb:
            settings.allowUSB = true
            if connectionManager.usbStatus == "USB: disconnected" { connectionManager.resumeUSBConnections() }
        case .finished:
            break
        }
        startScanIfIdle()
    }

    // MARK: - Error banner

    private func errorBanner(_ message: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 16))
                .foregroundStyle(Theme.warning)
                .padding(.top, 1)

            VStack(alignment: .leading, spacing: 10) {
                Text(message)
                    .font(.app(14, relativeTo: .callout))
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                VersionMismatchAction(mismatch: connectionManager.versionMismatch)
            }

            Spacer(minLength: 0)

            Button {
                withAnimation { connectionManager.connectionError = nil }
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .frame(width: 28, height: 28)
            }
            .accessibilityLabel("Dismiss error")
        }
        .card(padding: 16, tint: Theme.warning)
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    private func problemBanner(_ problem: ConnectionProblem) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 16))
                .foregroundStyle(Theme.warning)
                .padding(.top, 2)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 6) {
                Text(problem.title)
                    .font(.app(16, .semibold, relativeTo: .headline))
                    .foregroundStyle(Theme.text)
                    .accessibilityIdentifier("connect.problemTitle")
                Text(problem.detail)
                    .font(.app(14, relativeTo: .callout))
                    .foregroundStyle(Theme.textMuted)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("connect.problemDetail")
                if !problem.actions.isEmpty {
                    HStack(spacing: 10) {
                        ForEach(problem.actions, id: \.self) { action in
                            switch action {
                            case .retry:
                                // Only one filled button a card: Open Settings
                                // comes first where the card offers it.
                                // Problem cards stay English, buttons included,
                                // so they match the steps the card names.
                                Button { connectionManager.retry() } label: { Text(verbatim: "Try again") }
                                    .buttonStyle(problem.actions.contains(.openSettings)
                                        ? PillButtonStyle()
                                        : PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                                    .accessibilityIdentifier("connect.retry")
                            case .openSettings:
                                Button { openAppSettings() } label: { Text(verbatim: "Open Settings") }
                                    .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                                    .accessibilityIdentifier("connect.openSettings")
                            }
                        }
                    }
                    .padding(.top, 4)
                }
                if problem.canReport {
                    Button {
                        startFeedback(.problem(problem.title))
                    } label: {
                        Label("Report this problem", systemImage: "envelope")
                            .font(.app(13.5, .medium, relativeTo: .footnote))
                            .foregroundStyle(Theme.accent)
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 4)
                    .accessibilityIdentifier("connect.reportProblem")
                }
            }

            Spacer(minLength: 0)

            Button {
                withAnimation { connectionManager.problem = nil }
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel("Dismiss")
        }
        .accessibilityElement(children: .contain)
        .card(padding: 16, tint: Theme.warning)
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    /// Mail with the details and diagnostics, or the copy sheet.
    private func startFeedback(_ kind: Feedback.Kind) {
        let context = connectionManager.feedbackContext
        preparingFeedback = true
        Task {
            // Cleared in the same turn the sheet is set, or if this ends early.
            defer { preparingFeedback = false }
            #if DEBUG
            await UIPreview.feedbackPreparation()
            #endif
            feedbackRequest = await FeedbackRequest.make(kind, context: context)
        }
    }

    private func openAppSettings() {
        if let url = URL(string: UIApplication.openSettingsURLString) {
            UIApplication.shared.open(url)
        }
    }

    // MARK: - Connect

    private var connectCard: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(L("Connect to your PC"))
                    .font(.app(20, .semibold, relativeTo: .title3))
                    .foregroundStyle(Theme.text)
                Text("Your PC appears below when EternalMonitor is open on it. You can also scan its QR code, or type the Address it shows.")
                    .font(.app(14, relativeTo: .callout))
                    .foregroundStyle(Theme.textMuted)
            }

            HStack(alignment: .top, spacing: 10) {
                // .URL keyboard: hostnames and IPv6 need letters and colons, which
                // a decimal pad made impossible to type.
                field(label: L("PC address"), placeholder: "192.168.1.20", text: $hostIP, field: .host, keyboard: .URL)
                field(label: L("Port"), placeholder: "9876", text: $port, field: .port, keyboard: .numberPad)
                    .frame(width: 104)
            }

            if !settings.lastHost.isEmpty && settings.lastHost != normalizedHostIP {
                Button {
                    hostIP = settings.lastHost
                    if settings.lastPort != 0 { port = String(settings.lastPort) }
                } label: {
                    Label(L("Use last PC: \(settings.lastHost)"), systemImage: "clock.arrow.circlepath")
                        .font(.app(13, .medium, relativeTo: .footnote))
                        .foregroundStyle(Theme.accent)
                }
                .buttonStyle(.plain)
            }

            if !port.isEmpty && parsedPort == nil {
                Text("Enter a port between 1 and 65535.")
                    .font(.app(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.warning)
            }

            Button {
                focusedField = nil
                guard let p = parsedPort else { return }
                withAnimation(.easeInOut(duration: 0.2)) {
                    connectionManager.connect(host: normalizedHostIP, port: p)
                }
            } label: {
                Text(L("Connect"))
            }
            .buttonStyle(PrimaryButtonStyle())
            .disabled(!canConnect)
            .accessibilityLabel(L("Connect to PC"))
            .accessibilityIdentifier("connect.button")

            let layout = sizeClass == .compact
                ? AnyLayout(VStackLayout(spacing: 10))
                : AnyLayout(HStackLayout(spacing: 10))
            layout {
                Button {
                    focusedField = nil
                    showQRScanner = true
                } label: {
                    Label(L("Scan QR code"), systemImage: "qrcode.viewfinder")
                }
                .buttonStyle(SecondaryButtonStyle())
                .accessibilityIdentifier("connect.qr")

                Button {
                    focusedField = nil
                    if scanner.isScanning { scanner.stopScan() } else { scanner.startScan() }
                } label: {
                    HStack(spacing: 8) {
                        if scanner.isScanning {
                            ProgressView().tint(Theme.accent).controlSize(.small)
                        } else {
                            Image(systemName: "dot.radiowaves.left.and.right")
                        }
                        Text(scanner.isScanning ? L("Searching…") : L("Find PCs"))
                    }
                }
                .buttonStyle(SecondaryButtonStyle())
                .accessibilityIdentifier("connect.scan")
            }
        }
        .card()
    }

    private func field(label: String, placeholder: String, text: Binding<String>, field: Field, keyboard: UIKeyboardType) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label)
                .font(.app(12.5, .medium, relativeTo: .caption))
                .foregroundStyle(Theme.textMuted)

            TextField(placeholder, text: text, prompt: Text(placeholder).foregroundStyle(Theme.textFaint))
                .font(.appMono(17, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .padding(.horizontal, 14)
                .frame(minHeight: 50)
                .background(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .fill(Theme.canvas)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(focusedField == field ? Theme.accent : Theme.borderStrong, lineWidth: focusedField == field ? 1.5 : 1)
                )
                .animation(Motion.fade, value: focusedField == field)
                .keyboardType(keyboard)
                .textContentType(.none)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(field == .host ? .next : .go)
                .onSubmit {
                    if field == .host {
                        focusedField = .port
                    } else if canConnect, let p = parsedPort {
                        focusedField = nil
                        connectionManager.connect(host: normalizedHostIP, port: p)
                    }
                }
                .focused($focusedField, equals: field)
                .accessibilityLabel(label)
                .accessibilityIdentifier(field == .host ? "connect.host" : "connect.port")
        }
    }

    // MARK: - Connecting

    private var connectingCard: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 14) {
                PulseRings(size: 44)
                VStack(alignment: .leading, spacing: 3) {
                    Text(L("Connecting…"))
                        .font(.app(18, .semibold, relativeTo: .headline))
                        .foregroundStyle(Theme.text)
                    Text(normalizedHostIP.isEmpty ? L("Waiting for your PC") : L("Reaching \(normalizedHostIP)"))
                        .font(.app(14, relativeTo: .callout))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }

            Button {
                withAnimation(.easeInOut(duration: 0.2)) {
                    connectionManager.cancel()
                }
            } label: {
                Text(L("Cancel"))
            }
            .buttonStyle(SecondaryButtonStyle())
        }
        .card()
    }

    // MARK: - USB

    private var usbCard: some View {
        HStack(spacing: 14) {
            Image(systemName: "cable.connector")
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(settings.allowUSB ? Theme.accent : Theme.textFaint)
                .animation(Motion.fade, value: settings.allowUSB)
                .frame(width: 40, height: 40)
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(Theme.surfaceRaised)
                )
            VStack(alignment: .leading, spacing: 2) {
                Text(L("USB cable"))
                    .font(.app(15, .medium, relativeTo: .subheadline))
                    .foregroundStyle(Theme.text)
                Text(USBStatusText.describe(connectionManager.usbStatus))
                    .font(.app(13.5, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("connect.usbStatus")
            }
            Spacer(minLength: 8)
            if settings.allowUSB && connectionManager.usbStatus == "USB: disconnected" {
                Button(L("Connect USB")) { connectionManager.resumeUSBConnections() }
                    .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                    .accessibilityIdentifier("connect.usb")
            } else if !settings.allowUSB {
                Button(L("Turn on")) { settings.allowUSB = true }
                    .buttonStyle(PillButtonStyle())
                    .accessibilityLabel(L("Turn on USB connections"))
            }
        }
        .card(padding: 14)
    }

    // parse "eternaldisplay://host:port" and immediately connect.
    private func handleScannedQR(_ value: String) {
        switch PairingCode.parse(value) {
        case .success(let target):
            hostIP = target.host
            port = String(target.port)
            connectionManager.connect(host: target.host, port: target.port, token: target.token)
        case .failure(.wrongScheme):
            connectionManager.connectionError = "That QR code isn't from EternalMonitor. Scan the code shown on the PC’s Stream page."
        case .failure:
            connectionManager.connectionError = "That QR code couldn't be read. Try again, or type the address instead."
        }
    }

    // MARK: - Scan results

    private var scanEmptyState: some View {
        let advice = Self.scanEmptyAdvice(scanner.searchProblem)
        return HStack(alignment: .top, spacing: 12) {
            Image(systemName: "wifi.exclamationmark")
                .foregroundStyle(Theme.textMuted)
                .font(.system(size: 16))
            VStack(alignment: .leading, spacing: 10) {
                VStack(alignment: .leading, spacing: 6) {
                    if let title = advice.title {
                        Text(title)
                            .font(.app(16, .semibold, relativeTo: .headline))
                            .foregroundStyle(Theme.text)
                            .accessibilityIdentifier("connect.scanProblemTitle")
                    }
                    Text(advice.detail)
                        .font(.app(14, relativeTo: .callout))
                        .foregroundStyle(Theme.textMuted)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("connect.scanEmpty")
                }
                HStack(spacing: 10) {
                    if scanner.searchProblem == .localNetworkOff {
                        Button { openAppSettings() } label: { Text(verbatim: "Open Settings") }
                            .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                    }
                    Button { scanner.startScan() } label: { Text(verbatim: "Search again") }
                        .buttonStyle(PillButtonStyle())
                }
            }
            Spacer(minLength: 0)
        }
        .card(padding: 16)
        .transition(.opacity)
    }

    /// What an empty search says: what went wrong if the search itself
    /// failed, and what to do.
    static func scanEmptyAdvice(_ problem: NetworkScanner.SearchProblem?) -> (title: String?, detail: String) {
        switch problem {
        case .localNetworkOff:
            return ("EternalMonitor can't look for PCs",
                "Local Network is turned off for EternalMonitor. " + ConnectionProblem.localNetworkSteps(then: "Search again")
                    + " A USB cable works without this setting.")
        case .couldNotSearch:
            // NetServiceBrowser can report Local Network being off this way
            // too, so the Settings advice stays as the last resort.
            return ("Couldn't search for PCs",
                "Search again, or scan the QR code or type the Address shown on the PC. "
                    + "If it keeps failing, turn on Local Network for EternalMonitor in Settings.")
        case nil:
            return (nil, "No PCs found. Check that EternalMonitor is open on the PC and both are on the same Wi-Fi, not a guest network. Or type the Address shown on the PC.")
        }
    }

    private var discoveredSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeader(title: L("Found on this network"), trailing: scanner.statusMessage.isEmpty ? nil : scanner.statusMessage)
                .padding(.horizontal, 4)

            VStack(spacing: 0) {
                ForEach(Array(scanner.hosts.enumerated()), id: \.element.id) { index, host in
                    if index > 0 { rowDivider }
                    Button {
                        // Only an IPv4 address reaches the PC; see DiscoveredHost.
                        guard let address = host.address else { return }
                        hostIP = address
                        port = "\(host.port)"
                        connectionManager.connect(host: address, port: host.port)
                    } label: {
                        hostRow(
                            icon: "desktopcomputer",
                            title: host.name,
                            subtitle: Self.discoveredSubtitle(host),
                            badge: nil,
                            paired: false
                        )
                    }
                    .buttonStyle(RowButtonStyle())
                    .disabled(isConnecting || host.address == nil)
                    .accessibilityHint(host.address == nil ? "" : L("Connects to this PC"))
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            .card(padding: 0)
        }
        .transition(.opacity.combined(with: .move(edge: .bottom)))
    }

    static func discoveredSubtitle(_ host: DiscoveredHost) -> String {
        if let address = host.address { return L("Tap to connect · \(address)") }
        return host.finding
            ? L("Finding…")
            : "Couldn't get its address. Type the Address shown on the PC."
    }

    private var unlistedRecents: [RecentConnection] {
        RecentConnection.unlisted(recentStore.connections, discovered: scanner.hosts)
    }

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeader(title: L("Recent"))
                .padding(.horizontal, 4)

            VStack(spacing: 0) {
                ForEach(Array(unlistedRecents.enumerated()), id: \.element.id) { index, conn in
                    if index > 0 { rowDivider }
                    Button {
                        if conn.isUSB {
                            connectionManager.resumeUSBConnections()
                        } else {
                            hostIP = conn.host
                            port = "\(conn.port)"
                            connectionManager.connect(host: conn.host, port: conn.port)
                        }
                    } label: {
                        hostRow(
                            icon: conn.isUSB ? "cable.connector" : "desktopcomputer",
                            title: conn.host,
                            subtitle: conn.isUSB ? L("Plug in the cable to connect") : L("Tap to connect"),
                            badge: conn.isUSB ? "USB" : "Wi-Fi",
                            paired: conn.isUSB || pairings.isPaired(host: conn.host, port: conn.port)
                        )
                    }
                    .buttonStyle(RowButtonStyle())
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            .card(padding: 0)
        }
        .opacity(isConnecting ? 0.5 : 1)
    }

    private var rowDivider: some View {
        Rectangle()
            .fill(Theme.border)
            .frame(height: 1)
            .padding(.leading, 70)
    }

    private func hostRow(icon: String, title: String, subtitle: String, badge: String?, paired: Bool) -> some View {
        HStack(spacing: 14) {
            Image(systemName: icon)
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 40, height: 40)
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(Theme.surfaceRaised)
                )
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.app(15.5, .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text(subtitle)
                    .font(.app(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if paired {
                Image(systemName: "lock.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityLabel(L("Paired PC"))
                    .accessibilityIdentifier("recent.paired")
            }
            if let badge {
                Text(badge)
                    .font(.app(11.5, .medium, relativeTo: .caption))
                    .foregroundStyle(badge == "USB" ? Theme.accent : Theme.textMuted)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 4)
                    .background(Capsule().fill(Theme.surfaceRaised))
            }
            Image(systemName: "chevron.forward")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.textFaint)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .contentShape(Rectangle())
    }

    // MARK: - Diagnostics

    private var diagnosticsSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { showDetails.toggle() }
            } label: {
                HStack {
                    Text("Connection details")
                        .font(.app(14, .medium, relativeTo: .subheadline))
                        .foregroundStyle(Theme.textMuted)
                    Spacer()
                    Image(systemName: "chevron.down")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                        .rotationEffect(.degrees(showDetails ? 180 : 0))
                }
                .padding(16)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if showDetails {
                let entries = Array(connectionManager.diagnostics.suffix(8).reversed())
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(entries) { entry in
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Circle()
                                .fill(color(for: entry.level))
                                .frame(width: 7, height: 7)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(entry.category.capitalized)
                                    .font(.app(12, .medium, relativeTo: .caption))
                                    .foregroundStyle(Theme.textMuted)
                                Text(entry.message)
                                    .font(.appMono(12, relativeTo: .caption))
                                    .foregroundStyle(Theme.text)
                                    .textSelection(.enabled)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            Spacer(minLength: 0)
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 16)
                NavigationLink("Export diagnostics") { DiagnosticsView() }
                    .font(.app(14, .medium, relativeTo: .body))
                    .padding(.horizontal, 16)
                    .padding(.bottom, 16)
            }
        }
        .card(padding: 0)
    }

    private func color(for level: DiagnosticLevel) -> Color {
        switch level {
        case .info: return Theme.accent
        case .warning: return Theme.warning
        case .error: return Theme.danger
        }
    }
}
