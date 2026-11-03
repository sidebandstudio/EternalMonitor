import XCTest
@testable import EternalMonitor

final class SupportDiagnosticsTests: XCTestCase {
    func testRedactsPairingCredentialsWithoutLosingSessionOrErrorCodes() {
        let text = #"pairing_code=123456 auth_token_hex="00112233445566778899aabbccddeeff" token=[1, 2, 3] pairingCode:654321 status=-12909 session_id=123456 "auth_token_hex": "json-secret""#
        let redacted = SupportDiagnostics.redact(text)
        for secret in ["001122", "654321", "[1, 2, 3]", "pairing_code=123456", "json-secret"] {
            XCTAssertFalse(redacted.contains(secret), redacted)
        }
        XCTAssertTrue(redacted.contains("status=-12909 session_id=123456"))
    }

    func testUploadsLeaveOutThePCNameButTheReportKeepsIt() async throws {
        let line = "Session 5 established with MARIA-DESKTOP"
        XCTAssertEqual(SupportDiagnostics.redactForUpload(line, category: "ctrl"), "Session 5 established with %COMPUTERNAME%")
        XCTAssertEqual(SupportDiagnostics.redactForUpload("Session 5 established with Maria's PC token=x", category: "ctrl"),
                       "Session 5 established with %COMPUTERNAME%")
        // ConnectionManager's line ends the name at its own last session_id=.
        let connected = "Connected to Zoe session_id=1 owner=Zoe Chen session_id=5 host_caps=3 liveness_ms=3000 config=StreamConfig()"
        XCTAssertEqual(SupportDiagnostics.redactForUpload(connected, category: "ctrl"),
                       "Connected to %COMPUTERNAME% session_id=5 host_caps=3 liveness_ms=3000 config=StreamConfig()")
        for other in ["HELLO2 attempt 1/4 to 192.0.2.7:9876", "model=iPad hardware=iPad16,5"] {
            XCTAssertEqual(SupportDiagnostics.redactForUpload(other, category: "ctrl"), other)
        }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = SupportDiagnostics(directory: directory)
        store.record("info", "ctrl", line)
        store.record("info", "ctrl", connected)
        let report = await store.report()
        XCTAssertTrue(report.contains(line) && report.contains(connected), report)
    }

