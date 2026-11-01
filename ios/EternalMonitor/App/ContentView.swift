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
        .appLanguage()
    }
}
