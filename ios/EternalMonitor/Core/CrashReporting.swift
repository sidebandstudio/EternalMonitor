import Foundation
import MetricKit

/// Crash reports for the iPad app: MetricKit's crash, hang, CPU and exit
/// reports, and a marker for a launch that ended on screen. Each becomes one
/// support-log line, so it follows the diagnostics setting like every other
/// line: uploaded when sharing is on, kept on this iPad when it is off.
/// MetricKit reports a crash at the next launch, and the exit counts about
/// once a day.
final class CrashReporting: NSObject, MXMetricManagerSubscriber, @unchecked Sendable {
    static let shared = CrashReporting()

    let marker: ForegroundMarker
    private let record: (_ level: String, _ category: String, _ message: String) -> Void

    init(marker: ForegroundMarker = ForegroundMarker(),
         record: @escaping (String, String, String) -> Void = { SupportDiagnostics.shared.record($0, $1, $2) }) {
        self.marker = marker
        self.record = record
    }

    /// At launch: the previous launch's marker, then MetricKit.
    func start() {
        if let line = marker.takePreviousLaunch() { record("warning", "lifecycle", line) }
        MXMetricManager.shared.add(self)
    }

    func didReceive(_ payloads: [MXDiagnosticPayload]) {
        for payload in payloads {
            let crashes = (payload.crashDiagnostics ?? []).map { MetricKitDiagnostic($0, at: payload.timeStampEnd) }
            let hangs = (payload.hangDiagnostics ?? []).map { MetricKitDiagnostic($0, at: payload.timeStampEnd) }
            let cpu = (payload.cpuExceptionDiagnostics ?? []).map { MetricKitDiagnostic($0, at: payload.timeStampEnd) }
            handle(crashes + hangs + cpu)
        }
    }

    func didReceive(_ payloads: [MXMetricPayload]) {
        for payload in payloads {
            if let exits = MetricKitExits(payload) { handle(exits) }
        }
    }

    func handle(_ diagnostics: [MetricKitDiagnostic]) {
        for diagnostic in diagnostics {
            record(diagnostic.kind == .crash ? "error" : "warning", diagnostic.kind.category, diagnostic.line)
        }
    }

    func handle(_ exits: MetricKitExits) {
        record("info", "exits", exits.line)
    }
}

/// One MetricKit crash, hang or CPU exception, reduced to what its log line
/// shows: the exception, the signal, the termination namespace and code, the
/// versions and the top of the stack. No free text from MetricKit reaches the
/// line: the VM region info, the termination reason's description and the
/// Objective-C exception's message can name files or the user, so they are
/// left out.
struct MetricKitDiagnostic: Equatable {
    enum Kind: String {
        case crash, hang
        case cpuException = "cpu_exception"

        var category: String {
            switch self {
            case .crash: return "crash"
            case .hang: return "hang"
            case .cpuException: return "cpu"
            }
        }
    }

    /// One frame: the binary's name and the offset into its text segment,
    /// which `atos` resolves with the build's dSYM.
    struct Frame: Equatable {
        var binary: String
        var offset: UInt64
    }

    static let topFrames = 8

    var kind: Kind
    var appVersion: String
    var build: String
    var osVersion: String
    /// The end of the payload's window; for a crash, about when it happened.
    var at: Date
    var exceptionType: Int64? = nil
    var exceptionCode: Int64? = nil
    var signal: Int64? = nil
    /// An uncaught Objective-C exception's name, such as NSRangeException.
    var objcException: String? = nil
    /// MetricKit's termination reason, such as "Namespace SIGNAL, Code 11
    /// Segmentation fault: 11". Only its namespace and code are shown.
    var terminationReason: String? = nil
    var seconds: Double? = nil
    var sampledSeconds: Double? = nil
    /// The crashing (or attributed) thread's top frames, innermost first.
    var frames: [Frame] = []
    /// The frames are from the thread MetricKit attributed the report to.
    var attributed = false

