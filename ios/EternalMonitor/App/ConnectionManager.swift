import Foundation
@preconcurrency import CoreVideo
import QuartzCore
import Combine
import os
import UIKit

enum ConnectionTarget {
    case udp(host: String, port: UInt16)
    case usb(FramedLink)
}

enum ConnectionState: Equatable {
    case disconnected
    case connecting
    case connected
}

enum DiagnosticLevel: String {
    case info = "INFO"
    case warning = "WARN"
    case error = "ERROR"
}

struct DiagnosticEntry: Identifiable {
    let id = UUID()
    let timestamp = Date()
    let level: DiagnosticLevel
    let category: String
    let message: String
}

struct ConnectionDebugState {
    var host = ""
    var port: UInt16 = 0
    var listenerReadyPort: UInt16?
    var helloAttempts = 0
    var datagramsReceived = 0
    var datagramBytesReceived = 0
    var assembledFrames = 0
    var assembledFrameBytes = 0
    var decodePackets = 0
    var decodedFrames = 0
    var lastDecodedTimestampUs: UInt64 = 0

    /// For the support log. `handshakeDone` says the PC accepted the session.
    func timeoutSummary(handshakeDone: Bool) -> String {
        if listenerReadyPort == nil {
            return "Receiver never became ready."
        }
        if helloAttempts == 0 {
            return "Listener came up, but HELLO registration never sent."
        }
        if datagramsReceived == 0 {
            if handshakeDone {
                return "Session accepted by \(host):\(port), but no video datagrams arrived."
            }
            return "HELLO sent \(helloAttempts)x to \(host):\(port), but no datagrams came back."
        }
        if assembledFrames == 0 {
            return "Received \(datagramsReceived) datagrams (\(datagramBytesReceived) bytes), but no complete frame was reassembled."
        }
        if decodePackets == 0 {
            return "Reassembled \(assembledFrames) frame payloads, but none could be parsed as FramePacket."
        }
        if decodedFrames == 0 {
            return "Parsed \(decodePackets) packets and assembled \(assembledFrames) payloads, but VideoToolbox produced no frames."
        }
        return "Received \(decodedFrames) decoded frames, but the connection did not transition cleanly."
    }

    /// The timeout line's level. A HELLO no one answered is a warning (a PC
    /// that is off, asleep or elsewhere) unless the PC at this address
    /// accepted a session earlier in this launch.
    func timeoutLevel(handshakeDone: Bool, answeredHereBefore: Bool) -> DiagnosticLevel {
        let unanswered = listenerReadyPort != nil && helloAttempts > 0 && datagramsReceived == 0 && !handshakeDone
        return unanswered && !answeredHereBefore ? .warning : .error
    }
}

@MainActor
final class ConnectionManager: ObservableObject {
    @Published var state: ConnectionState = .disconnected
    @Published var fps: Double = 0
    @Published var transportMode: String = "WiFi"
    @Published var connectionError: String?
    /// A connection problem in plain words, with its next action. Shown in
    /// place of `connectionError` when set.
    @Published var problem: ConnectionProblem?
    /// The link failure classified for the current attempt. A link's close
    /// callback that follows its own failure reports this, not a removed cable.
    private var linkFailure: ConnectionProblem?
    /// Set with `connectionError` when the PC and iPad versions differ.
    @Published var versionMismatch: VersionMismatch?
    @Published var pairingPrompt: PairingSheetModel?

    /// HELLO2 feature bits. Every build says it checks for its own updates,
    /// so the PC tells only older iPad apps to update, and that it
    /// understands why a PC isn't streaming. It draws the PC's pointer
    /// itself, so pointer motion needs no video frame.
    nonisolated static func featureCaps(wantsInput: Bool, wantsAudio: Bool, isUSB: Bool) -> UInt16 {
        (wantsInput ? Hello2.featureWantsInput : 0) | (wantsAudio ? Hello2.featureWantsAudio : 0)
            | (isUSB ? 0 : Hello2.featureSupportsNack | Hello2.featureMediaSize) | Hello2.featureChecksUpdates
            | Hello2.featureStandbyStatus | (E2E.drawsPointer ? Hello2.featureDrawsPointer : 0)
    }
    private var awaitingPairing = false
    /// The PC answered that it isn't streaming; the attempt waits for it.
    private var waitingForStream = false
    private var explicitPairingToken: Data?
    private let pairingStore: PairingStore
    @Published private(set) var diagnostics: [DiagnosticEntry] = []
    /// Live stream health for the HUD, recomputed at the 4 Hz flush.
    @Published private(set) var stats = StreamStats()
    /// Host heartbeats/frames stopped arriving; the frozen frame is stale.
    @Published private(set) var signalLost = false
    /// Frames still arrive, but the picture has been held for over a second
    /// waiting for the keyframe that replaces a lost frame.
    @Published var pictureRestoring = false
    /// The PC is locked or shows a permission prompt, so it can't capture and
    /// the picture is held. Its heartbeats say so until capture works again.
    @Published var pcLocked = false
    /// The PC has no display turned on, so there is nothing to capture and
    /// the picture is held. Its heartbeats say so until one comes back.
    @Published var pcNoDisplay = false
    /// Nonzero while an automatic reconnect cycle is running.
    @Published private(set) var reconnectAttempt = 0

    /// Settled on the Connect screen: no attempt under way, no reconnect
    /// cycle waiting between attempts, and no problem card or error. An
    /// error such as a version mismatch says what to do, so it stays in view.
    var isIdleOnConnect: Bool {
        state == .disconnected && problem == nil && connectionError == nil && reconnectAttempt == 0
    }
    /// Decoded video dimensions in pixels — the touch relay maps gestures
    /// through the same aspect-fit rect the renderer draws.
    @Published private(set) var videoSize: CGSize = .zero
    /// Whether this session asked the host to relay input (the Settings
    /// toggle, latched at HELLO2 time — mid-session flips need a reconnect).
    @Published private(set) var sessionWantsInput = false
    @Published private(set) var sessionDrawingMode = false
    @Published private(set) var sessionWantsAudio = false
    @Published private(set) var audioStats = AudioStats()
    /// The host's HELLO_ACK identity (name, negotiated timing) for Settings.
    @Published private(set) var hostInfo: ControlChannel.SessionInfo?
    /// The one-time feedback card's progress, kept in UserDefaults.
    @Published private(set) var feedbackAsk = FeedbackAsk()
    /// When the current session's first frame arrived.
    private var streamingSince: Date?
    /// The host's live stream parameters — from the ACK, then refreshed by
    /// every heartbeat (so ABR bitrate changes and codec switches show up).
    @Published private(set) var hostStreamConfig: StreamConfig?

    @Published private(set) var usbStatus = "USB: waiting for a cable"
    /// A PC left the USB capability out of its HELLO_ACK: when it was
    /// sampled, that PC couldn't reach Apple's USB service. Kept only for
    /// the PC at that target, and only until `until`; see `USBStatusText`.
    struct AppleDevicesSighting: Equatable {
        static let lifetime: TimeInterval = 30 * 60
        /// "host:port" of the PC over Wi-Fi, or "USB".
        let target: String
        let at: Date
        let until: Date

        func applies(now: Date) -> Bool { now >= at && now < until }
    }
    /// Cleared when it expires, so an idle Connect screen drops the hint too.
    @Published private(set) var appleDevicesUnreachable: AppleDevicesSighting?
    private var appleDevicesExpiry: Task<Void, Never>?
    /// The wall clock for the Apple Devices sighting. Tests replace it.
    var now: () -> Date = { Date() }
    /// Hears, from each accepted HELLO_ACK, whether the PC is older than 0.3.4.
    var oldPCPrompt = OldPCPrompt.shared
    private var mediaLink: (any MediaLink)?
    private var usbListener: USBListener?
    private var isForeground = true
    private var usbPaused = false
    /// The cable stopped after attempts without a picture. Unlike a manual
    /// Disconnect, coming back to the app keeps it stopped until Try again
    /// or Connect USB.
    private var usbStoppedForNoPicture = false
    /// The PC turned the cable down: it isn't answered until the hold ends
    /// (see `CableHold`). Busy refusals in a row set the wait.
    private var cableHold: CableHold?
    private var cableBusyInARow = 0
    private var cableHoldTask: Task<Void, Never>?
    private let currentAttempt = OSAllocatedUnfairLock<UUID?>(initialState: nil)
    private var frameAssembler: FrameAssembler?
    private var controlChannel: ControlChannel?
    private(set) var videoDecoder: VideoDecoder?
    private var audioPlayer: AudioPlayer?
    private var audioPreference: Bool?
    private var timeoutTask: Task<Void, Never>?
    private var statsFlushTask: Task<Void, Never>?
    /// Hot-path counters written on the link queue and drained at 4 Hz — the
    /// old per-datagram MainActor Task (~2 per datagram, ~2,800 hops/s at
    /// 1080p60) throttled the whole receive path.
    private let streamCounters = StreamCounters()
    private(set) var debugState = ConnectionDebugState()
    // Whether we've already granted the one-time timeout extension that fires when datagrams
    // start arriving (so a slow/jittery network gets a fresh window to finish reassembly+decode).
    private var didExtendTimeout = false
    /// No-picture timeouts in a row at one PC; at the limit, attempts that
    /// start by themselves stop.
    private var noPictureStreak = NoPictureStreak()
    /// "host:port" of each PC that accepted a session in this launch.
    private var acceptedAddresses = Set<String>()
    /// System uptime when the current attempt started, and when its PC
    /// accepted the session, for the support log's timings.
    private var attemptStartedAt: TimeInterval = 0
    private var sessionAcceptedAt: TimeInterval?
    private var firstFrameAt: TimeInterval?
    /// When the HELLO2 the next summary describes was started: the attempt's
    /// start, or the latest pairing code's.
    private var summaryStartedAt: TimeInterval = 0
    /// The latest nonces that have their summary.
    private var summarizedNonces: [UInt32] = []

    let frameSlot = FrameSlot()
    /// The PC's pointer, drawn by `MetalView` when the PC sends it apart.
    let pointerOverlay = PointerOverlay()
    /// Lets hot-path closures reach the CURRENT control channel without
    /// retaining self (rebuilt each connect).
    private let controlChannelBox = ControlChannelBox()
    private var fpsCounter = FPSCounter()
    // Frame measurements are published with the other HUD stats at 4 Hz.
    private var lagMs: Double = 0
    private var lastTarget: (host: String, port: UInt16)?
    private var livenessTimeoutUs: UInt64 = 3_000_000
    private var degradedSinceUs: UInt64?
    private var reconnectTask: Task<Void, Never>?
    /// The automatic reconnect cycle allowed to write state. A cycle checks
    /// this after every wait and stops once a newer cycle, or a connection
    /// the user or a cable started, has replaced it.
    private var reconnectCycleID: UUID?
    /// The classified failure the current cycle shows if every attempt fails.
    private var reconnectFailure: ConnectionProblem?
    private var prevCounters = FrameAssembler.Counters()
    /// True when backgrounding ended a live session — foregrounding may resume it.
    private var resumeOnForeground = false
    private var backgroundGoodbyeTask: UIBackgroundTaskIdentifier = .invalid

    static let connectionTimeoutSeconds: UInt64 = 10

