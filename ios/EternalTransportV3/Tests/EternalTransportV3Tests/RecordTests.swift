import XCTest
@testable import EternalTransportV3

/// Records and the replay window: the Rust reference's `record.rs` and
/// `replay.rs` tests, ported.
final class RecordTests: XCTestCase {
    let key = SecretBytes(copying: [UInt8](repeating: 7, count: 32))

    func pair() -> (Sealer, Opener) {
        (Sealer(key: key, sessionId: 42), Opener(key: key, sessionId: 42))
    }

    func testSealOpenRoundTripAndOverhead() throws {
        let (sealer, opener) = pair()
        let body = [UInt8](repeating: 0xAB, count: 1200)
        let record = try sealer.seal(innerType: 0x20, body: body)
        XCTAssertEqual(record.count, body.count + RecordHeader.overhead)
        XCTAssertEqual(RecordHeader.overhead, 37)
        XCTAssertEqual(try RecordHeader.parse(record), RecordHeader(sessionId: 42, counter: 0))
        let opened = try opener.open(record)
        XCTAssertEqual(opened.counter, 0)
        XCTAssertEqual(opened.innerType, 0x20)
        XCTAssertEqual(opened.body, body)
        let second = try sealer.seal(innerType: 0x20, body: body)
        XCTAssertEqual(try opener.open(second).counter, 1)
        XCTAssertNotEqual(Array(record[20...]), Array(second[20...]), "fresh counter, fresh ciphertext")
    }

    func testEveryBitOfARecordIsAuthenticated() throws {
        let (sealer, _) = pair()
        let record = try sealer.seal(innerType: 0x13, body: [1, 2, 3])
        for at in 0..<record.count {
            for bit in 0..<8 {
                var bad = record
                bad[at] ^= 1 << bit
                let opener = Opener(key: key, sessionId: 42)
                XCTAssertThrowsError(try opener.open(bad), "byte \(at) bit \(bit)")
                XCTAssertNoThrow(try opener.open(record), "the failed attempt didn't move the window")
            }
        }
    }

    func testDuplicatesAndOldRecordsAreDroppedReordersKept() throws {
        let (sealer, opener) = pair()
        let records = try (0..<5000).map { _ in try sealer.seal(innerType: 1, body: Array("x".utf8)) }
        XCTAssertNoThrow(try opener.open(records[4999]))
        XCTAssertThrowsError(try opener.open(records[4999])) { XCTAssertEqual($0 as? V3Error, .replay) }
        XCTAssertNoThrow(try opener.open(records[4999 - 4095]), "inside the window")
        XCTAssertThrowsError(try opener.open(records[4999 - 4096])) { XCTAssertEqual($0 as? V3Error, .replay) }
        XCTAssertNoThrow(try opener.open(records[4000]))
    }

    func testWrongKeySessionOrDirectionFails() throws {
        let (sealer, _) = pair()
        let record = try sealer.seal(innerType: 1, body: Array("x".utf8))
        XCTAssertThrowsError(try Opener(key: SecretBytes(copying: [UInt8](repeating: 8, count: 32)), sessionId: 42).open(record)) {
            XCTAssertEqual($0 as? V3Error, .auth)
        }
        XCTAssertThrowsError(try Opener(key: key, sessionId: 43).open(record))
        let raw = (SecretBytes(copying: [UInt8](repeating: 1, count: 32)), SecretBytes(copying: [UInt8](repeating: 2, count: 32)))
        let (ipadSend, ipadRecv) = SessionKeys(split: raw, handshakeHash: []).initiator(sessionId: 5)
        let (hostSend, hostRecv) = SessionKeys(split: raw, handshakeHash: []).responder(sessionId: 5)
        let up = try ipadSend.seal(innerType: 1, body: Array("up".utf8))
        let down = try hostSend.seal(innerType: 1, body: Array("down".utf8))
        XCTAssertEqual(try hostRecv.open(up).body, Array("up".utf8))
        XCTAssertEqual(try ipadRecv.open(down).body, Array("down".utf8))
        let reflected = try ipadSend.seal(innerType: 1, body: Array("up".utf8))
        XCTAssertThrowsError(try ipadRecv.open(reflected)) { XCTAssertEqual($0 as? V3Error, .auth) }
    }

    func testTheCounterLimitStopsSealingAndOpening() throws {
        let (sealer, opener) = pair()
        sealer.skip(to: V3.maxRecordsPerDirection - 1)
        let last = try sealer.seal(innerType: 1, body: Array("x".utf8))
        XCTAssertNoThrow(try opener.open(last))
        XCTAssertThrowsError(try sealer.seal(innerType: 1, body: Array("x".utf8))) { XCTAssertEqual($0 as? V3Error, .exhausted) }
        var forged = last
        forged.replaceSubrange(12..<20, with: withUnsafeBytes(of: V3.maxRecordsPerDirection.littleEndian, Array.init))
        XCTAssertThrowsError(try opener.open(forged)) { XCTAssertEqual($0 as? V3Error, .replay) }
    }

    func testABodyTooLargeForADatagramUsesNoCounter() throws {
        let (sealer, _) = pair()
        XCTAssertThrowsError(try sealer.seal(innerType: 1, body: [UInt8](repeating: 0, count: 65_535 - 16)))
        XCTAssertEqual(sealer.sealed, 0)
        XCTAssertNoThrow(try sealer.seal(innerType: 1, body: [UInt8](repeating: 0, count: 65_535 - 17)))
    }

