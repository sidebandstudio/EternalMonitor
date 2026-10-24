import Foundation

/// Why the PC and this iPad could not connect, when their versions differ, and
/// which side must update. Uses what the handshake already carries: the
/// protocol range this app sends in HELLO2 and the protocol a rejecting host
/// reports in HELLO_ACK's accepted_version (2 for every 0.2 and 0.3 host).
enum VersionMismatch: Equatable {
    case iPadOlder
    case pcOlder
    /// A v0.1.x host, which has no in-app update.
    case pcLegacy

    static func classify(hostProtocol: UInt8, iPadMin: UInt8, iPadMax: UInt8) -> VersionMismatch? {
        if hostProtocol > iPadMax { return .iPadOlder }
        if hostProtocol < iPadMin { return .pcOlder }
        return nil
    }

    var message: String {
        message(publicApp: InstalledApp.current.receivesPublicUpdates)
    }

    /// A test app, under another bundle ID, updates from its own TestFlight
    /// build, never the public app's Update.
    func message(publicApp: Bool) -> String {
        switch self {
        case .iPadOlder:
            return publicApp
                ? "This \(DeviceNoun.current) app is older than EternalMonitor on your PC, so they can't connect. Update the \(DeviceNoun.current) app: open TestFlight and tap Update."
                : "This \(DeviceNoun.current) app is older than EternalMonitor on your PC, so they can't connect. Install the newest build of this test app from TestFlight."
        case .pcOlder:
            return "EternalMonitor on your PC is older than this \(DeviceNoun.current) app, so they can't connect. Update EternalMonitor on your PC: click Update now on the PC, or download it from eternalmonitor.dev."
        case .pcLegacy:
            return "EternalMonitor on your PC is too old to connect. On your PC, download the new version from eternalmonitor.dev."
        }
    }

    /// Only the iPad side can act here; the PC side has its own Update now.
    var opensTestFlight: Bool { self == .iPadOlder }

    /// Used when the host gave no usable version.
    static let unknownMessage = "This app and EternalMonitor on your PC are different versions. Update both, then try again."
}
