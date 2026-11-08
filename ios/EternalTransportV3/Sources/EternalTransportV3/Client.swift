import Foundation
import Security

// The iPad's side of v3 reconnects, as a sans-IO state machine: a port of
// the Rust reference client (`proto/src/v3/client.rs`), branch for branch.
// Callers feed it datagrams, a monotonic `now` and the wall clock in µs, and
// act on the `ClientOutput`s it returns. It never reads a clock or a socket.
//
// One attempt runs at a time:
//
// - Connecting. Message 1 goes out, and the same bytes are resent every
//   250 ms for at most 2 s. Then the attempt has failed and the caller picks
//   the next key (`KeyRing.connectOrder`).
// - Confirming. Message 2 arrived. That is the input cutover: the caller
//   releases every held touch, key and button locally, and no input is
//   sealed until the new session is live. `SessionConfirm` goes out under
//   the new keys, freshly sealed every 100 ms for at most 2 s.
// - The new session goes live on `SessionConfirmAck`, or on the first
//   authenticated application record under it, a v2 message with inner
//   type below 0x40 (confirms then continue until the ack or the end of the
//   window). Only sealed records count: the client never sees cleartext v2
//   media, so media under a session id is never taken as proof that the
//   session is live (phase 2b-i note for phase 3).
// - If the confirm window runs out, a new attempt starts, and the old one is
//   kept receive-only for 11 s from message 2 (the host's 5 s pending
//   lifetime, up to 5 s more to seal a refusal, 1 s in flight). Its ack,
//   records or refusal are still handled; nothing is ever sealed under it.
// - The old session drains: its records are still delivered until the new
//   session's first IDR decodes, for at most 1 s.
// - Liveness (gap 14): the host ends a superseded session without a BYE, so
//   no authenticated record on the live session within the Accept's
//   liveness timeout (3 s by default) closes it, and an attempt starts if
//   the app still wants that key.
//
// Not thread-safe: the caller serializes every call.

public struct ClientConfig: Sendable {
    public var hostId: Id16
    public var msg1RetryInterval: Duration = .milliseconds(250)
    /// How long one attempt sends message 1.
    public var msg1Window: Duration = .seconds(2)
    public var confirmRetryInterval: Duration = .milliseconds(100)
    /// How long `SessionConfirm` is retried after message 2.
    public var confirmWindow: Duration = .seconds(2)
    /// Longest an old session's records are still delivered after cutover.
    public var drainLimit: Duration = .seconds(1)
    public var maxAge: Duration = V3.sessionMaxAge
    /// Reconnect this long before the session's maximum age.
    public var reconnectMargin: Duration = .seconds(60)
    /// No authenticated record on the live session for this long ends it,
    /// unless the Accept gives its own `liveness_timeout_ms`.
    public var livenessTimeout: Duration = .seconds(3)
    /// How long, from message 2, an attempt whose confirm window ran out
    /// keeps its receive key: 5 + 5 + 1 s.
    public var abandonedKeep: Duration = .seconds(11)

    public init(hostId: Id16) {
        self.hostId = hostId
    }
}

/// One of the iPad's keys for this host. Copies share one `SecretBytes`,
/// wiped when the last copy goes. The description never shows the key.
public struct ClientKey: CustomStringConvertible {
    public let devId: Id16
    public let kDev: SecretBytes

    /// Nil unless `kDev` is 32 bytes.
    public init?(devId: Id16, kDev: SecretBytes) {
        guard kDev.count == 32 else { return nil }
        self.devId = devId
        self.kDev = kDev
    }

    public var description: String { "ClientKey(devId: \(devId))" }
}

public enum ClientClose: Equatable, Sendable {
    /// The old session's drain ended: the new session's IDR decoded, or 1 s
    /// passed.
    case drainEnded
    /// The host sent a sealed BYE with this v2 `ByeReason`.
    case hostBye(UInt8)
    /// The session reached its maximum age or counter limit.
    case expired
    /// Nothing authenticated arrived within the liveness timeout.
    case liveness
    /// The app ended it.
    case local
}

