import SwiftUI

@main
struct EternalMonitorApp: App {
    @StateObject private var connectionManager = ConnectionManager()
    @StateObject private var settings = AppSettings()
    @Environment(\.scenePhase) private var scenePhase

    init() {
        FontRegistry.shared.registerBundledFonts()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(connectionManager)
                .environmentObject(settings)
                .preferredColorScheme(.dark)
                .onAppear {
                    connectionManager.refreshUSBAvailability()
                    // Automated end-to-end harness: EM_AUTOCONNECT=host:port
                    // connects immediately, bypassing the connect screen.
                    if let target = E2E.autoconnectTarget {
                        connectionManager.connect(host: target.host, port: target.port)
                    }
                    #if DEBUG
                    UIPreview.apply(to: connectionManager)
                    #endif
                }
                .onChange(of: settings.allowUSB) { _, _ in
                    connectionManager.refreshUSBAvailability()
                }
                .onChange(of: settings.playPCaudio) { _, enabled in
                    connectionManager.refreshAudioPreference(enabled)
                }
        }
        .onChange(of: scenePhase) { _, phase in
            // A backgrounded app cannot keep receiving UDP; tell the host
            // goodbye so it stops streaming (and can tear the virtual display
            // down) instead of timing out on liveness. Coming back to the
            // foreground resumes the interrupted session (opt-out toggle).
            if phase == .background {
                connectionManager.handleAppBackgrounded()
            } else if phase == .active {
                connectionManager.handleAppForegrounded()
            }
        }
    }
}
