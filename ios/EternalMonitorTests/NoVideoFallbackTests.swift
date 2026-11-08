import Combine
import XCTest
import os
@testable import EternalMonitor

/// Pairings kept in memory.
private final class MemoryPairings: PairingStorage {
    var values: [String: Data] = [:]
    func records() throws -> [PairingRecord] { values.map { PairingRecord(account: $0.key, token: $0.value) } }
    func save(account: String, token: Data) throws { values[account] = token }
    func remove(account: String?) throws {
        if let account { values.removeValue(forKey: account) } else { values.removeAll() }
    }
}

/// A PC that accepts the session and sends video, over a path that drops
/// large datagrams or all of them: the iPad asks for smaller ones, then
/// tries the PC's other address.
@MainActor
final class NoVideoFallbackTests: XCTestCase {
    nonisolated private static let token = Data(17...32)
    /// The other address the first fake signs; the manager dials it at the
    /// second fake.
    nonisolated private static let otherAddress = Data([192, 0, 2, 7])
    nonisolated private static let bothAddresses = [Data([127, 0, 0, 1]), otherAddress]

    override func setUp() async throws {
        let store = RecentConnectionStore.shared
        let saved = store.connections
        let defaults = ["recentConnections", "lastHost", "lastPort", "noVideoPaths"]
            .map { ($0, UserDefaults.standard.object(forKey: $0)) }
        addTeardownBlock { @MainActor in
            store.connections = saved
            for (key, value) in defaults { UserDefaults.standard.set(value, forKey: key) }
        }
        UserDefaults.standard.removeObject(forKey: "noVideoPaths")
        store.connections = []
    }

