import SwiftUI

/// Who sees first-run setup.
enum FirstRun {
    /// Saved values that only a person's use of the app writes, in this
    /// version or one from before setup: a setting changed, the tips card
    /// closed, a PC connected to or tried, an update or feedback prompt
    /// answered. The app writes others on its own at launch (diagnostics
    /// installation ID, crash marker, update checks, remote config), so they
    /// say nothing about an earlier install. The language is new with setup.
    /// `preferUSB` and `promotionEnabled` are settings older versions had.
    static let upgradeKeys: Set<String> = [
        "didSeeOnboarding", "lastHost", "lastPort", "recentConnections", "deviceId",
        "playPCaudio", "allowUSB", "showHUD", "keepScreenAwake", "autoResumeOnForeground",
        "controlPC", "drawingMode", "commandAsControl", "autoReconnect", "targetFPS", "pictureFit",
        "preferUSB", "promotionEnabled",
        "checkForUpdates", "updateDismissedBuild", "updatePopupClosedBuild",
        DiagnosticsUpload.enabledKey, FeedbackAsk.sessionsKey, FeedbackAsk.doneKey,
    ]

    /// A fresh install: no PC paired and none of `upgradeKeys` saved.
    /// Anyone updating from before setup has one of them and goes straight
    /// to the connect screen.
    static func isFreshInstall(saved: [String: Any], hasPairings: Bool) -> Bool {
        !hasPairings && upgradeKeys.isDisjoint(with: saved.keys)
    }

    /// What this install has saved. Launch arguments are not in it.
    static var saved: [String: Any] {
        Bundle.main.bundleIdentifier.flatMap(UserDefaults.standard.persistentDomain(forName:)) ?? [:]
    }
}

/// How setup ended: the way to connect it chose, if any. The connect
/// screen carries it on once setup has closed.
enum SetupOutcome: Equatable {
    case finished
    case connect(host: String, port: UInt16)
    case scanQR
    case usb
}

/// First-run setup: language, Local Network (explained before iPadOS asks),
/// then a way to connect. Every step can be skipped, and Skip setup leaves
/// everything as it is.
struct SetupView: View {
    /// The connect screen's search, so PCs found here are listed there too.
    @ObservedObject var scanner: NetworkScanner
    @Binding var outcome: SetupOutcome
    @ObservedObject private var language = LanguageStore.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var step: Step = .language
    /// The Local Network step started a search, so iPadOS has asked.
    @State private var askedForNetwork = false

    enum Step: Int, CaseIterable {
        case language, network, connect
    }

    var body: some View {
        ZStack {
            AppBackground()
            VStack(spacing: 0) {
                topBar
                ScrollView {
                    VStack(alignment: .leading, spacing: 22) {
                        progress
                        Group {
                            switch step {
                            case .language: languageStep
                            case .network: networkStep
                            case .connect: connectStep
                            }
                        }
                        .id(step)
                        .transition(.opacity)
                    }
                    .frame(maxWidth: 560)
                    .padding(.horizontal, 24)
                    .padding(.vertical, 20)
                    .frame(maxWidth: .infinity)
                }
                bottomBar
            }
        }
        .tint(Theme.accent)
        // Back from Settings, where Local Network may now be on.
        .onChange(of: scenePhase) { _, phase in
            if phase == .active && scanner.searchProblem == .localNetworkOff { scanner.startScan() }
        }
    }

    // MARK: - Frame