/// What the caller does next. The description shows a delivered body's
/// length, never its bytes.
public enum ClientOutput: Equatable, CustomStringConvertible {
    case send([UInt8])
    /// The input cutover. End every held touch, key and button locally, and
    /// send no release for presses that began before it.
    case releaseAllInput
    /// Input and reports go to this session from now on.
    case live(sessionId: UInt32, accept: Accept)
    /// An authenticated slot status, for the key rule.
    case status(devId: Id16, status: SlotStatus)
    /// The host refused this key's record; its status tombstones the key.
    /// Ask the user to pair again. Any live session is untouched.
    case refused(devId: Id16, status: SlotStatus)
    /// This key can never connect again: pair again. Nothing more is
    /// attempted under it.
    case pairAgain(devId: Id16)
    /// No usable answer within the attempt's window. Try the next key.
    case attemptFailed(devId: Id16)
    case deliver(sessionId: UInt32, counter: UInt64, innerType: UInt8, body: [UInt8], draining: Bool)
    case closed(sessionId: UInt32, reason: ClientClose)

    public var description: String {
        switch self {
        case .send(let datagram): return "send(\(datagram.count) bytes)"
        case .releaseAllInput: return "releaseAllInput"
        case .live(let sessionId, _): return "live(\(sessionId))"
        case .status(let devId, let status): return "status(\(devId), \(status.held.count) held, \(status.tombstones.count) tombstones)"
        case .refused(let devId, _): return "refused(\(devId))"
        case .pairAgain(let devId): return "pairAgain(\(devId))"
        case .attemptFailed(let devId): return "attemptFailed(\(devId))"
        case let .deliver(sessionId, counter, innerType, body, draining):
            return "deliver(session: \(sessionId), counter: \(counter), innerType: \(innerType), bodyLength: \(body.count), draining: \(draining))"
        case let .closed(sessionId, reason): return "closed(\(sessionId), \(reason))"
        }
    }
}

/// Source of the random bytes the client uses outside CryptoKit: the
/// unauthenticated `client_nonce` of the HELLO2 shell.
public protocol RandomSource: AnyObject {
    func fill(_ buffer: inout [UInt8])
}

/// The system CSPRNG.
public final class SystemRandom: RandomSource {
    public init() {}

    public func fill(_ buffer: inout [UInt8]) {
        let count = buffer.count
        guard count > 0 else { return }
        let status = buffer.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, count, $0.baseAddress!) }
        precondition(status == errSecSuccess, "system random source failed")
    }
}

private final class Session {
    let sessionId: UInt32
    let sealer: Sealer
    let opener: Opener
    let established: Duration
    /// The last authenticated record from the host, once live.
    var lastRx: Duration
    let accept: Accept

    init(sessionId: UInt32, sealer: Sealer, opener: Opener, established: Duration, accept: Accept) {
        self.sessionId = sessionId
        self.sealer = sealer
        self.opener = opener
        self.established = established
        lastRx = established
        self.accept = accept
    }
}

private enum Attempt {
    case idle
    case connecting(initiator: Initiator, key: ClientKey, started: Duration, lastSent: Duration, staleRetried: Bool)
    case confirming(key: ClientKey, session: Session, started: Duration, lastSent: Duration)
    /// The session went live on a record; confirms continue until the ack.
    case acking(sessionId: UInt32, started: Duration, lastSent: Duration)
}

/// A confirming attempt the iPad stopped retrying. Nothing is sealed under
/// it; it can only be promoted by the host's answer, or reported refused.
private struct Abandoned {
    let key: ClientKey
    let session: Session
    let until: Duration
}

public final class Client {
    /// Records left in either direction before the client reconnects on
    /// its own.
    static let reconnectRecordMargin: UInt64 = 1 << 20
    /// Abandoned attempts kept at once: a new attempt starts at most every
    /// 2 s, so 11 s of retention holds at most 6.
    static let maxAbandoned = 6

    public let config: ClientConfig
    private let random: RandomSource
    private let hello: HelloPayload

    public private(set) var lastTimestamp: UInt64 = 0
    /// The live session's key, for reconnecting before its limits.
    private var key: ClientKey?
    private var attempt: Attempt = .idle
    private var live: Session?
    private var drain: (session: Session, since: Duration)?
    /// Attempts whose confirm window ran out, oldest first.
    private var abandoned: [Abandoned] = []
    /// The `dev_id` the app wants a session under: set by `connect`,
    /// cleared by `disconnect` and by a refusal of that key. An abandoned
    /// attempt is only ever promoted for it.
    private var intent: Id16?
    /// From a `TooNew` answer: the clock is used only up to this value.
    private var clockLimit: UInt64?
    /// Keys the host answered `PairAgain` for: no new attempt starts under
    /// them. Their live session, and an abandoned attempt the host may have
    /// committed, stay usable.
    private var noNewAttempts: [Id16] = []