    /// Connect and reconnect timing. The app uses the defaults, with the
    /// first-picture wait the update manifest may set (10–20 s); tests pass
    /// shorter ones when they create the manager, and may shorten them once
    /// a session streams, so a slow machine's first connect isn't cut off.
    struct Timing {
        var connectTimeoutSeconds = Double(RemoteConfig.launch.firstFrameTimeoutSeconds
            ?? Int(ConnectionManager.connectionTimeoutSeconds))
        var reconnectGraceSeconds = 2.0
        var reconnectBackoffSeconds: (Int) -> Double = { attempt in Double(min(1 << (attempt - 1), 15)) }
        var appleDevicesNoteSeconds = AppleDevicesSighting.lifetime
        /// How long video the PC says it sent may go missing before the
        /// next no-video step (see `NoVideoFallback`).
        var noVideoStepSeconds = 1.5
        /// How long the cable goes unanswered after the given number of
        /// Busy refusals in a row (see `CableHold`).
        var cableBusyHoldSeconds: (Int) -> Double = CableHold.busySeconds
        /// Waits out a cable hold. Tests replace it to end the wait when
        /// they choose.
        var cableHoldSleep: (Double) async -> Void = { try? await Task.sleep(for: .seconds($0)) }
    }
    var timing: Timing
    /// The Wi-Fi link to a host's port. Tests pass a fake one, or replace it
    /// to fail a lookup without asking DNS.
    var makeUDPLink: (UInt16) -> any MediaLink
    /// Looks for PCs with Bonjour, given the service name wanted. Tests
    /// replace it.
    var findPCs: (String) async -> [DiscoveredHost] = { await MovedPC.find($0) }
    /// Which addresses a saved PC may move from and to. Tests on loopback
    /// widen it.
    var homeAddress: (String) -> Bool = MovedPC.isHomeAddress
    /// How this iPad reaches an address. Tests replace it.
    var localRoute: (String, UInt16) async -> MovedPC.Route? = { await MovedPC.route(to: $0, port: $1) }
    /// Where the iPad sends to reach a PC address it was given. Tests on
    /// loopback point the addresses a fake signs at other fakes.
    var dialAddress: (String, UInt16) -> (host: String, port: UInt16) = { ($0, $1) }
    /// "host:port" of each address picked from Bonjour's list in this
    /// launch. Saved entries keep the mark once they show a picture.
    private var foundAddresses = Set<String>()
    /// A saved PC that didn't answer at its address and showed it is the
    /// same PC at another.
    private struct Move {
        let from: (host: String, port: UInt16)
        let name: String
        /// Its pairing token, which the PC proved it holds.
        let token: Data
        /// The PC's video didn't arrive at `from`; it still answers there.
        var noVideo = false
    }
    /// The move the next attempt makes, and the one the current attempt made.
    private var pendingMove: Move?
    private var attemptMove: Move?
    /// The PC's video didn't arrive at the address asked for before, and did
    /// at another: the next attempt goes there first, and back to the address
    /// asked for (kept here) if the PC doesn't answer there.
    private var pendingRedirect: (host: String, port: UInt16)?
    private var attemptRedirect: (host: String, port: UInt16)?
    /// The proof the PC at a remembered address is checked for, before any
    /// attempt starts (see `connectViaRemembered`).
    private var redirectCheck: UUID?
    /// The current session's no-video steps, with a PC that takes them, and
    /// the step whose result isn't known yet, as its log line names it.
    private var noVideoFallback: NoVideoFallback?
    private var noVideoStep: String?

    init(timing: Timing = Timing(), udpReceiver: @escaping (UInt16) -> any MediaLink = { UDPReceiver(port: $0) },
         pairingStore: PairingStore? = nil) {
        self.timing = timing
        self.makeUDPLink = udpReceiver
        self.pairingStore = pairingStore ?? .shared
    }

    func refreshUSBAvailability() {
        let allowed = UserDefaults.standard.flag(forKey: "allowUSB")
        guard allowed, isForeground, !usbPaused else {
            usbListener?.stop()
            usbListener = nil
            usbStatus = allowed ? "USB: disconnected" : "USB: off"
            if !allowed, mediaLink?.isUSB == true { beginReconnect(.usbTurnedOff) }
            return
        }
        guard usbListener == nil else { return }
        let listener = USBListener(port: E2E.usbListenPort)
        usbListener = listener
        listener.onState = { [weak self, weak listener] state in
            Task { @MainActor in
                guard let self, let listener, self.usbListener === listener else { return }
                if self.mediaLink?.isUSB == true { return }
                switch state {
                case .listening: self.usbStatus = "USB: waiting for a cable"
                case .failed(let message): self.usbStatus = "USB: unavailable"; self.record(.warning, "usb", message)
                case .stopped: self.usbStatus = "USB: disconnected"
                }
            }
        }
        listener.onAccepted = { [weak self, weak listener] link in
            Task { @MainActor in
                guard let self, let listener, self.usbListener === listener else { link.stop(); return }
                self.acceptUSBLink(link)
            }
        }
        listener.start()
    }

    /// A cable the listener accepted. It takes over from Wi-Fi, including an
    /// automatic reconnect cycle and any attempt that cycle has running.
    func acceptUSBLink(_ link: FramedLink) {
        guard isForeground, !usbPaused, mediaLink?.isUSB != true else { link.stop(); return }
        endReconnectCycle()
        if state != .disconnected {
            E2E.linkSwitch(from: "udp", to: "usb")
            disconnect(.superseded)
        }
        connect(target: .usb(link))
    }

    func resumeUSBConnections() {
        noPictureStreak.restartCount()
        usbStoppedForNoPicture = false
        endCableHold()
        cableBusyInARow = 0
        usbPaused = false
        refreshUSBAvailability()
    }

    /// The PC turned this iPad down over the cable. Stop answering the cable
    /// for the hold's wait, or until the user acts after a version refusal,
    /// instead of taking the PC's next dial a second later.
    private func holdCable(_ reason: CableHold.Reason) {
        var wait: Double?
        if reason == .busy {
            cableBusyInARow += 1
            wait = timing.cableBusyHoldSeconds(cableBusyInARow)
        }
        let hold = CableHold(reason: reason, until: wait.map { now().addingTimeInterval($0) })
        endCableHold()
        cableHold = hold
        record(.info, "usb", "The PC turned the cable down (\(reason)); " + (wait.map { "answering it again in \(Int($0.rounded())) s" }
            ?? "answering it again after Try again or Connect USB"))
        usbPaused = true
        usbListener?.stop()
        usbListener = nil
        usbStatus = "USB: disconnected"
        guard let wait else { return }
        let sleep = timing.cableHoldSleep
        cableHoldTask = Task { @MainActor [weak self] in
            await sleep(wait)
            guard let self, !Task.isCancelled, self.cableHold == hold else { return }
            self.endCableHold()
            self.usbPaused = self.usbStoppedForNoPicture
            self.refreshUSBAvailability()
        }
    }

    private func endCableHold() {
        cableHoldTask?.cancel()
        cableHoldTask = nil
        cableHold = nil
    }

    // MARK: - Frame slot (thread-safe single-slot)

    var latestFrame: CVPixelBuffer? {
        frameSlot.take()
    }

    // MARK: - Input relay

    /// Relay one input event to the host (hot path — the channel hops to its
    /// own queue; dropped silently when no session is up).
    private var inputSequence = InputEventSequencer()
    var sessionHasKeyboard: Bool {
        sessionWantsInput && (hostInfo?.hostCaps ?? 0) & HelloAck.hostCapKeyboard != 0
    }
    var sessionHasPen: Bool {
        sessionWantsInput && (hostInfo?.hostCaps ?? 0) & HelloAck.hostCapPen != 0
    }

    func sendInputs(_ events: [WireInputEvent]) {
        guard sessionWantsInput else { return }
        controlChannelBox.value?.sendInputs(inputSequence.packets(events,
            timeUs: ControlChannel.clientNowUs(), reliable: mediaLink?.isUSB == true))
    }

    // MARK: - Connect / Disconnect

    /// `found`: the address is the one Bonjour listed for the PC, so the PC
    /// may be looked up by name again if the address changes.
    func connect(host: String, port: UInt16, token: Data? = nil, found: Bool = false) {
        guard state == .disconnected else { return }
        explicitPairingToken = token
        if found { foundAddresses.insert(PairingStore.address(host: host, port: port)) }
        if let token {
            do { try pairingStore.save(token: token, hostName: "", address: PairingStore.address(host: host, port: port)) }
            catch { record(.warning, "pairing", "Could not save the scanned pairing on this iPad.") }
        } else if let remembered = rememberedAddress(host: host, port: port) {
            connectViaRemembered(from: (host.trimmingCharacters(in: .whitespacesAndNewlines), port), remembered)
            return
        }
        connect(target: .udp(host: host, port: port))
    }

    /// Where the PC paired at `host:port` showed video after none arrived
    /// there, kept for this pairing, with the pairing and the PC's name.
    private func rememberedAddress(host: String, port: UInt16) -> (to: NoVideoPaths.Address, token: Data, name: String)? {
        let host = host.trimmingCharacters(in: .whitespacesAndNewlines)
        let address = PairingStore.address(host: host, port: port)
        guard let token = (try? pairingStore.token(address: address)) ?? nil,
              let to = NoVideoPaths.better(than: host, port: port, pairing: token),
              let name = pairingStore.pcName(address: address) ?? RecentConnectionStore.shared.entry(host: host, port: port)?.name
        else { return nil }
        return (to, token, name)
    }

    /// Connecting to `from`, where the PC's video didn't arrive before, goes
    /// to the address where it did, once that address is on this iPad's own
    /// network and the PC there shows it holds this pairing and receives
    /// there: the address may belong to another device now. Only then does a
    /// HELLO2 with the token go there, as a move, with the same-PC checks of
    /// one. Otherwise the entry is forgotten and the attempt goes to `from`,
    /// as it does if that move fails (see `leaveRememberedAddress`).
    private func connectViaRemembered(from: (host: String, port: UInt16),
                                      _ remembered: (to: NoVideoPaths.Address, token: Data, name: String)) {
        endReconnectCycle()
        // Try again and a resume go to the address tapped.
        lastTarget = from
        let check = UUID()
        redirectCheck = check
        connectionError = nil
        problem = nil
        state = .connecting
        Task { @MainActor [weak self] in
            guard let self else { return }
            let to = remembered.to
            let toFrom = { [weak self] (result: String) in
                guard let self else { return }
                self.redirectCheck = nil
                self.state = .disconnected
                NoVideoPaths.forget(from: .init(host: from.host, port: from.port))
                self.startAttempt(.udp(host: from.host, port: from.port))
                self.record(.info, "connection", "no_video_fallback step=remembered result=\(result)")
            }
            let route = await self.localRoute(to.host, to.port)
            guard self.redirectCheck == check else { return }
            guard let route, MovedPC.isLocal(to.host, over: route) else { return toFrom("not_local") }
            let same = await MovedPC.confirm(host: to.host, port: to.port, token: remembered.token,
                                             link: self.makeUDPLink(to.port))
            guard self.redirectCheck == check else { return }
            guard same else { return toFrom("not_confirmed") }
            self.redirectCheck = nil
            self.state = .disconnected
            self.pendingMove = Move(from: from, name: remembered.name, token: remembered.token, noVideo: true)
            self.pendingRedirect = from
            self.explicitPairingToken = remembered.token
            self.startAttempt(.udp(host: to.host, port: to.port))
            self.pendingRedirect = nil
        }
    }

    /// The attempt at a remembered address (see `connectViaRemembered`)
    /// passed its proof and failed after it: it ends with `end`, the entry is
    /// forgotten, and a new attempt goes to the address tapped. False when
    /// this attempt didn't go to a remembered address.
    private func leaveRememberedAddress(_ end: AttemptEnd, result: String) -> Bool {
        guard let from = attemptRedirect else { return false }
        NoVideoPaths.forget(from: .init(host: from.host, port: from.port))
        record(.info, "connection", "no_video_fallback step=remembered result=\(result)")
        disconnect(end)
        startAttempt(.udp(host: from.host, port: from.port))
        return true
    }

