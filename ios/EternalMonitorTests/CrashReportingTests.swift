import XCTest
@testable import EternalMonitor

/// MetricKit reports and the on-screen marker, as the support log gets them.
final class CrashReportingTests: XCTestCase {
    private let at = Date(timeIntervalSince1970: 1_793_000_000) // 2026-10-26T07:33:20Z

    /// A per-thread call stack tree as MetricKit writes it for a crash: the
    /// first thread isn't the crashed one, and each root frame's sub-frame
    /// is its caller.
    private func perThreadTree(frames: Int, binary: String = "EternalMonitor") -> Data {
        func chain(_ depth: Int) -> [String: Any] {
            var frame: [String: Any] = ["binaryUUID": "70B89F27-1634-3580-A695-57CDB41D7743", "address": 4_295_000_000 + depth,
                                        "offsetIntoBinaryTextSegment": 0x1a2b00 + depth * 0x10, "sampleCount": 1,
                                        "binaryName": depth == 0 ? "libsystem_kernel.dylib" : binary]
            if depth + 1 < frames { frame["subFrames"] = [chain(depth + 1)] }
            return frame
        }
        let idle: [String: Any] = ["threadAttributed": false, "callStackRootFrames": [
            ["binaryName": "libsystem_kernel.dylib", "offsetIntoBinaryTextSegment": 0x1000, "sampleCount": 1]]]
        let tree: [String: Any] = ["callStackPerThread": true, "callStacks": [
            idle, ["threadAttributed": true, "callStackRootFrames": [chain(0)]]]]
        return try! JSONSerialization.data(withJSONObject: tree)
    }

    private func crash(_ tree: Data? = nil, reason: String? = "Namespace SIGNAL, Code 11 Segmentation fault: 11") -> MetricKitDiagnostic {
        let top = MetricKitDiagnostic.topFrames(tree ?? perThreadTree(frames: 12))
        return MetricKitDiagnostic(kind: .crash, appVersion: "0.3.4", build: "370", osVersion: "iPadOS 26.1 (23B85)", at: at,
                                   exceptionType: 1, exceptionCode: 2, signal: 11, objcException: nil,
                                   terminationReason: reason, frames: top.frames, attributed: top.attributed)
    }

    func testACrashIsOneLineWithTheCrashedThreadsTopFrames() {
        XCTAssertEqual(crash().line,
            "MetricKit crash app=0.3.4 build=370 os=ipados:26.1 os_build=23B85 at=2026-10-26T07:33:20Z"
            + " exception_type=1(EXC_BAD_ACCESS) exception_code=2 signal=11(SIGSEGV) objc_exception=- thread=attributed"
            + " frames=libsystem_kernel.dylib+0x1a2b00,EternalMonitor+0x1a2b10,EternalMonitor+0x1a2b20,EternalMonitor+0x1a2b30,"
            + "EternalMonitor+0x1a2b40,EternalMonitor+0x1a2b50,EternalMonitor+0x1a2b60,EternalMonitor+0x1a2b70"
            + " termination_reason=\"Namespace SIGNAL, Code 11 Segmentation fault: 11\"")
        var objc = crash(perThreadTree(frames: 2), reason: nil)
        objc.exceptionType = 10
        objc.signal = 6
        objc.objcException = "NSRangeException"
        XCTAssertTrue(objc.line.hasSuffix("exception_type=10(EXC_CRASH) exception_code=2 signal=6(SIGABRT)"
            + " objc_exception=NSRangeException thread=attributed frames=libsystem_kernel.dylib+0x1a2b00,EternalMonitor+0x1a2b10"
            + " termination_reason=-"), objc.line)
        // Values MetricKit left out, and a tree it couldn't give.
        let bare = MetricKitDiagnostic(kind: .crash, appVersion: "", build: "", osVersion: "", at: at)
        XCTAssertEqual(bare.line, "MetricKit crash app=- build=- os=ipados:0.0 os_build=- at=2026-10-26T07:33:20Z exception_type=-"
            + " exception_code=- signal=- objc_exception=- thread=- frames=- termination_reason=-")
        XCTAssertEqual(MetricKitDiagnostic.topFrames(Data("not json".utf8)).frames, [])
    }