    /// `hello` holds the session fields; its timestamp is set per attempt.
    public init(config: ClientConfig, hello: HelloPayload, random: RandomSource = SystemRandom()) {
        self.config = config
        self.hello = hello
        self.random = random
    }

    /// Restores the timestamp persisted per host. Values at or above
    /// `V3.maxTimestampUs` are ignored; it never moves backwards.
    public func setLastTimestamp(_ timestampUs: UInt64) {
        if timestampUs < V3.maxTimestampUs {
            lastTimestamp = max(lastTimestamp, timestampUs)
        }
    }

    /// The next message 1 timestamp: `max(clock, last + 1)`, using only
    /// values below `V3.maxTimestampUs`. An out-of-range clock, or one past
    /// a `TooNew` limit, is ignored. The sequence never restarts: once
    /// `last + 1` would reach the bound it stays at the last valid value,
    /// and the host answers `PairAgain`.
    func nextTimestamp(clockUs: UInt64) -> UInt64 {
        var clock: UInt64? = clockUs < V3.maxTimestampUs ? clockUs : nil
        if let limit = clockLimit, let c = clock, c > limit {
            clock = nil
        }
        let (sum, overflow) = lastTimestamp.addingReportingOverflow(1)
        let afterLast = !overflow && sum < V3.maxTimestampUs ? sum : V3.maxTimestampUs - 1
        return clock.map { max($0, afterLast) } ?? afterLast
    }

    public var abandonedCount: Int { abandoned.count }
    public var liveSession: UInt32? { live?.sessionId }
    public var drainingSession: UInt32? { drain?.session.sessionId }

    public var isConnecting: Bool {
        if case .connecting = attempt { return true }
        return false
    }

    public var isConfirming: Bool {
        if case .confirming = attempt { return true }
        return false
    }

    /// Starts an attempt with `key`. An attempt in progress ends the way
    /// every attempt ends (`abandonCurrent`), and a live session keeps
    /// working until the cutover. Abandoned attempts under keys other than
    /// `key` and the live session's key are dropped.
    public func connect(now: Duration, key: ClientKey, clockUs: UInt64) -> [ClientOutput] {
        var out: [ClientOutput] = []
        beginAttempt(now: now, key: key, clockUs: clockUs, staleRetried: false, &out)
        let liveKey = self.key?.devId
        abandoned.removeAll { $0.key.devId != key.devId && $0.key.devId != liveKey }
        return out
    }

    /// Ends the current attempt. A confirming attempt becomes abandoned:
    /// receive-only, because the host may still commit or refuse it. Any
    /// other attempt simply ends. Every path that ends or replaces an
    /// attempt goes through here.
    private func abandonCurrent() {
        let current = attempt
        attempt = .idle
        if case let .confirming(key, session, started, _) = current {
            if abandoned.count >= Self.maxAbandoned {
                abandoned.removeFirst()
            }
            abandoned.append(Abandoned(key: key, session: session, until: started + config.abandonedKeep))
        }
    }

    /// The one way an attempt starts.
    private func beginAttempt(now: Duration, key: ClientKey, clockUs: UInt64, staleRetried: Bool, _ out: inout [ClientOutput]) {
        abandonCurrent()
        intent = key.devId
        if noNewAttempts.contains(key.devId) {
            // The host said this key can never connect again.
            out.append(.pairAgain(devId: key.devId))
            return
        }
        startAttempt(now: now, key: key, clockUs: clockUs, staleRetried: staleRetried, &out)
    }

    /// An attempt got no usable answer. The intent falls back to the live
    /// session's key only while a live session exists.
    private func failAttempt(_ devId: Id16, _ out: inout [ClientOutput]) {
        attempt = .idle
        if live != nil, let liveKey = key {
            intent = liveKey.devId
        }
        out.append(.attemptFailed(devId: devId))
    }

    /// The host refused this key: stop using it. Ends an attempt under it,
    /// drops every abandoned attempt under it, and forgets it as the
    /// reconnect key.
    private func retireKey(_ devId: Id16) {
        switch attempt {
        case .connecting(_, let key, _, _, _), .confirming(let key, _, _, _):
            if key.devId == devId {
                attempt = .idle
            }
        default:
            break
        }
        abandoned.removeAll { $0.key.devId == devId }
        if key?.devId == devId {
            key = nil
        }
        if intent == devId {
            intent = key?.devId
        }
    }

