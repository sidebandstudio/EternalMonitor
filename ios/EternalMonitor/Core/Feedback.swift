import Foundation
import UIKit

/// Feedback by email to the studio. Nothing here sends anything: Mail does,
/// once the user taps Send, or they paste the copied text themselves.
enum Feedback {
    static let email = "hello@sideband.studio"
    static let repoURL = "https://github.com/sidebandstudio/EternalMonitor"
    static let attachmentName = "EternalMonitor-diagnostics.txt"

    enum Kind: Equatable {
        /// From a problem card, named by its title.
        case problem(String)
        case general

        var label: String {
            switch self {
            case .problem: return "Problem"
            case .general: return "Feedback"
            }
        }
    }

    /// What the email says about this iPad and the PC. Never the PC's name,
    /// the iPad's name or any address.
    struct Context: Equatable {
        var appVersion: String
        var build: String
        var systemVersion: String
        var model: String
        /// EternalMonitor on the PC, when this iPad has heard it.
        var pcVersion: String?
        /// "USB" or "Wi-Fi" while connected.
        var connection: String?

        @MainActor static func current(pcVersion: String?, connection: String?) -> Context {
            let info = Bundle.main.infoDictionary
            return Context(
                appVersion: info?["CFBundleShortVersionString"] as? String ?? "unknown",
                build: info?["CFBundleVersion"] as? String ?? "unknown",
                systemVersion: UIDevice.current.systemVersion,
                model: modelIdentifier(),
                pcVersion: pcVersion,
                connection: connection
            )
        }

        /// The hardware identifier, such as iPad16,3.
        static func modelIdentifier() -> String {
            if let simulated = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] {
                return simulated
            }
            var system = utsname()
            uname(&system)
            return withUnsafeBytes(of: &system.machine) { bytes in
                String(decoding: bytes.prefix(while: { $0 != 0 }), as: UTF8.self)
            }
        }
    }

    /// "EternalMonitor 0.3.2 (7) for iPad: Problem", so the studio can sort mail.
    static func subject(_ kind: Kind, _ context: Context) -> String {
        "EternalMonitor \(context.appVersion) (\(context.build)) for iPad: \(kind.label)"
    }

    /// Said first in Mail whenever the Diagnostics report is attached, since
    /// the report can name the PC and iPad.
    static let attachmentNotice = "We attached a diagnostics report to help us look into it. "
        + "It can include your PC's and iPad's names and local network addresses. "
        + "Remove it before sending if you'd rather not share it."

    /// Room to write, then the details in short lines. With the Diagnostics
    /// report attached, the email says so first.
    static func body(_ kind: Kind, _ context: Context, attachesDiagnostics: Bool = false) -> String {
        var lines: [String] = []
        if case .problem(let title) = kind {
            lines.append("Problem: \(title)")
        }
        lines.append("App: EternalMonitor \(context.appVersion) (\(context.build)) for iPad")
        lines.append("iPadOS: \(context.systemVersion)")
        lines.append("iPad: \(context.model)")
        lines.append("PC app: \(context.pcVersion.map { "EternalMonitor \($0)" } ?? "unknown")")
        lines.append("Connection: \(context.connection ?? "not connected")")
        let notice = attachesDiagnostics ? attachmentNotice + "\n" : ""
        return notice + "\n\n\n--\n" + lines.joined(separator: "\n")
    }

    /// The whole message for Copy message, with where it goes.
    static func fullText(_ kind: Kind, _ context: Context) -> String {
        "To: \(email)\nSubject: \(subject(kind, context))\n\(body(kind, context))"
    }

    /// Mail when it is set up; otherwise the sheet to copy from.
    enum Route: Equatable {
        case mail
        case copy
    }

    static func route(canSendMail: Bool) -> Route {
        canSendMail ? .mail : .copy
    }
}

/// The one-time "How's EternalMonitor working for you? :)" card. After the
/// third session of five minutes or more it shows on the Connect screen;
/// Send feedback or Not now retires it for good.
struct FeedbackAsk {
    static let longSession: TimeInterval = 5 * 60
    static let askAfter = 3
    static let sessionsKey = "feedbackLongSessions"
    static let doneKey = "feedbackAskDone"

    private(set) var longSessions: Int
    private(set) var done: Bool

    init(defaults: UserDefaults = .standard) {
        longSessions = defaults.integer(forKey: Self.sessionsKey)
        done = defaults.bool(forKey: Self.doneKey)
    }

    /// A streaming session ended after `duration` seconds.
    mutating func sessionEnded(after duration: TimeInterval, defaults: UserDefaults = .standard) {
        guard !done, duration >= Self.longSession, longSessions < Self.askAfter else { return }
        longSessions += 1
        defaults.set(longSessions, forKey: Self.sessionsKey)
    }

    /// Never while streaming, so it never covers the picture.
    func due(streaming: Bool) -> Bool {
        !done && !streaming && longSessions >= Self.askAfter
    }

    mutating func finish(defaults: UserDefaults = .standard) {
        done = true
        defaults.set(true, forKey: Self.doneKey)
    }
}

/// EternalMonitor versions PCs announce over Bonjour, by address and name.
@MainActor
enum PCVersions {
    private static var byKey: [String: String] = [:]

    static func record(_ version: String, name: String, address: String?) {
        guard !version.isEmpty else { return }
        byKey[name] = version
        if let address { byKey[address] = version }
    }

    static func version(address: String?, name: String?) -> String? {
        address.flatMap { byKey[$0] } ?? name.flatMap { byKey[$0] }
    }

    /// The `version` entry of a Bonjour TXT record.
    nonisolated static func version(fromTXT data: Data?) -> String? {
        guard let data else { return nil }
        let entries = NetService.dictionary(fromTXTRecord: data)
        return entries["version"].flatMap { String(data: $0, encoding: .utf8) }
    }
}