    func submitPairing() {
        guard awaitingPairing, let code = pairingPrompt?.submit() else { return }
        // The code goes in a new HELLO2. The refused one ends here, as on the
        // PC, which wrote a line for its nonce.
        recordSummary(.retriedWithCode)
        summaryStartedAt = ProcessInfo.processInfo.systemUptime
        controlChannel?.retryPairing(code: code)
    }

    func pairingSheetDismissed() {
        if awaitingPairing { cancel() }
    }

    func forgetPairedHosts() {
        do { try pairingStore.forgetAll() }
        catch { connectionError = "Could not remove saved pairings from this iPad." }
        NoVideoPaths.forgetAll()
    }

    /// A connection the user or a cable starts. It replaces any automatic
    /// reconnect cycle, so that cycle's count and kept failure end with it.
    func connect(target: ConnectionTarget) {
        guard state == .disconnected else { return }
        if case .udp(let host, let port) = target,
           host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || port == 0 { return }
        endReconnectCycle()
        if case .udp(let host, _) = target, let typed = ConnectionProblem.typedAddress(host) {
            connectionError = nil
            problem = typed
            record(.warning, "connection", "Not connecting: the typed address looks like the pairing code")
            return
        }
        startAttempt(target)
    }

    /// Starts one attempt. The reconnect cycle calls this directly, so its
    /// own attempts don't end it.
    private func startAttempt(_ target: ConnectionTarget) {
        guard state == .disconnected else { return }
        let normalizedHost: String
        let port: UInt16
        let receiver: any MediaLink
        switch target {
        case .udp(let host, let targetPort):
            normalizedHost = host.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !normalizedHost.isEmpty, targetPort != 0 else { return }
            SupportDiagnostics.noteTypedAddress(normalizedHost)
            port = targetPort
            receiver = makeUDPLink(port)
            // A moved attempt becomes the target to retry only once the PC
            // there is the saved one (see onSessionEstablished).
            if pendingMove == nil { lastTarget = (normalizedHost, port) }
        case .usb(let link):
            normalizedHost = "USB"
            port = 9877
            receiver = link
            if lastTarget == nil, let host = UserDefaults.standard.string(forKey: "lastHost"), !host.isEmpty {
                let savedPort = UserDefaults.standard.integer(forKey: "lastPort")
                lastTarget = (host, UInt16(exactly: savedPort).flatMap { $0 == 0 ? nil : $0 } ?? 9876)
            }
        }
        let isUSB = receiver.isUSB
        let pairingAddress = isUSB ? "USB" : PairingStore.address(host: normalizedHost, port: port)
        let pcTarget = isUSB ? "USB" : "\(normalizedHost):\(port)"
        // What one PC said about Apple Devices says nothing about another.
        if appleDevicesUnreachable?.target != pcTarget { keepAppleDevicesSighting(nil) }
        let pairingToken = explicitPairingToken ?? ((try? pairingStore.token(address: pairingAddress)) ?? Data(repeating: 0, count: 16))
        explicitPairingToken = nil
        attemptMove = isUSB ? nil : pendingMove
        pendingMove = nil
        attemptRedirect = isUSB ? nil : pendingRedirect
        pendingRedirect = nil
        noVideoFallback = nil
        noVideoStep = nil
        let connectionID = UUID()
        currentAttempt.withLock { $0 = connectionID }
        transportMode = isUSB ? "USB" : "WiFi"
        if isUSB { usbStatus = "USB: connecting" }
        fpsCounter = FPSCounter()

        state = .connecting
        connectionError = nil
        problem = nil
        linkFailure = nil
        versionMismatch = nil
        signalLost = false
        pcLocked = false
        pcNoDisplay = false
        degradedSinceUs = nil
        prevCounters = FrameAssembler.Counters()
        diagnostics.removeAll()
        didExtendTimeout = false
        waitingForStream = false
        attemptStartedAt = ProcessInfo.processInfo.systemUptime
        sessionAcceptedAt = nil
        firstFrameAt = nil
        summaryStartedAt = attemptStartedAt
        videoSize = .zero
        let drawing = DrawingProfile(enabled: UserDefaults.standard.bool(forKey: "drawingMode"), isUSB: isUSB)
        sessionDrawingMode = drawing.enabled
        let wantsInput = drawing.enabled || UserDefaults.standard.flag(forKey: "controlPC")
        sessionWantsInput = wantsInput
        let wantsAudio = !drawing.enabled && (audioPreference ?? UserDefaults.standard.flag(forKey: "playPCaudio"))
        sessionWantsAudio = wantsAudio
        audioStats = AudioStats()
        debugState = ConnectionDebugState(host: normalizedHost, port: port)
        record(.info, "connection", "Starting connection attempt=\(connectionID) transport=\(transportMode) to \(normalizedHost):\(port) drawing=\(drawing.enabled) input=\(wantsInput) audio=\(wantsAudio)")
        if attemptMove != nil {
            record(.info, "connection", attemptMove?.noVideo == true
                ? "Trying the PC at another of its addresses: its video didn't arrive at the first"
                : "Trying the saved PC at the new address Bonjour gave")
        }
        if attemptRedirect != nil { record(.info, "connection", "no_video_fallback step=remembered result=tried") }

        let decoder = VideoDecoder()
        let metrics = ReceiverMetrics()
        let assembler = FrameAssembler()
        let audio = wantsAudio ? AudioPlayer() : nil
        audio?.onError = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.warning, "audio", message)
            }
        }
        receiver.onAudioPacket = { [weak self, weak audio] header, opus in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            audio?.receive(header: header, opus: opus)
        }

        assembler.onDiagnostic = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.warning, "assembly", message)
            }
        }
        assembler.onInfo = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.info, "assembly", message)
            }
        }

        decoder.onFrameDecoded = { [weak self] pixelBuffer, timestampUs, sequence in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            // Store frame in lock-protected slot (no Sendable boundary crossed).
            // A frame that came out after a newer one is never shown.
            guard self.frameSlot.set(pixelBuffer, sequence: sequence) else {
                Logger(subsystem: "com.eternal.monitor", category: "decode").debug("Refused frame \(sequence) output after a newer one")
                return
            }
            // Capture only the scalars we need — avoids sending CVPixelBuffer across boundary
            let ts = timestampUs
            metrics.recordDecoded(at: ControlChannel.clientNowUs(), captureUs: ts)
            let frameWidth = CVPixelBufferGetWidth(pixelBuffer)
            let frameHeight = CVPixelBufferGetHeight(pixelBuffer)
            Task { @MainActor in
                guard self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.debugState.decodedFrames += 1
                self.debugState.lastDecodedTimestampUs = ts
                if self.debugState.decodedFrames == 1 {
                    self.firstFrameAt = ProcessInfo.processInfo.systemUptime
                    self.record(.info, "decode", "First frame decoded successfully" + self.attemptTimings)
                    self.endNoVideoStep("video")
                    self.noVideoFallback = nil
                    self.timeoutTask?.cancel()
                    self.noPictureStreak = NoPictureStreak()
                    self.state = .connected
                    self.streamingSince = Date()
                    // A reconnect attempt that streams ends its cycle.
                    self.endReconnectCycle()
                    self.transportMode = isUSB ? "USB" : "WiFi"
                    // Streaming is a passive activity — keep the iPad from
                    // dimming mid-session unless the user opted out.
                    UIApplication.shared.isIdleTimerDisabled =
                        UserDefaults.standard.flag(forKey: "keepScreenAwake")
                    E2E.firstFrame(width: frameWidth, height: frameHeight, link: isUSB ? "usb" : "udp")
                    var saved = true
                    if let move = self.attemptMove {
                        saved = self.savedMove(move, to: (normalizedHost, port), pairingAddress: pairingAddress)
                    } else {
                        RecentConnectionStore.shared.add(host: isUSB ? (self.hostInfo?.hostName ?? "USB host") : normalizedHost,
                            port: port, isUSB: isUSB, name: self.hostInfo?.hostName,
                            found: self.foundAddresses.contains(pairingAddress))
                    }
                    if !isUSB && saved {
                        UserDefaults.standard.set(normalizedHost, forKey: "lastHost")
                        UserDefaults.standard.set(Int(port), forKey: "lastPort")
                    }
                }
                self.fpsCounter.tick()
                if E2E.enabled && self.debugState.decodedFrames % 60 == 0 {
                    E2E.stats(
                        decoded: self.debugState.decodedFrames,
                        width: frameWidth,
                        height: frameHeight,
                        fps: self.fpsCounter.currentFPS,
                        counters: self.frameAssembler?.counters.withLock { $0 } ?? .init(),
                        decodeDepth: self.videoDecoder?.decodeDepth ?? 0
                    )
                }
                if self.signalLost { self.signalLost = false }
                self.degradedSinceUs = nil
                if Int(self.videoSize.width) != frameWidth || Int(self.videoSize.height) != frameHeight {
                    self.videoSize = CGSize(width: frameWidth, height: frameHeight)
                }
                if let offset = self.controlChannelBox.value?.clockSnapshot.withLock({ $0.offsetUs }),
                   let lag = Self.latencyMs(captureUs: ts, offsetUs: offset, nowUs: ControlChannel.clientNowUs()) {
                    self.lagMs = lag
                }
            }
        }

        decoder.onNeedsKeyframe = { [weak self, weak controlChannelBox] in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            controlChannelBox?.value?.sendKeyframeRequest(
                streamEpoch: 0, lastCompleteSeq: 0, reason: .decodeError
            )
        }
        // A picture held 2 s: the strongest request short of reconnecting.
        decoder.onRestartStream = { [weak self, weak controlChannelBox] in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            controlChannelBox?.value?.sendKeyframeRequest(streamEpoch: 0, lastCompleteSeq: 0, reason: .resume)
        }
        // Held 5 s: reconnect, as for a PC that stopped responding.
        decoder.onStalled = { [weak self] in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }), self.state == .connected else { return }
                self.record(.warning, "decode", "Picture held 5 s without a keyframe — reconnecting")
                self.beginReconnect(.pcStoppedResponding, end: .pictureStalled)
            }
        }

        decoder.onEvent = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.info, "decode", message)
            }
        }
        decoder.onFailure = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else {
                    // A late failure of an ended attempt still reaches the support log.
                    SupportDiagnostics.shared.record("error", "decode", message)
                    return
                }
                self.record(.error, "decode", message)
            }
        }

        assembler.onFrameAssembled = { [weak self, weak decoder, weak assembler] data, seq, captureTsUs, isKeyframe in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            self.streamCounters.recordAssembled(bytes: data.count)
            self.streamCounters.recordParsed()
            decoder?.decode(packet: FramePacket(
                seq: seq,
                timestampUs: captureTsUs,
                data: data,
                width: 0,
                height: 0,
                isKeyframe: isKeyframe,
                // The reassembler only accepts the current epoch's fragments.
                streamEpoch: assembler?.counters.withLock { $0.streamEpoch } ?? 0
            ))
        }

        receiver.assembler = assembler

        let channel = ControlChannel(queue: receiver.controlQueue, send: { [weak receiver] data in
            receiver?.send(data)
        })
        controlChannelBox.value = channel
        pointerOverlay.reset()
        channel.pointer = pointerOverlay
        assembler.rttUs = { [weak channel] in channel?.clockSnapshot.withLock { $0.rttUs } ?? 10_000 }
        assembler.onNack = { [weak channel] nack in channel?.sendNack(nack) }
        assembler.onNeedsKeyframe = { [weak channel] epoch, seq in
            channel?.sendKeyframeRequest(streamEpoch: epoch, lastCompleteSeq: seq, reason: .gapLoss)
        }
        // Video arrives but the first frame never completes: ask for a fresh
        // keyframe. The PC's packet size is its own setting; no message
        // asks it for smaller datagrams.
        assembler.onNoFrameYet = { [weak self, weak channel] epoch, missing in
            channel?.sendKeyframeRequest(streamEpoch: epoch, lastCompleteSeq: 0, reason: .startup)
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.warning, "assembly", "Video arrives but no frame completed in 2 s; asked the PC for a fresh keyframe: \(missing)")
            }
        }
        channel.reportProvider = { [weak assembler, weak channel, weak decoder, weak audio] in
            var report = metrics.makeReport(assembler: assembler?.counters.withLock { $0 } ?? .init(),
                decodeDepth: decoder?.decodeDepth ?? 0,
                clock: channel?.clockSnapshot.withLock { $0 } ?? .init(),
                now: ControlChannel.clientNowUs())
            let stats = audio?.stats ?? AudioStats()
            report.audioPacketsLost = UInt32(clamping: stats.lost)
            report.audioBufferMs = UInt16(clamping: stats.bufferMs)
            report.pointerShapeId = channel?.pointer?.completeShapeId ?? 0
            return report
        }
        channel.onHelloAttempt = { [weak self] attempt, total in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.debugState.helloAttempts = attempt
                self.record(.info, "ctrl", "HELLO2 attempt \(attempt)/\(total) to \(normalizedHost):\(port)")
            }
        }
        channel.onDiagnostic = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.info, "ctrl", message)
            }
        }
        channel.onSessionEstablished = { [weak self, weak receiver, weak assembler, weak decoder] info in
            assembler?.repairEnabled = !isUSB && info.hostCaps & HelloAck.hostCapNack != 0
            decoder?.setFrameSeqContiguous(info.hostCaps & HelloAck.hostCapContiguousFrameSeq != 0)
            assembler?.framePeriodUs = 1_000_000 / UInt64(max(1, info.streamConfig.fps))
            receiver?.setAcceptedSessionId(info.sessionId)
            let acceptedAt = ProcessInfo.processInfo.systemUptime
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                if let move = self.attemptMove {
                    guard Self.isSamePC(move, info) else {
                        self.record(.warning, "connection", "The PC at the new address isn't the saved one")
                        if self.leaveRememberedAddress(.otherPC, result: "other_pc") { return }
                        self.disconnect(.otherPC)
                        self.problem = .noReply(usb: false)
                        return
                    }
                    self.lastTarget = (normalizedHost, port)
                }
                self.sessionAcceptedAt = acceptedAt
                self.noVideoFallback = !isUSB && info.hostCaps & HelloAck.hostCapMediaSize != 0
                    ? NoVideoFallback(wait: self.timing.noVideoStepSeconds) : nil
                self.acceptedAddresses.insert("\(normalizedHost):\(port)")
                self.noPictureStreak.answered(by: info.hostName, pairingToken: info.authToken,
                                              at: isUSB ? nil : "\(normalizedHost):\(port)")
                self.livenessTimeoutUs = UInt64(info.livenessTimeoutMs) * 1000
                self.hostInfo = info
                self.oldPCPrompt.answered(hostName: info.hostName, pairingToken: info.authToken,
                                          hostCaps: info.hostCaps, address: isUSB ? nil : normalizedHost)
                if self.waitingForStream {
                    self.waitingForStream = false
                    if case .pcNotStreaming = self.problem { self.problem = nil }
                    self.armConnectTimeout(seconds: self.timing.connectTimeoutSeconds)
                }
                let seen = self.now()
                self.keepAppleDevicesSighting(info.hostCaps & HelloAck.hostCapUSB == 0
                    ? AppleDevicesSighting(target: pcTarget, at: seen,
                                           until: seen.addingTimeInterval(self.timing.appleDevicesNoteSeconds))
                    : nil)
                if self.awaitingPairing {
                    self.awaitingPairing = false
                    self.pairingPrompt = nil
                    self.armConnectTimeout(seconds: self.timing.connectTimeoutSeconds)
                }
                if PairingStore.valid(info.authToken) {
                    do { try self.pairingStore.save(token: info.authToken, hostName: info.hostName, address: pairingAddress) }
                    catch { self.record(.warning, "pairing", "Pairing was not saved on this iPad. Pair again next time.") }
                }
                if isUSB {
                    self.usbStatus = "USB: connected to \(info.hostName)"
                    self.cableBusyInARow = 0
                }
                self.hostStreamConfig = info.streamConfig
                self.record(
                    .info, "ctrl",
                    "Connected to \(info.hostName) session_id=\(info.sessionId) host_caps=\(info.hostCaps) liveness_ms=\(info.livenessTimeoutMs) config=\(info.streamConfig)"
                )
            }
        }
        channel.onRejected = { [weak self, weak channel] status in
            // Read on the channel's queue, where the refusal was recorded.
            let mismatch = channel?.lastMismatch
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                // A PC that wants a code didn't take the saved PC's pairing:
                // it is another PC, and the saved pairing stays.
                if self.attemptMove != nil, status == .unauthorized || status == .rateLimited {
                    self.record(.warning, "connection", "The PC at the new address isn't the saved one: it asked for pairing")
                    if self.leaveRememberedAddress(.otherPC, result: "other_pc") { return }
                    self.disconnect(.otherPC)
                    self.problem = .noReply(usb: false)
                    return
                }
                // Busy passes: the PC is streaming elsewhere, often to this
                // iPad's own cable session that hasn't closed yet. An
                // automatic cycle keeps retrying and shows it if it runs out.
                // Pairing and a version mismatch need the user, so they end it.
                if status != .busy { self.endReconnectCycle() }
                if status == .unauthorized || status == .rateLimited {
                    self.timeoutTask?.cancel()
                    self.didExtendTimeout = true
                    if status == .unauthorized {
                        do { try self.pairingStore.remove(address: pairingAddress) }
                        catch { self.record(.warning, "pairing", "Could not clear an expired pairing on this iPad.") }
                    }
                    self.awaitingPairing = true
                    let prompt = self.pairingPrompt ?? PairingSheetModel()
                    prompt.reject(status)
                    self.pairingPrompt = prompt
                    self.record(.info, "pairing", status == .rateLimited ? "Pairing rate limited for 60 seconds" : "Pairing code required")
                    return
                }
                switch status {
                case .busy:
                    self.problem = .pcBusy
                case .versionUnsupported:
                    self.versionMismatch = mismatch
                    self.connectionError = self.versionMismatch?.message ?? VersionMismatch.unknownMessage
                default:
                    self.connectionError = "Your PC turned down the connection."
                }
                self.record(.error, "ctrl", self.connectionError ?? self.problem?.title ?? "rejected")
                switch status {
                case .busy: self.disconnect(.pcBusy)
                case .versionUnsupported where mismatch == .pcOlder: self.disconnect(.oldPC)
                default: self.disconnect(.pcRefused)
                }
                // Over the cable the PC dials again a second later, and would
                // be turned down again: wait for a change instead.
                if isUSB, status == .busy || status == .versionUnsupported {
                    self.holdCable(status == .busy ? .busy : .version)
                }
            }
        }
        channel.onPCNotStreaming = { [weak self] status, reason in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }),
                      let wait = ConnectionProblem.StreamWait(status, reason: reason) else { return }
                if self.problem != .pcNotStreaming(wait) {
                    self.record(.info, "ctrl", "PC isn't streaming (\(status)\(reason.isEmpty ? "" : " \(reason)")); waiting for it")
                }
                // No reconnect attempt is spent while the PC says why it
                // isn't streaming. If it stops answering, the timeout says so.
                self.waitingForStream = true
                self.problem = .pcNotStreaming(wait)
                self.armConnectTimeout(seconds: self.timing.connectTimeoutSeconds)
            }
        }
        channel.onHandshakeTimeout = { [weak self, weak receiver] in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                if self.awaitingPairing {
                    self.pairingPrompt?.timedOut()
                    return
                }
                if let receiver, receiver.legacyLookingDatagrams > 0 {
                    self.endReconnectCycle()
                    self.versionMismatch = .pcLegacy
                    self.connectionError = VersionMismatch.pcLegacy.message
                    self.record(.error, "ctrl", "Legacy v1 host detected")
                    self.disconnect(.legacyPC)
                    if isUSB { self.holdCable(.version) }
                } else {
                    self.record(.warning, "ctrl", "HELLO2 handshake got no reply yet")
                    if self.leaveRememberedAddress(.pcMoved, result: "no_reply") {
                        // Back to the address tapped.
                    } else if !isUSB {
                        self.lookForMovedPC(connectionID, host: normalizedHost, port: port)
                    }
                }
            }
        }
        channel.onBye = { [weak self] reason in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.info, "ctrl", "Host BYE (\(reason))")
                let failure = ConnectionProblem.pcGoodbye(reason)
                switch reason {
                case .hostShuttingDown:
                    // Only Stop and update on the PC sends this. The PC comes
                    // back on the new version, so reconnect as for a restart.
                    if self.reconnectCycleID != nil {
                        self.disconnect(.pcGoodbye)
                        self.reconnectFailure = failure
                        self.problem = failure
                    } else {
                        self.beginReconnect(.pcStoppedResponding, failure: failure, end: .pcGoodbye)
                    }
                case .userDisconnect, .superseded:
                    // Disconnected from the PC, or another device took over:
                    // stay away until the user connects again.
                    self.endReconnectCycle()
                    self.disconnect(.pcGoodbye)
                    self.problem = failure
                default:
                    // A PC that stops or restarts its stream may come back,
                    // as may one that stopped hearing this iPad (an error).
                    // A running cycle keeps going; otherwise start one.
                    if self.reconnectCycleID != nil {
                        self.disconnect(.pcGoodbye)
                        self.problem = failure
                    } else {
                        self.beginReconnect(.pcStoppedResponding, failure: failure, end: .pcGoodbye)
                    }
                }
            }
        }
        channel.onHeartbeat = { [weak self, weak assembler] heartbeat in
            assembler?.framePeriodUs = 1_000_000 / UInt64(max(1, heartbeat.streamConfig.fps))
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.hostStreamConfig = heartbeat.streamConfig
                if let frames = heartbeat.framesSent {
                    self.noVideoFallback?.heard(framesSent: frames, at: ProcessInfo.processInfo.systemUptime)
                }
                let locked = heartbeat.notice == Heartbeat.noticePCLocked
                if locked != self.pcLocked {
                    self.pcLocked = locked
                    self.record(.info, "ctrl", locked ? "PC locked: holding the picture" : "PC unlocked")
                }
                let noDisplay = heartbeat.notice == Heartbeat.noticeNoDisplay
                if noDisplay != self.pcNoDisplay {
                    self.pcNoDisplay = noDisplay
                    self.record(.info, "ctrl", noDisplay ? "PC has no active display: holding the picture" : "PC display back")
                }
            }
        }
        channel.onStreamConfig = { [weak self, weak assembler] config in
            assembler?.framePeriodUs = 1_000_000 / UInt64(max(1, config.fps))
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.hostStreamConfig = config
            }
        }

        receiver.onControlDatagram = { [weak channel] data in
            channel?.handleControl(data)
        }
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first { $0.activationState == .foregroundActive }
        let screen = scene?.screen ?? UIScreen.main
        let identity = ControlChannel.ClientIdentity(
            deviceName: UIDevice.current.name,
            screenPxW: UInt16(clamping: Int(screen.nativeBounds.width)),
            screenPxH: UInt16(clamping: Int(screen.nativeBounds.height)),
            screenPtW: UInt16(clamping: Int(screen.bounds.width)),
            screenPtH: UInt16(clamping: Int(screen.bounds.height)),
            refreshHz: UInt8(clamping: screen.maximumFramesPerSecond),
            decoderCaps: Hello2.capDecodeH264 | Hello2.capDecodeHEVC,
            featureCaps: Self.featureCaps(wantsInput: wantsInput, wantsAudio: wantsAudio, isUSB: isUSB),
            deviceId: DeviceIdentity.load(),
            preferredFPS: drawing.preferredFPS(UserDefaults.standard.frameRatePreference(), maximum: screen.maximumFramesPerSecond),
            authToken: pairingToken
        )
        record(.info, "connection", "Client capabilities pixels=\(identity.screenPxW)x\(identity.screenPxH) refresh=\(identity.refreshHz) preferred_fps=\(identity.preferredFPS) decoder_caps=\(identity.decoderCaps) feature_caps=\(identity.featureCaps)")
        E2E.emit("E2E_HELLO w=\(identity.screenPxW) h=\(identity.screenPxH) refresh_hz=\(identity.refreshHz)")
        receiver.onListenerReady = { [weak self, weak channel] actualPort in
            channel?.startHandshake(listenPort: actualPort, identity: identity)
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.debugState.listenerReadyPort = actualPort
                self.record(.info, isUSB ? "usb" : "udp", isUSB ? "USB tunnel ready" : "Socket ready on ephemeral port \(actualPort)")
            }
        }
        receiver.onDatagramReceived = { [streamCounters] byteCount in
            // Link-queue side: just count. The 4 Hz flush loop below moves the
            // totals onto the MainActor.
            streamCounters.recordDatagram(bytes: byteCount)
        }
        receiver.onDatagramIgnored = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.warning, isUSB ? "usb" : "udp", message)
            }
        }
        receiver.onConnectionEstablished = { [weak self] in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.info, isUSB ? "usb" : "udp", "Accepted stream from selected host")
            }
        }
        receiver.onError = { [weak self] error in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                // A path that isn't ready yet (no network, Local Network off)
                // is this iPad's situation, not a fault: a warning.
                let waiting = error.kind == .pathUnavailable || error.kind == .localNetworkDenied
                self.record(waiting ? .warning : .error, isUSB ? "usb" : "udp", "state=\(self.state) \(error.message)")
                let failure = ConnectionProblem.linkFailure(error, isUSB: isUSB, host: normalizedHost)
                // A live stream rides out transient errors; a USB link that
                // failed for good closes next, and that close reports this.
                guard self.state == .connecting else {
                    if isUSB { self.linkFailure = failure }
                    return
                }
                guard isUSB else {
                    self.disconnect(failure == .wifiOff ? .noNetwork : .linkFailed)
                    self.problem = failure
                    return
                }
                // Decide once, here, as a close would: fall back to Wi-Fi when
                // automatic reconnecting is on, otherwise show the cable
                // advice. beginReconnect disconnects, which retires
                // connectionID, so the close FramedLink sends next is ignored
                // instead of relabelling this as an unplugged cable.
                self.usbStatus = "USB: waiting for a cable"
                if self.stopsAfterClosingWithoutPicture() { return }
                self.beginReconnect(.cableUnplugged, failure: failure)
            }
        }

        receiver.onClosed = { [weak self] in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                if isUSB { self.usbStatus = "USB: waiting for a cable" }
                if self.stopsAfterClosingWithoutPicture() { return }
                // An ordinary close is the cable coming out; a close after a
                // classified failure keeps that failure's advice.
                self.beginReconnect(.cableUnplugged, failure: self.linkFailure)
            }
        }

        self.videoDecoder = decoder
        self.audioPlayer = audio
        self.frameAssembler = assembler
        self.controlChannel = channel
        self.mediaLink = receiver

        guard receiver.start(host: normalizedHost) else {
            connectionError = "Couldn't start streaming on this iPad. Try again."
            record(.error, "connection", "Receiver failed to start")
            disconnect(.receiverFailed)
            return
        }

        startStatsFlushLoop()

        // Timeout after N seconds (re-armed once when datagrams start arriving).
        armConnectTimeout(seconds: timing.connectTimeoutSeconds)
    }

    /// Drains the hot-path counters onto the MainActor at 4 Hz — cheap enough
    /// to be invisible, fresh enough for diagnostics and the connect flow.
    private func startStatsFlushLoop() {
        statsFlushTask?.cancel()
        statsFlushTask = Task { @MainActor [weak self] in
            var lastSummary = ProcessInfo.processInfo.systemUptime
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 250_000_000)
                guard let self, !Task.isCancelled else { return }
                let snapshot = self.streamCounters.drain()

                if snapshot.datagrams > 0 || snapshot.assembled > 0 {
                    let firstDatagrams =
                        self.debugState.datagramsReceived == 0 && snapshot.datagrams > 0
                    let firstAssembled =
                        self.debugState.assembledFrames == 0 && snapshot.assembled > 0
                    self.debugState.datagramsReceived += snapshot.datagrams
                    self.debugState.datagramBytesReceived += snapshot.datagramBytes
                    self.debugState.assembledFrames += snapshot.assembled
                    self.debugState.assembledFrameBytes += snapshot.assembledBytes
                    self.debugState.decodePackets += snapshot.parsed

                    if firstDatagrams {
                        let linkName = self.mediaLink?.isUSB == true ? "USB" : "UDP"
                        self.record(
                            .info, linkName.lowercased(),
                            "\(linkName) data flowing (\(snapshot.datagrams) datagrams, \(snapshot.datagramBytes) bytes in first batch)"
                        )
                        // Data is flowing — grant a fresh full window (once) so a slow/jittery
                        // network gets time to finish reassembly + decode instead of being cut
                        // off mid-handshake.
                        if self.state == .connecting && !self.didExtendTimeout {
                            self.didExtendTimeout = true
                            self.armConnectTimeout(seconds: self.timing.connectTimeoutSeconds)
                        }
                    }
                    if firstAssembled {
                        self.record(.info, "assembly", "First frame payload assembled (\(snapshot.assembledBytes) bytes)")
                    }
                }

                self.advanceNoVideoFallback()
                // Runs on EVERY tick, including silent ones: a dead host sends
                // neither media nor heartbeats, so skipping this when nothing
                // arrived is exactly when the liveness watchdog is needed.
                self.refreshStatsAndWatchdog()
                if E2E.enabled, self.state == .connected {
                    E2E.pointer(self.pointerOverlay.counters, decoded: self.debugState.decodedFrames)
                }
                if ProcessInfo.processInfo.systemUptime - lastSummary >= 10 {
                    lastSummary = ProcessInfo.processInfo.systemUptime
                    self.recordStreamSummary(reason: "periodic")
                }
            }
        }
    }

    /// Recomputes the HUD stats from the hot-path counters and runs the
    /// heartbeat liveness watchdog. MainActor, 4 Hz.
    private func refreshStatsAndWatchdog() {
        guard let channel = controlChannel else { return }
        audioStats = audioPlayer?.stats ?? AudioStats()
        fps = fpsCounter.currentFPS

        // --- Stats snapshot ---
        var next = StreamStats()
        next.decodeFps = fps
        let clock = channel.clockSnapshot.withLock { $0 }
        if let rtt = clock.rttUs { next.rttMs = Double(rtt) / 1000.0 }
        if clock.offsetUs != nil { next.e2eMs = lagMs }
        if let counters = frameAssembler?.counters.withLock({ $0 }) {
            // The assembler zeroes its counters whenever it resets (host
            // pipeline restart bumps the stream epoch). A value BELOW last
            // tick's therefore means "restarted", not "wrapped" — re-baseline,
            // because subtracting through zero produced two ~2^64 deltas whose
            // sum overflowed and killed the app on every host restart.
            if counters.fragsReceived < prevCounters.fragsReceived
                || counters.fragsLost < prevCounters.fragsLost {
                prevCounters = FrameAssembler.Counters()
            }
            let deltaReceived = counters.fragsReceived - prevCounters.fragsReceived
            let deltaLost = counters.fragsLost - prevCounters.fragsLost
            prevCounters = counters
            let total = deltaReceived + deltaLost
            next.lossPercent = total == 0 ? 0 : Double(deltaLost) * 100.0 / Double(total)
            next.framesDropped = counters.framesDropped
            next.fragsRepaired = counters.fragsRepaired
            next.jitterMs = Double(counters.jitterUs) / 1000
        }
        next.bars = StreamStats.bars(lossPercent: next.lossPercent, rttMs: next.rttMs)
        stats = next
        let restoring = state == .connected
            && (videoDecoder?.keyframeWaitUs ?? 0) >= VideoDecoder.showRestoringAfterUs
        if restoring != pictureRestoring { pictureRestoring = restoring }

        // --- Liveness watchdog (host heartbeats stopped => stale picture) ---
        guard state == .connected else { return }
        let lastHeartbeat = channel.lastHeartbeatAtUs.withLock { $0 }
        guard lastHeartbeat > 0 else { return }
        let sinceUs = ControlChannel.clientNowUs() &- lastHeartbeat
        if sinceUs > livenessTimeoutUs {
            if !signalLost {
                signalLost = true
                #if DEBUG
                UITestProbe.shared.signalLost()
                #endif
                degradedSinceUs = ControlChannel.clientNowUs()
                record(.warning, "ctrl", "Host heartbeats stopped — signal lost")
            } else if let since = degradedSinceUs,
                      ControlChannel.clientNowUs() &- since > 5_000_000 {
                // 5s of degraded: give up on this session and (optionally)
                // start the reconnect cycle.
                beginReconnect(.pcStoppedResponding)
            }
        } else if signalLost {
            signalLost = false
            degradedSinceUs = nil
            record(.info, "ctrl", "Signal recovered")
        }
    }

    /// Ends the session that `reason` interrupted (`end` when it was not
    /// the link: the PC said goodbye) and reconnects if that is on.
    private func beginReconnect(_ reason: ConnectionProblem.LostReason, failure: ConnectionProblem? = nil,
                                end: AttemptEnd? = nil) {
        let target = lastTarget
        let auto = UserDefaults.standard.flag(forKey: "autoReconnect")
        // Support logs keep the original wording so reports stay comparable.
        let logged: String
        switch reason {
        case .pcStoppedResponding: logged = "host stopped responding"
        case .cableUnplugged: logged = failure == nil ? "USB disconnected" : "USB link failed"
        case .usbTurnedOff: logged = "USB connections turned off"
        }
        record(.warning, "ctrl", "Connection lost (\(logged))")
        switch reason {
        case .pcStoppedResponding: disconnect(end ?? .pcStoppedResponding)
        case .cableUnplugged: disconnect(end ?? (failure == nil ? .cableUnplugged : .usbLinkFailed))
        case .usbTurnedOff: disconnect(end ?? .usbTurnedOff)
        }
        guard auto, let target else {
            problem = (failure ?? .lost(reason)).withoutReconnecting
            return
        }
        startReconnectCycle(to: target, failure: failure)
    }

    /// Retries the last Wi-Fi PC. `failure` is the classified problem that
    /// ended the session (a failed cable); a more specific failure in a later
    /// attempt replaces it, and whichever is left is shown if every attempt
    /// fails, rather than the generic message. The cycle writes state only
    /// while `reconnectCycleID` is its own.
    private func startReconnectCycle(to target: (host: String, port: UInt16), failure: ConnectionProblem? = nil) {
        endReconnectCycle()
        let cycleID = UUID()
        reconnectCycleID = cycleID
        reconnectFailure = failure
        reconnectTask = Task { @MainActor [weak self] in
            for attempt in 1...6 {
                guard let self, self.reconnectCycleID == cycleID else { return }
                self.reconnectAttempt = attempt
                self.problem = .reconnecting(attempt: attempt, of: 6)
                try? await Task.sleep(for: .seconds(self.timing.reconnectBackoffSeconds(attempt)))
                guard self.reconnectCycleID == cycleID else { return }
                self.startAttempt(.udp(host: target.host, port: target.port))
                // Give the attempt its connect-timeout window, and wait out a
                // window that arriving data extended. Streaming ends the cycle.
                try? await Task.sleep(for: .seconds(self.timing.connectTimeoutSeconds + self.timing.reconnectGraceSeconds))
                while self.reconnectCycleID == cycleID, self.state == .connecting {
                    try? await Task.sleep(for: .milliseconds(250))
                }
                guard self.reconnectCycleID == cycleID else { return }
                self.reconnectFailure = ConnectionProblem.kept(self.reconnectFailure, after: self.problem)
            }
            guard let self, self.reconnectCycleID == cycleID else { return }
            let kept = self.reconnectFailure
            self.endReconnectCycle()
            self.problem = (kept ?? .reconnectFailed).afterTheCycle
        }
    }

    /// The attempt's HELLO2 got no answer at `host:port`. A paired PC that
    /// this iPad found with Bonjour is looked for by name. Where Bonjour has
    /// it at another home network address that this iPad reaches directly,
    /// the iPad checks that it is the same PC, at that address, and only
    /// then moves the attempt there with its pairing.
    /// Otherwise the attempt goes on as before.
    private func lookForMovedPC(_ connectionID: UUID, host: String, port: UInt16) {
        let address = PairingStore.address(host: host, port: port)
        let saved = RecentConnectionStore.shared.entry(host: host, port: port)
        let name = saved?.name ?? pairingStore.pcName(address: address)
        let token = (try? pairingStore.token(address: address)) ?? nil
        guard attemptMove == nil, let name, let token,
              MovedPC.canLookUp(host: host, pcName: name, found: foundAddresses.contains(address) || saved?.found == true,
                                paired: PairingStore.valid(token), homeAddress: homeAddress)
        else { return }
        record(.info, "connection", "Looking for the saved PC by name: no answer at its address")
        let current = { [weak self] in
            // The PC may have answered meanwhile, or the attempt ended.
            guard let self else { return false }
            return self.currentAttempt.withLock { $0 == connectionID } && self.state == .connecting
                && self.hostInfo == nil && !self.awaitingPairing
        }
        Task { @MainActor [weak self] in
            guard let self else { return }
            let found = await self.findPCs(MovedPC.serviceName(pcName: name))
            guard current() else { return }
            guard let moved = MovedPC.newAddress(pcName: name, host: host, port: port, found: found,
                                                 homeAddress: self.homeAddress) else {
                self.record(.info, "connection", "The saved PC wasn't found at another address")
                return
            }
            let route = await self.localRoute(moved.host, moved.port)
            guard current() else { return }
            guard let route, MovedPC.isLocal(moved.host, over: route) else {
                self.record(.info, "connection", "The saved PC was found, but not on this iPad's local network: staying at the saved address")
                return
            }
            self.record(.info, "connection", "Checking that the PC at \(moved.host):\(moved.port) is the saved one")
            let same = await MovedPC.confirm(host: moved.host, port: moved.port, token: token,
                                             link: self.makeUDPLink(moved.port))
            guard current() else { return }
            guard same else {
                self.record(.info, "connection", "The PC there didn't show it is the saved one: staying at the saved address")
                return
            }
            self.record(.info, "connection", "It is the saved PC: connecting there")
            self.disconnect(.pcMoved)
            self.pendingMove = Move(from: (host, port), name: name, token: token)
            self.explicitPairingToken = token
            self.startAttempt(.udp(host: moved.host, port: moved.port))
        }
    }

    /// The PC that accepted a moved attempt is the saved one: it has the
    /// saved name, and gives back the saved pairing token, or none if its
    /// pairing is off now.
    private static func isSamePC(_ move: Move, _ info: ControlChannel.SessionInfo) -> Bool {
        info.hostName == move.name && (!PairingStore.valid(info.authToken) || info.authToken == move.token)
    }

    /// The moved PC showed a picture: its pairing is saved at the new
    /// address, then the new entry replaces the old one. If the pairing
    /// couldn't be saved, the old entry and pairing stay as they were.
    private func savedMove(_ move: Move, to new: (host: String, port: UInt16), pairingAddress: String) -> Bool {
        do {
            try pairingStore.save(token: move.token, hostName: hostInfo?.hostName ?? move.name, address: pairingAddress)
            guard try pairingStore.token(address: pairingAddress) == move.token else { throw CocoaError(.fileWriteUnknown) }
        } catch {
            record(.warning, "pairing", "The PC's new address wasn't saved on this iPad; keeping the saved one")
            return false
        }
        RecentConnectionStore.shared.moved(from: move.from, to: new, name: hostInfo?.hostName ?? move.name,
                                           replacesOld: move.noVideo || homeAddress(move.from.host))
        if move.noVideo {
            // The PC still answers at the old address: its pairing stays,
            // and connecting there goes here first.
            NoVideoPaths.remember(from: .init(host: move.from.host, port: move.from.port),
                                  to: .init(host: new.host, port: new.port), pairing: move.token)
        } else if homeAddress(move.from.host) {
            try? pairingStore.remove(address: PairingStore.address(host: move.from.host, port: move.from.port))
        }
        return true
    }

    /// Takes the next no-video step once it is due: the PC said it sent
    /// video and no picture came of it (see `NoVideoFallback`). A path that
    /// drops large datagrams often lets a frame's small last fragment by.
    private func advanceNoVideoFallback() {
        guard noVideoFallback != nil, state == .connecting, hostInfo != nil, !awaitingPairing, !waitingForStream,
              let step = noVideoFallback?.next(at: ProcessInfo.processInfo.systemUptime) else { return }
        endNoVideoStep("no_video")
        switch step {
        case .smallerPackets(let size):
            // The PC is sending and the iPad is working on it: one fresh
            // window, as when data starts to arrive.
            if !didExtendTimeout {
                didExtendTimeout = true
                armConnectTimeout(seconds: timing.connectTimeoutSeconds)
            }
            noVideoStep = "step=smaller_packets size=\(size)"
            controlChannel?.sendKeyframeRequest(streamEpoch: 0, lastCompleteSeq: 0, reason: .startup, maxMediaDgram: size)
        case .otherAddress:
            tryOtherAddress()
        }
    }

    /// Logs the pending no-video step's `result`.
    private func endNoVideoStep(_ result: String) {
        guard let step = noVideoStep else { return }
        noVideoStep = nil
        record(.info, "connection", "no_video_fallback \(step) result=\(result)")
    }

    /// No picture came of the PC's video at this address, even in small
    /// datagrams. Asked here, the PC signs its home network addresses with
    /// this iPad's pairing; the first other one on this iPad's own network,
    /// where the PC shows again that it holds the pairing and receives
    /// there, gets a new attempt. A forwarded or spoofed
    /// address can't make either answer.
    private func tryOtherAddress() {
        let host = debugState.host, port = debugState.port
        let done = { [weak self] (result: String) -> Void in
            self?.record(.info, "connection", "no_video_fallback step=other_address result=\(result)")
        }
        guard attemptMove == nil else { return done("moved_before") }
        guard let connectionID = currentAttempt.withLock({ $0 }), let name = hostInfo?.hostName,
              let token = (try? pairingStore.token(address: PairingStore.address(host: host, port: port))) ?? nil
        else { return done("unpaired") }
        let current = { [weak self] in
            guard let self else { return false }
            return self.currentAttempt.withLock { $0 == connectionID } && self.state == .connecting
        }
        Task { @MainActor [weak self] in
            guard let self else { return }
            let identity = await MovedPC.identify(host: host, port: port, token: token, link: self.makeUDPLink(port))
            guard current() else { return }
            guard let identity else { return done("no_answer") }
            for other in MovedPC.otherAddresses(identity, asked: host, homeAddress: self.homeAddress) {
                let target = self.dialAddress(other, identity.port)
                let route = await self.localRoute(target.host, target.port)
                guard current() else { return }
                // As for a PC Bonjour moved: only an address on this iPad's
                // own network, so nothing relays the stream elsewhere.
                guard let route, MovedPC.isLocal(target.host, over: route) else { continue }
                let same = await MovedPC.confirm(host: target.host, port: target.port, token: token,
                                                 link: self.makeUDPLink(target.port))
                guard current() else { return }
                guard same else { continue }
                done("moved")
                self.disconnect(.noVideoPath)
                self.pendingMove = Move(from: (host, port), name: name, token: token, noVideo: true)
                self.explicitPairingToken = token
                self.startAttempt(.udp(host: target.host, port: target.port))
                return
            }
            done("none")
        }
    }

    /// Retires the automatic reconnect cycle, if one is running: its task
    /// stops at its next check and can no longer write state.
    private func endReconnectCycle() {
        reconnectTask?.cancel()
        reconnectTask = nil
        reconnectCycleID = nil
        reconnectFailure = nil
        reconnectAttempt = 0
    }

    /// The session and how long the current attempt has taken, appended to
    /// the support log's first-frame and timeout lines.
    private var attemptTimings: String {
        let now = ProcessInfo.processInfo.systemUptime
        var text = ""
        if let session = hostInfo?.sessionId { text += " session_id=\(session)" }
        text += " ms_since_start=\(Int((now - attemptStartedAt) * 1000))"
        if let accepted = sessionAcceptedAt { text += " ms_since_accept=\(Int((now - accepted) * 1000))" }
        return text
    }

    /// (Re)arm the connect-timeout watchdog. Cancels any existing timer and starts a fresh one;
    /// if we're still `.connecting` when it fires, surface a diagnostic and disconnect.
    private func armConnectTimeout(seconds: Double) {
        timeoutTask?.cancel()
        timeoutTask = Task { @MainActor in
            try? await Task.sleep(for: .seconds(seconds))
            if !Task.isCancelled && self.state == .connecting {
                let debug = self.debugState
                let handshakeDone = self.hostInfo != nil
                let isUSB = self.transportMode == "USB"
                let level = debug.timeoutLevel(handshakeDone: handshakeDone,
                                               answeredHereBefore: self.acceptedAddresses.contains("\(debug.host):\(debug.port)"))
                self.record(level, "timeout", debug.timeoutSummary(handshakeDone: handshakeDone) + self.attemptTimings)
                if self.leaveRememberedAddress(.connectTimeout, result: "no_picture") { return }
                // Only an attempt the PC answered counts.
                if handshakeDone { self.noPictureStreak.endedWithoutPicture() }
                // A PC that said it isn't streaming and then went quiet (it
                // closed, or slept) didn't answer: its answers were no picture.
                let problem: ConnectionProblem = self.waitingForStream ? .noReply(usb: isUSB) : self.noPictureStreak.shown(.timeout(
                    listenerReady: debug.listenerReadyPort != nil, helloAttempts: debug.helloAttempts,
                    handshakeDone: handshakeDone, datagramsReceived: debug.datagramsReceived,
                    isUSB: isUSB, host: debug.host), at: isUSB ? nil : "\(debug.host):\(debug.port)")
                self.problem = problem == .noReply(usb: false) && self.attemptMove != nil
                    ? .noReplyAtNewAddress(debug.host) : problem
                // The app gave up, not the user: the PC's log tells them apart.
                self.disconnect(.connectTimeout)
                if handshakeDone && self.noPictureStreak.stopsReconnecting { self.stopAutomaticAttempts(isUSB: isUSB) }
            }
        }
    }

    /// The PC answered this attempt and closed the connection before a
    /// picture, as a PC on 0.3.1 does over the cable each time its second
    /// screen restarts. That counts like a no-picture timeout; at the limit
    /// it ends the attempt here and returns true, instead of reconnecting.
    private func stopsAfterClosingWithoutPicture() -> Bool {
        guard state == .connecting, hostInfo != nil else { return false }
        noPictureStreak.endedWithoutPicture()
        guard noPictureStreak.stopsReconnecting else { return false }
        let isUSB = transportMode == "USB"
        record(.warning, "ctrl", "Connection closed by the PC before a picture" + attemptTimings)
        disconnect(.closedBeforePicture)
        problem = .noPicture
        stopAutomaticAttempts(isUSB: isUSB)
        return true
    }

    /// The PC answers but never sends a picture: stop the attempts that
    /// start by themselves, the reconnect cycle and the cable the PC
    /// connects again, and leave the problem card with Try again up.
    private func stopAutomaticAttempts(isUSB: Bool) {
        record(.warning, "connection", "Stopped reconnecting after \(noPictureStreak.timeouts) no-picture timeouts in a row")
        endReconnectCycle()
        if isUSB {
            usbPaused = true
            usbStoppedForNoPicture = true
            usbListener?.stop()
            usbListener = nil
            usbStatus = "USB: disconnected"
        }
    }

    func cancel() {
        if mediaLink?.isUSB == true {
            usbPaused = true
            usbListener?.stop()
            usbListener = nil
            usbStatus = "USB: disconnected"
        }
        timeoutTask?.cancel()
        endReconnectCycle()
        disconnect()
        // The user stopped waiting for the PC to stream.
        if case .pcNotStreaming = problem { problem = nil }
    }

    /// Try the last connection again: the cable when that is what failed,
    /// otherwise the last PC address. Cable advice kept after a Wi-Fi
    /// fallback ran out still means the cable, though Wi-Fi was tried last.
    func retry() {
        guard state == .disconnected else { return }
        let cable = transportMode == "USB" || problem == .usbFailed
        endReconnectCycle()
        noPictureStreak.restartCount()
        problem = nil
        connectionError = nil
        if cable {
            resumeUSBConnections()
        } else if let target = lastTarget {
            connect(host: target.host, port: target.port)
        }
    }

    /// Settings negotiated at connect (Drawing mode, Control PC) apply after
    /// a new handshake. USB's listener accepts the retry on its own.
    func reconnectToApply() {
        guard state == .connected else { return }
        let isUSB = mediaLink?.isUSB == true
        let target = lastTarget
        disconnect()
        if !isUSB, let target { connect(host: target.host, port: target.port) }
    }

    func refreshAudioPreference(_ enabled: Bool) {
        audioPreference = enabled
        guard !sessionDrawingMode || state == .disconnected else { return }
        if !enabled || sessionWantsAudio {
            audioPlayer?.setEnabled(enabled)
            audioStats.playing = false
        } else if state != .disconnected {
            // Audio is negotiated in HELLO2. Enabling it after an audio-free
            // session needs a new handshake; USB's listener accepts the retry.
            let isUSB = mediaLink?.isUSB == true
            let target = lastTarget
            disconnect()
            if !isUSB, let target { connect(host: target.host, port: target.port) }
        }
    }

    /// Request execution time before entering the background. UIKit grants
    /// this assertion asynchronously; requesting it after backgrounding can
    /// leave the goodbye queued when the app is suspended.
    func prepareForBackground() {
        guard state != .disconnected, backgroundGoodbyeTask == .invalid else { return }
        backgroundGoodbyeTask = UIApplication.shared.beginBackgroundTask(withName: "EternalMonitor goodbye") { [weak self] in
            self?.endBackgroundGoodbye()
        }
        E2E.emit("E2E_BACKGROUND_PREPARED task=\(backgroundGoodbyeTask.rawValue)")
    }

    private func endBackgroundGoodbye() {
        let task = backgroundGoodbyeTask
        backgroundGoodbyeTask = .invalid
        guard task != .invalid else { return }
        UIApplication.shared.endBackgroundTask(task)
        E2E.emit("E2E_BACKGROUND_ENDED task=\(task.rawValue)")
    }

    /// The app is leaving the foreground: say goodbye so the host stops
    /// streaming promptly instead of waiting out its liveness window.
    func handleAppBackgrounded() {
        // Fallback for a transition that did not send willResignActive.
        prepareForBackground()
        isForeground = false
        usbListener?.stop()
        usbListener = nil
        // A suspended app can't reconnect. Backgrounding ends an automatic
        // cycle where running out would, and foregrounding resumes it as an
        // interrupted session.
        let cycleOutcome = reconnectCycleID == nil ? nil : reconnectFailure ?? .reconnectFailed
        endReconnectCycle()
        if cycleOutcome != nil { resumeOnForeground = true }
        guard state != .disconnected else {
            if let cycleOutcome { problem = cycleOutcome }
            endBackgroundGoodbye()
            return
        }
        resumeOnForeground = true
        record(.info, "ctrl", "App backgrounded — sending BYE and disconnecting")
        let goodbye = backgroundGoodbyeTask
        disconnect(.appBackground)
        if let cycleOutcome { problem = cycleOutcome }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
            guard let self, self.backgroundGoodbyeTask == goodbye else { return }
            self.endBackgroundGoodbye()
        }
    }

    /// Shows `sighting` until it expires, then clears it, as long as no
    /// newer one has replaced it.
    private func keepAppleDevicesSighting(_ sighting: AppleDevicesSighting?) {
        appleDevicesExpiry?.cancel()
        appleDevicesExpiry = nil
        guard let sighting, sighting.applies(now: now()) else {
            appleDevicesUnreachable = nil
            return
        }
        appleDevicesUnreachable = sighting
        let wait = sighting.until.timeIntervalSince(now())
        appleDevicesExpiry = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(wait))
            guard !Task.isCancelled, let self, self.appleDevicesUnreachable == sighting else { return }
            self.appleDevicesUnreachable = nil
        }
    }

    /// A suspended app's timer may not have fired on time: check the
    /// sighting against the clock again, and wait out what is left of it.
    func recheckAppleDevicesSighting() {
        if let sighting = appleDevicesUnreachable { keepAppleDevicesSighting(sighting) }
    }

    /// Resume the session that backgrounding interrupted (opt-out toggle).
    func handleAppForegrounded() {
        endBackgroundGoodbye()
        isForeground = true
        recheckAppleDevicesSighting()
        // A hold whose wait ran out in the background ends here.
        if let hold = cableHold, !hold.holds(at: now()) { endCableHold() }
        usbPaused = usbStoppedForNoPicture || cableHold != nil
        refreshUSBAvailability()
        guard resumeOnForeground else { return }
        resumeOnForeground = false
        guard UserDefaults.standard.flag(forKey: "autoResumeOnForeground"),
              state == .disconnected,
              let target = lastTarget
        else { return }
        record(.info, "ctrl", "App foregrounded — resuming the interrupted session")
        connect(host: target.host, port: target.port)
    }

    /// Send feedback or Not now on the one-time card.
    func finishFeedbackAsk() {
        feedbackAsk.finish()
    }

    /// The details a feedback email carries about this connection.
    var feedbackContext: Feedback.Context {
        Feedback.Context.current(
            pcVersion: PCVersions.version(address: lastTarget?.host, name: hostInfo?.hostName),
            connection: state == .disconnected ? nil : (transportMode == "USB" ? "USB" : "Wi-Fi")
        )
    }

    /// Ends the attempt. `end` says why; only the user's own action is
    /// `.userDisconnect`.
    func disconnect(_ end: AttemptEnd = .userDisconnect) {
        // Still checking the remembered address: nothing went out yet.
        if redirectCheck != nil {
            redirectCheck = nil
            state = .disconnected
            return
        }
        if state != .disconnected {
            endNoVideoStep("no_video")
            recordStreamSummary(reason: "disconnect=\(end.name)")
            record(.info, "connection", "Disconnect state=\(state) reason=\(end.name)")
            recordSummary(end)
        }
        awaitingPairing = false
        waitingForStream = false
        noVideoFallback = nil
        pairingPrompt = nil
        currentAttempt.withLock { $0 = nil }
        timeoutTask?.cancel()
        timeoutTask = nil
        statsFlushTask?.cancel()
        statsFlushTask = nil
        didExtendTimeout = false

        // Say goodbye first (fire-and-forget), then stop the socket, then shut
        // the decoder down on its own queue (provably after any in-flight decode).
        controlChannel?.sendBye(end.bye)
        controlChannel?.stop()
        mediaLink?.stop()
        videoDecoder?.shutdown()
        audioPlayer?.stop()
        frameAssembler?.reset()
        _ = streamCounters.drain()

        controlChannel = nil
        controlChannelBox.value = nil
        mediaLink = nil
        fpsCounter = FPSCounter()
        frameAssembler = nil
        videoDecoder = nil
        audioPlayer = nil
        sessionWantsAudio = false
        audioStats = AudioStats()

        if state == .connected, let since = streamingSince {
            feedbackAsk.sessionEnded(after: Date().timeIntervalSince(since))
        }
        streamingSince = nil
        state = .disconnected
        fps = 0
        lagMs = 0
        stats = StreamStats()
        signalLost = false
        pictureRestoring = false
        pcLocked = false
        pcNoDisplay = false
        degradedSinceUs = nil
        prevCounters = FrameAssembler.Counters()
        hostInfo = nil
        hostStreamConfig = nil
        UIApplication.shared.isIdleTimerDisabled = false
    }

    /// Records the summary of the HELLO2 that ends with `end`: one per nonce,
    /// whichever paths reach it.
    private func recordSummary(_ end: AttemptEnd) {
        let summary = summary(end)
        if summary.nonce != 0 {
            guard !summarizedNonces.contains(summary.nonce) else { return }
            summarizedNonces.append(summary.nonce)
            if summarizedNonces.count > 32 { summarizedNonces.removeFirst() }
        }
        record(.info, "summary", summary.line)
    }

    /// The attempt's summary as it ends with `end`.
    private func summary(_ end: AttemptEnd) -> SessionSummary {
        let now = ProcessInfo.processInfo.systemUptime
        return SessionSummary(
            end: end, isUSB: mediaLink?.isUSB ?? (transportMode == "USB"), sessionId: hostInfo?.sessionId,
            nonce: controlChannel?.helloNonce ?? 0, awaitingPairing: awaitingPairing,
            datagramsReceived: debugState.datagramsReceived, firstFrame: firstFrameAt.map { $0 - summaryStartedAt },
            duration: now - summaryStartedAt)
    }

    /// Capture-to-decode latency of a frame stamped `captureUs` on the host
    /// clock. Nil for an unstamped frame or a stamp too far from either clock
    /// to subtract, which a broken or hostile host could send.
    nonisolated static func latencyMs(captureUs: UInt64, offsetUs: Int64, nowUs: UInt64) -> Double? {
        guard captureUs > 0 else { return nil }
        let (captureClient, captureOverflow) = Int64(bitPattern: captureUs).subtractingReportingOverflow(offsetUs)
        let (latencyUs, latencyOverflow) = Int64(bitPattern: nowUs).subtractingReportingOverflow(captureClient)
        guard !captureOverflow, !latencyOverflow else { return nil }
        return latencyUs > 0 ? Double(latencyUs) / 1000.0 : 0
    }

    private func recordStreamSummary(reason: String) {
        let counters = frameAssembler?.counters.withLock { $0 } ?? .init()
        let clock = controlChannel?.clockSnapshot.withLock { $0 }
        SupportDiagnostics.shared.record("info", "stream", "reason=\(reason) state=\(state) transport=\(transportMode) session_id=\(hostInfo?.sessionId ?? 0) decoded=\(debugState.decodedFrames) assembled=\(debugState.assembledFrames) datagrams=\(debugState.datagramsReceived) bytes=\(debugState.datagramBytesReceived) fps=\(fps) size=\(videoSize) depth=\(videoDecoder?.decodeDepth ?? 0) dropped=\(counters.framesDropped) mismatched_count_frags=\(frameAssembler?.mismatchedCountFragments.withLock { $0 } ?? 0) repaired=\(counters.fragsRepaired) nacks=\(counters.nacksSent) jitter_us=\(counters.jitterUs) rtt_us=\(clock?.rttUs ?? 0) e2e_ms=\(lagMs) audio=\(audioStats) thermal=\(ProcessInfo.processInfo.thermalState.rawValue) low_power=\(ProcessInfo.processInfo.isLowPowerModeEnabled)")
    }

    private func record(_ level: DiagnosticLevel, _ category: String, _ message: String) {
        SupportDiagnostics.shared.record(String(describing: level), category, message)
        E2E.emit("E2E_DIAGNOSTIC category=\(category) message=\(message)")
        diagnostics.append(DiagnosticEntry(level: level, category: category, message: message))
        if diagnostics.count > 80 {
            diagnostics.removeFirst(diagnostics.count - 80)
        }
        Logger(subsystem: "com.eternal.monitor", category: category)
            .log(level: level.osLogType, "\(message, privacy: .public)")
    }
}

