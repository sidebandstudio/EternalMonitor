import SwiftUI

struct ConnectView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings
    @StateObject private var recentStore = RecentConnectionStore.shared
    @StateObject private var scanner = NetworkScanner()
    @ObservedObject private var pairings = PairingStore.shared

    @State private var hostIP: String = ""
    @State private var port: String = "9876"
    @State private var showSettings = false
    // track whether we've already pre-filled from lastHost so we
    // don't clobber user edits on every re-render.
    @State private var didPrefillFromLastHost = false
    @State private var showQRScanner = false
    @State private var showDetails = false
    @AppStorage("didSeeOnboarding") private var didSeeOnboarding = false
    @FocusState private var focusedField: Field?
    @Environment(\.horizontalSizeClass) private var sizeClass

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

                            if !didSeeOnboarding {
                                onboardingCard
                            }

                            if let error = connectionManager.connectionError {
                                errorBanner(error)
                            }

                            if isConnecting {
                                connectingCard
                            } else {
                                connectCard
                            }

                            usbCard

                            if scanner.hosts.isEmpty && !scanner.isScanning && !scanner.statusMessage.isEmpty {
                                scanEmptyState
                            }

                            if !scanner.hosts.isEmpty {
                                discoveredSection
                            }

                            if !recentStore.connections.isEmpty && scanner.hosts.isEmpty {
                                recentSection
                            }

                            if isConnecting || connectionManager.connectionError != nil || !connectionManager.diagnostics.isEmpty {
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
                    .accessibilityLabel("Settings")
                    .accessibilityIdentifier("settings.button")
                }
            }
            .sheet(item: $connectionManager.pairingPrompt, onDismiss: connectionManager.pairingSheetDismissed) { model in
                PairingSheet(model: model, submit: connectionManager.submitPairing, cancel: connectionManager.cancel)
            }
            .sheet(isPresented: $showSettings) {
                SettingsView()
            }
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
                #if DEBUG
                if UIPreview.opensSettings { showSettings = true }
                #endif
                if !didPrefillFromLastHost && hostIP.isEmpty && !settings.lastHost.isEmpty {
                    hostIP = settings.lastHost
                    if settings.lastPort != 0 {
                        port = String(settings.lastPort)
                    }
                    didPrefillFromLastHost = true
                }
            }
            .animation(.easeInOut(duration: 0.2), value: isConnecting)
            .animation(.easeInOut(duration: 0.2), value: connectionManager.connectionError)
        }
        .tint(Theme.accent)
    }

    // MARK: - Header

    private var header: some View {
        VStack(spacing: 14) {
            LogoMark(size: 68)
            VStack(spacing: 6) {
                Text("EternalMonitor")
                    .font(.app(30, .semibold, relativeTo: .largeTitle))
                    .foregroundStyle(Theme.text)
                Text("Use this iPad as a second screen for your Windows PC.")
                    .font(.app(16, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: - Onboarding

    private var onboardingCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                SectionHeader(title: "How it works")
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) { didSeeOnboarding = true }
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                        .frame(width: 32, height: 32)
                }
                .accessibilityLabel("Dismiss tips")
            }
            onboardingStep(1, "Open EternalMonitor on your Windows PC.")
            onboardingStep(2, "Choose the PC below, scan its QR code, or plug in a USB cable.")
            onboardingStep(3, "The first time on Wi-Fi, enter the six-digit code the PC shows.")
        }
        .card()
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    private func onboardingStep(_ n: Int, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Text("\(n)")
                .font(.appMono(12, medium: true))
                .foregroundStyle(Theme.onAccent)
                .frame(width: 22, height: 22)
                .background(Circle().fill(Theme.accent))
                .accessibilityHidden(true)
            Text(text)
                .font(.app(15, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
    }

    // MARK: - Error banner

    private func errorBanner(_ message: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 16))
                .foregroundStyle(Theme.warning)
                .padding(.top, 1)

            Text(message)
                .font(.app(14, relativeTo: .callout))
                .foregroundStyle(Theme.text)
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)

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

    // MARK: - Connect

    private var connectCard: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Connect to a PC")
                    .font(.app(20, .semibold, relativeTo: .title3))
                    .foregroundStyle(Theme.text)
                Text("Enter the address shown in EternalMonitor on your PC.")
                    .font(.app(14, relativeTo: .callout))
                    .foregroundStyle(Theme.textMuted)
            }

            HStack(alignment: .top, spacing: 10) {
                // .URL keyboard: hostnames and IPv6 need letters and colons, which
                // a decimal pad made impossible to type.
                field(label: "PC address", placeholder: "192.168.1.20", text: $hostIP, field: .host, keyboard: .URL)
                field(label: "Port", placeholder: "9876", text: $port, field: .port, keyboard: .numberPad)
                    .frame(width: 104)
            }

            if !settings.lastHost.isEmpty && settings.lastHost != normalizedHostIP {
                Button {
                    hostIP = settings.lastHost
                    if settings.lastPort != 0 { port = String(settings.lastPort) }
                } label: {
                    Label("Use last PC: \(settings.lastHost)", systemImage: "clock.arrow.circlepath")
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
                Text("Connect")
            }
            .buttonStyle(PrimaryButtonStyle())
            .disabled(!canConnect)
            .accessibilityLabel("Connect to PC")
            .accessibilityIdentifier("connect.button")

            let layout = sizeClass == .compact
                ? AnyLayout(VStackLayout(spacing: 10))
                : AnyLayout(HStackLayout(spacing: 10))
            layout {
                Button {
                    focusedField = nil
                    showQRScanner = true
                } label: {
                    Label("Scan QR code", systemImage: "qrcode.viewfinder")
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
                        Text(scanner.isScanning ? "Searching…" : "Find PCs")
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
                .accessibilityLabel(field == .host ? "PC address" : "Port")
                .accessibilityIdentifier(field == .host ? "connect.host" : "connect.port")
        }
    }

    // MARK: - Connecting

    private var connectingCard: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 14) {
                ProgressView()
                    .tint(Theme.accent)
                    .controlSize(.regular)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Connecting…")
                        .font(.app(18, .semibold, relativeTo: .headline))
                        .foregroundStyle(Theme.text)
                    Text(normalizedHostIP.isEmpty ? "Waiting for your PC" : "Reaching \(normalizedHostIP)")
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
                Text("Cancel")
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
                .frame(width: 40, height: 40)
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(Theme.surfaceRaised)
                )
            VStack(alignment: .leading, spacing: 2) {
                Text("USB cable")
                    .font(.app(15, .medium, relativeTo: .subheadline))
                    .foregroundStyle(Theme.text)
                Text(connectionManager.usbStatus)
                    .font(.app(13.5, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("connect.usbStatus")
            }
            Spacer(minLength: 8)
            if settings.allowUSB && connectionManager.usbStatus == "USB: disconnected" {
                Button("Connect USB") { connectionManager.resumeUSBConnections() }
                    .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                    .accessibilityIdentifier("connect.usb")
            } else if !settings.allowUSB {
                Button("Turn on") { settings.allowUSB = true }
                    .buttonStyle(PillButtonStyle())
                    .accessibilityLabel("Turn on USB connections")
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
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "wifi.exclamationmark")
                .foregroundStyle(Theme.textMuted)
                .font(.system(size: 16))
            Text("No PCs found. Check that both devices are on the same Wi-Fi (not a guest network), or type the address shown on the PC.")
                .font(.app(14, relativeTo: .callout))
                .foregroundStyle(Theme.textMuted)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .card(padding: 16)
        .transition(.opacity)
    }

    private var discoveredSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeader(title: "Found on this network", trailing: scanner.statusMessage.isEmpty ? nil : scanner.statusMessage)
                .padding(.horizontal, 4)

            VStack(spacing: 0) {
                ForEach(Array(scanner.hosts.enumerated()), id: \.element.id) { index, host in
                    if index > 0 { rowDivider }
                    Button {
                        hostIP = host.address
                        port = "\(host.port)"
                    } label: {
                        hostRow(
                            icon: "desktopcomputer",
                            title: host.name != host.address ? host.name : host.address,
                            subtitle: host.name != host.address ? "\(host.address) · port \(host.port)" : "Port \(host.port)",
                            badge: nil,
                            paired: false
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
            .card(padding: 0)
        }
        .transition(.opacity.combined(with: .move(edge: .bottom)))
    }

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeader(title: "Recent")
                .padding(.horizontal, 4)

            VStack(spacing: 0) {
                ForEach(Array(recentStore.connections.enumerated()), id: \.element.id) { index, conn in
                    if index > 0 { rowDivider }
                    Button {
                        if conn.isUSB {
                            connectionManager.resumeUSBConnections()
                        } else {
                            hostIP = conn.host
                            port = "\(conn.port)"
                        }
                    } label: {
                        hostRow(
                            icon: conn.isUSB ? "cable.connector" : "desktopcomputer",
                            title: conn.host,
                            subtitle: conn.isUSB ? "Connect with a USB cable" : "Port \(conn.port)",
                            badge: conn.isUSB ? "USB" : "Wi-Fi",
                            paired: conn.isUSB || pairings.isPaired(host: conn.host, port: conn.port)
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
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
                    .accessibilityLabel("Paired PC")
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
            Image(systemName: "chevron.right")
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
