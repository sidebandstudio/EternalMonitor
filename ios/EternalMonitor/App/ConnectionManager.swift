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
    /// so the PC tells only older iPad apps to update.
    nonisolated static func featureCaps(wantsInput: Bool, wantsAudio: Bool, isUSB: Bool) -> UInt16 {
        (wantsInput ? Hello2.featureWantsInput : 0) | (wantsAudio ? Hello2.featureWantsAudio : 0)
            | (isUSB ? 0 : Hello2.featureSupportsNack) | Hello2.featureChecksUpdates
    }
    private var awaitingPairing = false
    private var explicitPairingToken: Data?
    private let pairingStore = PairingStore.shared
    @Published private(set) var diagnostics: [DiagnosticEntry] = []
    /// Live stream health for the HUD, recomputed at the 4 Hz flush.
    @Published private(set) var stats = StreamStats()
    /// Host heartbeats/frames stopped arriving; the frozen frame is stale.
    @Published private(set) var signalLost = false
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
    private var mediaLink: (any MediaLink)?
    private var usbListener: USBListener?
    private var isForeground = true
    private var usbPaused = false
    private let currentAttempt = OSAllocatedUnfairLock<UUID?>(initialState: nil)
    private var frameAssembler: FrameAssembler?
    private var controlChannel: ControlChannel?
    private var videoDecoder: VideoDecoder?
    private var audioPlayer: AudioPlayer?
    private var audioPreference: Bool?
    private var timeoutTask: Task<Void, Never>?
    private var statsFlushTask: Task<Void, Never>?
    /// Hot-path counters written on the link queue and drained at 4 Hz — the
    /// old per-datagram MainActor Task (~2 per datagram, ~2,800 hops/s at
    /// 1080p60) throttled the whole receive path.
    private let streamCounters = StreamCounters()
    private var debugState = ConnectionDebugState()
    // Whether we've already granted the one-time timeout extension that fires when datagrams
    // start arriving (so a slow/jittery network gets a fresh window to finish reassembly+decode).
    private var didExtendTimeout = false

    let frameSlot = FrameSlot()
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

    /// Connect and reconnect timing. The app uses the defaults; tests pass
    /// shorter ones when they create the manager.
    struct Timing {
        var connectTimeoutSeconds = Double(ConnectionManager.connectionTimeoutSeconds)
        var reconnectGraceSeconds = 2.0
        var reconnectBackoffSeconds: (Int) -> Double = { attempt in Double(min(1 << (attempt - 1), 15)) }
    }
    private let timing: Timing

    init(timing: Timing = Timing()) {
        self.timing = timing
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
        let listener = USBListener()
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
            disconnect(reason: .superseded)
        }
        connect(target: .usb(link))
    }

    func resumeUSBConnections() {
        usbPaused = false
        refreshUSBAvailability()
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

    func connect(host: String, port: UInt16, token: Data? = nil) {
        guard state == .disconnected else { return }
        explicitPairingToken = token
        if let token {
            do { try pairingStore.save(token: token, hostName: "", address: PairingStore.address(host: host, port: port)) }
            catch { record(.warning, "pairing", "Could not save the scanned pairing on this iPad.") }
        }
        connect(target: .udp(host: host, port: port))
    }

    func submitPairing() {
        guard awaitingPairing, let code = pairingPrompt?.submit() else { return }
        controlChannel?.retryPairing(code: code)
    }

    func pairingSheetDismissed() {
        if awaitingPairing { cancel() }
    }

    func forgetPairedHosts() {
        do { try pairingStore.forgetAll() }
        catch { connectionError = "Could not remove saved pairings from this iPad." }
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
            port = targetPort
            receiver = UDPReceiver(port: port)
            lastTarget = (normalizedHost, port)
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
        let pairingToken = explicitPairingToken ?? ((try? pairingStore.token(address: pairingAddress)) ?? Data(repeating: 0, count: 16))
        explicitPairingToken = nil
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
        degradedSinceUs = nil
        prevCounters = FrameAssembler.Counters()
        diagnostics.removeAll()
        didExtendTimeout = false
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

        decoder.onFrameDecoded = { [weak self] pixelBuffer, timestampUs in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            // Store frame in lock-protected slot (no Sendable boundary crossed)
            self.frameSlot.set(pixelBuffer)
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
                    self.record(.info, "decode", "First frame decoded successfully")
                    self.timeoutTask?.cancel()
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
                    RecentConnectionStore.shared.add(host: isUSB ? (self.hostInfo?.hostName ?? "USB host") : normalizedHost,
                        port: port, isUSB: isUSB)
                    if !isUSB {
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
                if ts > 0,
                   let offset = self.controlChannelBox.value?.clockSnapshot.withLock({ $0.offsetUs }) {
                    let nowUs = ControlChannel.clientNowUs()
                    let captureClient = Int64(bitPattern: ts) - offset
                    let latencyUs = Int64(bitPattern: nowUs) - captureClient
                    self.lagMs = latencyUs > 0 ? Double(latencyUs) / 1000.0 : 0
                }
            }
        }

        decoder.onNeedsKeyframe = { [weak self, weak controlChannelBox] in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            controlChannelBox?.value?.sendKeyframeRequest(
                streamEpoch: 0, lastCompleteSeq: 0, reason: .decodeError
            )
        }

        decoder.onEvent = { [weak self] message in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.info, "decode", message)
            }
        }

        assembler.onFrameAssembled = { [weak self, weak decoder] data, seq, captureTsUs, isKeyframe in
            guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
            self.streamCounters.recordAssembled(bytes: data.count)
            self.streamCounters.recordParsed()
            decoder?.decode(packet: FramePacket(
                seq: seq,
                timestampUs: captureTsUs,
                data: data,
                width: 0,
                height: 0,
                isKeyframe: isKeyframe
            ))
        }

        receiver.assembler = assembler

        let channel = ControlChannel(queue: receiver.controlQueue, send: { [weak receiver] data in
            receiver?.send(data)
        })
        controlChannelBox.value = channel
        assembler.rttUs = { [weak channel] in channel?.clockSnapshot.withLock { $0.rttUs } ?? 10_000 }
        assembler.onNack = { [weak channel] nack in channel?.sendNack(nack) }
        assembler.onNeedsKeyframe = { [weak channel] epoch, seq in
            channel?.sendKeyframeRequest(streamEpoch: epoch, lastCompleteSeq: seq, reason: .gapLoss)
        }
        channel.reportProvider = { [weak assembler, weak channel, weak decoder, weak audio] in
            var report = metrics.makeReport(assembler: assembler?.counters.withLock { $0 } ?? .init(),
                decodeDepth: decoder?.decodeDepth ?? 0,
                clock: channel?.clockSnapshot.withLock { $0 } ?? .init(),
                now: ControlChannel.clientNowUs())
            let stats = audio?.stats ?? AudioStats()
            report.audioPacketsLost = UInt32(clamping: stats.lost)
            report.audioBufferMs = UInt16(clamping: stats.bufferMs)
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
        channel.onSessionEstablished = { [weak self, weak receiver, weak assembler] info in
            assembler?.repairEnabled = !isUSB && info.hostCaps & HelloAck.hostCapNack != 0
            assembler?.framePeriodUs = 1_000_000 / UInt64(max(1, info.streamConfig.fps))
            receiver?.setAcceptedSessionId(info.sessionId)
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.livenessTimeoutUs = UInt64(info.livenessTimeoutMs) * 1000
                self.hostInfo = info
                if self.awaitingPairing {
                    self.awaitingPairing = false
                    self.pairingPrompt = nil
                    self.armConnectTimeout(seconds: self.timing.connectTimeoutSeconds)
                }
                if PairingStore.valid(info.authToken) {
                    do { try self.pairingStore.save(token: info.authToken, hostName: info.hostName, address: pairingAddress) }
                    catch { self.record(.warning, "pairing", "Pairing was not saved on this iPad. Pair again next time.") }
                }
                if isUSB { self.usbStatus = "USB: connected to \(info.hostName)" }
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
                self.disconnect()
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
                    self.disconnect()
                } else {
                    self.record(.warning, "ctrl", "HELLO2 handshake got no reply yet")
                }
            }
        }
        channel.onBye = { [weak self] reason in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.record(.info, "ctrl", "Host BYE (\(reason))")
                switch reason {
                case .hostShuttingDown:
                    // Only Stop and update on the PC sends this. The PC comes
                    // back on the new version, so reconnect as for a restart.
                    if self.reconnectCycleID != nil {
                        self.disconnect()
                        self.reconnectFailure = .pcUpdating(.reconnecting)
                        self.problem = .pcUpdating(.reconnecting)
                    } else {
                        self.beginReconnect(.pcStoppedResponding, failure: .pcUpdating(.reconnecting))
                    }
                case .userDisconnect, .superseded:
                    // Disconnected from the PC, or another device took over:
                    // stay away until the user connects again.
                    self.endReconnectCycle()
                    self.disconnect()
                    self.problem = .pcEndedSession
                default:
                    // A PC that stops or restarts its stream may come back.
                    // A running cycle keeps going; otherwise start one.
                    if self.reconnectCycleID != nil {
                        self.disconnect()
                        self.problem = .pcEndedSession
                    } else {
                        self.beginReconnect(.pcStoppedResponding, failure: .pcEndedSession)
                    }
                }
            }
        }
        channel.onHeartbeat = { [weak self, weak assembler] heartbeat in
            assembler?.framePeriodUs = 1_000_000 / UInt64(max(1, heartbeat.streamConfig.fps))
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                self.hostStreamConfig = heartbeat.streamConfig
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
                self.record(.error, isUSB ? "usb" : "udp", "state=\(self.state) \(error.message)")
                let failure = ConnectionProblem.linkFailure(error, isUSB: isUSB, host: normalizedHost)
                // A live stream rides out transient errors; a USB link that
                // failed for good closes next, and that close reports this.
                guard self.state == .connecting else {
                    if isUSB { self.linkFailure = failure }
                    return
                }
                guard isUSB else {
                    self.disconnect()
                    self.problem = failure
                    return
                }
                // Decide once, here, as a close would: fall back to Wi-Fi when
                // automatic reconnecting is on, otherwise show the cable
                // advice. beginReconnect disconnects, which retires
                // connectionID, so the close FramedLink sends next is ignored
                // instead of relabelling this as an unplugged cable.
                self.usbStatus = "USB: waiting for a cable"
                self.beginReconnect(.cableUnplugged, failure: failure)
            }
        }

        receiver.onClosed = { [weak self] in
            Task { @MainActor in
                guard let self, self.currentAttempt.withLock({ $0 == connectionID }) else { return }
                if isUSB { self.usbStatus = "USB: waiting for a cable" }
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
            disconnect()
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

                // Runs on EVERY tick, including silent ones: a dead host sends
                // neither media nor heartbeats, so skipping this when nothing
                // arrived is exactly when the liveness watchdog is needed.
                self.refreshStatsAndWatchdog()
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

        // --- Liveness watchdog (host heartbeats stopped => stale picture) ---
        guard state == .connected else { return }
        let lastHeartbeat = channel.lastHeartbeatAtUs.withLock { $0 }
        guard lastHeartbeat > 0 else { return }
        let sinceUs = ControlChannel.clientNowUs() &- lastHeartbeat
        if sinceUs > livenessTimeoutUs {
            if !signalLost {
                signalLost = true
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

    private func beginReconnect(_ reason: ConnectionProblem.LostReason, failure: ConnectionProblem? = nil) {
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
        disconnect()
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

    /// Retires the automatic reconnect cycle, if one is running: its task
    /// stops at its next check and can no longer write state.
    private func endReconnectCycle() {
        reconnectTask?.cancel()
        reconnectTask = nil
        reconnectCycleID = nil
        reconnectFailure = nil
        reconnectAttempt = 0
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
                self.record(.error, "timeout", debug.timeoutSummary(handshakeDone: handshakeDone))
                self.problem = .timeout(
                    listenerReady: debug.listenerReadyPort != nil, helloAttempts: debug.helloAttempts,
                    handshakeDone: handshakeDone, datagramsReceived: debug.datagramsReceived,
                    isUSB: self.transportMode == "USB", host: debug.host)
                self.disconnect()
            }
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
    }

    /// Try the last connection again: the cable when that is what failed,
    /// otherwise the last PC address. Cable advice kept after a Wi-Fi
    /// fallback ran out still means the cable, though Wi-Fi was tried last.
    func retry() {
        guard state == .disconnected else { return }
        let cable = transportMode == "USB" || problem == .usbFailed
        endReconnectCycle()
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
        disconnect(reason: .appBackground)
        if let cycleOutcome { problem = cycleOutcome }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
            guard let self, self.backgroundGoodbyeTask == goodbye else { return }
            self.endBackgroundGoodbye()
        }
    }

    /// Resume the session that backgrounding interrupted (opt-out toggle).
    func handleAppForegrounded() {
        endBackgroundGoodbye()
        isForeground = true
        usbPaused = false
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

    func disconnect(reason: ByeReason = .userDisconnect) {
        if state != .disconnected {
            recordStreamSummary(reason: "disconnect=\(reason)")
            record(.info, "connection", "Disconnect state=\(state) reason=\(reason)")
        }
        awaitingPairing = false
        pairingPrompt = nil
        currentAttempt.withLock { $0 = nil }
        timeoutTask?.cancel()
        timeoutTask = nil
        statsFlushTask?.cancel()
        statsFlushTask = nil
        didExtendTimeout = false

        // Say goodbye first (fire-and-forget), then stop the socket, then shut
        // the decoder down on its own queue (provably after any in-flight decode).
        controlChannel?.sendBye(reason)
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
        degradedSinceUs = nil
        prevCounters = FrameAssembler.Counters()
        hostInfo = nil
        hostStreamConfig = nil
        UIApplication.shared.isIdleTimerDisabled = false
    }

    private func recordStreamSummary(reason: String) {
        let counters = frameAssembler?.counters.withLock { $0 } ?? .init()
        let clock = controlChannel?.clockSnapshot.withLock { $0 }
        SupportDiagnostics.shared.record("info", "stream", "reason=\(reason) state=\(state) transport=\(transportMode) session_id=\(hostInfo?.sessionId ?? 0) decoded=\(debugState.decodedFrames) assembled=\(debugState.assembledFrames) datagrams=\(debugState.datagramsReceived) bytes=\(debugState.datagramBytesReceived) fps=\(fps) size=\(videoSize) depth=\(videoDecoder?.decodeDepth ?? 0) dropped=\(counters.framesDropped) repaired=\(counters.fragsRepaired) nacks=\(counters.nacksSent) jitter_us=\(counters.jitterUs) rtt_us=\(clock?.rttUs ?? 0) e2e_ms=\(lagMs) audio=\(audioStats) thermal=\(ProcessInfo.processInfo.thermalState.rawValue) low_power=\(ProcessInfo.processInfo.isLowPowerModeEnabled)")
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
    private let lock = os.OSAllocatedUnfairLock<PixelBufferBox?>(initialState: nil)

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

    func set(_ buffer: CVPixelBuffer) {
        let box = PixelBufferBox(buffer)
        lock.withLock { $0 = box }
        // Copy the closure out under the lock, then call it outside: holding
        // the lock across arbitrary caller code invites deadlock, and a local
        // strong reference keeps a concurrent detach from freeing it mid-call.
        let callback = stored.withLock { $0.callback }
        callback?()
    }

    func take() -> CVPixelBuffer? {
        lock.withLock { current in
            let wrapped = current
            current = nil
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

    func add(host: String, port: UInt16, isUSB: Bool) {
        connections.removeAll { $0.host == host && $0.port == port && $0.isUSB == isUSB }
        connections.insert(
            RecentConnection(host: host, port: port, lastUsed: Date(), isUSB: isUSB),
            at: 0
        )
        if connections.count > 10 { connections = Array(connections.prefix(10)) }
        save()
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
