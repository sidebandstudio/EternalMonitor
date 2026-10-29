import SwiftUI

@main
struct EternalMonitorApp: App {
    @StateObject private var connectionManager = ConnectionManager()
    @StateObject private var settings = AppSettings()
    @Environment(\.scenePhase) private var scenePhase

    init() {
        FontRegistry.shared.registerBundledFonts()
        SupportDiagnostics.shared.recordDeviceSnapshot()
        SupportDiagnostics.shared.record("info", "update",
            RemoteConfig.summary(RemoteConfig.launch, rollout: UpdateChecker.shared.available?.rollout))
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
                connectionManager.handleAppBackgrounded()
            } else if phase == .active {
                connectionManager.handleAppForegrounded()
                UpdateChecker.shared.checkIfDue()
            }
        }
    }
}
