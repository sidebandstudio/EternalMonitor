import Foundation

/// What the app calls this device: "iPad", or "iPhone" on an iPhone such as
/// iPhone Duo. On iPad every message reads exactly as before, and that is the
/// form SETUP.md and QUICKSTART.txt quote: scripts/check_messages.py reads
/// `\(DeviceNoun.current)` in a message as "iPad".
enum DeviceNoun {
    static let current = noun(forModel: Feedback.Context.modelIdentifier())
    static var isPhone: Bool { current == "iPhone" }

    /// From the hardware identifier, such as iPhone14,4, which any thread can
    /// read. UIDevice's interface idiom is main-actor only, and these messages
    /// are built off it too.
    static func noun(forModel model: String) -> String {
        model.hasPrefix("iPhone") ? "iPhone" : "iPad"
    }
}