    private func until(_ description: String, seconds: Double = 60, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    /// A manager whose steps come 0.3 s apart, paired with "Test PC" at
    /// `pc`, reaching every address directly, and dialling the other
    /// address the PC signs at `other`.
    private func manager(pairedAt pc: FakeUDPHost, other: FakeUDPHost? = nil,
                         connectTimeout: Double = 60) throws -> (ConnectionManager, PairingStore) {
        let pairings = PairingStore(storage: MemoryPairings())
        try pairings.save(token: Self.token, hostName: "Test PC", address: PairingStore.address(host: "127.0.0.1", port: pc.port))
        var timing = ConnectionManager.Timing(connectTimeoutSeconds: connectTimeout, reconnectGraceSeconds: 0.1,
                                              reconnectBackoffSeconds: { _ in 0.05 })
        timing.noVideoStepSeconds = 0.3
        let manager = ConnectionManager(timing: timing, pairingStore: pairings)
        manager.homeAddress = { _ in true }
        manager.localRoute = { _, _ in MovedPC.Route(interface: "en0", direct: true, address: "127.0.0.1", netmask: "255.0.0.0") }
        if let other {
            manager.dialAddress = { host, port in host == "192.0.2.7" ? ("127.0.0.1", other.port) : (host, port) }
        }
        return (manager, pairings)
    }

    /// Every line the manager logs, across attempts (each clears the list).
    private func lines(_ manager: ConnectionManager) -> (() -> [String], AnyCancellable) {
        var lines: [String] = []
        let sink = manager.$diagnostics.sink { if let line = $0.last?.message { lines.append(line) } }
        return ({ lines }, sink)
    }

    // MARK: - The steps

    /// Each step waits for frames the PC says it sent after the last one,
    /// and none comes while the PC sends nothing.
    func testStepsWaitForVideoThePCSaysItSent() {
        var steps = NoVideoFallback(wait: 1.5)
        XCTAssertNil(steps.next(at: 100))
        steps.heard(framesSent: 0, at: 0)
        XCTAssertNil(steps.next(at: 100), "a PC that sent nothing yet")
        steps.heard(framesSent: 3, at: 100)
        XCTAssertNil(steps.next(at: 101))
        XCTAssertEqual(steps.next(at: 101.5), .smallerPackets(1200))
        XCTAssertNil(steps.next(at: 110))
        steps.heard(framesSent: 3, at: 110)
        XCTAssertNil(steps.next(at: 120), "nothing sent since the step")
        steps.heard(framesSent: 4, at: 120)
        steps.heard(framesSent: 9, at: 121)
        XCTAssertNil(steps.next(at: 121.4))
        XCTAssertEqual(steps.next(at: 121.5), .smallerPackets(576))
        steps.heard(framesSent: 12, at: 122)
        XCTAssertEqual(steps.next(at: 123.5), .otherAddress)
        steps.heard(framesSent: 20, at: 124)
        XCTAssertNil(steps.next(at: 200), "then no more")
    }

    // MARK: - Connecting

    /// Datagrams over 1,000 bytes never arrive; only each frame's short last
    /// fragment does. The iPad asks for 1,200 bytes, then 576, and streams.
    func testAPathThatDropsLargeDatagramsStreamsWithSmallerOnes() async throws {
        let pc = FakeUDPHost()
        let (manager, _) = try manager(pairedAt: pc)
        defer { manager.cancel() }
        let (logged, sink) = lines(manager)
        defer { sink.cancel() }
        let streaming = OSAllocatedUnfairLock(initialState: false)
        manager.connect(host: "127.0.0.1", port: pc.port)
        let frame = try FakeUDPHost.keyframe()
        let served = Task.detached {
            pc.servePath(limit: 1000, frame: frame, hostCaps: HelloAck.hostCapMediaSize) { streaming.withLock { $0 } }
        }
        try await until("streaming") { manager.state == .connected }
        streaming.withLock { $0 = true }
        let run = await served.value
        XCTAssertEqual(run.sizesAsked, [1200, 576])
        XCTAssertEqual(run.identifyAnswers, 0, "smaller datagrams were enough")
        let steps = logged().filter { $0.hasPrefix("no_video_fallback ") }
        XCTAssertEqual(steps, ["no_video_fallback step=smaller_packets size=1200 result=no_video",
                               "no_video_fallback step=smaller_packets size=576 result=video"])
    }

    /// The first address carries no video at all. Asked there, the PC signs
    /// another of its addresses; at that one it shows again that it holds
    /// the pairing before the token goes there, and the iPad streams from it
    /// with no tap. Connecting to the first address later goes there first.
    func testAnAddressThatDropsAllVideoMovesToTheVerifiedOtherAddress() async throws {
        let first = FakeUDPHost(), other = FakeUDPHost()
        let (manager, pairings) = try manager(pairedAt: first, other: other)
        defer { manager.cancel() }
        let (logged, sink) = lines(manager)
        defer { sink.cancel() }
        let moved = OSAllocatedUnfairLock(initialState: false)
        manager.connect(host: "127.0.0.1", port: first.port)
        let frame = try FakeUDPHost.keyframe()
        let served = Task.detached {
            first.servePath(limit: 0, frame: frame, hostCaps: HelloAck.hostCapMediaSize,
                            addresses: Self.bothAddresses) { moved.withLock { $0 } }
        }
        let accepted = await Task.detached {
            other.answerIdentify(token: Self.token) && other.acceptSession(authToken: Self.token)
        }.value
        XCTAssertTrue(accepted, "the iPad never checked and tried the other address")
        moved.withLock { $0 = true }
        let run = await served.value
        try await until("handshake there") { manager.hostInfo != nil && manager.debugState.port == other.port }
        other.sendKeyframe(frame)
        try await until("streaming") { manager.state == .connected }

        XCTAssertEqual(run.sizesAsked, [1200, 576])
        XCTAssertGreaterThan(run.identifyAnswers, 0)
        XCTAssertFalse(other.helloBeforeIdentify, "nothing with the token before the PC showed it is the same there")
        XCTAssertEqual(other.lastAuthToken, Self.token)
        let all = logged()
        let steps = all.filter { $0.hasPrefix("no_video_fallback ") }
        XCTAssertEqual(steps, ["no_video_fallback step=smaller_packets size=1200 result=no_video",
                               "no_video_fallback step=smaller_packets size=576 result=no_video",
                               "no_video_fallback step=other_address result=moved"])
        XCTAssertTrue(all.contains("Disconnect state=connecting reason=noVideoPath"), all.joined(separator: "\n"))
        let summary = all.first { $0.hasPrefix("session_summary ") } ?? ""
        XCTAssertTrue(summary.contains(" stage=accepted outcome=no_picture failure=no_video fallback=none end=superseded "), summary)

        // The working address is the saved one now; the first keeps its
        // pairing, as the PC still answers there.
        XCTAssertEqual(RecentConnectionStore.shared.connections.first?.port, other.port)
        XCTAssertNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: first.port))
        XCTAssertEqual(try pairings.token(address: PairingStore.address(host: "127.0.0.1", port: other.port)), Self.token)
        XCTAssertEqual(try pairings.token(address: PairingStore.address(host: "127.0.0.1", port: first.port)), Self.token)
        XCTAssertEqual(UserDefaults.standard.integer(forKey: "lastPort"), Int(other.port))