    private func startAttempt(now: Duration, key: ClientKey, clockUs: UInt64, staleRetried: Bool, _ out: inout [ClientOutput]) {
        let timestamp = nextTimestamp(clockUs: clockUs)
        lastTimestamp = timestamp
        var nonce = [UInt8](repeating: 0, count: 4)
        random.fill(&nonce)
        var payload = hello
        payload.timestampUs = timestamp
        let initiator: Initiator
        do {
            initiator = try Initiator(key: key, hostId: config.hostId, clientNonce: le32(nonce, 0), payload: payload)
        } catch {
            // Message 1 always fits; the Rust side asserts it. Fail the
            // attempt rather than trap.
            failAttempt(key.devId, &out)
            return
        }
        out.append(.send(initiator.msg1))
        attempt = .connecting(initiator: initiator, key: key, started: now, lastSent: now, staleRetried: staleRetried)
    }

    public func handleDatagram(now: Duration, datagram: [UInt8], clockUs: UInt64) -> [ClientOutput] {
        var out: [ClientOutput] = []
        switch Wire.classify(datagram) {
        case .handshake2: handleMsg2(now: now, datagram: datagram, clockUs: clockUs, &out)
        case .sealed: handleRecord(now: now, datagram: datagram, &out)
        case .handshake1, .other: break
        }
        return out
    }

    private func handleMsg2(now: Duration, datagram: [UInt8], clockUs: UInt64, _ out: inout [ClientOutput]) {
        guard case let .connecting(initiator, key, _, _, staleRetried) = attempt else { return }
        guard let (reply, keys) = try? initiator.readMsg2(datagram) else { return }
        switch reply {
        case .stale(let stored):
            setLastTimestamp(stored)
            if staleRetried {
                failAttempt(key.devId, &out)
            } else {
                beginAttempt(now: now, key: key, clockUs: clockUs, staleRetried: true, &out)
            }
        case .tooNew(let limit):
            // Use timestamps at or below the host's limit from now on.
            clockLimit = limit
            lastTimestamp = min(lastTimestamp, limit > 0 ? limit - 1 : 0)
            if staleRetried {
                failAttempt(key.devId, &out)
            } else {
                beginAttempt(now: now, key: key, clockUs: clockUs, staleRetried: true, &out)
            }
        case .pairAgain:
            // Unlike a refusal, the record isn't retired: a session already
            // committed under this key stays valid. Only new attempts stop.
            attempt = .idle
            if !noNewAttempts.contains(key.devId) {
                noNewAttempts.append(key.devId)
            }
            out.append(.pairAgain(devId: key.devId))
        case .accept(let accept):
            guard let keys else { return }
            out.append(.releaseAllInput)
            let (sealer, opener) = keys.initiator(sessionId: accept.sessionId)
            if let confirm = Self.sealConfirm(sealer) {
                out.append(.send(confirm))
            }
            let session = Session(sessionId: accept.sessionId, sealer: sealer, opener: opener, established: now, accept: accept)
            attempt = .confirming(key: key, session: session, started: now, lastSent: now)
        }
    }

    /// What an authenticated record is to the state machine.
    private enum Content {
        case session(SessionMessage)
        /// A v2 message (inner type below 0x40), for the application.
        case application
    }

    /// Nil for a record the state machine doesn't act on: a session message
    /// that doesn't parse, or an inner type of 0x40 and up that it doesn't
    /// know. Such a record never promotes an attempt and is never
    /// delivered; on a live session it still counts for liveness, being
    /// genuine traffic under its keys. As the reference client does.
    private static func content(_ opened: Opened) -> Content? {
        switch Result(catching: { try SessionMessage.parse(innerType: opened.innerType, body: opened.body) }) {
        case .success(.some(let message)): return .session(message)
        case .success(.none): return V3.isApplication(opened.innerType) ? .application : nil
        case .failure: return nil
        }
    }

