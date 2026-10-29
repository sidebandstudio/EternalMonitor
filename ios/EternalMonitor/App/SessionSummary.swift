import Foundation

/// Why a connection attempt ended. It names the end in the support log and
/// the attempt's summary, and picks the goodbye the PC gets.
enum AttemptEnd: Equatable {
    /// Disconnect or Cancel, or a setting that needs a new handshake.
    case userDisconnect
    /// The user turned USB connections off during a cable session.
    case usbTurnedOff
    case appBackground
    /// The cable took over from Wi-Fi.
    case superseded
    case pcGoodbye
    /// The PC turned the connection down.
    case pcRefused
    case legacyPC
    /// The connect watchdog ran out before a picture.
    case connectTimeout
    /// Heartbeats stopped during a session.
    case pcStoppedResponding
    /// The picture stayed held without a keyframe, though the PC still
    /// answered: 5 s, or longer while the PC reopened its encoder. The app
    /// reconnects.
    case pictureStalled
    case cableUnplugged
    case usbLinkFailed
    /// The PC closed the cable before a picture, too many times in a row.
    case closedBeforePicture
    /// This iPad's network side failed, for example Local Network denied.
    case linkFailed
    case receiverFailed
    /// The PC asked for the pairing code, and a new HELLO2 sends it: the
    /// refused one is over, the attempt goes on.
    case retriedWithCode

    /// The support log's name for it.
    var name: String { "\(self)" }

    /// The goodbye the PC gets. Only the user's own action is `userDisconnect`.
    var bye: ByeReason {
        switch self {
        case .userDisconnect, .usbTurnedOff: return .userDisconnect
        case .appBackground: return .appBackground
        case .superseded: return .superseded
        default: return .error
        }
    }
}

/// The attempt's one `session_summary v=1` line, recorded when it ends. The PC
/// records the same line for its side; the two share `session_id` and
/// `nonce`. Every value is from a closed set, a bounded number or a strict
/// pattern: no names, addresses or error text.
struct SessionSummary: Equatable {
    var end: AttemptEnd
    var isUSB: Bool
    /// The PC's session, once it accepted one.
    var sessionId: UInt32?
    /// The last HELLO2's client nonce; 0 before one was sent.
    var nonce: UInt32 = 0
    /// The PC asked for the pairing code and the attempt ended before a session.
    var awaitingPairing = false
    var datagramsReceived = 0
    /// Seconds from the start to the first decoded frame.
    var firstFrame: TimeInterval?
    var duration: TimeInterval
    var os = SessionSummary.currentOS
    var hardware = SessionSummary.currentHardware

    /// A picture for this long makes the attempt `steady`.
    static let steadyAfter: TimeInterval = 10

    var line: String {
        let stage: String
        if let firstFrame {
            stage = duration - firstFrame >= Self.steadyAfter ? "steady" : "first_frame"
        } else if sessionId != nil {
            stage = "accepted"
        } else {
            stage = nonce != 0 ? "hello_sent" : "started"
        }
        let ttff = firstFrame.map { String(Self.milliseconds($0, max: 600_000)) } ?? "-"
        return "session_summary v=1 role=ipad session_id=\(sessionId.map(String.init) ?? "-")"
            + " nonce=\(nonce == 0 ? "-" : Self.hex16(nonce)) transport=\(isUSB ? "usb" : "wifi") mode=unknown"
            + " stage=\(stage) outcome=\(outcome) failure=\(failure) fallback=none end=\(endToken)"
            + " ttff_ms=\(ttff) duration_ms=\(Self.milliseconds(duration, max: 2_592_000_000))"
            + " os=\(os) hw=\(hardware) gpu=- hybrid=-"
    }

    private var outcome: String {
        if firstFrame != nil { return "streamed" }
        if awaitingPairing && sessionId == nil { return "pairing" }
        switch end {
        case .userDisconnect, .usbTurnedOff: return "user_ended"
        case .appBackground, .superseded, .pcGoodbye: return "app_ended"
        case .connectTimeout: return sessionId == nil ? "no_reply" : "no_picture"
        case .closedBeforePicture: return "no_picture"
        default: return "error"
        }
    }

    private var failure: String {
        switch end {
        case .connectTimeout where sessionId == nil: return "no_reply"
        case .connectTimeout: return datagramsReceived == 0 ? "no_video" : "no_frame"
        case .pcStoppedResponding: return "signal_lost"
        case .cableUnplugged, .usbLinkFailed, .closedBeforePicture: return "usb_lost"
        case .pcRefused, .legacyPC, .linkFailed, .receiverFailed, .pictureStalled: return "other"
        default: return "none"
        }
    }

    private var endToken: String {
        switch end {
        case .userDisconnect, .usbTurnedOff: return "user"
        case .appBackground: return "background"
        case .superseded, .retriedWithCode: return "superseded"
        case .pcGoodbye: return "goodbye"
        case .connectTimeout: return "timeout"
        case .pcStoppedResponding: return "liveness"
        default: return "error"
        }
    }

    private static func milliseconds(_ seconds: TimeInterval, max: Int) -> Int {
        guard seconds.isFinite, seconds > 0 else { return 0 }
        return Int(min(seconds * 1000, Double(max)))
    }

    /// 16 lowercase hex digits, as the PC writes the nonce it echoed.
    static func hex16(_ value: UInt32) -> String {
        let hex = String(value, radix: 16)
        return String(repeating: "0", count: 16 - hex.count) + hex
    }

    /// `ipados:<major>.<minor>[.<patch>]`; `ipados:0.0` if a part is out of range.
    static func osToken(_ version: OperatingSystemVersion) -> String {
        let parts = [version.majorVersion, version.minorVersion, version.patchVersion]
        guard parts.allSatisfy({ (0...999).contains($0) }) else { return "ipados:0.0" }
        let shown = version.patchVersion == 0 ? parts.prefix(2) : parts.prefix(3)
        return "ipados:" + shown.map(String.init).joined(separator: ".")
    }

    /// An iPad model id such as `iPad16,3`. Anything else is `iPad0,0`: the
    /// summary has no other value for an unknown model.
    static func hardwareToken(_ machine: String) -> String {
        machine.range(of: #"^iPad[0-9]{1,2},[0-9]{1,2}$"#, options: .regularExpression) != nil ? machine : "iPad0,0"
    }

    static let currentOS = osToken(ProcessInfo.processInfo.operatingSystemVersion)

    static let currentHardware: String = {
        // A simulator reports the Mac's architecture; it names the model it runs.
        if let model = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] {
            return hardwareToken(model)
        }
        var system = utsname()
        uname(&system)
        let machine = withUnsafeBytes(of: &system.machine) { bytes in
            String(decoding: bytes.prefix(while: { $0 != 0 }), as: UTF8.self)
        }
        return hardwareToken(machine)
    }()
}
