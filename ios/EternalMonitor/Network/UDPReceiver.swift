import Foundation
import Darwin
import Network
import os

/// One UDP socket to the host: media fragments in, control datagrams both
/// ways. Binds an EPHEMERAL local port (advertised to the host in HELLO2);
/// demuxes inbound traffic by the v2 wire prefix — media goes to the
/// `FrameAssembler`, control to the `ControlChannel`.
final class UDPReceiver {
    enum Backend: String {
        case bsd
        case networkFramework = "nw"

        static var configured: Backend {
            let value = ProcessInfo.processInfo.environment["EM_UDP_BACKEND"]
                .flatMap { $0.isEmpty ? nil : $0 }
                ?? UserDefaults.standard.string(forKey: "udpBackend")
            // Both paths measured 60 FPS with zero drops in the simulator's
            // 1440p burst row. Keep the established default until device tests
            // demonstrate a benefit; the buffered path remains selectable.
            return value.flatMap(Backend.init(rawValue:)) ?? .networkFramework
        }
    }

    /// The host's control/media port we connect to.
    let port: UInt16
    var assembler: FrameAssembler?
    var onControlDatagram: ((Data) -> Void)?
    var onConnectionEstablished: (() -> Void)?
    var onError: ((String) -> Void)?
    var onListenerReady: ((UInt16) -> Void)?
    var onDatagramReceived: ((Int) -> Void)?
    var onDatagramIgnored: ((String) -> Void)?

    /// Media datagrams must carry this session id (set after HELLO_ACK);
    /// 0 = no session yet, drop all media.
    private let acceptedSessionId = OSAllocatedUnfairLock<UInt32>(initialState: 0)
    /// Count of v1-shaped datagrams (legacy host detection): 16+ bytes that
    /// classify as neither v2 nor the legacy hello.
    private let unknownDatagramCount = OSAllocatedUnfairLock<Int>(initialState: 0)

    private var connection: NWConnection?
    private var socketFD: Int32 = -1
    private var readSource: DispatchSourceRead?
    private var receiveBuffer = [UInt8](repeating: 0, count: 2048)
    private var repairTimer: DispatchSourceTimer?
    private let queue = DispatchQueue(label: "com.eternal.udp", qos: .userInteractive)
    private let queueKey = DispatchSpecificKey<UInt8>()
    private let backend: Backend

    init(port: UInt16, backend: Backend = .configured) {
        self.port = port
        self.backend = backend
        queue.setSpecific(key: queueKey, value: 1)
    }

    var controlQueue: DispatchQueue { queue }

    deinit { onQueue { stopOnQueue() } }

    private func onQueue<T>(_ operation: () -> T) -> T {
        if DispatchQueue.getSpecific(key: queueKey) != nil { return operation() }
        return queue.sync(execute: operation)
    }

    /// The actual kernel buffer, including a possible simulator clamp.
    var receiveBufferBytes: Int {
        onQueue {
            var size: Int32 = 0
            var length = socklen_t(MemoryLayout<Int32>.size)
            guard socketFD >= 0, getsockopt(socketFD, SOL_SOCKET, SO_RCVBUF, &size, &length) == 0 else { return 0 }
            return Int(size)
        }
    }

    func setAcceptedSessionId(_ id: UInt32) {
        acceptedSessionId.withLock { $0 = id }
    }

    /// v1-shaped datagrams observed (used to tell "old host" apart from "no host").
    var legacyLookingDatagrams: Int {
        unknownDatagramCount.withLock { $0 }
    }

    /// Start the socket toward `host`. HELLO2 is the ControlChannel's job once
    /// `onListenerReady` reports the ephemeral local port.
    @discardableResult
    func start(host: String) -> Bool {
        guard port != 0 else {
            onError?("Invalid port \(port)")
            return false
        }
        queue.async { [self] in
            stopOnQueue()
            switch backend {
            case .bsd: startBSD(host: host)
            case .networkFramework: startNetworkFramework(host: host)
            }
        }
        return true
    }

    private func startNetworkFramework(host: String) {
        let params = NWParameters.udp

        guard let remotePort = NWEndpoint.Port(rawValue: port) else {
            onError?("Invalid port \(port)")
            return
        }
        let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(host), port: remotePort)

