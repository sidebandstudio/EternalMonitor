import SwiftUI

struct ContentView: View {
    @EnvironmentObject var connectionManager: ConnectionManager

    var body: some View {
        Group {
            switch connectionManager.state {
            case .disconnected, .connecting:
                ConnectView()
            case .connected:
                DisplayView()
            }
        }
        #if DEBUG
        // UI tests only: an overlay takes no space, so nothing moves.
        .overlay(alignment: .topLeading) {
            if UITestHooks.enabled { SignalHistoryProbe() }
        }
        #endif
    }
}