    private func handleRecord(now: Duration, datagram: [UInt8], _ out: inout [ClientOutput]) {
        guard let header = try? RecordHeader.parse(datagram) else { return }
        let sessionId = header.sessionId
        func deliver(_ opened: Opened, draining: Bool) {
            out.append(.deliver(sessionId: sessionId, counter: opened.counter, innerType: opened.innerType,
                                body: opened.body, draining: draining))
        }

        if case let .confirming(key, session, started, lastSent) = attempt, session.sessionId == sessionId {
            guard let opened = try? session.opener.open(datagram) else { return }
            let devId = key.devId
            switch Self.content(opened) {
            case .session(.confirmAck(let status)):
                promote(now: now, &out)
                attempt = .idle
                out.append(.status(devId: devId, status: status))
            case .session(.refused(let status)):
                retireKey(devId)
                out.append(.refused(devId: devId, status: status))
            case .application:
                promote(now: now, &out)
                attempt = .acking(sessionId: sessionId, started: started, lastSent: lastSent)
                deliver(opened, draining: false)
            case .session, nil:
                break
            }
            return
        }

        if let index = abandoned.firstIndex(where: { $0.session.sessionId == sessionId }) {
            guard let opened = try? abandoned[index].session.opener.open(datagram) else { return }
            let devId = abandoned[index].key.devId
            switch Self.content(opened) {
            case .session(.confirmAck(let status)):
                if promoteAbandoned(now: now, index: index, &out) {
                    out.append(.status(devId: devId, status: status))
                }
            case .session(.refused(let status)):
                retireKey(devId)
                out.append(.refused(devId: devId, status: status))
            case .application:
                guard promoteAbandoned(now: now, index: index, &out) else { return }
                deliver(opened, draining: false)
            case .session, nil:
                break
            }
            return
        }

        if let session = live, session.sessionId == sessionId {
            guard let opened = try? session.opener.open(datagram) else { return }
            // Any authenticated record shows the host still holds the keys.
            session.lastRx = now
            switch Self.content(opened) {
            case .application:
                deliver(opened, draining: false)
            case .session(.confirmAck(let status)):
                if case .acking(let acking, _, _) = attempt, acking == sessionId {
                    attempt = .idle
                }
                if let key {
                    out.append(.status(devId: key.devId, status: status))
                }
            case .session(.bye(let reason)):
                live = nil
                if case .acking = attempt {
                    attempt = .idle
                }
                out.append(.closed(sessionId: sessionId, reason: .hostBye(reason)))
            case .session, nil:
                break
            }
            return
        }

        if let session = drain?.session, session.sessionId == sessionId {
            guard let opened = try? session.opener.open(datagram) else { return }
            if case .application = Self.content(opened) {
                deliver(opened, draining: true)
            }
        }
        // Anything else is for keys this side no longer has: drop.
    }

    /// Makes the confirming session live.
    private func promote(now: Duration, _ out: inout [ClientOutput]) {
        guard case let .confirming(key, session, _, _) = attempt else { return }
        attempt = .idle
        // Every abandoned attempt is older than this one.
        abandoned.removeAll()
        goLive(now: now, session: session, key: key, &out)
    }

    /// The host answered an abandoned attempt: it committed it after all.
    /// Only for the key the app currently wants; older abandoned attempts
    /// are dropped, and an unanswered message 1 is no longer needed. Being
    /// a new cutover, it releases held input first. Returns whether it
    /// promoted.
    private func promoteAbandoned(now: Duration, index: Int, _ out: inout [ClientOutput]) -> Bool {
        guard intent == abandoned[index].key.devId else { return false }
        let promoted = abandoned.remove(at: index)
        abandoned.removeFirst(index)
        if isConnecting {
            attempt = .idle
        }
        out.append(.releaseAllInput)
        goLive(now: now, session: promoted.session, key: promoted.key, &out)
        return true
    }

    /// Makes `session` live and moves the old one to the drain. A drain
    /// still running ends at once: there is at most one.
    private func goLive(now: Duration, session: Session, key: ClientKey, _ out: inout [ClientOutput]) {
        if let old = drain?.session {
            drain = nil
            out.append(.closed(sessionId: old.sessionId, reason: .drainEnded))
        }
        if let old = live {
            live = nil
            drain = (old, now)
        }
        out.append(.live(sessionId: session.sessionId, accept: session.accept))
        session.lastRx = now
        live = session
        self.key = key
    }

    /// The new session's first IDR frame decoded: drop the old session.
    public func firstIdrDecoded(sessionId: UInt32) -> [ClientOutput] {
        var out: [ClientOutput] = []
        if liveSession == sessionId, let old = drain?.session {
            drain = nil
            out.append(.closed(sessionId: old.sessionId, reason: .drainEnded))
        }
        return out
    }

    /// Seals an application message under the live session. Nil without
    /// one, and for input while confirming: no input crosses the cutover.
    public func seal(now: Duration, innerType: UInt8, body: [UInt8]) -> [UInt8]? {
        if isConfirming && innerType == V2.inputEventType {
            return nil
        }
        guard let live else { return nil }
        if elapsed(now, since: live.established) >= config.maxAge {
            return nil
        }
        return try? live.sealer.seal(innerType: innerType, body: body)
    }

