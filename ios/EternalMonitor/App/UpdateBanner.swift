import SwiftUI
import UIKit

/// Opens the EternalMonitor page in TestFlight, or the web invite when the
/// TestFlight app is missing (it then offers to install TestFlight).
@MainActor
enum TestFlightLink {
    static func open() {
        let join = UpdateChecker.shared.joinURL
        var components = URLComponents(url: join, resolvingAgainstBaseURL: false)
        components?.scheme = "itms-beta"
        guard let app = components?.url else {
            UIApplication.shared.open(join)
            return
        }
        UIApplication.shared.open(app) { opened in
            if !opened { UIApplication.shared.open(join) }
        }
    }
}

/// Connect screen: a newer build is ready in TestFlight.
struct UpdateBanner: View {
    @ObservedObject private var updates = UpdateChecker.shared

    var body: some View {
        if let banner = updates.banner {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "arrow.down.circle.fill")
                    .font(.system(size: 18))
                    .foregroundStyle(Theme.accent)
                    .padding(.top, 1)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 10) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(banner.title)
                            .font(.app(15, .semibold, relativeTo: .headline))
                            .foregroundStyle(Theme.text)
                        Text(banner.detail)
                            .font(.app(13.5, relativeTo: .footnote))
                            .foregroundStyle(Theme.textMuted)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Button("Open TestFlight") { TestFlightLink.open() }
                        .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                        .accessibilityIdentifier("update.openTestFlight")
                }
                Spacer(minLength: 0)
                Button {
                    withAnimation { updates.dismissBanner() }
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.textMuted)
                        .frame(width: 28, height: 28)
                }
                .accessibilityLabel("Hide update notice")
                .accessibilityIdentifier("update.dismiss")
            }
            .card(padding: 16, tint: Theme.accent)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("update.banner")
            .transition(.opacity.combined(with: .move(edge: .top)))
        }
    }
}

/// Connect screen: a small card over the screen when a newer build is ready.
/// `allowed` is false while the user is busy (connecting, typing, or in a
/// sheet). The Connect screen is never on screen during a stream.
struct UpdatePopup: View {
    @ObservedObject private var updates = UpdateChecker.shared
    let allowed: Bool

    var body: some View {
        ZStack {
            if allowed, let popup = updates.popup {
                Color.black.opacity(0.55)
                    .ignoresSafeArea()
                    .onTapGesture { close() }
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 18) {
                    Image(systemName: "arrow.down.circle.fill")
                        .font(.system(size: 30))
                        .foregroundStyle(Theme.accent)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 6) {
                        Text(popup.title)
                            .font(.app(20, .semibold, relativeTo: .title3))
                            .foregroundStyle(Theme.text)
                        Text(popup.message)
                            .font(.app(15, .medium, relativeTo: .body))
                            .foregroundStyle(Theme.text)
                        if let note = popup.note {
                            Text(note)
                                .font(.app(14, relativeTo: .callout))
                                .foregroundStyle(Theme.textMuted)
                        }
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 10) {
                        Spacer(minLength: 0)
                        Button("Later") { close() }
                            .buttonStyle(PillButtonStyle())
                            .accessibilityIdentifier("updatePopup.later")
                        Button("Open TestFlight") {
                            close()
                            TestFlightLink.open()
                        }
                        .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                        .accessibilityIdentifier("updatePopup.openTestFlight")
                    }
                }
                .card(padding: 24)
                .frame(maxWidth: 420)
                .padding(24)
                .accessibilityElement(children: .contain)
                .accessibilityAddTraits(.isModal)
                .accessibilityIdentifier("update.popup")
                .transition(.opacity.combined(with: .scale(scale: 0.96)))
            }
        }
        .animation(.easeInOut(duration: 0.2), value: allowed && updates.popup != nil)
    }

    private func close() {
        updates.closePopup()
    }
}

/// Settings: automatic checks and, when there is one, the waiting update.
struct UpdateSettingsSection: View {
    @ObservedObject private var updates = UpdateChecker.shared

    var body: some View {
        Section {
            Toggle(isOn: $updates.automatic) {
                label("Check for updates automatically", "arrow.triangle.2.circlepath")
            }
            .accessibilityIdentifier("settings.checkForUpdates")
            if updates.automatic, let available = updates.available {
                Button { TestFlightLink.open() } label: {
                    HStack {
                        label("A new version is ready in TestFlight", "arrow.down.circle")
                        Spacer()
                        Text("Open TestFlight")
                            .font(.app(15, relativeTo: .callout))
                            .foregroundStyle(Theme.accent)
                    }
                }
                .accessibilityLabel("A new version is ready in TestFlight. Version \(available.version), build \(available.build).")
                .accessibilityHint("Opens TestFlight")
                .accessibilityIdentifier("settings.openTestFlight")
            }
        } header: {
            Text("UPDATES")
                .font(.app(12, .medium, relativeTo: .caption))
                .tracking(1.1)
                .foregroundStyle(Theme.textFaint)
        } footer: {
            Text("Updates install through TestFlight. In TestFlight, open EternalMonitor and turn on Automatic Updates to get new versions without asking. Update EternalMonitor on your PC at the same time.")
                .font(.app(13, relativeTo: .footnote))
                .foregroundStyle(Theme.textFaint)
        }
        .listRowBackground(Theme.surfaceRaised)
    }

    private func label(_ title: String, _ symbol: String) -> some View {
        Label {
            Text(title)
                .font(.app(16, relativeTo: .body))
                .foregroundStyle(Theme.text)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(Theme.textMuted)
        }
    }
}

/// The button under a version-mismatch error, on the side that can act.
struct VersionMismatchAction: View {
    let mismatch: VersionMismatch?

    var body: some View {
        if mismatch?.opensTestFlight == true {
            Button("Open TestFlight") { TestFlightLink.open() }
                .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                .accessibilityIdentifier("mismatch.openTestFlight")
        }
    }
}