private extension DiagnosticLevel {
    var osLogType: OSLogType {
        switch self {
        case .info:
            return .info
        case .warning:
            return .default
        case .error:
            return .error
        }
    }
}

// MARK: - Control channel box

/// Thread-safe holder so non-MainActor closures can reach the live channel.
final class ControlChannelBox: @unchecked Sendable {
    private let lock = OSAllocatedUnfairLock<ControlChannel?>(initialState: nil)
    var value: ControlChannel? {
        get { lock.withLock { $0 } }
        set { lock.withLock { $0 = newValue } }
    }
}

// MARK: - Hot-path stream counters

/// Written from the link queue on every datagram/frame; drained by the
/// MainActor flush loop. The unfair lock costs nanoseconds where a per-event
/// `Task { @MainActor }` cost a scheduler hop.
final class StreamCounters: @unchecked Sendable {
    struct Snapshot {
        var datagrams = 0
        var datagramBytes = 0
        var assembled = 0
        var assembledBytes = 0
        var parsed = 0
    }

    private let lock = OSAllocatedUnfairLock(initialState: Snapshot())

    func recordDatagram(bytes: Int) {
        lock.withLock {
            $0.datagrams += 1
            $0.datagramBytes += bytes
        }
    }

    func recordAssembled(bytes: Int) {
        lock.withLock {
            $0.assembled += 1
            $0.assembledBytes += bytes
        }
    }