    func testAHangAndACPUExceptionAreOneLineEach() {
        let top = MetricKitDiagnostic.topFrames(perThreadTree(frames: 3))
        let hang = MetricKitDiagnostic(kind: .hang, appVersion: "0.3.4", build: "370", osVersion: "iPhone OS 18.6 (22G86)", at: at,
                                       seconds: 2.5, frames: top.frames, attributed: top.attributed)
        XCTAssertEqual(hang.line, "MetricKit hang app=0.3.4 build=370 os=ipados:18.6 os_build=22G86 at=2026-10-26T07:33:20Z"
            + " duration_ms=2500 thread=attributed frames=libsystem_kernel.dylib+0x1a2b00,EternalMonitor+0x1a2b10,EternalMonitor+0x1a2b20")
        // A CPU report aggregates samples from the outermost caller: the
        // heaviest path ends in the innermost frame, which is shown first.
        let tree: [String: Any] = ["callStackPerThread": false, "callStacks": [["callStackRootFrames": [
            ["binaryName": "dyld", "offsetIntoBinaryTextSegment": 0x10, "sampleCount": 3],
            ["binaryName": "dyld", "offsetIntoBinaryTextSegment": 0x20, "sampleCount": 90, "subFrames": [
                ["binaryName": "EternalMonitor", "offsetIntoBinaryTextSegment": 0x30, "sampleCount": 20],
                ["binaryName": "EternalMonitor", "offsetIntoBinaryTextSegment": 0x40, "sampleCount": 70, "subFrames": [
                    ["binaryName": "VideoToolbox", "offsetIntoBinaryTextSegment": 0x50, "sampleCount": 70]]]]]]]]]
        let samples = MetricKitDiagnostic.topFrames(try! JSONSerialization.data(withJSONObject: tree))
        let cpu = MetricKitDiagnostic(kind: .cpuException, appVersion: "0.3.4", build: "370", osVersion: "iPadOS 26.1 (23B85)", at: at,
                                      seconds: 81, sampledSeconds: 180, frames: samples.frames, attributed: samples.attributed)
        XCTAssertEqual(cpu.line, "MetricKit cpu_exception app=0.3.4 build=370 os=ipados:26.1 os_build=23B85 at=2026-10-26T07:33:20Z"
            + " cpu_ms=81000 sampled_ms=180000 thread=first frames=VideoToolbox+0x50,EternalMonitor+0x40,dyld+0x20")
    }

    func testTheLineStaysWithinTheCapWithoutPathsOrSecondLines() {
        let path = "/private/var/containers/Bundle/Application/1D2C/EternalMonitor.app/Frameworks/Copus.framework/Copus"
        let reason = "Namespace DYLD, Code 1 Library missing\nLibrary not loaded: @rpath/Copus.framework/Copus | Referenced from: \(path)"
            + " \"quoted\" \\ " + String(repeating: "watchdog transgression ", count: 2000)
        let long = crash(perThreadTree(frames: 50, binary: String(repeating: "ß/x", count: 500)), reason: reason)
        let line = long.line
        XCTAssertLessThanOrEqual(line.utf8.count, 8192)
        XCTAssertLessThan(line.utf8.count, 2048, "every field is bounded")
        XCTAssertFalse(line.contains("\n"))
        XCTAssertFalse(line.contains("/private"), line)
        XCTAssertFalse(line.contains("@rpath"), line)
        XCTAssertTrue(line.contains("termination_reason=\"Namespace DYLD, Code 1 Library missing Library not loaded: <path> | Referenced from: <path> quoted watchdog"), line)
        let shown = line.components(separatedBy: "termination_reason=\"")[1].dropLast()
        XCTAssertLessThanOrEqual(shown.utf8.count, MetricKitDiagnostic.maxReasonBytes)
        XCTAssertFalse(shown.contains("\""))
        let frames = line.components(separatedBy: " frames=")[1].components(separatedBy: " ")[0].split(separator: ",")
        XCTAssertEqual(frames.count, MetricKitDiagnostic.topFrames)
        XCTAssertEqual(frames[1], Substring(String(repeating: "__x", count: 21) + "_+0x1a2b10"), "names are ASCII tokens of 64 at most")
        // The support log and the upload cut at 8 KiB anyway; the line is far inside.
        XCTAssertEqual(SupportDiagnostics.redact(line), line)
    }

    func testExitCountsAreOneLineOfCounts() {
        let exits = MetricKitExits(appVersion: "0.3.4", build: "370", osVersion: "iPadOS 26.1 (23B85)", multipleVersions: false,
                                   from: at.addingTimeInterval(-86_400), to: at,
                                   foreground: .init(normal: 12, abnormal: 1, memoryLimit: 2, watchdog: 3, badAccess: 4, illegalInstruction: 5),
                                   background: .init(normal: 30, abnormal: 0, memoryLimit: 6, watchdog: 0, badAccess: 0, illegalInstruction: 0))
        XCTAssertEqual(exits.line, "MetricKit exits app=0.3.4 build=370 os=ipados:26.1 os_build=23B85 versions=one"
            + " from=2026-10-25T07:33:20Z to=2026-10-26T07:33:20Z"
            + " fg_normal=12 fg_abnormal=1 fg_memory_limit=2 fg_watchdog=3 fg_bad_access=4 fg_illegal_instruction=5"
            + " bg_normal=30 bg_abnormal=0 bg_memory_limit=6 bg_watchdog=0 bg_bad_access=0 bg_illegal_instruction=0")
    }

