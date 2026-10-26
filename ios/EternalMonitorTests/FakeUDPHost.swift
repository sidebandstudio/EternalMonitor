import Darwin
import Foundation
@testable import EternalMonitor

/// A fake PC on a loopback UDP socket: accepts the HELLO2, then collects
/// whatever the iPad sends until it goes quiet.
final class FakeUDPHost: @unchecked Sendable {
    let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
    private(set) var port: UInt16 = 0
    /// The iPad whose HELLO2 was answered last.
    private var client: (address: sockaddr_storage, length: socklen_t)?
    /// HELLO2 nonces already answered, so a late copy of one isn't taken
    /// for the iPad's next attempt.
    private var answered = Set<UInt32>()

    init() {
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        var size = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bindSize = size
        port = withUnsafeMutablePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                _ = Darwin.bind(fd, $0, bindSize)
                _ = getsockname(fd, $0, &size)
            }
            return UInt16(bigEndian: pointer.pointee.sin_port)
        }
        var timeout = timeval(tv_sec: 0, tv_usec: 300_000)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    deinit { Darwin.close(fd) }

    private func receive() -> (Data, sockaddr_storage, socklen_t)? {
        var buffer = [UInt8](repeating: 0, count: 2048)
        var from = sockaddr_storage()
        var length = socklen_t(MemoryLayout<sockaddr_storage>.size)
        let count = withUnsafeMutablePointer(to: &from) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                recvfrom(fd, &buffer, buffer.count, 0, $0, &length)
            }
        }
        return count > 0 ? (Data(buffer.prefix(count)), from, length) : nil
    }

    /// Answers the next HELLO2 attempt, with session 41 unless `status`
    /// refuses it. False after ten quiet seconds.
    func acceptSession(status: HelloStatus = .ok, acceptedVersion: UInt8 = 2) -> Bool {
        for _ in 0..<33 {
            guard let (data, from, length) = receive() else { continue }
            guard case .hello2(let hello) = Wire.parseControl(data)?.message,
                  answered.insert(hello.clientNonce).inserted else { continue }
            let ack = Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .helloAck(HelloAck(
                status: status, acceptedVersion: acceptedVersion, clientNonce: hello.clientNonce, sessionId: status == .ok ? 41 : 0,
                heartbeatIntervalMs: 1000, reportIntervalMs: 500, livenessTimeoutMs: 3000,
                streamConfig: StreamConfig(), hostName: "Test PC", authToken: Data(17...32), hostCaps: 0)))
            client = (from, length)
            send(ack)
            return true
        }
        return false
    }

    private func send(_ datagram: Data) {
        guard var (to, length) = client else { return }
        _ = datagram.withUnsafeBytes { bytes in
            withUnsafePointer(to: &to) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    sendto(fd, bytes.baseAddress, bytes.count, 0, $0, length)
                }
            }
        }
    }

    /// Ends session 41 from the PC's side.
    func sendBye(_ reason: ByeReason) {
        send(Wire.encodeControl(sessionId: 41, msgSeq: 2, message: .bye(reason)))
    }

    /// The 64x64 H.264 keyframe fixture.
    static func keyframe() throws -> Data {
        guard let url = Bundle(for: FakeUDPHost.self).url(forResource: "single_idr_64x64", withExtension: "h264",
                                                          subdirectory: "Fixtures") else { throw CocoaError(.fileNoSuchFile) }
        return try Data(contentsOf: url)
    }

    /// Streams `frame` to the accepted iPad as the session's first keyframe.
    func sendKeyframe(_ frame: Data) {
        let size = MediaHeader.maxPayload
        let count = (frame.count + size - 1) / size
        for index in 0..<count {
            let payload = Data(frame[(frame.startIndex + index * size)..<min(frame.endIndex, frame.startIndex + (index + 1) * size)])
            send(MediaHeader(sessionId: 41, streamEpoch: 1, frameSeq: 1, fragIndex: UInt16(index),
                fragCount: UInt16(count), isKeyframe: true, captureTimestampUs: 16_667,
                payloadLen: UInt16(payload.count)).encode(payload: payload))
        }
    }

    /// The BYE reasons received until the iPad has been quiet for 300 ms.
    /// Unanswered HELLO2 attempts (distinct client nonces) received within
    /// `seconds`.
    func helloAttempts(within seconds: Double) -> Int {
        var nonces = Set<UInt32>()
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            guard let (data, _, _) = receive() else { continue }
            if case .hello2(let hello) = Wire.parseControl(data)?.message, !answered.contains(hello.clientNonce) {
                nonces.insert(hello.clientNonce)
            }
        }
        return nonces.count
    }

    func goodbyes() -> [ByeReason] {
        var reasons: [ByeReason] = []
        while let (data, _, _) = receive() {
            if case .bye(let reason) = Wire.parseControl(data)?.message { reasons.append(reason) }
        }
        return reasons
    }
}