    func recordParsed() {
        lock.withLock { $0.parsed += 1 }
    }

    func drain() -> Snapshot {
        lock.withLock { current in
            let snapshot = current
            current = Snapshot()
            return snapshot
        }
    }
}

// MARK: - Thread-safe single-slot pixel buffer
// CVPixelBuffer is not Sendable in strict concurrency. We wrap it in an
// unchecked-Sendable box so the lock-protected slot can be shared across
// queues safely (the lock serialises all access).

final class PixelBufferBox: @unchecked Sendable {
    let buffer: CVPixelBuffer

    init(_ buffer: CVPixelBuffer) {
        self.buffer = buffer
    }
}

final class FrameSlot: @unchecked Sendable {
    /// The frame waiting to be drawn, and the newest decode sequence ever
    /// accepted, which `take()` keeps.
    private struct Slot {
        var frame: PixelBufferBox?
        var newestSequence: UInt64 = 0
    }
    private let lock = os.OSAllocatedUnfairLock<Slot>(initialState: Slot())

    /// Holder so the redraw callback can be swapped under a lock. The Metal
    /// view attaches and detaches it on the MainActor while the VideoToolbox
    /// decode thread is calling it; an unguarded stored closure raced on its
    /// own retain/release there, which crashes on connect and on teardown.
    private struct CallbackHolder: @unchecked Sendable {
        var callback: (() -> Void)?
    }
    private let stored = os.OSAllocatedUnfairLock<CallbackHolder>(
        initialState: CallbackHolder()
    )

