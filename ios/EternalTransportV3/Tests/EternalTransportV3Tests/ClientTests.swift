import XCTest
@testable import EternalTransportV3

/// The reconnect state machine, row by row: the iPad table of section 4.4,
/// the phase notes (rounds 3-6, gap 7, gap 14, PairAgain), and the Rust
/// reference client's own tests. A Swift test host answers through the same
/// Noise responder the golden tests check against the Rust host's bytes.
final class ClientTests: XCTestCase {
    let hostId = id16(0xB2)
    let keyA = clientKey(0xD2, 0x24)
    let keyB = clientKey(0xD3, 0x25)
    let t0: Duration = .seconds(50)
    var client: Client!

    override func setUp() {
        client = Client(config: ClientConfig(hostId: hostId), hello: HelloPayload(), random: SeededRandom(3))
    }

    // MARK: - Helpers

    func accept(_ sessionId: UInt32, liveness: UInt16 = 0, status: SlotStatus = SlotStatus()) -> Msg2Payload {
        var a = Accept()
        a.sessionId = sessionId
        a.livenessTimeoutMs = liveness
        a.slotStatus = status
        return .accept(a)
    }

    /// The host reads `msg1` under `key` and answers `reply`. Returns
    /// message 2 and, for an accept, the host's sealer and opener.
    func answer(_ msg1: [UInt8], _ reply: Msg2Payload, key: ClientKey? = nil) throws -> ([UInt8], Sealer?, Opener?) {
        let host = try TestResponder(msg1: msg1, kDev: (key ?? keyA).kDev, hostId: hostId)
        let (msg2, keys) = host.writeMsg2(reply)
        guard let keys, case .accept(let a) = reply else { return (msg2, nil, nil) }
        let (sealer, opener) = keys.responder(sessionId: a.sessionId)
        return (msg2, sealer, opener)
    }

    func sealed(_ sealer: Sealer, _ message: SessionMessage) -> [UInt8] {
        try! sealer.seal(innerType: message.innerType, body: message.encodeBody())
    }

    func heartbeat(_ sealer: Sealer) -> [UInt8] {
        try! sealer.seal(innerType: V2.heartbeatType, body: [UInt8](repeating: 0, count: 8))
    }

    func handle(_ datagram: [UInt8], at now: Duration, clock: UInt64 = 1_000) -> [ClientOutput] {
        client.handleDatagram(now: now, datagram: datagram, clockUs: clock)
    }

    /// Connects under `key` and goes live on `sessionId` through an ack.
    @discardableResult
    func goLive(_ sessionId: UInt32, key: ClientKey? = nil, at now: Duration? = nil, liveness: UInt16 = 0) throws -> (Sealer, Opener) {
        let now = now ?? t0
        let msg1 = try XCTUnwrap(client.connect(now: now, key: key ?? keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, opener) = try answer(msg1, accept(sessionId, liveness: liveness), key: key)
        _ = handle(msg2, at: now)
        _ = handle(sealed(sealer!, .confirmAck(SlotStatus())), at: now)
        XCTAssertEqual(client.liveSession, sessionId)
        return (sealer!, opener!)
    }

    // MARK: - Connecting and confirming

    func testCleanConnectConfirmsUnderTheNewKeysAndGoesLiveOnTheAck() throws {
        let out = client.connect(now: t0, key: keyA, clockUs: 1_000)
        XCTAssertEqual(out.count, 1)
        let msg1 = try XCTUnwrap(out.sent.first)
        XCTAssertTrue(client.isConnecting)
        let host = try TestResponder(msg1: msg1, kDev: keyA.kDev, hostId: hostId)
        XCTAssertEqual(host.msg1.devId, keyA.devId)
        XCTAssertEqual(host.payload.timestampUs, 1_000)

        let status = SlotStatus(held: [(keyA.devId, .active)])
        let (msg2, keys) = host.writeMsg2(accept(77, status: status))
        let (hostSealer, hostOpener) = try XCTUnwrap(keys).responder(sessionId: 77)
        let confirming = handle(msg2, at: t0)
        XCTAssertEqual(confirming.first, .releaseAllInput, "message 2 is the input cutover")
        XCTAssertEqual(confirming.count, 2)
        let confirm = try hostOpener.open(try XCTUnwrap(confirming.sent.first))
        XCTAssertEqual(confirm.innerType, SessionMessage.innerConfirm)
        XCTAssertEqual(confirm.counter, 0)
        XCTAssertTrue(client.isConfirming)
        XCTAssertNil(client.liveSession)

        let live = handle(sealed(hostSealer, .confirmAck(status)), at: t0)
        guard case .live(77, let a)? = live.first else { return XCTFail("\(live)") }
        XCTAssertEqual(a.slotStatus, status, "the HelloAck's status rides in the accept")
        XCTAssertEqual(live.last, .status(devId: keyA.devId, status: status))
        XCTAssertEqual(client.liveSession, 77)
        XCTAssertFalse(client.isConfirming)
        let input = try XCTUnwrap(client.seal(now: t0, innerType: V2.inputEventType, body: [1, 2, 3]))
        XCTAssertEqual(try hostOpener.open(input).body, [1, 2, 3])
    }

    func testMessage1IsResentByteForByteEvery250MsThenTheAttemptFails() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        var resends = 0
        for step in stride(from: 10, through: 1990, by: 10) {
            let out = client.poll(now: t0 + ms(step), clockUs: 1_000)
            for datagram in out.sent {
                XCTAssertEqual(datagram, msg1, "a retry reuses the same bytes and ephemeral")
                XCTAssertEqual(step % 250, 0, "only every 250 ms")
                resends += 1
            }
        }
        XCTAssertEqual(resends, 7)
        XCTAssertEqual(client.poll(now: t0 + ms(2000), clockUs: 1_000), [.attemptFailed(devId: keyA.devId)])
        XCTAssertFalse(client.isConnecting)
        XCTAssertEqual(client.poll(now: t0 + ms(5000), clockUs: 1_000), [])
    }