    func testEachReportHasItsLevelAndCategory() {
        var lines: [[String]] = []
        let reporting = CrashReporting(marker: ForegroundMarker(defaults: UserDefaults(suiteName: UUID().uuidString)!, version: "1"),
                                       record: { lines.append([$0, $1, $2]) })
        var hang = crash()
        hang.kind = .hang
        var cpu = crash()
        cpu.kind = .cpuException
        reporting.handle([crash(), hang, cpu])
        reporting.handle(MetricKitExits(appVersion: "0.3.4", build: "370", osVersion: "", multipleVersions: true, from: at, to: at,
                                        foreground: .init(), background: .init()))
        XCTAssertEqual(lines.map { Array($0.prefix(2)) }, [["error", "crash"], ["warning", "hang"], ["warning", "cpu"], ["info", "exits"]])
        XCTAssertTrue(lines[3][2].contains(" versions=multiple "), lines[3][2])
    }

    /// The lines go through the support log, so they upload only while
    /// sharing is on, and stay in the iPad's own log either way.
    func testReportsFollowTheSharingSetting() async throws {
        for sharing in [true, false] {
            let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            let suite = UUID().uuidString
            let defaults = UserDefaults(suiteName: suite)!
            defer { try? FileManager.default.removeItem(at: root); defaults.removePersistentDomain(forName: suite) }
            let upload = DiagnosticsUpload(directory: root.appendingPathComponent("outbox"),
                configuration: .init(endpoint: URL(string: "http://127.0.0.1:1/v1/events")!, ingestKey: "write-only", allowHTTP: true),
                defaults: defaults)
            defer { upload.shutdown() }
            upload.setEnabled(sharing)
            let log = SupportDiagnostics(directory: root.appendingPathComponent("log"), upload: upload)
            CrashReporting(marker: ForegroundMarker(defaults: defaults, version: "1"), record: { log.record($0, $1, $2) })
                .handle([crash()])
            let report = await log.report()
            XCTAssertTrue(report.contains("error [crash] MetricKit crash app=0.3.4 build=370"), report)
            let outbox = try FileManager.default.contentsOfDirectory(at: root.appendingPathComponent("outbox"), includingPropertiesForKeys: nil)
            let uploaded = try outbox.flatMap { try JSONDecoder().decode(DiagnosticsUpload.Batch.self, from: Data(contentsOf: $0)).events }
            XCTAssertEqual(uploaded.map(\.category), sharing ? ["crash"] : [], "sharing=\(sharing)")
            XCTAssertEqual(uploaded.first?.message, sharing ? crash().line : nil)
        }
    }

    // MARK: - Foreground marker

    private func marker(version: String = "0.3.4 (370)", _ defaults: UserDefaults) -> ForegroundMarker {
        ForegroundMarker(defaults: defaults, version: version)
    }

    func testALaunchThatEndedOnScreenIsReportedOnce() {
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        XCTAssertNil(marker(defaults).takePreviousLaunch(), "a first launch")
        marker(defaults).activated(.disconnected)
        marker(defaults).update(.connecting)
        marker(defaults).update(.connected)
        // The app was killed on screen while streaming.
        XCTAssertEqual(marker(defaults).takePreviousLaunch(),
            "Previous launch ended while EternalMonitor was on screen (possible crash or forced quit) stream=active")
        XCTAssertNil(marker(defaults).takePreviousLaunch(), "reported once")

        marker(defaults).activated(.connected)
        marker(defaults).update(.disconnected)
        XCTAssertEqual(marker(defaults).takePreviousLaunch()?.hasSuffix(" stream=none"), true)
    }

    func testBackgroundAndACleanExitLeaveNoMarker() {
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        marker(defaults).activated(.connected)
        marker(defaults).clear() // background, or willTerminate
        // The connection ends after the app left the screen: no marker comes back.
        marker(defaults).update(.disconnected)
        XCTAssertNil(marker(defaults).takePreviousLaunch())
    }

    /// An update replaces the app, so the marker an older version left says
    /// nothing about this one.
    func testTheFirstLaunchAfterAnUpdateReportsNothing() {
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        marker(version: "0.3.4 (369)", defaults).activated(.connected)
        XCTAssertNil(marker(version: "0.3.4 (370)", defaults).takePreviousLaunch())
        XCTAssertNil(defaults.object(forKey: ForegroundMarker.key), "cleared")
        marker(version: "0.3.3 (364)", defaults).activated(.connecting)
        XCTAssertNil(marker(version: "0.3.4 (370)", defaults).takePreviousLaunch())
    }

    func testStartLogsThePreviousLaunchAsAWarning() {
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        marker(defaults).activated(.connecting)
        var lines: [[String]] = []
        let reporting = CrashReporting(marker: marker(defaults), record: { lines.append([$0, $1, $2]) })
        reporting.start()
        XCTAssertEqual(lines, [["warning", "lifecycle",
            "Previous launch ended while EternalMonitor was on screen (possible crash or forced quit) stream=connecting"]])
    }
}