    /// Fired (on the storing thread — the VideoToolbox callback) every time a
    /// frame lands, so the renderer can schedule an on-demand redraw.
    var onFrameStored: (() -> Void)? {
        get { stored.withLock { $0.callback } }
        set { stored.withLock { $0.callback = newValue } }
    }

    /// Stores the frame unless a frame submitted after it was already stored:
    /// decoder output can arrive out of order, and after a stream restart or
    /// a session rebuild the late frame is the old picture. The check and the
    /// store are one step under the lock, so two outputs can't both pass it.
    /// Returns whether the frame was stored.
    @discardableResult
    func set(_ buffer: CVPixelBuffer, sequence: UInt64) -> Bool {
        let box = PixelBufferBox(buffer)
        let accepted = lock.withLock { slot in
            guard sequence >= slot.newestSequence else { return false }
            slot.newestSequence = sequence
            slot.frame = box
            return true
        }
        guard accepted else { return false }
        // Copy the closure out under the lock, then call it outside: holding
        // the lock across arbitrary caller code invites deadlock, and a local
        // strong reference keeps a concurrent detach from freeing it mid-call.
        let callback = stored.withLock { $0.callback }
        callback?()
        return true
    }

    func take() -> CVPixelBuffer? {
        lock.withLock { slot in
            let wrapped = slot.frame
            slot.frame = nil
            return wrapped?.buffer
        }
    }
}