    func testConfirmsAreFreshRecordsEvery100MsThenANewAttemptStarts() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, _, opener) = try answer(msg1, accept(77))
        var counters = [try opener!.open(handle(msg2, at: t0).sent[0]).counter]
        for step in stride(from: 50, through: 1950, by: 50) {
            for datagram in client.poll(now: t0 + ms(step), clockUs: 1_000).sent {
                counters.append(try opener!.open(datagram).counter)
            }
        }
        XCTAssertEqual(counters, Array(0...19), "a fresh counter each time, every 100 ms")
        let out = client.poll(now: t0 + ms(2000), clockUs: 2_000)
        let retry = try XCTUnwrap(out.sent.first)
        XCTAssertNotEqual(retry, msg1, "a new attempt has a new ephemeral")
        XCTAssertTrue(client.isConnecting)
        XCTAssertEqual(client.abandonedCount, 1, "the confirming attempt is kept receive-only")
        XCTAssertGreaterThan(try TestResponder(msg1: retry, kDev: keyA.kDev, hostId: hostId).payload.timestampUs, 1_000)
    }

    func testWhileConfirmingOnlyInputIsHeldBack() throws {
        try goLive(77)
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 2_000).sent.first)
        let (msg2, _, _) = try answer(msg1, accept(78))
        _ = handle(msg2, at: t0)
        XCTAssertTrue(client.isConfirming)
        XCTAssertNil(client.seal(now: t0, innerType: V2.inputEventType, body: [UInt8](repeating: 0, count: 30)))
        XCTAssertNotNil(client.seal(now: t0, innerType: V2.receiverReportType, body: [UInt8](repeating: 0, count: 36)))
        XCTAssertNotNil(client.seal(now: t0, innerType: V2.pingType, body: [UInt8](repeating: 0, count: 8)))
    }

    func testARecordBeforeTheAckMakesTheSessionLiveAndConfirmsContinueUntilTheAck() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, opener) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        let out = handle(heartbeat(sealer!), at: t0 + ms(30))
        guard case .live(77, _)? = out.first,
              case .deliver(77, 0, V2.heartbeatType, _, false)? = out.last
        else { return XCTFail("\(out)") }
        XCTAssertFalse(client.isConfirming)
        XCTAssertNotNil(client.seal(now: t0 + ms(30), innerType: V2.inputEventType, body: [1]), "input flows once live")
        let confirm = try XCTUnwrap(client.poll(now: t0 + ms(100), clockUs: 1_000).sent.first)
        XCTAssertEqual(try opener!.open(confirm).innerType, SessionMessage.innerConfirm)
        XCTAssertEqual(handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(150)),
                       [.status(devId: keyA.devId, status: SlotStatus())])
        XCTAssertEqual(client.poll(now: t0 + ms(400), clockUs: 1_000).sent, [], "the ack stops the confirms")
    }

    func testCleartextMediaUnderTheNewSessionIdIsNeverProofOfLife() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, _, _) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        // A v2 media datagram naming session 77 in the clear.
        var media: [UInt8] = []
        Wire.appendPrefix(&media, packetType: V2.mediaType, bodyLength: 32)
        appendLE(&media, UInt32(77))
        media += [UInt8](repeating: 0x5A, count: 28)
        XCTAssertEqual(handle(media, at: t0), [])
        // A record naming 77 that doesn't authenticate.
        var forged: [UInt8] = []
        Wire.appendPrefix(&forged, packetType: V3.sealedType, bodyLength: 40)
        appendLE(&forged, UInt32(77))
        appendLE(&forged, UInt64(0))
        forged += [UInt8](repeating: 0, count: 40)
        XCTAssertEqual(handle(forged, at: t0), [])
        XCTAssertTrue(client.isConfirming)
        XCTAssertNil(client.liveSession)
    }

    /// The reference client's `only_application_types_count_as_proof_of_a_commit`,
    /// plus the drain and the liveness refresh.
    func testOnlyApplicationTypesCountAsProofOfACommit() throws {
        XCTAssertTrue(V3.isApplication(0x3F))
        XCTAssertFalse(V3.isApplication(0x40))

        // Confirming: an unknown protocol type changes nothing. 0x53 is a
        // newer host's busy answer, unknown to this client.
        var msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        var (msg2, sealer, _) = try answer(msg1, accept(81))
        _ = handle(msg2, at: t0)
        for unknown: UInt8 in [0x40, 0x53, 0x5F, 0xFF] {
            XCTAssertEqual(handle(try sealer!.seal(innerType: unknown, body: [1, 2, 3]), at: t0), [], "type \(unknown)")
            XCTAssertTrue(client.isConfirming)
            XCTAssertNil(client.liveSession)
        }
        // An application record is proof.
        let out = handle(try sealer!.seal(innerType: V2.pingType, body: [UInt8](repeating: 0, count: 8)), at: t0)
        XCTAssertEqual(client.liveSession, 81)
        guard case .deliver(81, _, V2.pingType, _, false)? = out.last else { return XCTFail("\(out)") }

        // Live: an unknown type isn't delivered, but it is genuine traffic
        // under the session's keys, so it keeps the session alive.
        XCTAssertEqual(handle(try sealer!.seal(innerType: 0x60, body: [9]), at: t0 + ms(2900)), [])
        XCTAssertEqual(client.poll(now: t0 + ms(3000), clockUs: 1_000), [])
        XCTAssertEqual(client.liveSession, 81)
        XCTAssertEqual(client.poll(now: t0 + ms(5900), clockUs: 1_000).first, .closed(sessionId: 81, reason: .liveness))

        // Draining: an unknown type isn't delivered either.
        setUp()
        let (old, _) = try goLive(77)
        try goLive(78, at: t0 + ms(100))
        XCTAssertEqual(client.drainingSession, 77)
        XCTAssertEqual(handle(try old.seal(innerType: 0x60, body: [9]), at: t0 + ms(150)), [])
        guard case .deliver(77, _, _, _, true)? = handle(heartbeat(old), at: t0 + ms(160)).first else { return XCTFail() }

        // Abandoned: an unknown type doesn't promote it.
        setUp()
        msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        (msg2, sealer, _) = try answer(msg1, accept(82))
        _ = handle(msg2, at: t0)
        _ = client.poll(now: t0 + ms(2000), clockUs: 3_000)
        XCTAssertEqual(client.abandonedCount, 1)
        XCTAssertEqual(handle(try sealer!.seal(innerType: 0x5F, body: []), at: t0 + ms(2000)), [])
        XCTAssertNil(client.liveSession)
        XCTAssertEqual(client.abandonedCount, 1)
    }

    func testAMessage2ForAnEarlierAttemptIsDropped() throws {
        let first = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let second = try XCTUnwrap(client.connect(now: t0 + ms(10), key: keyA, clockUs: 2_000).sent.first)
        XCTAssertEqual(handle(try answer(first, accept(77)).0, at: t0 + ms(20)), [])
        XCTAssertTrue(client.isConnecting)
        XCTAssertEqual(handle(try answer(second, accept(78)).0, at: t0 + ms(20)).first, .releaseAllInput)
    }

    // MARK: - Gap 7

    func testAnAuthenticatedButUnparseableMessage2EndsTheAttempt() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let bad = try TestResponder(msg1: msg1, kDev: keyA.kDev, hostId: hostId).writeRaw([0x09]).0
        XCTAssertEqual(handle(bad, at: t0), [])
        let genuine = try answer(msg1, accept(77)).0
        XCTAssertEqual(handle(genuine, at: t0 + ms(10)), [], "the attempt can't read another message 2")
        XCTAssertTrue(client.isConnecting, "message 1 retries continue to the end of the window")
        XCTAssertEqual(client.poll(now: t0 + ms(2000), clockUs: 1_000), [.attemptFailed(devId: keyA.devId)])
    }

    // MARK: - Timestamps

    func testOutOfRangeTimestampsAreIgnoredNeverClamped() throws {
        client.setLastTimestamp(.max)
        client.setLastTimestamp(V3.maxTimestampUs)
        XCTAssertEqual(client.lastTimestamp, 0)
        client.setLastTimestamp(500)
        XCTAssertEqual(client.nextTimestamp(clockUs: .max), 501)
        client.setLastTimestamp(V3.maxTimestampUs - 1)
        XCTAssertEqual(client.nextTimestamp(clockUs: 1_000), V3.maxTimestampUs - 1)
        XCTAssertEqual(client.nextTimestamp(clockUs: .max), V3.maxTimestampUs - 1)
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: .max).sent.first)
        XCTAssertLessThan(try TestResponder(msg1: msg1, kDev: keyA.kDev, hostId: hostId).payload.timestampUs, V3.maxTimestampUs)
        client.setLastTimestamp(10)
        XCTAssertEqual(client.lastTimestamp, V3.maxTimestampUs - 1, "it never moves backwards")
    }

    func testStaleJumpsPastTheStoredValueAndRetriesOnce() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let retry = try XCTUnwrap(handle(try answer(msg1, .stale(storedTimestampUs: 5_000)).0, at: t0).sent.first)
        XCTAssertEqual(try TestResponder(msg1: retry, kDev: keyA.kDev, hostId: hostId).payload.timestampUs, 5_001)
        XCTAssertEqual(handle(try answer(retry, .stale(storedTimestampUs: 9_000)).0, at: t0),
                       [.attemptFailed(devId: keyA.devId)], "one retry only")
        XCTAssertEqual(client.lastTimestamp, 9_000)
    }

    func testTooNewCapsTheClockAndRetriesOnce() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 9_000_000).sent.first)
        let out = handle(try answer(msg1, .tooNew(limitUs: 5_000_000)).0, at: t0, clock: 9_000_000)
        let retry = try XCTUnwrap(out.sent.first)
        XCTAssertLessThanOrEqual(try TestResponder(msg1: retry, kDev: keyA.kDev, hostId: hostId).payload.timestampUs, 5_000_000)
        XCTAssertLessThanOrEqual(client.nextTimestamp(clockUs: 9_000_000), 5_000_001)
        XCTAssertEqual(handle(try answer(retry, .tooNew(limitUs: 5_000_000)).0, at: t0, clock: 9_000_000),
                       [.attemptFailed(devId: keyA.devId)])
    }

    // MARK: - PairAgain and refusals

    func testPairAgainStopsNewAttemptsButKeepsTheLiveSession() throws {
        let (hostSealer, hostOpener) = try goLive(77)
        let msg1 = try XCTUnwrap(client.connect(now: t0 + ms(100), key: keyA, clockUs: 2_000).sent.first)
        XCTAssertEqual(handle(try answer(msg1, .pairAgain).0, at: t0 + ms(100)), [.pairAgain(devId: keyA.devId)])
        XCTAssertFalse(client.isConnecting)
        XCTAssertEqual(client.liveSession, 77, "the session that reached the ceiling stays")
        let report = try XCTUnwrap(client.seal(now: t0 + ms(200), innerType: V2.receiverReportType, body: [7]))
        XCTAssertEqual(try hostOpener.open(report).body, [7])
        _ = handle(heartbeat(hostSealer), at: t0 + ms(2000))
        XCTAssertEqual(client.poll(now: t0 + ms(2500), clockUs: 3_000), [], "no AttemptFailed, no retry")
        XCTAssertEqual(client.connect(now: t0 + ms(2600), key: keyA, clockUs: 3_000), [.pairAgain(devId: keyA.devId)])
    }

    func testARefusalWhileConfirmingRetiresThatKeyAndKeepsTheLiveSession() throws {
        try goLive(77)
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyB, clockUs: 2_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(78), key: keyB)
        _ = handle(msg2, at: t0)
        let status = SlotStatus(held: [(keyA.devId, .active)], tombstones: [keyB.devId])
        XCTAssertEqual(handle(sealed(sealer!, .refused(status)), at: t0 + ms(50)),
                       [.refused(devId: keyB.devId, status: status)])
        XCTAssertFalse(client.isConfirming)
        XCTAssertEqual(client.liveSession, 77)
        XCTAssertEqual(client.abandonedCount, 0)
        XCTAssertEqual(client.poll(now: t0 + ms(2500), clockUs: 2_000).sent, [], "nothing more under the refused key")
    }

    func testAnAbandonedAttemptIsPromotedByALateAckAndReleasesInput() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        XCTAssertEqual(client.poll(now: t0 + ms(2000), clockUs: 2_000).sent.count, 1)
        XCTAssertEqual(client.abandonedCount, 1)
        let out = handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(6000))
        XCTAssertEqual(out.first, .releaseAllInput, "a promotion is a cutover")
        guard case .live(77, _)? = out.dropFirst().first else { return XCTFail("\(out)") }
        XCTAssertEqual(out.last, .status(devId: keyA.devId, status: SlotStatus()))
        XCTAssertFalse(client.isConnecting, "the unanswered message 1 is no longer needed")
        XCTAssertEqual(client.abandonedCount, 0)
    }

    func testARefusalUnderAnAbandonedAttemptStopsUsingThatKey() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        _ = client.poll(now: t0 + ms(2000), clockUs: 2_000)
        XCTAssertTrue(client.isConnecting)
        let status = SlotStatus(tombstones: [keyA.devId])
        XCTAssertEqual(handle(sealed(sealer!, .refused(status)), at: t0 + ms(9000)),
                       [.refused(devId: keyA.devId, status: status)])
        XCTAssertFalse(client.isConnecting, "the attempt under the refused key ends")
        XCTAssertEqual(client.abandonedCount, 0)
    }

    func testAbandonedAttemptsAreKeptElevenSecondsAndAtMostSix() throws {
        var msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        var now = t0
        var lastSealer: Sealer?
        for index in 0..<8 {
            let (msg2, sealer, _) = try answer(msg1, accept(100 + UInt32(index)))
            lastSealer = sealer
            _ = handle(msg2, at: now)
            now += .seconds(2)
            msg1 = try XCTUnwrap(client.poll(now: now, clockUs: 1_000).sent.first)
            XCTAssertLessThanOrEqual(client.abandonedCount, Client.maxAbandoned)
        }
        XCTAssertEqual(client.abandonedCount, 5, "older than 11 s from message 2 are gone")
        // The newest abandoned attempt (message 2 at now - 2 s) still answers.
        _ = client.poll(now: now + ms(8_900), clockUs: 1_000)
        let late = handle(sealed(lastSealer!, .confirmAck(SlotStatus())), at: now + ms(8_900))
        XCTAssertTrue(late.contains(.releaseAllInput))
        XCTAssertEqual(client.liveSession, 107)
    }

    func testAnAbandonedAttemptPastElevenSecondsIsGone() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        _ = client.poll(now: t0 + ms(2000), clockUs: 2_000)
        _ = client.poll(now: t0 + ms(11_000), clockUs: 2_000)
        XCTAssertEqual(client.abandonedCount, 0)
        XCTAssertEqual(handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(11_000)), [])
    }

    func testAfterDisconnectALateCommitNeverBringsTheSessionBack() throws {
        let (_, hostOpener) = try goLive(77)
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 2_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(78))
        _ = handle(msg2, at: t0)
        let out = client.disconnect(reason: 0)
        XCTAssertEqual(out.count, 2)
        let bye = try hostOpener.open(try XCTUnwrap(out.sent.first))
        XCTAssertEqual(try SessionMessage.parse(innerType: bye.innerType, body: bye.body), .bye(0))
        XCTAssertEqual(out.last, .closed(sessionId: 77, reason: .local))
        XCTAssertEqual(client.abandonedCount, 0)
        XCTAssertEqual(handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(500)), [])
        XCTAssertNil(client.liveSession)
    }

    func testAfterAFailedConnectALateCommitUnderTheLiveKeyIsUsed() throws {
        let (hostSealer, _) = try goLive(77)
        // A reconnect under A whose confirms all go unanswered.
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 2_000).sent.first)
        let (msg2, sealer78, _) = try answer(msg1, accept(78))
        _ = handle(msg2, at: t0)
        _ = handle(heartbeat(hostSealer), at: t0 + ms(1900))
        _ = client.poll(now: t0 + ms(2000), clockUs: 3_000)
        // The app switches to B, which fails: the intent falls back to the
        // live session's key, so A's late commit is still taken.
        _ = client.connect(now: t0 + ms(2100), key: keyB, clockUs: 4_000)
        XCTAssertEqual(client.abandonedCount, 1, "kept: it's under the live key")
        _ = handle(heartbeat(hostSealer), at: t0 + ms(4000))
        XCTAssertEqual(client.poll(now: t0 + ms(4100), clockUs: 5_000), [.attemptFailed(devId: keyB.devId)])
        let out = handle(sealed(sealer78!, .confirmAck(SlotStatus())), at: t0 + ms(4200))
        XCTAssertTrue(out.contains(.releaseAllInput))
        XCTAssertEqual(client.liveSession, 78)
        XCTAssertEqual(client.drainingSession, 77)
    }

    func testWithoutALiveSessionAFailedAttemptKeepsTheIntent() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        _ = client.poll(now: t0 + ms(2000), clockUs: 2_000)
        XCTAssertEqual(client.poll(now: t0 + ms(4000), clockUs: 3_000), [.attemptFailed(devId: keyA.devId)])
        let out = handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(5000))
        XCTAssertTrue(out.contains(.releaseAllInput))
        XCTAssertEqual(client.liveSession, 77)
    }

    /// Round 4 (Rust fuzz find): an abandoned attempt is promoted only for
    /// the key the app currently wants.
    func testALateAckIsNotPromotedOnceTheIntentMovedToAnotherKey() throws {
        let (hostSealer, _) = try goLive(77)
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 2_000).sent.first)
        let (msg2, sealer78, _) = try answer(msg1, accept(78))
        _ = handle(msg2, at: t0)
        _ = handle(heartbeat(hostSealer), at: t0 + ms(1900))
        _ = client.poll(now: t0 + ms(2000), clockUs: 3_000)
        _ = client.connect(now: t0 + ms(2100), key: keyB, clockUs: 4_000)
        XCTAssertEqual(client.abandonedCount, 1, "kept: it's under the live key")
        XCTAssertTrue(client.isConnecting, "B's attempt is running, so the intent is B")
        XCTAssertEqual(handle(sealed(sealer78!, .confirmAck(SlotStatus())), at: t0 + ms(2200)), [])
        XCTAssertEqual(client.liveSession, 77, "A's late ack doesn't cut over while the app wants B")
        XCTAssertNil(client.drainingSession)
    }

    /// Round 5 (Rust fuzz find): PairAgain stops new attempts under a key
    /// but keeps its abandoned attempts, which the host may have committed.
    func testPairAgainKeepsAnAbandonedAttemptTheHostMayHaveCommitted() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        let retry = try XCTUnwrap(client.poll(now: t0 + ms(2000), clockUs: 2_000).sent.first)
        XCTAssertEqual(client.abandonedCount, 1)
        XCTAssertEqual(handle(try answer(retry, .pairAgain).0, at: t0 + ms(2100)), [.pairAgain(devId: keyA.devId)])
        XCTAssertEqual(client.abandonedCount, 1, "PairAgain retires nothing")
        let out = handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(3000))
        XCTAssertTrue(out.contains(.releaseAllInput))
        XCTAssertEqual(client.liveSession, 77)
    }

    /// Round 6 (Rust fuzz find): a failed attempt falls the intent back to
    /// the live session's key only while a session is live; a key remembered
    /// from an ended session never overrides the one the app connects with.
    func testAKeyFromAnEndedSessionNeverTakesTheIntentBack() throws {
        let (hostSealer, _) = try goLive(77)
        XCTAssertEqual(handle(sealed(hostSealer, .bye(2)), at: t0 + ms(10)), [.closed(sessionId: 77, reason: .hostBye(2))])
        let msg1 = try XCTUnwrap(client.connect(now: t0 + ms(100), key: keyB, clockUs: 2_000).sent.first)
        let (msg2, sealerB, _) = try answer(msg1, accept(78), key: keyB)
        _ = handle(msg2, at: t0 + ms(100))
        _ = client.poll(now: t0 + ms(2100), clockUs: 3_000)
        XCTAssertEqual(client.abandonedCount, 1)
        XCTAssertEqual(client.poll(now: t0 + ms(4100), clockUs: 4_000), [.attemptFailed(devId: keyB.devId)])
        let out = handle(sealed(sealerB!, .confirmAck(SlotStatus())), at: t0 + ms(4200))
        XCTAssertTrue(out.contains(.releaseAllInput), "the intent stayed B")
        XCTAssertEqual(client.liveSession, 78)
    }

    func testConnectUnderAnotherKeyDropsAbandonedAttemptsOfOtherKeys() throws {
        let msg1 = try XCTUnwrap(client.connect(now: t0, key: keyA, clockUs: 1_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(77))
        _ = handle(msg2, at: t0)
        _ = client.poll(now: t0 + ms(2000), clockUs: 2_000)
        XCTAssertEqual(client.abandonedCount, 1)
        _ = client.connect(now: t0 + ms(2100), key: keyB, clockUs: 3_000)
        XCTAssertEqual(client.abandonedCount, 0, "neither the new key nor a live one")
        XCTAssertEqual(handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(2200)), [])
    }

    // MARK: - Live sessions: drain, liveness, BYE, limits

    func testTheOldSessionDrainsUntilTheNewIdrDecodes() throws {
        let (old, _) = try goLive(77)
        let (new, _) = try goLive(78, at: t0 + ms(100))
        XCTAssertEqual(client.drainingSession, 77)
        let draining = handle(heartbeat(old), at: t0 + ms(150))
        guard case .deliver(77, _, _, _, true)? = draining.first else { return XCTFail("\(draining)") }
        guard case .deliver(78, _, _, _, false)? = handle(heartbeat(new), at: t0 + ms(150)).first else { return XCTFail() }
        XCTAssertEqual(client.firstIdrDecoded(sessionId: 77), [], "only the live session's IDR ends the drain")
        XCTAssertEqual(client.firstIdrDecoded(sessionId: 78), [.closed(sessionId: 77, reason: .drainEnded)])
        XCTAssertNil(client.drainingSession)
        XCTAssertEqual(handle(heartbeat(old), at: t0 + ms(160)), [], "its keys are gone")
    }

    func testTheDrainEndsAfterOneSecondWithoutAnIdr() throws {
        try goLive(77)
        try goLive(78, at: t0 + ms(100))
        XCTAssertEqual(client.poll(now: t0 + ms(1099), clockUs: 1_000), [])
        XCTAssertEqual(client.poll(now: t0 + ms(1100), clockUs: 1_000), [.closed(sessionId: 77, reason: .drainEnded)])
    }

    func testANewerSessionEndsARunningDrainAtOnce() throws {
        try goLive(77)
        try goLive(78, at: t0 + ms(100))
        let msg1 = try XCTUnwrap(client.connect(now: t0 + ms(200), key: keyA, clockUs: 3_000).sent.first)
        let (msg2, sealer, _) = try answer(msg1, accept(79))
        _ = handle(msg2, at: t0 + ms(200))
        let out = handle(sealed(sealer!, .confirmAck(SlotStatus())), at: t0 + ms(210))
        XCTAssertEqual(out.first, .closed(sessionId: 77, reason: .drainEnded))
        XCTAssertEqual(client.drainingSession, 78)
    }

    func testSilenceOnTheLiveSessionEndsItAndReconnects() throws {
        let (hostSealer, _) = try goLive(77, liveness: 3000)
        _ = handle(heartbeat(hostSealer), at: t0 + ms(2000))
        XCTAssertEqual(client.poll(now: t0 + ms(4999), clockUs: 2_000), [], "a heartbeat keeps it alive")
        let out = client.poll(now: t0 + ms(5000), clockUs: 2_000)
        XCTAssertEqual(out.first, .closed(sessionId: 77, reason: .liveness))
        XCTAssertEqual(out.sent.count, 1, "the app still wants the key: a new attempt")
        XCTAssertTrue(client.isConnecting)
        XCTAssertNil(client.liveSession)
    }

    func testTheAcceptsLivenessTimeoutWins() throws {
        try goLive(77, liveness: 1500)
        XCTAssertEqual(client.poll(now: t0 + ms(1499), clockUs: 1_000), [])
        XCTAssertEqual(client.poll(now: t0 + ms(1500), clockUs: 1_000).first, .closed(sessionId: 77, reason: .liveness))
    }

    func testAfterDisconnectNoLivenessReconnect() throws {
        try goLive(77)
        _ = client.disconnect(reason: 0)
        XCTAssertEqual(client.poll(now: t0 + .seconds(10), clockUs: 1_000), [])
    }

    func testAHostByeClosesTheSession() throws {
        let (hostSealer, _) = try goLive(77)
        XCTAssertEqual(handle(sealed(hostSealer, .bye(2)), at: t0 + ms(10)), [.closed(sessionId: 77, reason: .hostBye(2))])
        XCTAssertNil(client.liveSession)
        XCTAssertNil(client.seal(now: t0 + ms(20), innerType: V2.pingType, body: []))
    }

    func testReplayedRecordsAreDroppedAndOnlyAuthenticatedOnesCountForLiveness() throws {
        let (hostSealer, _) = try goLive(77, liveness: 3000)
        let record = heartbeat(hostSealer)
        XCTAssertEqual(handle(record, at: t0 + ms(1000)).count, 1)
        XCTAssertEqual(handle(record, at: t0 + ms(2900)), [], "a replay")
        var forged = heartbeat(hostSealer)
        forged[forged.count - 1] ^= 1
        XCTAssertEqual(handle(forged, at: t0 + ms(2900)), [])
        XCTAssertEqual(client.poll(now: t0 + ms(4000), clockUs: 1_000).first, .closed(sessionId: 77, reason: .liveness),
                       "neither the replay nor the forgery moved the liveness clock")
    }

    func testTheClientReconnectsBeforeTheMaximumAgeAndEndsAtIt() throws {
        let (hostSealer, _) = try goLive(77)
        let nearly = t0 + V3.sessionMaxAge - .seconds(60)
        _ = handle(heartbeat(hostSealer), at: nearly - ms(10))
        XCTAssertEqual(client.poll(now: nearly - ms(10), clockUs: 2_000), [])
        _ = handle(heartbeat(hostSealer), at: nearly)
        XCTAssertEqual(client.poll(now: nearly, clockUs: 2_000).sent.count, 1)
        XCTAssertTrue(client.isConnecting)
        let end = t0 + V3.sessionMaxAge
        _ = handle(heartbeat(hostSealer), at: end)
        XCTAssertNil(client.seal(now: end, innerType: V2.pingType, body: []))
        XCTAssertTrue(client.poll(now: end, clockUs: 3_000).contains(.closed(sessionId: 77, reason: .expired)))
        XCTAssertNil(client.liveSession)
    }

    func testTheReceiveDirectionNearItsLimitStartsAReconnectAndEndsAtIt() throws {
        let (hostSealer, _) = try goLive(77)
        // An unauthenticated record with a high counter changes nothing.
        hostSealer.skip(to: V3.maxRecordsPerDirection - 10)
        var forged = try hostSealer.seal(innerType: 0x01, body: [UInt8](repeating: 0, count: 8))
        forged[forged.count - 1] ^= 1
        _ = handle(forged, at: t0)
        XCTAssertEqual(client.poll(now: t0 + ms(10), clockUs: 1_000).sent, [])

        hostSealer.skip(to: V3.maxRecordsPerDirection - Client.reconnectRecordMargin)
        _ = handle(try hostSealer.seal(innerType: 0x01, body: [UInt8](repeating: 0, count: 8)), at: t0)
        XCTAssertEqual(client.poll(now: t0 + ms(20), clockUs: 1_000).sent.count, 1, "message 1 of the new attempt")
        XCTAssertTrue(client.isConnecting)
        XCTAssertEqual(client.liveSession, 77)

        hostSealer.skip(to: V3.maxRecordsPerDirection - 1)
        _ = handle(try hostSealer.seal(innerType: 0x01, body: [UInt8](repeating: 0, count: 8)), at: t0)
        XCTAssertTrue(client.poll(now: t0 + ms(30), clockUs: 1_000).contains(.closed(sessionId: 77, reason: .expired)))
        XCTAssertNil(client.liveSession)
    }

    // MARK: - Hygiene

    func testDescriptionsNeverShowKeysOrBodies() {
        XCTAssertFalse(keyA.description.contains("24242424"))
        XCTAssertTrue(keyA.description.contains(keyA.devId.description), "the id isn't secret")
        XCTAssertEqual(keyA.kDev.description, "SecretBytes(32 bytes)")
        let output = ClientOutput.deliver(sessionId: 1, counter: 0, innerType: 0x01, body: Array("frame bytes".utf8), draining: false)
        XCTAssertTrue(output.description.contains("bodyLength: 11"))
        XCTAssertFalse(output.description.contains("102"))
        XCTAssertNil(ClientKey(devId: keyA.devId, kDev: SecretBytes(copying: [UInt8](repeating: 1, count: 31))))
    }

    func testWipeZeroesABuffer() {
        var buffer: [UInt8] = [1, 2, 3, 4]
        wipe(&buffer)
        XCTAssertEqual(buffer, [0, 0, 0, 0])
        XCTAssertTrue(SecretBytes(copying: [9, 9]).constantTimeEquals(SecretBytes(copying: [9, 9])))
        XCTAssertFalse(SecretBytes(copying: [9, 9]).constantTimeEquals(SecretBytes(copying: [9, 8])))
        XCTAssertFalse(SecretBytes(copying: [9, 9]).constantTimeEquals(SecretBytes(copying: [9])))
    }

    func testTheTimingRuleHolds() {
        let config = ClientConfig(hostId: hostId)
        // The host keeps a pending session 5 s: every retry lands while it exists.
        XCTAssertLessThan(config.msg1Window + config.confirmWindow, .seconds(5))
        XCTAssertEqual(config.msg1RetryInterval, ms(250))
        XCTAssertEqual(config.confirmRetryInterval, ms(100))
        XCTAssertEqual(config.drainLimit, ms(1000))
        XCTAssertEqual(config.abandonedKeep, .seconds(11))
        XCTAssertEqual(config.livenessTimeout, .seconds(3))
    }
}
