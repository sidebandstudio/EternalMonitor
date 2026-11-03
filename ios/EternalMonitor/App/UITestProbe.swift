#if DEBUG
import SwiftUI

/// For UI tests launched with `-uiTestProbe YES`: what a starved test
/// runner can miss while it lasts, a lost signal that has already come back
/// and the HUD showing on its own after connecting. ContentView shows it on
/// an invisible element, `debug.probe`, which stays through a reconnect.
final class UITestProbe: ObservableObject {
    static let shared = UITestProbe()

    @Published private(set) var signalLosses = 0
    /// The HUD's Keyboard button the first time it showed while connected:
    /// "never", "shown" while it still shows, then how many seconds it stayed.
    @Published private(set) var firstKeyboardButton = "never"
    private var shownAt: TimeInterval = 0

    func signalLost() { signalLosses += 1 }

    func keyboardButtonAppeared() {
        guard firstKeyboardButton == "never" else { return }
        shownAt = ProcessInfo.processInfo.systemUptime
        firstKeyboardButton = "shown"
    }

    func keyboardButtonDisappeared() {
        guard firstKeyboardButton == "shown" else { return }
        firstKeyboardButton = String(format: "%.1f", ProcessInfo.processInfo.systemUptime - shownAt)
    }

    struct Element: View {
        @ObservedObject private var probe = UITestProbe.shared

        var body: some View {
            if UserDefaults.standard.bool(forKey: "uiTestProbe") {
                Color.clear
                    .frame(width: 1, height: 1)
                    .accessibilityElement()
                    .accessibilityIdentifier("debug.probe")
                    .accessibilityValue("keyboardButton=\(probe.firstKeyboardButton) signalLosses=\(probe.signalLosses)")
                    .allowsHitTesting(false)
            }
        }
    }
}
#endif