// MARK: - FPS counter

struct FPSCounter {
    private var timestamps: [CFTimeInterval] = []
    var currentFPS: Double = 0

    mutating func tick() {
        let now = CACurrentMediaTime()
        timestamps.append(now)
        timestamps.removeAll { now - $0 > 1.0 }
        currentFPS = Double(timestamps.count)
    }
}

// MARK: - Settings

final class AppSettings: ObservableObject {
    @Published var playPCaudio: Bool {
        didSet { UserDefaults.standard.set(playPCaudio, forKey: "playPCaudio") }
    }
    @Published var allowUSB: Bool {
        didSet { UserDefaults.standard.set(allowUSB, forKey: "allowUSB") }
    }
    @Published var showHUD: Bool {
        didSet { UserDefaults.standard.set(showHUD, forKey: "showHUD") }
    }
    /// Keep the display from dimming/locking while streaming (default on).
    @Published var keepScreenAwake: Bool {
        didSet { UserDefaults.standard.set(keepScreenAwake, forKey: "keepScreenAwake") }
    }
    /// Reconnect automatically when returning from the app switcher.
    @Published var autoResumeOnForeground: Bool {
        didSet { UserDefaults.standard.set(autoResumeOnForeground, forKey: "autoResumeOnForeground") }
    }
    /// Relay touch, pen and keyboard input (negotiated at connect).
    @Published var controlPC: Bool {
        didSet { UserDefaults.standard.set(controlPC, forKey: "controlPC") }
    }
    @Published var drawingMode: Bool {
        didSet { UserDefaults.standard.set(drawingMode, forKey: "drawingMode") }
    }
    @Published var commandAsControl: Bool {
        didSet { UserDefaults.standard.set(commandAsControl, forKey: "commandAsControl") }
    }
    @Published var autoReconnect: Bool {
        didSet { UserDefaults.standard.set(autoReconnect, forKey: "autoReconnect") }
    }
    @Published var targetFPS: Int {
        didSet { UserDefaults.standard.set(targetFPS, forKey: "targetFPS") }
    }
    /// Fit (default) shows the whole PC picture; Fill covers the iPad.
    @Published var pictureFit: PictureFit {
        didSet { UserDefaults.standard.set(pictureFit.rawValue, forKey: "pictureFit") }
    }
    // last successfully-connected host+port for pre-fill on relaunch.
    @Published var lastHost: String {
        didSet { UserDefaults.standard.set(lastHost, forKey: "lastHost") }
    }
    @Published var lastPort: UInt16 {
        didSet { UserDefaults.standard.set(Int(lastPort), forKey: "lastPort") }
    }