    func testASeenPCNameLeavesEveryUploadedLine() {
        let name = #"Zoë's "Big" PC (2)"#
        SupportDiagnostics.notePCName(name)
        for (line, uploaded) in [
            (#"Paired with zoë's "big" pc (2) at 192.0.2.7"#, "Paired with %COMPUTERNAME% at 192.0.2.7"),
            (#"Connection lost (Zoë's "Big" PC (2) stopped answering)"#, "Connection lost (%COMPUTERNAME% stopped answering)"),
        ] {
            XCTAssertEqual(SupportDiagnostics.redactForUpload(line, category: "ctrl"), uploaded)
        }
        // A typed name, with and without its domain; addresses stay.
        for address in ["studio-quinn.local", "192.0.2.7", "fe80::1%en0", "[2001:db8::1]"] {
            SupportDiagnostics.noteTypedAddress(address)
        }
        for (line, uploaded) in [
            ("Starting connection transport=WiFi to studio-quinn.local:9876 drawing=false",
             "Starting connection transport=WiFi to %COMPUTERNAME%:9876 drawing=false"),
            ("Cannot resolve STUDIO-QUINN: nodename nor servname provided", "Cannot resolve %COMPUTERNAME%: nodename nor servname provided"),
            // Another PC, a field name, and the addresses.
            ("studio-quinn-2 studio-quinn=1 HELLO2 attempt 1/4 to 192.0.2.7:9876 fe80::1%en0 [2001:db8::1]",
             "studio-quinn-2 studio-quinn=1 HELLO2 attempt 1/4 to 192.0.2.7:9876 fe80::1%en0 [2001:db8::1]"),
        ] {
            XCTAssertEqual(SupportDiagnostics.redactForUpload(line, category: "connection"), uploaded)
        }
        // A session summary uploads as written (#111).
        let summary = "session_summary v=1 role=ipad session_id=5 hw=studio-quinn"
        XCTAssertEqual(SupportDiagnostics.redactForUpload(summary, category: "summary"), summary)
    }

    /// Values, common words and this iPad's hardware names are never PC names, and a PC name in a
    /// field's value stays: replacing either would corrupt the line.
    func testValuesAndHardwareNamesAreNeverReplaced() {
        SupportDiagnostics.noteHardware("iPad16,5")
        for name in ["true", "5060", "v1.2.3", "auto", "none", "Server", "iPad16", "iPad16,5"] {
            SupportDiagnostics.notePCName(name)
        }
        // Seen before the hardware name: let go once it is known.
        SupportDiagnostics.notePCName("iPad13")
        SupportDiagnostics.noteHardware("iPad13,4")
        SupportDiagnostics.notePCName("Quinn")
        for line in [
            "app=0.3.4 build=5060 hardware=iPad16,5 simulator=false model=iPad running=true version=v1.2.3",
            "Waiting for the server: auto, none, true; hardware=iPad13,4",
            "reason=Quinn decoder=hevc",
        ] {
            XCTAssertEqual(SupportDiagnostics.redactForUpload(line, category: "device"), line)
        }
        XCTAssertEqual(SupportDiagnostics.redactForUpload("Paired with Quinn at 192.0.2.7 reason=none", category: "ctrl"),
                       "Paired with %COMPUTERNAME% at 192.0.2.7 reason=none")
    }

    /// A name without a letter or digit, or holding placeholder syntax, is never a PC name, even with
    /// another name already seen; redacting an uploaded line again changes nothing.
    func testEmojiAndPlaceholderNamesAreNeverPCNames() {
        SupportDiagnostics.notePCName("Ines-PC")
        for name in ["🎮🎧🎵", "!!!", "%COMPUTERNAME% Chen", "<device-name> Chen", "Kim <3", "100% Kim"] {
            SupportDiagnostics.notePCName(name)
        }
        SupportDiagnostics.noteTypedAddress("🎮🎧🎵")
        for (line, uploaded) in [
            ("Paired with 🎮🎧🎵 at 192.0.2.7", "Paired with 🎮🎧🎵 at 192.0.2.7"),
            ("Ines-PC Chen and the lobby", "%COMPUTERNAME% Chen and the lobby"),
            ("<device-name> Chen and 100% Kim <3", "<device-name> Chen and 100% Kim <3"),
        ] {
            let once = SupportDiagnostics.redactForUpload(line, category: "ctrl")
            XCTAssertEqual(once, uploaded)
            XCTAssertEqual(SupportDiagnostics.redactForUpload(once, category: "ctrl"), once)
        }
    }

    func testAnAcceptedPCNameLeavesUploadedLines() async throws {
        let queue = DispatchQueue(label: "pc-name-test")
        let (stream, continuation) = AsyncStream<Data>.makeStream()
        let channel = ControlChannel(queue: queue) { continuation.yield($0) }
        defer { channel.stop(); continuation.finish() }
        let identity = ControlChannel.ClientIdentity(deviceName: "Test iPad", screenPxW: 640, screenPxH: 360,
            screenPtW: 640, screenPtH: 360, refreshHz: 60, authToken: Data(1...16))
        channel.startHandshake(listenPort: 50000, identity: identity)
        var messages = stream.makeAsyncIterator()
        let sent = await messages.next()
        guard case .hello2(let hello) = try XCTUnwrap(Wire.parseControl(XCTUnwrap(sent))).message else {
            return XCTFail("missing handshake")
        }
        let ack = Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .helloAck(HelloAck(
            status: .ok, acceptedVersion: 2, clientNonce: hello.clientNonce, sessionId: 41,
            heartbeatIntervalMs: 1000, reportIntervalMs: 500, livenessTimeoutMs: 3000,
            streamConfig: StreamConfig(), hostName: "RIVER-OFFICE", authToken: Data(17...32), hostCaps: 0)))
        queue.sync { channel.handleControl(ack) }
        XCTAssertEqual(channel.currentSessionId, 41)
        XCTAssertEqual(SupportDiagnostics.redactForUpload("USB: connected to River-Office", category: "usb"),
                       "USB: connected to %COMPUTERNAME%")
    }

    func testReportSurvivesRelaunchIncludesLatestErrorAndExportsAFile() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let first = SupportDiagnostics(directory: directory)
        first.record("error", "decode", "VTDecodeFrame status=-12909 token=secret")
        let firstReport = await first.report()
        XCTAssertTrue(firstReport.contains("VTDecodeFrame status=-12909"))
        XCTAssertFalse(firstReport.contains("token=secret"))
        let second = SupportDiagnostics(directory: directory)
        second.record("info", "connection", "new launch")
        let report = await second.report()
        XCTAssertTrue(report.contains("VTDecodeFrame status=-12909"))
        XCTAssertTrue(report.contains("new launch"))
        XCTAssertTrue(report.contains("Storage error: none"))
        // The Support ID is the upload's installation_id, near the top.
        let header = report.components(separatedBy: "\n").prefix(2)
        XCTAssertEqual(header.last, "Support ID: \(DiagnosticsUpload.shared.installationID)", report)
        XCTAssertTrue(report.contains("installation_id=\(DiagnosticsUpload.shared.installationID);"))
        let export = try await second.export()
        XCTAssertTrue(try String(contentsOf: export).contains("VTDecodeFrame status=-12909"))
    }

    func testStorageFailureKeepsLatestErrorInMemory() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try Data().write(to: directory)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = SupportDiagnostics(directory: directory)
        for event in 0..<120 { store.record("info", "test", "event=\(event)") }
        store.record("error", "decode", "latest-error status=-12909")
        let report = await store.report()
        XCTAssertFalse(report.contains("Storage error: none"))
        XCTAssertTrue(report.contains("latest-error status=-12909"))
    }

    func testRotationBoundsStorageAndRetainsLatestError() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = SupportDiagnostics(directory: directory, limit: 1024)
        for event in 0..<100 { store.record("info", "test", "event=\(event) " + String(repeating: "x", count: 100)) }
        store.record("error", "decode", "last-error status=-123")
        let report = await store.report()
        XCTAssertTrue(report.contains("last-error status=-123"))
        let files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.fileSizeKey])
        XCTAssertEqual(files.count, 3)
        for file in files { XCTAssertLessThanOrEqual(try file.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0, 1024) }
    }
}