    var line: String {
        var parts = ["MetricKit \(kind.rawValue)", "app=\(Self.token(appVersion))", "build=\(Self.token(build))"]
        parts += Self.osTokens(osVersion)
        parts.append("at=\(timestamp(at))")
        switch kind {
        case .crash:
            parts.append("exception_type=\(Self.number(exceptionType, Self.exceptionNames))")
            parts.append("exception_code=\(exceptionCode.map(String.init) ?? "-")")
            parts.append("signal=\(Self.number(signal, Self.signalNames))")
            parts.append("objc_exception=\(objcException.map(Self.exceptionName) ?? "-")")
        case .hang:
            parts.append("duration_ms=\(Self.milliseconds(seconds))")
        case .cpuException:
            parts.append("cpu_ms=\(Self.milliseconds(seconds)) sampled_ms=\(Self.milliseconds(sampledSeconds))")
        }
        parts.append("thread=\(frames.isEmpty ? "-" : attributed ? "attributed" : "first")")
        parts.append("frames=\(frames.isEmpty ? "-" : frames.map { "\($0.binary)+0x\(String($0.offset, radix: 16))" }.joined(separator: ","))")
        if kind == .crash {
            parts.append("termination=\(terminationReason.map(Self.termination) ?? "-")")
        }
        return Self.scrub(parts.joined(separator: " "))
    }

    // MARK: - Tokens

    /// ASCII letters, digits and `._+-`, at most 64; other characters become
    /// `_`, and nothing at all is `-`.
    static func token(_ text: String) -> String {
        let shown = String(text.unicodeScalars.prefix(64).map {
            CharacterSet.alphanumerics.contains($0) && $0.isASCII || "._+-".unicodeScalars.contains($0) ? Character($0) : "_"
        })
        return shown.isEmpty ? "-" : shown
    }

