import Foundation
import Network

/// Only loopback is exposed. Apple's USB service forwards its tunnel to this
/// listener; other machines on the WiFi network cannot connect to it.
final class USBListener {
    enum State: Equatable {
        case stopped
        case listening(UInt16)
        case failed(String)
    }
    var onState: ((State) -> Void)?
    var onAccepted: ((FramedLink) -> Void)?

    private let port: UInt16
    private let queue = DispatchQueue(label: "com.eternal.usb.listener", qos: .userInitiated)
    private let queueKey = DispatchSpecificKey<UInt8>()
    private var listener: NWListener?
    private var pending: [UUID: FramedLink] = [:]

    init(port: UInt16 = 9877) {
        self.port = port
        queue.setSpecific(key: queueKey, value: 1)
    }

    deinit { stop() }

    func start() {
        queue.async { [self] in
            guard listener == nil else { return }
            do {
                let parameters = NWParameters.tcp
                parameters.allowLocalEndpointReuse = true
                let localPort = port == 0 ? NWEndpoint.Port.any : NWEndpoint.Port(rawValue: port)!
                parameters.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: localPort)
                let listener = try NWListener(using: parameters)
                self.listener = listener
                listener.stateUpdateHandler = { [weak self, weak listener] state in
                    guard let self, let listener, self.listener === listener else { return }
                    switch state {
                    case .ready: self.onState?(.listening(listener.port?.rawValue ?? self.port))
                    case .failed(let error):
                        self.stopOnQueue()
                        self.onState?(.failed(error.localizedDescription))
                    default: break
                    }
                }
                listener.newConnectionHandler = { [weak self, weak listener] connection in
                    guard let self, let listener, self.listener === listener, self.pending.count < 2 else { connection.cancel(); return }
                    let id = UUID()
                    let link = FramedLink(connection: connection)
                    self.pending[id] = link
                    link.onPreambleReady = { [weak self, weak link] in
                        guard let self, let link else { return }
                        self.queue.async { [weak self] in
                            guard let self, self.pending.removeValue(forKey: id) != nil else { link.stop(); return }
                            self.onAccepted?(link)
                        }
                    }
                    link.onClosed = { [weak self] in
                        self?.queue.async { [weak self] in self?.pending.removeValue(forKey: id) }
                    }
                    link.validatePreamble()
                }
                listener.start(queue: queue)
            } catch { onState?(.failed(error.localizedDescription)) }
        }
    }

    func stop() {
        if DispatchQueue.getSpecific(key: queueKey) != nil { stopOnQueue() }
        else { queue.sync { stopOnQueue() } }
    }

    private func stopOnQueue() {
        listener?.newConnectionHandler = nil
        listener?.stateUpdateHandler = nil
        listener?.cancel()
        listener = nil
        for link in pending.values { link.stop() }
        pending.removeAll()
        onState?(.stopped)
    }
}