    init() {
        let defaults = UserDefaults.standard
        self.playPCaudio = defaults.flag(forKey: "playPCaudio")
        self.allowUSB = defaults.flag(forKey: "allowUSB")
        self.showHUD = defaults.flag(forKey: "showHUD")
        self.keepScreenAwake = defaults.flag(forKey: "keepScreenAwake")
        self.autoResumeOnForeground =
            defaults.flag(forKey: "autoResumeOnForeground")
        self.controlPC = defaults.flag(forKey: "controlPC")
        self.drawingMode = defaults.bool(forKey: "drawingMode")
        self.commandAsControl = defaults.flag(forKey: "commandAsControl")
        self.autoReconnect = defaults.flag(forKey: "autoReconnect")
        self.targetFPS = defaults.frameRatePreference()
        self.pictureFit = PictureFit(rawValue: defaults.string(forKey: "pictureFit") ?? "") ?? .fit
        self.lastHost = defaults.object(forKey: "lastHost") as? String ?? ""
        let port = defaults.integer(forKey: "lastPort")
        self.lastPort = UInt16(clamping: port)
    }
}

// MARK: - Recent connections persistence

struct RecentConnection: Codable, Identifiable {
    var id: String { "\(isUSB ? "usb" : "udp")|\(host):\(port)" }
    let host: String
    let port: UInt16
    let lastUsed: Date
    let isUSB: Bool
    /// The name the PC gave. Entries saved before 0.3.5 have none.
    var name: String? = nil
    /// The address is the one Bonjour listed for the PC. Only such an
    /// entry may move to a new address by itself; nil is unknown.
    var found: Bool? = nil
}

extension RecentConnection {
    /// Recent PCs that discovery has not already listed at the same address
    /// and port. Cable entries always stay; they connect another way.
    static func unlisted(_ recents: [RecentConnection], discovered: [DiscoveredHost]) -> [RecentConnection] {
        recents.filter { recent in
            recent.isUSB || !discovered.contains { $0.address == recent.host && $0.port == recent.port }
        }
    }
}

final class RecentConnectionStore: ObservableObject {
    static let shared = RecentConnectionStore()

    @Published var connections: [RecentConnection] = []

    private let key = "recentConnections"

    private init() {
        load()
    }

    /// Saves a connection that showed a picture. A Wi-Fi entry keeps its
    /// name and found mark when this connection gives none.
    func add(host: String, port: UInt16, isUSB: Bool, name: String? = nil, found: Bool = false) {
        let previous = isUSB ? nil : entry(host: host, port: port)
        connections.removeAll { $0.host == host && $0.port == port && $0.isUSB == isUSB }
        connections.insert(
            RecentConnection(host: host, port: port, lastUsed: Date(), isUSB: isUSB,
                             name: isUSB ? nil : name.flatMap { $0.isEmpty ? nil : $0 } ?? previous?.name,
                             found: isUSB ? nil : (found || previous?.found == true)),
            at: 0
        )
        if connections.count > 10 { connections = Array(connections.prefix(10)) }
        save()
    }

    /// The saved Wi-Fi entry for `host:port`.
    func entry(host: String, port: UInt16) -> RecentConnection? {
        connections.first { !$0.isUSB && $0.host == host && $0.port == port }
    }

    /// The PC moved from `old` to `new`, an address Bonjour gave. A home
    /// network entry gives way to it (`replacesOld`).
    func moved(from old: (host: String, port: UInt16), to new: (host: String, port: UInt16), name: String,
               replacesOld: Bool) {
        if replacesOld {
            connections.removeAll { !$0.isUSB && $0.host == old.host && $0.port == old.port }
        }
        add(host: new.host, port: new.port, isUSB: false, name: name, found: true)
    }

    private func load() {
        guard let data = UserDefaults.standard.data(forKey: key),
              let decoded = try? JSONDecoder().decode([RecentConnection].self, from: data) else { return }
        connections = decoded
    }

    private func save() {
        guard let data = try? JSONEncoder().encode(connections) else { return }
        UserDefaults.standard.set(data, forKey: key)
    }
}

private extension UserDefaults {
    // Foundation also accepts string-valued launch arguments used by UI tests.
    func flag(forKey key: String) -> Bool {
        object(forKey: key) == nil ? true : bool(forKey: key)
    }

    func frameRatePreference() -> Int {
        let fps = integer(forKey: "targetFPS")
        return [30, 60, 120].contains(fps) ? fps : 60
    }
}