    /// `os=ipados:<major>.<minor>[.<patch>] os_build=<build>` from MetricKit's
    /// "iPadOS 26.1 (23B85)" (older releases say "iPhone OS").
    static func osTokens(_ text: String) -> [String] {
        var version = OperatingSystemVersion(majorVersion: -1, minorVersion: 0, patchVersion: 0)
        if let match = text.range(of: #"[0-9]{1,3}\.[0-9]{1,3}(\.[0-9]{1,3})?"#, options: .regularExpression) {
            let parts = text[match].split(separator: ".").compactMap { Int($0) }
            version = OperatingSystemVersion(majorVersion: parts[0], minorVersion: parts[1],
                                             patchVersion: parts.count > 2 ? parts[2] : 0)
        }
        let build = text.range(of: #"\([0-9A-Za-z]{1,16}\)"#, options: .regularExpression)
            .map { String(text[$0].dropFirst().dropLast()) } ?? "-"
        return ["os=\(SessionSummary.osToken(version))", "os_build=\(build)"]
    }

    /// `namespace:<NAMESPACE> code:<code>` from a termination reason that
    /// starts "Namespace SIGNAL, Code 11" or "Namespace SPRINGBOARD, Code
    /// 0x8badf00d"; the description after the code is dropped. Any other
    /// reason is `other`.
    static func termination(_ text: String) -> String {
        guard let match = text.range(of: #"^\s*Namespace [A-Z][A-Z0-9_]{0,31}, Code (0x[0-9A-Fa-f]{1,16}|[0-9]{1,20})(?![0-9A-Za-z])"#,
                                     options: .regularExpression) else { return "other" }
        let words = text[match].split(whereSeparator: { $0 == " " || $0 == "," })
        return "namespace:\(words[1]) code:\(words[3])"
    }

    /// An Objective-C exception name as Apple's classes name them, such as
    /// NSRangeException; any other name, which an app or library could fill
    /// with anything, is `other`.
    static func exceptionName(_ name: String) -> String {
        name.range(of: #"\ANS[A-Za-z]{1,60}Exception\z"#, options: .regularExpression) != nil
            || appleExceptionNames.contains(name) ? name : "other"
    }

    /// Apple's exception names that don't follow the NS…Exception pattern.
    static let appleExceptionNames: Set<String> = [
        "CALayerInvalid", "CALayerInvalidGeometry", "UIViewControllerHierarchyInconsistency",
        "UIApplicationInvalidInterfaceOrientation",
    ]

    /// The last pass over a whole line: UUIDs, long hex runs and long
    /// base64-like runs become `<id>`, wherever they are. A binary name can
    /// carry one.
    static func scrub(_ line: String) -> String {
        [#"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}"#,
         // 16 hex digits or more with a letter among them (offsets stay far
         // shorter, and decimal counts and codes have no letter).
         #"(?<![0-9A-Za-z])(0x)?(?=[0-9A-Fa-f]*[A-Fa-f])[0-9A-Fa-f]{16,}(?![0-9A-Za-z])"#,
         // 20 letters, digits, `_` or `-` or more, with letters and digits
         // both present. `=`, `+` and `.` split it, as they split the fields.
         #"(?<![0-9A-Za-z_-])(?=[0-9A-Za-z_-]*[0-9])(?=[0-9A-Za-z_-]*[A-Za-z])[0-9A-Za-z_-]{20,}(?![0-9A-Za-z_-])"#]
            .reduce(line) { $0.replacingOccurrences(of: $1, with: "<id>", options: .regularExpression) }
    }

    private static func milliseconds(_ seconds: Double?) -> String {
        guard let seconds, seconds.isFinite, seconds >= 0 else { return "-" }
        return String(Int(min(seconds * 1000, 86_400_000)))
    }

    private static func number(_ value: Int64?, _ names: [Int64: String]) -> String {
        guard let value else { return "-" }
        return names[value].map { "\(value)(\($0))" } ?? String(value)
    }

    static let exceptionNames: [Int64: String] = [
        1: "EXC_BAD_ACCESS", 2: "EXC_BAD_INSTRUCTION", 3: "EXC_ARITHMETIC", 4: "EXC_EMULATION",
        5: "EXC_SOFTWARE", 6: "EXC_BREAKPOINT", 10: "EXC_CRASH", 11: "EXC_RESOURCE", 12: "EXC_GUARD",
    ]

    static let signalNames: [Int64: String] = [
        4: "SIGILL", 5: "SIGTRAP", 6: "SIGABRT", 7: "SIGEMT", 8: "SIGFPE", 9: "SIGKILL",
        10: "SIGBUS", 11: "SIGSEGV", 12: "SIGSYS", 13: "SIGPIPE", 15: "SIGTERM",
    ]

    // MARK: - Call stacks

    /// MetricKit's call stack tree (`MXCallStackTree.jsonRepresentation()`).
    /// Frames carry a binary name, its UUID, an address and the offset into
    /// the binary's text segment: no paths, symbols or arguments.
    private struct Tree: Decodable {
        var callStackPerThread: Bool?
        var callStacks: [Stack]?
        struct Stack: Decodable {
            var threadAttributed: Bool?
            var callStackRootFrames: [Node]?
        }
        struct Node: Decodable {
            var binaryName: String?
            var offsetIntoBinaryTextSegment: UInt64?
            var sampleCount: Int?
            var subFrames: [Node]?
        }
    }

    /// The attributed thread's top `topFrames` frames, innermost first, and
    /// whether a thread was attributed. In a per-thread tree (a crash or a
    /// hang) a root frame is the innermost frame and its sub-frame is its
    /// caller. Otherwise the tree aggregates samples from the outermost
    /// caller down, and the heaviest path leads to the innermost frame.
    static func topFrames(_ json: Data) -> (frames: [Frame], attributed: Bool) {
        guard let tree = try? JSONDecoder().decode(Tree.self, from: json),
              let stacks = tree.callStacks, !stacks.isEmpty else { return ([], false) }
        let stack = stacks.first { $0.threadAttributed == true } ?? stacks[0]
        func frame(_ node: Tree.Node) -> Frame {
            Frame(binary: token(node.binaryName ?? "-"), offset: node.offsetIntoBinaryTextSegment ?? 0)
        }
        var frames: [Frame] = []
        if tree.callStackPerThread ?? true {
            var node = stack.callStackRootFrames?.first
            while let current = node, frames.count < topFrames {
                frames.append(frame(current))
                node = current.subFrames?.first
            }
        } else {
            var path: [Tree.Node] = []
            var level = stack.callStackRootFrames ?? []
            // Bounded like the tree MetricKit captured (at most a few hundred frames).
            while let heaviest = level.max(by: { ($0.sampleCount ?? 0) < ($1.sampleCount ?? 0) }), path.count < 1024 {
                path.append(heaviest)
                level = heaviest.subFrames ?? []
            }
            frames = path.reversed().prefix(topFrames).map(frame)
        }
        return (frames, stack.threadAttributed == true)
    }
}

extension MetricKitDiagnostic {
    private init(kind: Kind, _ diagnostic: MXDiagnostic, stack: MXCallStackTree, at: Date) {
        let top = Self.topFrames(stack.jsonRepresentation())
        self.init(kind: kind, appVersion: diagnostic.applicationVersion, build: diagnostic.metaData.applicationBuildVersion,
                  osVersion: diagnostic.metaData.osVersion, at: at, frames: top.frames, attributed: top.attributed)
    }

    init(_ crash: MXCrashDiagnostic, at: Date) {
        self.init(kind: .crash, crash, stack: crash.callStackTree, at: at)
        exceptionType = crash.exceptionType?.int64Value
        exceptionCode = crash.exceptionCode?.int64Value
        signal = crash.signal?.int64Value
        objcException = crash.exceptionReason?.exceptionName
        terminationReason = crash.terminationReason
    }

    init(_ hang: MXHangDiagnostic, at: Date) {
        self.init(kind: .hang, hang, stack: hang.callStackTree, at: at)
        seconds = hang.hangDuration.converted(to: .seconds).value
    }

    init(_ cpu: MXCPUExceptionDiagnostic, at: Date) {
        self.init(kind: .cpuException, cpu, stack: cpu.callStackTree, at: at)
        seconds = cpu.totalCPUTime.converted(to: .seconds).value
        sampledSeconds = cpu.totalSampledTime.converted(to: .seconds).value
    }
}

/// MetricKit's daily app exit counts, in the foreground and the background.
/// Counts only, one line per payload.
struct MetricKitExits: Equatable {
    /// The exits only a background app has.
    struct BackgroundOnly: Equatable {
        var cpuLimit = 0
        var memoryPressure = 0
        var lockedFile = 0
        var taskTimeout = 0

        var tokens: [String] {
            [("cpu_limit", cpuLimit), ("memory_pressure", memoryPressure), ("locked_file", lockedFile), ("task_timeout", taskTimeout)]
                .map { "bg_\($0.0)=\(max(0, $0.1))" }
        }
    }

    struct Counts: Equatable {
        var normal = 0
        var abnormal = 0
        var memoryLimit = 0
        var watchdog = 0
        var badAccess = 0
        var illegalInstruction = 0

        func tokens(_ prefix: String) -> [String] {
            [("normal", normal), ("abnormal", abnormal), ("memory_limit", memoryLimit), ("watchdog", watchdog),
             ("bad_access", badAccess), ("illegal_instruction", illegalInstruction)]
                .map { "\(prefix)_\($0.0)=\(max(0, $0.1))" }
        }
    }

    var appVersion: String
    var build: String
    var osVersion: String
    /// The payload covers more than one app version, as across an update.
    var multipleVersions: Bool
    var from: Date
    var to: Date
    var foreground: Counts
    var background: Counts
    var backgroundOnly = BackgroundOnly()

    var line: String {
        var parts = ["MetricKit exits", "app=\(MetricKitDiagnostic.token(appVersion))", "build=\(MetricKitDiagnostic.token(build))"]
        parts += MetricKitDiagnostic.osTokens(osVersion)
        parts += ["versions=\(multipleVersions ? "multiple" : "one")", "from=\(timestamp(from))", "to=\(timestamp(to))"]
        parts += foreground.tokens("fg") + background.tokens("bg") + backgroundOnly.tokens
        return MetricKitDiagnostic.scrub(parts.joined(separator: " "))
    }
}

/// A MetricKit time in UTC, to the second.
private func timestamp(_ date: Date) -> String {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime]
    return formatter.string(from: date)
}

extension MetricKitExits {
    init?(_ payload: MXMetricPayload) {
        guard let exits = payload.applicationExitMetrics else { return nil }
        let fg = exits.foregroundExitData
        let bg = exits.backgroundExitData
        self.init(
            appVersion: payload.latestApplicationVersion, build: payload.metaData?.applicationBuildVersion ?? "-",
            osVersion: payload.metaData?.osVersion ?? "", multipleVersions: payload.includesMultipleApplicationVersions,
            from: payload.timeStampBegin, to: payload.timeStampEnd,
            foreground: Counts(normal: fg.cumulativeNormalAppExitCount, abnormal: fg.cumulativeAbnormalExitCount,
                               memoryLimit: fg.cumulativeMemoryResourceLimitExitCount, watchdog: fg.cumulativeAppWatchdogExitCount,
                               badAccess: fg.cumulativeBadAccessExitCount, illegalInstruction: fg.cumulativeIllegalInstructionExitCount),
            background: Counts(normal: bg.cumulativeNormalAppExitCount, abnormal: bg.cumulativeAbnormalExitCount,
                               memoryLimit: bg.cumulativeMemoryResourceLimitExitCount, watchdog: bg.cumulativeAppWatchdogExitCount,
                               badAccess: bg.cumulativeBadAccessExitCount, illegalInstruction: bg.cumulativeIllegalInstructionExitCount),
            backgroundOnly: BackgroundOnly(cpuLimit: bg.cumulativeCPUResourceLimitExitCount,
                                           memoryPressure: bg.cumulativeMemoryPressureExitCount,
                                           lockedFile: bg.cumulativeSuspendedWithLockedFileExitCount,
                                           taskTimeout: bg.cumulativeBackgroundTaskAssertionTimeoutExitCount))
    }
}

/// Whether the last launch ended on screen. Set when the app becomes active
/// and cleared when it goes to the background or terminates cleanly, so a
/// marker found at launch means the app stopped while in use: a crash, a
/// watchdog kill or a forced quit. It keeps the app version, so the first
/// launch of an update never reports how the old version ended.
struct ForegroundMarker {
    static let key = "foregroundMarker"
    let defaults: UserDefaults
    let version: String

    init(defaults: UserDefaults = .standard, version: String = ForegroundMarker.currentVersion) {
        self.defaults = defaults
        self.version = version
    }

    static let currentVersion: String = {
        let info = Bundle.main.infoDictionary ?? [:]
        return "\(info["CFBundleShortVersionString"] as? String ?? "unknown") (\(info["CFBundleVersion"] as? String ?? "unknown"))"
    }()

    /// What the connection was doing, as the marker keeps it.
    static func stream(_ state: ConnectionState) -> String {
        switch state {
        case .connected: return "active"
        case .connecting: return "connecting"
        case .disconnected: return "none"
        }
    }

    /// The support-log line for the marker the last launch left, if it was
    /// this version's; clears the marker.
    func takePreviousLaunch() -> String? {
        let marker = defaults.dictionary(forKey: Self.key)
        defaults.removeObject(forKey: Self.key)
        guard let marker, marker["version"] as? String == version else { return nil }
        let stream = marker["stream"] as? String ?? "none"
        return "Previous launch ended while EternalMonitor was on screen (possible crash or forced quit) stream=\(stream)"
    }

    func activated(_ state: ConnectionState) {
        defaults.set(["version": version, "stream": Self.stream(state)], forKey: Self.key)
    }

    /// The connection changed; kept only while the app is on screen.
    func update(_ state: ConnectionState) {
        guard defaults.dictionary(forKey: Self.key) != nil else { return }
        activated(state)
    }

    func clear() {
        defaults.removeObject(forKey: Self.key)
    }
}