    private var topBar: some View {
        HStack {
            if step != .language {
                Button {
                    go(to: Step(rawValue: step.rawValue - 1) ?? .language)
                } label: {
                    Label(L("Back"), systemImage: "chevron.backward")
                        .font(.app(16, .medium, relativeTo: .body))
                        .foregroundStyle(Theme.text)
                        .frame(minHeight: 44)
                }
                .accessibilityIdentifier("setup.back")
            }
            Spacer()
            Button {
                finish(.finished)
            } label: {
                Text(L("Skip setup"))
                    .font(.app(16, .medium, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                    .frame(minHeight: 44)
            }
            .accessibilityIdentifier("setup.skip")
        }
        .padding(.horizontal, 24)
        .padding(.top, 8)
    }

    private var progress: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                ForEach(Step.allCases, id: \.self) { item in
                    Capsule()
                        .fill(item.rawValue <= step.rawValue ? Theme.accent : Theme.borderStrong)
                        .frame(width: item == step ? 28 : 14, height: 5)
                }
            }
            .animation(Motion.gentle, value: step)
            .accessibilityHidden(true)
            Text(L("Step \(step.rawValue + 1) of \(Step.allCases.count)"))
                .font(.app(12.5, .medium, relativeTo: .caption))
                .foregroundStyle(Theme.textFaint)
        }
    }

    private var bottomBar: some View {
        VStack(spacing: 10) {
            switch step {
            case .language:
                Button(L("Next")) { go(to: .network) }
                    .buttonStyle(PrimaryButtonStyle())
                    .accessibilityIdentifier("setup.next")
            case .network:
                if askedForNetwork {
                    Button(L("Next")) { go(to: .connect) }
                        .buttonStyle(PrimaryButtonStyle())
                        .accessibilityIdentifier("setup.next")
                } else {
                    Button(L("Continue")) {
                        askedForNetwork = true
                        scanner.startScan()
                    }
                    .buttonStyle(PrimaryButtonStyle())
                    .accessibilityIdentifier("setup.network.allow")
                    Button(L("Skip")) { go(to: .connect) }
                        .buttonStyle(SecondaryButtonStyle())
                        .accessibilityIdentifier("setup.network.skip")
                }
            case .connect:
                Button(L("Done")) { finish(.finished) }
                    .buttonStyle(PrimaryButtonStyle())
                    .accessibilityIdentifier("setup.done")
            }
        }
        .frame(maxWidth: 560)
        .padding(.horizontal, 24)
        .padding(.top, 12)
        .padding(.bottom, 20)
        .frame(maxWidth: .infinity)
    }

    private func heading(_ symbol: String, _ title: String, _ detail: String, id: String) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Image(systemName: symbol)
                .font(.system(size: 24, weight: .medium))
                .foregroundStyle(Theme.accent)
                .frame(width: 56, height: 56)
                .background(
                    RoundedRectangle(cornerRadius: 16, style: .continuous)
                        .fill(Theme.accent.opacity(0.1))
                )
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 6) {
                Text(title)
                    .font(.app(28, .semibold, relativeTo: .title))
                    .foregroundStyle(Theme.text)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
                    .accessibilityIdentifier(id)
                Text(detail)
                    .font(.app(16, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    // MARK: - Language

    private var languageStep: some View {
        VStack(alignment: .leading, spacing: 18) {
            heading("globe", L("Language"),
                    L("Choose the language for EternalMonitor. You can change it later in Settings."),
                    id: "setup.step.language")
            VStack(spacing: 10) {
                languageOption(nil, title: L("System language"),
                               detail: AppLanguage.matching(Locale.preferredLanguages).nativeName,
                               id: "setup.language.system")
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), spacing: 10)], spacing: 10) {
                    ForEach(AppLanguage.allCases) { item in
                        languageOption(item, title: item.nativeName, detail: nil, id: "setup.language.\(item.rawValue)")
                    }
                }
            }
        }
    }

    private func languageOption(_ item: AppLanguage?, title: String, detail: String?, id: String) -> some View {
        let selected = language.choice == item
        return Button {
            language.choice = item
        } label: {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: title)
                        .font(.app(16, .medium, relativeTo: .body))
                        .foregroundStyle(Theme.text)
                    if let detail {
                        Text(verbatim: detail)
                            .font(.app(13, relativeTo: .footnote))
                            .foregroundStyle(Theme.textMuted)
                    }
                }
                Spacer(minLength: 8)
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 18))
                    .foregroundStyle(selected ? Theme.accent : Theme.textFaint)
            }
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity, minHeight: 56, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(selected ? Theme.accent.opacity(0.08) : Theme.surface)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(selected ? Theme.accent.opacity(0.5) : Theme.border, lineWidth: 1)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(RowButtonStyle())
        .animation(Motion.fade, value: selected)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier(id)
    }

    // MARK: - Local Network

    private var networkStep: some View {
        VStack(alignment: .leading, spacing: 18) {
            heading("wifi", L("Local network"),
                    L("EternalMonitor finds your PC and receives its screen over your local network. iPadOS asks for permission next: tap Allow."),
                    id: "setup.step.network")
            if askedForNetwork {
                if scanner.searchProblem == .localNetworkOff {
                    networkBlocked
                } else {
                    searchStatus
                }
            }
        }
    }

    /// iPadOS said no: where to turn it on.
    private var networkBlocked: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 16))
                    .foregroundStyle(Theme.warning)
                    .padding(.top, 2)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 6) {
                    Text(L("Local network access is off"))
                        .font(.app(16, .semibold, relativeTo: .headline))
                        .foregroundStyle(Theme.text)
                        .accessibilityIdentifier("setup.network.blocked")
                    Text(L("Tap Open Settings, turn on Local Network, then come back here. A USB cable works without it."))
                        .font(.app(14, relativeTo: .callout))
                        .foregroundStyle(Theme.textMuted)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            HStack(spacing: 10) {
                Button(L("Open Settings")) {
                    if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                }
                .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                .accessibilityIdentifier("setup.network.openSettings")
                Button(L("Search again")) { scanner.startScan() }
                    .buttonStyle(PillButtonStyle())
            }
            .padding(.leading, 28)
        }
        .card(padding: 16, tint: Theme.warning)
        .transition(.opacity)
    }

    private var searchStatus: some View {
        HStack(spacing: 12) {
            if scanner.isScanning {
                ProgressView().tint(Theme.accent).controlSize(.small)
            } else {
                Image(systemName: scanner.hosts.isEmpty ? "dot.radiowaves.left.and.right" : "checkmark.circle.fill")
                    .foregroundStyle(scanner.hosts.isEmpty ? Theme.textMuted : Theme.accent)
            }
            Text(scanner.statusMessage)
                .font(.app(15, relativeTo: .callout))
                .foregroundStyle(Theme.text)
                .accessibilityIdentifier("setup.network.status")
            Spacer(minLength: 0)
        }
        .card(padding: 16)
        .transition(.opacity)
    }

    // MARK: - Connect

    private var connectStep: some View {
        VStack(alignment: .leading, spacing: 18) {
            heading("desktopcomputer", L("Connect to your PC"),
                    L("Open EternalMonitor on your PC, then choose how to connect."),
                    id: "setup.step.connect")

            VStack(alignment: .leading, spacing: 10) {
                SectionHeader(title: L("Found on this network"),
                              trailing: scanner.statusMessage.isEmpty ? nil : scanner.statusMessage)
                    .padding(.horizontal, 4)
                VStack(spacing: 0) {
                    let reachable = scanner.hosts.filter { $0.address != nil }
                    if reachable.isEmpty {
                        Button {
                            scanner.startScan()
                        } label: {
                            optionRow(scanner.isScanning ? nil : "dot.radiowaves.left.and.right",
                                      scanner.isScanning ? L("Searching…") : L("Find PCs"),
                                      L("EternalMonitor must be open on your PC."))
                        }
                        .buttonStyle(RowButtonStyle())
                        .disabled(scanner.isScanning)
                        .accessibilityIdentifier("setup.connect.find")
                    }
                    ForEach(Array(reachable.enumerated()), id: \.element.id) { index, host in
                        if index > 0 { divider }
                        Button {
                            if let address = host.address { finish(.connect(host: address, port: host.port)) }
                        } label: {
                            optionRow("desktopcomputer", host.name, ConnectView.discoveredSubtitle(host))
                        }
                        .buttonStyle(RowButtonStyle())
                        .accessibilityIdentifier("setup.connect.host")
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                .card(padding: 0)
            }

            VStack(spacing: 0) {
                Button { finish(.scanQR) } label: {
                    optionRow("qrcode.viewfinder", L("Scan QR code"), L("Scan the code shown in EternalMonitor on your PC."))
                }
                .buttonStyle(RowButtonStyle())
                .accessibilityIdentifier("setup.connect.qr")
                divider
                Button { finish(.usb) } label: {
                    optionRow("cable.connector", L("USB cable"), L("Plug this iPad into your PC. Keep Apple Devices open on the PC."))
                }
                .buttonStyle(RowButtonStyle())
                .accessibilityIdentifier("setup.connect.usb")
            }
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            .card(padding: 0)

            Text(L("You can also type your PC's address on the connect screen."))
                .font(.app(13, relativeTo: .footnote))
                .foregroundStyle(Theme.textFaint)
                .padding(.horizontal, 4)
        }
    }

    /// A row in the connect step; no icon shows a spinner.
    private func optionRow(_ symbol: String?, _ title: String, _ detail: String) -> some View {
        HStack(spacing: 14) {
            Group {
                if let symbol {
                    Image(systemName: symbol)
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(Theme.accent)
                } else {
                    ProgressView().tint(Theme.accent).controlSize(.small)
                }
            }
            .frame(width: 40, height: 40)
            .background(
                RoundedRectangle(cornerRadius: 11, style: .continuous)
                    .fill(Theme.surfaceRaised)
            )
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: title)
                    .font(.app(15.5, .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                Text(verbatim: detail)
                    .font(.app(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            Image(systemName: "chevron.forward")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.textFaint)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .contentShape(Rectangle())
    }

    private var divider: some View {
        Rectangle()
            .fill(Theme.border)
            .frame(height: 1)
            .padding(.leading, 70)
    }

    // MARK: - Moving on

    private func go(to next: Step) {
        withAnimation(reduceMotion ? nil : Motion.fade) { step = next }
    }

    private func finish(_ result: SetupOutcome) {
        outcome = result
        dismiss()
    }
}