    /// Ends the live session with a sealed BYE. Abandoned attempts go too,
    /// so a late commit of one can't bring the session back.
    public func disconnect(reason: UInt8) -> [ClientOutput] {
        var out: [ClientOutput] = []
        abandonCurrent()
        abandoned.removeAll()
        intent = nil
        if let session = live {
            live = nil
            let bye = SessionMessage.bye(reason)
            if let record = try? session.sealer.seal(innerType: bye.innerType, body: bye.encodeBody()) {
                out.append(.send(record))
            }
            out.append(.closed(sessionId: session.sessionId, reason: .local))
        }
        return out
    }

    /// Runs the timers. Call it often; every 10-50 ms is plenty.
    public func poll(now: Duration, clockUs: UInt64) -> [ClientOutput] {
        var out: [ClientOutput] = []
        switch attempt {
        case .idle:
            break
        case let .connecting(initiator, key, started, lastSent, staleRetried):
            if elapsed(now, since: started) >= config.msg1Window {
                failAttempt(key.devId, &out)
            } else if elapsed(now, since: lastSent) >= config.msg1RetryInterval {
                attempt = .connecting(initiator: initiator, key: key, started: started, lastSent: now, staleRetried: staleRetried)
                out.append(.send(initiator.msg1))
            }
        case let .confirming(key, session, started, lastSent):
            if elapsed(now, since: started) >= config.confirmWindow {
                // Stop confirming and start over; `abandonCurrent` keeps
                // the attempt receive-only.
                beginAttempt(now: now, key: key, clockUs: clockUs, staleRetried: false, &out)
            } else if elapsed(now, since: lastSent) >= config.confirmRetryInterval {
                attempt = .confirming(key: key, session: session, started: started, lastSent: now)
                if let confirm = Self.sealConfirm(session.sealer) {
                    out.append(.send(confirm))
                }
            }
        case let .acking(sessionId, started, lastSent):
            if elapsed(now, since: started) >= config.confirmWindow {
                // The next HelloAck brings the status.
                attempt = .idle
            } else if elapsed(now, since: lastSent) >= config.confirmRetryInterval {
                attempt = .acking(sessionId: sessionId, started: started, lastSent: now)
                if let live, live.sessionId == sessionId, let confirm = Self.sealConfirm(live.sealer) {
                    out.append(.send(confirm))
                }
            }
        }

        abandoned.removeAll { now >= $0.until }

        if let old = drain, elapsed(now, since: old.since) >= config.drainLimit {
            out.append(.closed(sessionId: old.session.sessionId, reason: .drainEnded))
            drain = nil
        }

        if let session = live {
            let timeout = session.accept.livenessTimeoutMs == 0
                ? config.livenessTimeout
                : .milliseconds(Int(session.accept.livenessTimeoutMs))
            if elapsed(now, since: session.lastRx) >= timeout {
                // The host has ended the session, or the path is gone: stop
                // treating it as live, and reconnect if it's still wanted.
                live = nil
                out.append(.closed(sessionId: session.sessionId, reason: .liveness))
                if let wanted = key, intent == wanted.devId, case .idle = attempt {
                    beginAttempt(now: now, key: wanted, clockUs: clockUs, staleRetried: false, &out)
                }
                return out
            }
        }

        if let session = live {
            let age = elapsed(now, since: session.established)
            // Authenticated progress in each direction: sealed here, opened
            // from the host.
            let sent = session.sealer.sealed
            let received = session.opener.highest.map { $0 + 1 } ?? 0
            let used = max(sent, received)
            if age >= config.maxAge || used >= V3.maxRecordsPerDirection {
                out.append(.closed(sessionId: session.sessionId, reason: .expired))
                live = nil
            } else if case .idle = attempt,
                      age + config.reconnectMargin >= config.maxAge
                      || used + Self.reconnectRecordMargin >= V3.maxRecordsPerDirection {
                if let key {
                    beginAttempt(now: now, key: key, clockUs: clockUs, staleRetried: false, &out)
                }
            }
        }
        return out
    }

    private static func sealConfirm(_ sealer: Sealer) -> [UInt8]? {
        let confirm = SessionMessage.confirm
        return try? sealer.seal(innerType: confirm.innerType, body: confirm.encodeBody())
    }
}

/// `now - since`, or zero if `since` is later (Rust's `saturating_sub`).
func elapsed(_ now: Duration, since: Duration) -> Duration {
    now > since ? now - since : .zero
}
