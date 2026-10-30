import Foundation
import Network

/// An accepted USB tunnel. The preamble is validated before the app sees the
/// link; session setup then starts the same control and media path as UDP.
final class FramedLink: MediaLink {
    let datagrams = MediaDatagrams()
    let isUSB = true
    var onConnectionEstablished: (() -> Void)?
    var onError: ((LinkError) -> Void)?
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
    private let record: (_ level: String, _ category: String, _ message: String) -> Void

    var controlQueue: DispatchQueue { queue }

    init(connection: NWConnection,
         record: @escaping (String, String, String) -> Void = { SupportDiagnostics.shared.record($0, $1, $2) }) {
        self.connection = connection
        self.record = record
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
            let deadline = DispatchWorkItem { [weak self] in self?.fail(LinkError(.timeout, "USB handshake timed out.")) }
            preambleDeadline = deadline
            queue.asyncAfter(deadline: .now() + .seconds(2), execute: deadline)
            connection.stateUpdateHandler = { [weak self, weak connection] state in
                guard let self, let connection, self.connection === connection else { return }
                self.handle(state)
            }
            connection.start(queue: queue)
        }
    }

    /// The connection's state, on the link's queue. A failure is logged once,
    /// by `fail`.
    func handle(_ state: NWConnection.State) {
        switch state {
        case .ready: receiveNext()
        case .failed(let error):
            fail(LinkError(.connectionFailed, "USB connection failed: \(error.localizedDescription)", underlying: error))
        default: break
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
            guard pendingSends.count < 64 else { fail(LinkError(.timeout, "USB connection stopped responding.")); return }
            do { pendingSends.append(try EMLinkFramer.encode(data)) }
            catch { fail(LinkError(.protocolMismatch, "Invalid USB packet.")); return }
            sendNext()
        }
        if DispatchQueue.getSpecific(key: queueKey) != nil { write() }
        else { queue.async(execute: write) }
    }

    private func sendNext() {
        guard let connection, !sending, !pendingSends.isEmpty else { return }
        sending = true
        // Coalesced Pencil samples already waiting can share one TCP write.
        // There is no timer or extra buffering delay, and framing preserves
        // every sample and edge in its original order.
        let packet = pendingSends.reduce(into: Data()) { $0.append($1) }
        pendingSends.removeAll(keepingCapacity: true)
        connection.send(content: packet, completion: .contentProcessed { [weak self, weak connection] error in
            guard let self, let connection, self.connection === connection else { return }
            self.sending = false
            if let error {
                self.fail(LinkError(.send, "USB send failed: \(error.localizedDescription)", underlying: error))
            }
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
                            self.fail(LinkError(.protocolMismatch, "USB data arrived before a session was ready.")); return
                        }
                        self.bufferedPackets.append(contentsOf: packets)
                    }
                } catch {
                    self.fail(LinkError(.protocolMismatch, "USB protocol mismatch.")); return
                }
            }
            if let error {
                self.fail(LinkError(.receive, "USB receive failed: \(error.localizedDescription)", underlying: error))
            }
            else if complete { self.finish() }
            else { self.receiveNext() }
        }
    }

    func stop() { onQueue { stopOnQueue(flush: true) } }

    private func stopOnQueue(flush: Bool = false) {
        preambleDeadline?.cancel()
        preambleDeadline = nil
        if let closing = connection {
            closing.stateUpdateHandler = nil
            if flush {
                // The active send is already queued in Network.framework.
                // Append our bounded control queue and BYE before write-close.
                for packet in pendingSends {
                    closing.send(content: packet, completion: .contentProcessed { _ in })
                }
                closing.send(content: nil, contentContext: .finalMessage, isComplete: true,
                    completion: .contentProcessed { _ in closing.cancel() })
                queue.asyncAfter(deadline: .now() + .milliseconds(250)) { closing.cancel() }
            } else {
                closing.cancel()
            }
        }
        connection = nil
        bufferedPackets.removeAll()
        pendingSends.removeAll()
        datagrams.reset()
    }

    private func fail(_ error: LinkError) {
        guard connection != nil else { return }
        // The connection that took this link logs the failure with its state;
        // a link still waiting for its preamble logs it here.
        if let onError { onError(error) } else { record("error", "usb", error.message) }
        finish()
    }

    private func finish() {
        guard connection != nil else { return }
        stopOnQueue()
        onClosed?()
    }
}