    func testFramingRejectsShortFlagsLengthAndSessionZero() throws {
        let (sealer, _) = pair()
        let record = try sealer.seal(innerType: 1, body: [])
        XCTAssertEqual(record.count, RecordHeader.overhead)
        XCTAssertNoThrow(try RecordHeader.parse(record))
        XCTAssertThrowsError(try RecordHeader.parse(Array(record[..<36]))) { XCTAssertEqual($0 as? WireError, .truncated) }
        var flags = record
        flags[4] = 1
        XCTAssertThrowsError(try RecordHeader.parse(flags))
        XCTAssertThrowsError(try RecordHeader.parse(record + [0])) { XCTAssertEqual($0 as? WireError, .lengthMismatch) }
        var zero = record
        zero.replaceSubrange(8..<12, with: [0, 0, 0, 0])
        XCTAssertThrowsError(try RecordHeader.parse(zero))
    }

    func testConcurrentOpensOfTheSameRecordPassOnce() throws {
        let (sealer, opener) = pair()
        let records = try (0..<500).map { _ in try sealer.seal(innerType: 1, body: Array("x".utf8)) }
        let passed = UnsafeMutablePointer<Int>.allocate(capacity: 6)
        passed.initialize(repeating: 0, count: 6)
        defer { passed.deallocate() }
        DispatchQueue.concurrentPerform(iterations: 6) { thread in
            passed[thread] = records.filter { (try? opener.open($0)) != nil }.count
        }
        XCTAssertEqual((0..<6).map { passed[$0] }.reduce(0, +), 500)
    }

    func testOpenedDescriptionNeverShowsPlaintext() throws {
        let (sealer, opener) = pair()
        let opened = try opener.open(try sealer.seal(innerType: 0x20, body: Array("secret keystrokes".utf8)))
        XCTAssertFalse(opened.description.contains("secret"))
        XCTAssertTrue(opened.description.contains("bodyLength: 17"))
    }

    func testHighestMovesOnlyOnAuthenticatedRecords() throws {
        let (sealer, opener) = pair()
        XCTAssertNil(opener.highest)
        sealer.skip(to: 1000)
        let record = try sealer.seal(innerType: 1, body: Array("x".utf8))
        var forged = record
        forged[forged.count - 1] ^= 1
        XCTAssertThrowsError(try opener.open(forged))
        XCTAssertNil(opener.highest)
        _ = try opener.open(record)
        XCTAssertEqual(opener.highest, 1000)
        sealer.skip(to: 5)
        XCTAssertEqual(sealer.sealed, 1001, "skip never moves back")
    }

    // MARK: - Replay window

    func testTheWindowAcceptsOnceInAnyOrderInside() {
        var window = ReplayWindow()
        XCTAssertTrue(window.checkAndMark(10))
        XCTAssertFalse(window.checkAndMark(10))
        XCTAssertTrue(window.checkAndMark(3))
        XCTAssertFalse(window.check(3))
        XCTAssertTrue(window.checkAndMark(11))
        XCTAssertTrue(window.checkAndMark(0))
        XCTAssertEqual(window.top, 11)
    }

    func testTheWindowEdgeIsExactly4096Wide() {
        var window = ReplayWindow()
        XCTAssertTrue(window.checkAndMark(5000))
        XCTAssertFalse(window.check(5000 - 4096))
        XCTAssertTrue(window.check(5000 - 4095))
        XCTAssertTrue(window.checkAndMark(5000 - 4095))
        XCTAssertFalse(window.checkAndMark(5000 - 4095))
    }

    func testABigJumpForgetsEverythingOlder() {
        var window = ReplayWindow()
        for counter in 0..<100 as Range<UInt64> {
            XCTAssertTrue(window.checkAndMark(counter))
        }
        XCTAssertTrue(window.checkAndMark(1_000_000))
        XCTAssertFalse(window.check(99))
        for counter in (1_000_000 - 4095)..<1_000_000 as Range<UInt64> where !window.check(counter) {
            XCTFail("\(counter)")
        }
    }

    func testCheckAloneNeverMovesTheWindow() {
        var window = ReplayWindow()
        XCTAssertTrue(window.checkAndMark(1))
        XCTAssertTrue(window.check(1_000_000))
        XCTAssertEqual(window.top, 1)
        XCTAssertTrue(window.checkAndMark(2))
    }

    /// The window against its contract stated without bitmaps.
    func testTheWindowMatchesTheModelOnReorderedDuplicatedStreams() {
        var rng = XorShift(state: 0x9E37_79B9_7F4A_7C15)
        for _ in 0..<20 {
            var window = ReplayWindow()
            var seen = Set<UInt64>()
            var top: UInt64?
            var base: UInt64 = 0
            for _ in 0..<5_000 {
                let r = rng.next()
                let counter: UInt64
                switch r % 10 {
                case 0: counter = base >= r % 5000 ? base - r % 5000 : 0
                case 1: counter = base + r % 70_000
                default: counter = base + r % 64
                }
                base = max(base, counter)
                let wouldAccept = !seen.contains(counter) && !(top.map { counter <= $0 && $0 - counter >= 4096 } ?? false)
                XCTAssertEqual(window.check(counter), wouldAccept)
                XCTAssertEqual(window.checkAndMark(counter), wouldAccept)
                if wouldAccept {
                    seen.insert(counter)
                    top = max(top ?? 0, counter)
                }
            }
        }
    }
}
