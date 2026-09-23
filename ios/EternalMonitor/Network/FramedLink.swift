import Foundation
import Network

/// An accepted USB tunnel. The preamble is validated before the app sees the
/// link; session setup then starts the same control and media path as UDP.
final class FramedLink: MediaLink {
    let datagrams = MediaDatagrams()
    let isUSB = true
    var onConnectionEstablished: (() -> Void)?
    var onError: ((String) -> Void)?
    var onListenerReady: ((UInt16) -> Void)?
    var onClosed: (() -> Void)?
    var onPreambleReady: (() -> Void)?

    private let queue = DispatchQueue(label: "com.eternal.usb", qos: .userInteractive)
    private let queueKey = DispatchSpecificKey<UInt8>()
    private var connection: NWConnection?
    private var framer = EMLinkFramer()
    private var preambleDeadline: DispatchWorkItem?
    private var announcedPreamble = false
    private var started = false
    private var bufferedPackets: [Data] = []
    private var pendingSends: [Data] = []
    private var sending = false

    var controlQueue: DispatchQueue { queue }

    init(connection: NWConnection) {
        self.connection = connection
        queue.setSpecific(key: queueKey, value: 1)
    }

    deinit { onQueue { stopOnQueue() } }

    private func onQueue<T>(_ operation: () -> T) -> T {
        if DispatchQueue.getSpecific(key: queueKey) != nil { return operation() }
        return queue.sync(execute: operation)
    }

    func validatePreamble() {
        queue.async { [self] in
            guard let connection else { return }
            let deadline = DispatchWorkItem { [weak self] in self?.fail("USB handshake timed out.") }
            preambleDeadline = deadline
            queue.asyncAfter(deadline: .now() + .seconds(2), execute: deadline)
            connection.stateUpdateHandler = { [weak self, weak connection] state in
                guard let self, let connection, self.connection === connection else { return }
                switch state {
                case .ready: self.receiveNext()
                case .failed(let error): self.fail("USB connection failed: \(error.localizedDescription)")
                default: break
                }
            }
            connection.start(queue: queue)
        }
    }

    @discardableResult
    func start(host: String) -> Bool {
        onQueue {
            guard connection != nil, framer.preambleReceived else { return false }
            guard !started else { return true }
            started = true
            onConnectionEstablished?()
            onListenerReady?(9877)
            let packets = bufferedPackets
            bufferedPackets.removeAll()
            for packet in packets { datagrams.handle(packet) }
            return true
        }
    }

    func send(_ data: Data) {
        let write = { [self] in
            guard connection != nil else { return }
            guard pendingSends.count < 64 else { fail("USB connection stopped responding."); return }
            do { pendingSends.append(try EMLinkFramer.encode(data)) }
            catch { fail("Invalid USB packet."); return }
            sendNext()
        }
        if DispatchQueue.getSpecific(key: queueKey) != nil { write() }
        else { queue.async(execute: write) }
    }

    private func sendNext() {
        guard let connection, !sending, !pendingSends.isEmpty else { return }
        sending = true
        let packet = pendingSends.removeFirst()
        connection.send(content: packet, completion: .contentProcessed { [weak self, weak connection] error in
            guard let self, let connection, self.connection === connection else { return }
            self.sending = false
            if let error { self.fail("USB send failed: \(error.localizedDescription)") }
            else { self.sendNext() }
        })
    }

    private func receiveNext() {
        guard let connection else { return }
        connection.receive(minimumIncompleteLength: 1, maximumLength: 16 * 1024) { [weak self, weak connection] data, _, complete, error in
            guard let self, let connection, self.connection === connection else { return }
            if let data, !data.isEmpty {
                do {
                    let packets = try self.framer.append(data)
                    if self.framer.preambleReceived && !self.announcedPreamble {
                        self.announcedPreamble = true
                        self.preambleDeadline?.cancel()
                        self.preambleDeadline = nil
                        self.onPreambleReady?()
                    }
                    if self.started {
                        for packet in packets { self.datagrams.handle(packet) }
                    } else {
                        guard self.bufferedPackets.count + packets.count <= 64 else {
                            self.fail("USB data arrived before a session was ready."); return
                        }
                        self.bufferedPackets.append(contentsOf: packets)
                    }
                } catch {
                    self.fail("USB protocol mismatch."); return
                }
            }
            if let error { self.fail("USB receive failed: \(error.localizedDescription)") }
            else if complete { self.finish() }
            else { self.receiveNext() }
        }
    }

    func stop() { onQueue { stopOnQueue() } }

    private func stopOnQueue() {
        preambleDeadline?.cancel()
        preambleDeadline = nil
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
        bufferedPackets.removeAll()
        pendingSends.removeAll()
        datagrams.reset()
    }

    private func fail(_ message: String) {
        guard connection != nil else { return }
        onError?(message)
        finish()
    }

    private func finish() {
        guard connection != nil else { return }
        stopOnQueue()
        onClosed?()
    }
}
