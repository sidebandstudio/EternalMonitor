import SwiftUI

struct ContentView: View {
    @EnvironmentObject var connectionManager: ConnectionManager

    private var isConnected: Bool {
        connectionManager.state == .connected
    }

    var body: some View {
        ZStack {
            if isConnected {
                DisplayView()
                    .transition(.opacity)
            } else {
                ConnectView()
                    .transition(.opacity.combined(with: .scale(scale: 0.98)))
            }
        }
        .animation(.easeInOut(duration: 0.3), value: isConnected)
    }
}