        // Connecting to the first address later goes to the other first,
        // once the PC there shows again that it holds the pairing.
        manager.cancel()
        manager.connect(host: "127.0.0.1", port: first.port)
        async let atFirst = Task.detached { first.received(within: 1) }.value
        let answered = await Task.detached {
            other.answerIdentify(token: Self.token) && other.acceptSession(authToken: Self.token)
        }.value
        XCTAssertTrue(answered, "the address that showed video goes first")
        XCTAssertFalse(other.helloBeforeIdentify, "nothing with the token before the proof")
        let firstTypes = await atFirst
        XCTAssertFalse(firstTypes.contains(.hello2), "\(firstTypes)")
        XCTAssertTrue(logged().contains("no_video_fallback step=remembered result=tried"), logged().joined(separator: "\n"))

        // Where nothing at the other address shows the proof, the attempt
        // goes to the first, and the other is forgotten.
        manager.cancel()
        manager.connect(host: "127.0.0.1", port: first.port)
        let backAtFirst = await Task.detached { first.helloAttempts(within: 8) }.value
        XCTAssertGreaterThan(backAtFirst, 0)
        XCTAssertTrue(logged().contains("no_video_fallback step=remembered result=not_confirmed"),
                      logged().joined(separator: "\n"))
        XCTAssertNil(NoVideoPaths.better(than: "127.0.0.1", port: first.port, pairing: Self.token))
    }

    /// The remembered address belongs to another PC now: one with pairing
    /// on, which can't make this pairing's proof, or one with pairing off,
    /// which would take any HELLO2 and doesn't answer the identity check.
    /// The token never goes there, the pairing stays, and the attempt goes
    /// to the address tapped.
    func testARememberedAddressThatIsNowAnotherPCNeverGetsTheToken() async throws {
        for pairingOn in [true, false] {
            let first = FakeUDPHost(), other = FakeUDPHost()
            let (manager, pairings) = try manager(pairedAt: first)
            defer { manager.cancel() }
            let (logged, sink) = lines(manager)
            defer { sink.cancel() }
            NoVideoPaths.remember(from: .init(host: "127.0.0.1", port: first.port),
                                  to: .init(host: "127.0.0.1", port: other.port), pairing: Self.token)
            manager.connect(host: "127.0.0.1", port: first.port)
            async let atOther = Task.detached { () -> [Wire.PacketType] in
                if pairingOn, !other.answerIdentify(token: Data(2...17)) { return [] }
                return other.received(within: 4)
            }.value
            let accepted = await Task.detached { first.acceptSession() }.value
            XCTAssertTrue(accepted, "\(pairingOn): the tapped address got no HELLO2")
            XCTAssertEqual(first.lastAuthToken, Self.token, "\(pairingOn)")
            let types = await atOther
            XCTAssertFalse(types.contains(.hello2), "\(pairingOn): \(types)")
            XCTAssertFalse(other.helloBeforeIdentify, "\(pairingOn)")
            if !pairingOn { XCTAssertEqual(Set(types), [.identify]) }
            try await until("handshake at the tapped address") { manager.hostInfo != nil }
            XCTAssertEqual(manager.debugState.port, first.port)
            XCTAssertEqual(try pairings.token(address: PairingStore.address(host: "127.0.0.1", port: first.port)), Self.token)
            XCTAssertNil(NoVideoPaths.better(than: "127.0.0.1", port: first.port, pairing: Self.token))
            XCTAssertTrue(logged().contains("no_video_fallback step=remembered result=not_confirmed"), "\(pairingOn)")
        }
        // An entry kept for another pairing is never used for this one.
        NoVideoPaths.remember(from: .init(host: "127.0.0.1", port: 1), to: .init(host: "127.0.0.1", port: 2),
                              pairing: Data(2...17))
        XCTAssertNil(NoVideoPaths.better(than: "127.0.0.1", port: 1, pairing: Self.token))
        XCTAssertEqual(NoVideoPaths.better(than: "127.0.0.1", port: 1, pairing: Data(2...17))?.port, 2)
    }

    /// A remembered address that is outside this iPad's own network now
    /// gets nothing: no identity check, no HELLO2. The attempt goes to the
    /// address tapped, and Try again goes there too.
    func testARememberedAddressOffThisNetworkIsNeverAsked() async throws {
        let first = FakeUDPHost(), other = FakeUDPHost()
        let (manager, pairings) = try manager(pairedAt: first)
        defer { manager.cancel() }
        manager.localRoute = { _, _ in MovedPC.Route(interface: "en0", direct: true, address: "10.0.0.5", netmask: "255.255.255.0") }
        let (logged, sink) = lines(manager)
        defer { sink.cancel() }
        NoVideoPaths.remember(from: .init(host: "127.0.0.1", port: first.port),
                              to: .init(host: "127.0.0.1", port: other.port), pairing: Self.token)
        manager.connect(host: "127.0.0.1", port: first.port)
        let accepted = await Task.detached { first.acceptSession() }.value
        XCTAssertTrue(accepted)
        XCTAssertEqual(first.lastAuthToken, Self.token)
        let types = await Task.detached { other.received(within: 1) }.value
        XCTAssertEqual(types, [])
        XCTAssertTrue(logged().contains("no_video_fallback step=remembered result=not_local"), logged().joined(separator: "\n"))
        XCTAssertNil(NoVideoPaths.better(than: "127.0.0.1", port: first.port, pairing: Self.token))
        XCTAssertEqual(try pairings.token(address: PairingStore.address(host: "127.0.0.1", port: first.port)), Self.token)
    }

    /// The PC at the remembered address passed its proof, then the attempt
    /// there failed: HELLO2 went unanswered, the session showed no frame,
    /// or the PC that answered had another name. The entry is forgotten,
    /// the attempt goes to the address tapped at once, and so does the
    /// next tap, with no identity check at the remembered address.
    func testARememberedAddressThatFailsAfterItsProofIsRetired() async throws {
        for failure in ["no_reply", "no_picture", "other_pc"] {
            let first = FakeUDPHost(), other = FakeUDPHost()
            // Well past HELLO2's 2 s of retries, so an unanswered HELLO2 always
            // gives up before the connect timeout, even on a slow runner.
            let (manager, _) = try manager(pairedAt: first, connectTimeout: 6)
            defer { manager.cancel() }
            let (logged, sink) = lines(manager)
            defer { sink.cancel() }
            NoVideoPaths.remember(from: .init(host: "127.0.0.1", port: first.port),
                                  to: .init(host: "127.0.0.1", port: other.port), pairing: Self.token)
            manager.connect(host: "127.0.0.1", port: first.port)
            let atOther = await Task.detached { () -> Bool in
                guard other.answerIdentify(token: Self.token) else { return false }
                switch failure {
                case "no_picture": return other.acceptSession(authToken: Self.token)
                case "other_pc": return other.acceptSession(hostName: "Other PC", authToken: Data(repeating: 0, count: 16))
                default: return other.helloAttempts(within: 1) > 0
                }
            }.value
            XCTAssertTrue(atOther, "\(failure): the proof passed and HELLO2 went there")
            let atFirst = await Task.detached { first.acceptSession() }.value
            XCTAssertTrue(atFirst, "\(failure): then to the address tapped")
            try await until("\(failure): handshake at the address tapped", seconds: 20) {
                manager.hostInfo != nil && manager.debugState.port == first.port
            }
            XCTAssertTrue(logged().contains("no_video_fallback step=remembered result=\(failure)"), logged().joined(separator: "\n"))
            XCTAssertNil(NoVideoPaths.better(than: "127.0.0.1", port: first.port, pairing: Self.token), failure)
            _ = await Task.detached { other.received(within: 0.5) }.value

            manager.cancel()
            manager.connect(host: "127.0.0.1", port: first.port)
            let again = await Task.detached { first.acceptSession() }.value
            XCTAssertTrue(again, "\(failure): the next tap goes to the address tapped")
            let types = await Task.detached { other.received(within: 0.5) }.value
            XCTAssertFalse(types.contains(.identify) || types.contains(.hello2), "\(failure): \(types)")
        }
    }

    /// The other address the PC signs is outside this iPad's own network:
    /// it is never asked, as for a PC Bonjour moved there.
    func testAnOtherAddressOffThisNetworkIsNeverAsked() async throws {
        let first = FakeUDPHost(), other = FakeUDPHost()
        let (manager, _) = try manager(pairedAt: first, other: other, connectTimeout: 8)
        defer { manager.cancel() }
        manager.localRoute = { _, _ in MovedPC.Route(interface: "en0", direct: true, address: "10.0.0.5", netmask: "255.255.255.0") }
        let (logged, sink) = lines(manager)
        defer { sink.cancel() }
        let ended = OSAllocatedUnfairLock(initialState: false)
        manager.connect(host: "127.0.0.1", port: first.port)
        let frame = try FakeUDPHost.keyframe()
        let served = Task.detached {
            first.servePath(limit: 0, frame: frame, hostCaps: HelloAck.hostCapMediaSize,
                            addresses: Self.bothAddresses) { ended.withLock { $0 } }
        }
        try await until("asked the PC for its addresses", seconds: 20) {
            logged().contains("no_video_fallback step=other_address result=none")
        }
        let types = await Task.detached { other.received(within: 1) }.value
        XCTAssertEqual(types, [])
        try await until("timed out", seconds: 20) { manager.state == .disconnected }
        ended.withLock { $0 = true }
        let run = await served.value
        XCTAssertGreaterThan(run.identifyAnswers, 0)
    }

    /// The other address the PC signs answers without the pairing's proof,
    /// as a forwarder or another device would: no HELLO2 and no token go
    /// there, and the attempt ends as before.
    func testAnOtherAddressThatDoesNotShowItIsTheSamePCIsLeftAlone() async throws {
        let first = FakeUDPHost(), other = FakeUDPHost()
        let (manager, _) = try manager(pairedAt: first, other: other, connectTimeout: 8)
        defer { manager.cancel() }
        let (logged, sink) = lines(manager)
        defer { sink.cancel() }
        let ended = OSAllocatedUnfairLock(initialState: false)
        manager.connect(host: "127.0.0.1", port: first.port)
        let frame = try FakeUDPHost.keyframe()
        let served = Task.detached {
            first.servePath(limit: 0, frame: frame, hostCaps: HelloAck.hostCapMediaSize,
                            addresses: Self.bothAddresses) { ended.withLock { $0 } }
        }
        let types = await Task.detached { () -> [Wire.PacketType] in
            guard other.answerIdentify(token: Data(2...17)) else { return [] }
            return other.received(within: 3)
        }.value
        XCTAssertFalse(types.contains(.hello2), "\(types)")
        XCTAssertFalse(other.helloBeforeIdentify)
        try await until("timed out", seconds: 20) { manager.state == .disconnected }
        ended.withLock { $0 = true }
        _ = await served.value
        XCTAssertEqual(manager.problem, .noPicture)
        XCTAssertTrue(logged().contains("no_video_fallback step=other_address result=none"), logged().joined(separator: "\n"))
        XCTAssertNil(UserDefaults.standard.data(forKey: "noVideoPaths"))
    }

    /// A PC that doesn't offer smaller datagrams, as every PC before this
    /// version: the iPad asks for nothing new and asks no identity, and the
    /// attempt times out as it always did.
    func testAnOlderPCIsLeftAsBefore() async throws {
        let pc = FakeUDPHost()
        let (manager, _) = try manager(pairedAt: pc, connectTimeout: 3)
        defer { manager.cancel() }
        let (logged, sink) = lines(manager)
        defer { sink.cancel() }
        let ended = OSAllocatedUnfairLock(initialState: false)
        manager.connect(host: "127.0.0.1", port: pc.port)
        let frame = try FakeUDPHost.keyframe()
        let served = Task.detached {
            pc.servePath(limit: 0, frame: frame, hostCaps: 0, countFrames: false) { ended.withLock { $0 } }
        }
        try await until("timed out", seconds: 20) { manager.state == .disconnected }
        ended.withLock { $0 = true }
        let run = await served.value
        XCTAssertEqual(run.sizesAsked, [])
        XCTAssertEqual(run.identifyAnswers, 0)
        XCTAssertEqual(manager.problem, .noPicture)
        let all = logged()
        XCTAssertFalse(all.contains { $0.hasPrefix("no_video_fallback ") }, all.joined(separator: "\n"))
        XCTAssertTrue(all.contains { $0.hasPrefix("Session accepted by 127.0.0.1:") && $0.contains("no video datagrams arrived") })
    }
}
