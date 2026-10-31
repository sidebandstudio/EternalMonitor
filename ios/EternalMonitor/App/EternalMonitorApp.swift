import SwiftUI

@main
struct EternalMonitorApp: App {
    @StateObject private var connectionManager = ConnectionManager()
    @StateObject private var settings = AppSettings()
    @Environment(\.scenePhase) private var scenePhase

    init() {
        #if DEBUG
        // UI tests: EM_FRESH_INSTALL=1 starts with nothing saved, as a new
        // install does. Pairings are in the Keychain and stay.
        if ProcessInfo.processInfo.environment["EM_FRESH_INSTALL"] == "1", let id = Bundle.main.bundleIdentifier {
            UserDefaults.standard.removePersistentDomain(forName: id)
        }
        #endif
        FontRegistry.shared.registerBundledFonts()
        SupportDiagnostics.shared.recordDeviceSnapshot()
        SupportDiagnostics.shared.record("info", "update",
            RemoteConfig.summary(RemoteConfig.launch, rollout: UpdateChecker.shared.available?.rollout))
        CrashReporting.shared.start()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(connectionManager)
                .environmentObject(settings)
                .preferredColorScheme(.dark)
                .onAppear {
                    connectionManager.refreshUSBAvailability()
                    UpdateChecker.shared.checkIfDue()
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
                .onReceive(NotificationCenter.default.publisher(for: UIApplication.didReceiveMemoryWarningNotification)) { _ in
                    let footprint = MemoryFootprint.megabytes().map(String.init) ?? "unknown"
                    let video = connectionManager.videoSize
                    SupportDiagnostics.shared.record("warning", "device", "Memory warning footprint_mb=\(footprint) video=\(Int(video.width))x\(Int(video.height)) state=\(connectionManager.state)")
                }
                .onReceive(NotificationCenter.default.publisher(for: ProcessInfo.thermalStateDidChangeNotification)) { _ in
                    SupportDiagnostics.shared.record("warning", "device", "Thermal state changed to \(ProcessInfo.processInfo.thermalState.rawValue)")
                }
                .onReceive(NotificationCenter.default.publisher(for: UIApplication.willTerminateNotification)) { _ in
                    CrashReporting.shared.marker.clear()
                }
                .onChange(of: connectionManager.state) { _, state in
                    CrashReporting.shared.marker.update(state)
                }
                .onReceive(NotificationCenter.default.publisher(for: UIApplication.willResignActiveNotification)) { _ in
                    DiagnosticsUpload.shared.flushSoon()
                    connectionManager.prepareForBackground()
                }
        }
        .onChange(of: scenePhase) { _, phase in
            SupportDiagnostics.shared.record("info", "lifecycle", "scene=\(phase)")
            E2E.emit("E2E_SCENE phase=\(phase) uptime=\(ProcessInfo.processInfo.systemUptime)")
            // A backgrounded app cannot keep receiving UDP; tell the host
            // goodbye so it stops streaming (and can tear the virtual display
            // down) instead of timing out on liveness. Coming back to the
            // foreground resumes the interrupted session (opt-out toggle).
            if phase == .background {
                CrashReporting.shared.marker.clear()
                connectionManager.handleAppBackgrounded()
            } else if phase == .active {
                CrashReporting.shared.marker.activated(connectionManager.state)
                connectionManager.handleAppForegrounded()
                UpdateChecker.shared.checkIfDue()
            }
        }
    }
}
