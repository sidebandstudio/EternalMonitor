#if DEBUG
import SwiftUI

/// UI tests only, and only in Debug builds launched with `-EMUITest YES`.
/// A signal lost and regained within one session leaves no trace on the
/// host, and the HUD's "Reconnecting" can come and go between two looks of
/// a test, so the app keeps the count itself. Release builds have none of
/// this; without the argument a Debug build counts nothing and shows nothing.
enum UITestHooks {
    static let enabled = ProcessInfo.processInfo.arguments.contains("-EMUITest")
}

/// How often this launch connected, lost the signal, got it back, and
/// started reconnecting after losing the connection.
struct SignalHistory: Equatable {
    var connects = 0
    var signalLost = 0
    var signalRecovered = 0
    var reconnects = 0

    var summary: String {
        "connects=\(connects) lost=\(signalLost) recovered=\(signalRecovered) reconnects=\(reconnects)"
    }
}

/// An invisible point that takes no touches, for XCUITest to read the
/// history from on every screen.
struct SignalHistoryProbe: View {
    @EnvironmentObject var connectionManager: ConnectionManager

    var body: some View {
        Color.clear
            .frame(width: 1, height: 1)
            .allowsHitTesting(false)
            .accessibilityElement()
            .accessibilityIdentifier("uitest.signalHistory")
            .accessibilityValue(connectionManager.signalHistory.summary)
    }
}
#endif