        let connection = NWConnection(to: endpoint, using: params)
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let self, let connection, self.connection === connection else { return }
            switch state {
            case .ready:
                let actualPort = self.localPort() ?? 0
                print("[UDPReceiver] NW socket ready, ephemeral port \(actualPort)")
                self.onListenerReady?(actualPort)
                self.onConnectionEstablished?()
                self.startRepairTimer()
                self.receiveLoop()
            case .waiting(let error):
                print("[UDPReceiver] Connection waiting: \(error)")
                self.onError?("UDP path not ready: \(error.localizedDescription)")
            case .failed(let error):
                print("[UDPReceiver] Connection failed: \(error)")
                self.onError?("UDP connection failed: \(error.localizedDescription)")
                self.stopOnQueue()
            case .cancelled:
                break
            default:
                break
            }
        }

        self.connection = connection
        connection.start(queue: queue)
    }

    private func startBSD(host: String) {
        var hints = addrinfo()
        hints.ai_family = AF_UNSPEC
        hints.ai_socktype = SOCK_DGRAM
        hints.ai_protocol = IPPROTO_UDP
        var addresses: UnsafeMutablePointer<addrinfo>?
        let result = getaddrinfo(host, String(port), &hints, &addresses)
        guard result == 0, let first = addresses else {
            onError?("Cannot resolve \(host): \(String(cString: gai_strerror(result)))")
            return
        }
        defer { freeaddrinfo(first) }
        var current: UnsafeMutablePointer<addrinfo>? = first
        var lastError = "No UDP address available"
        while let address = current {
            current = address.pointee.ai_next
            let info = address.pointee
            let fd = Darwin.socket(info.ai_family, SOCK_DGRAM, IPPROTO_UDP)
            guard fd >= 0 else { lastError = String(cString: strerror(errno)); continue }
            let flags = fcntl(fd, F_GETFL)
            guard flags >= 0, fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0 else {
                lastError = String(cString: strerror(errno))
                Darwin.close(fd)
                continue
            }
            // Some simulator kernels cap this below the device allowance.
            for requested in [4, 2, 1].map({ Int32($0 * 1024 * 1024) }) {
                var size = requested
                if setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &size, socklen_t(MemoryLayout<Int32>.size)) == 0 { break }
            }
            var tos: Int32 = 0xB8
            let level = info.ai_family == AF_INET6 ? IPPROTO_IPV6 : IPPROTO_IP
            let option = info.ai_family == AF_INET6 ? IPV6_TCLASS : IP_TOS
            _ = setsockopt(fd, level, option, &tos, socklen_t(MemoryLayout<Int32>.size))
            var local = sockaddr_storage()
            local.ss_len = UInt8(info.ai_family == AF_INET6 ? MemoryLayout<sockaddr_in6>.size : MemoryLayout<sockaddr_in>.size)
            local.ss_family = sa_family_t(info.ai_family)
            let bindSize = socklen_t(local.ss_len)
            let bound = withUnsafePointer(to: &local) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, bindSize) }
            }
            guard bound == 0, Darwin.connect(fd, info.ai_addr, info.ai_addrlen) == 0 else {
                lastError = String(cString: strerror(errno))
                Darwin.close(fd)
                continue
            }
            socketFD = fd
            let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
            source.setEventHandler { [weak self] in self?.drainSocket(fd) }
            // Closing only after cancellation prevents a pending read handler
            // from accessing a descriptor the OS has already reused.
            source.setCancelHandler { Darwin.close(fd) }
            readSource = source
            source.resume()
            let actualPort = localPort() ?? 0
            print("[UDPReceiver] BSD socket ready, ephemeral port \(actualPort), SO_RCVBUF=\(receiveBufferBytes)")
            onListenerReady?(actualPort)
            onConnectionEstablished?()
            startRepairTimer()
            return
        }
        onError?("UDP socket failed: \(lastError)")
    }

    /// Send one datagram to the host (control plane). Safe from any thread.
    func send(_ data: Data) {
        let write: () -> Void = { [self] in
            if socketFD >= 0 {
                data.withUnsafeBytes { bytes in
                    _ = Darwin.send(socketFD, bytes.baseAddress, bytes.count, 0)
                }
            } else {
                connection?.send(content: data, completion: .contentProcessed { _ in })
            }
        }
        if DispatchQueue.getSpecific(key: queueKey) != nil { write() }
        else { queue.async(execute: write) }
    }

    /// The ephemeral local port the OS assigned to this connection's socket.
    func localPort() -> UInt16? {
        onQueue {
            if socketFD >= 0 {
                var address = sockaddr_storage()
                var size = socklen_t(MemoryLayout<sockaddr_storage>.size)
                return withUnsafeMutablePointer(to: &address) { pointer in
                    pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { raw in
                        guard getsockname(socketFD, raw, &size) == 0 else { return nil }
                        if raw.pointee.sa_family == sa_family_t(AF_INET6) {
                            return pointer.withMemoryRebound(to: sockaddr_in6.self, capacity: 1) { UInt16(bigEndian: $0.pointee.sin6_port) }
                        }
                        return pointer.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt16(bigEndian: $0.pointee.sin_port) }
                    }
                }
            }
            guard case .hostPort(_, let port)? = connection?.currentPath?.localEndpoint else { return nil }
            return port.rawValue
        }
    }

    func stop() {
        onQueue { stopOnQueue() }
    }

    private func stopOnQueue() {
        repairTimer?.setEventHandler {}
        repairTimer?.cancel()
        repairTimer = nil
        readSource?.setEventHandler {}
        readSource?.cancel()
        readSource = nil
        socketFD = -1
        // Break the handler → connection reference before cancel so each
        // connect/disconnect cycle can actually deallocate the NWConnection.
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
        acceptedSessionId.withLock { $0 = 0 }
        unknownDatagramCount.withLock { $0 = 0 }
    }

    // MARK: - Receive loop

    private func drainSocket(_ fd: Int32) {
        while socketFD == fd {
            let count = receiveBuffer.withUnsafeMutableBytes {
                recvfrom(fd, $0.baseAddress, $0.count, 0, nil, nil)
            }
            if count < 0 {
                if errno == EINTR { continue }
                if errno != EAGAIN && errno != EWOULDBLOCK && errno != ECONNREFUSED {
                    onError?("UDP receive error: \(String(cString: strerror(errno)))")
                }
                return
            }
            guard count <= Wire.maxDatagramSize else {
                onDatagramIgnored?("Dropped oversized datagram")
                continue
            }
            handleDatagram(Data(receiveBuffer.prefix(count)))
        }
    }

    private func startRepairTimer() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(1), leeway: .microseconds(250))
        timer.setEventHandler { [weak self] in self?.assembler?.tick() }
        repairTimer?.cancel()
        repairTimer = timer
        timer.resume()
    }

    private func receiveLoop() {
        guard let connection else { return }
        connection.receiveMessage { [weak self, weak connection] content, _, _, error in
            guard let self, let connection, self.connection === connection else { return }

            if let error {
                print("[UDPReceiver] Receive error: \(error)")
                // A transient error (ICMP port-unreachable while the host
                // restarts its pipeline, a path blip) must not silently kill
                // reception forever — re-arm after a short delay as long as the
                // connection object is still live.
                self.onError?("UDP receive error: \(error.localizedDescription)")
                if self.connection != nil {
                    self.queue.asyncAfter(deadline: .now() + .milliseconds(100)) { [weak self, weak connection] in
                        guard let self, let connection, self.connection === connection else { return }
                        self.receiveLoop()
                    }
                }
                return
            }

            if let data = content {
                self.handleDatagram(data)
            }

            // Continue receiving
            self.receiveLoop()
        }
    }

    private func handleDatagram(_ data: Data) {
        switch Wire.classify(data) {
        case .media:
            onDatagramReceived?(data.count)
            guard let (header, payloadRange) = MediaHeader.decode(data) else {
                onDatagramIgnored?("Dropped malformed media datagram (\(data.count) bytes)")
                return
            }
            let expected = acceptedSessionId.withLock { $0 }
            guard header.sessionId == expected, expected != 0 else {
                onDatagramIgnored?("Dropped media for foreign session \(header.sessionId)")
                return
            }
            assembler?.addFragment(
                seq: header.frameSeq,
                index: header.fragIndex,
                count: header.fragCount,
                epoch: header.streamEpoch,
                isKeyframe: header.isKeyframe,
                captureTimestampUs: header.captureTimestampUs,
                payload: data.subdata(in: payloadRange),
                isRetransmit: header.isRetransmit
            )
        case .control:
            onControlDatagram?(data)
        case .audio:
            // Playback is added in the audio-client phase. This client does
            // not advertise WANTS_AUDIO yet, so no host should send it audio.
            break
        case .legacyHello:
            // The host never sends this; ignore.
            break
        case .unknown:
            if data.count >= 16 {
                // Looks like v1 media from an old host — count it so the app
                // can say "update the Windows host" instead of "no host found".
                unknownDatagramCount.withLock { $0 += 1 }
            }
            onDatagramIgnored?("Ignored unrecognized datagram (\(data.count) bytes)")
        }
    }
}
