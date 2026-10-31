import XCTest
@testable import EternalMonitor

/// The manifest's staged prompt and first-picture wait. Bad values mean the
/// defaults; nothing here can stop a working app.
final class RemoteConfigTests: XCTestCase {
    /// 2026-11-02T00:00:00Z, the example manifest's rollout start.
    private let start = Date(timeIntervalSince1970: 1_793_577_600)
    private let day: TimeInterval = 86_400

    /// The example manifest shared with the PC's and check-versions' tests.
    private func example() throws -> Data {
        let url = try XCTUnwrap(Bundle(for: RemoteConfigTests.self)
            .url(forResource: "latest_remote_config", withExtension: "json", subdirectory: "testdata"),
            "is proto/testdata bundled?")
        return try Data(contentsOf: url)
    }

    private func ids(_ count: Int) -> [String] {
        (0..<count).map { String(format: "%032llx", UInt64($0) &* 0x9e37_79b9) }
    }

    func testTheBucketMatchesThePC() {
        // remote_config/tests.rs asserts the same two values.
        XCTAssertEqual(RemoteConfig.bucket(id: "00000000000000000000000000000000", version: "0.3.4"), 5_056_594_762_205_651_656)
        XCTAssertEqual(RemoteConfig.bucket(id: "0123456789abcdef0123456789abcdef", version: "0.3.4"), 4_748_426_906_821_000_708)
    }

    func testTheBucketIsUniform() {
        var deciles = [Int](repeating: 0, count: 10)
        for id in ids(10_000) {
            deciles[Int(RemoteConfig.bucket(id: id, version: "0.3.4").multipliedFullWidth(by: 10).high)] += 1
        }
        for (decile, count) in deciles.enumerated() {
            XCTAssertTrue((880...1120).contains(count), "decile \(decile): \(count)")
        }
    }

    func testARolloutRampsFromFewToEveryone() throws {
        let rollout = try XCTUnwrap(RemoteConfig.Rollout(start: "2026-11-02T00:00:00Z", days: 3))
        let all = ids(10_000)
        let shown = { (now: Date) in all.filter { rollout.shows(id: $0, version: "0.3.4", now: now) }.count }
        XCTAssertEqual(shown(start.addingTimeInterval(-1)), 0)
        XCTAssertLessThan(shown(start.addingTimeInterval(60)), 20)
        let firstDay = shown(start.addingTimeInterval(day))
        XCTAssertTrue((3_100...3_550).contains(firstDay), "day 1: \(firstDay)")
        XCTAssertEqual(shown(start.addingTimeInterval(3 * day)), 10_000)
        let atOnce = try XCTUnwrap(RemoteConfig.Rollout(start: "2026-11-02T00:00:00Z", days: 0))
        XCTAssertTrue(all.prefix(100).allSatisfy { atOnce.shows(id: $0, version: "0.3.4", now: start) })
        let typo = try XCTUnwrap(RemoteConfig.Rollout(start: "2099-01-01T00:00:00Z", days: 7))
        XCTAssertTrue(all.prefix(100).allSatisfy { typo.shows(id: $0, version: "0.3.4", now: start) })
    }

    private func ipad(_ extra: String) -> Data {
        Data(#"{"schema":1,"ipad":{"version":"0.3.4","build":320\#(extra)}}"#.utf8)
    }

    func testABadRolloutIsNoRolloutAndTheUpdateStays() throws {
        XCTAssertEqual(UpdateManifest.parse(ipad(#","rollout":{"start":"2026-11-02T00:00:00Z","days":3}"#))?.rollout?.days, 3)
        for extra in [#","rollout":null"#, #","rollout":"soon""#, #","rollout":{"start":"2026-11-02T00:00:00Z"}"#,
                      #","rollout":{"start":"2026-11-02T00:00:00Z","days":15}"#,
                      #","rollout":{"start":"2026-11-02T00:00:00Z","days":-1}"#,
                      #","rollout":{"start":"2026-11-02T00:00:00Z","days":2.5}"#,
                      #","rollout":{"start":"2026-11-02T00:00:00Z","days":true}"#,
                      #","rollout":{"start":"2026-11-02T00:00:00+01:00","days":3}"#,
                      #","rollout":{"start":"2026-02-30T00:00:00Z","days":3}"#,
                      #","rollout":{"start":1793577600,"days":3}"#] {
            let parsed = try XCTUnwrap(UpdateManifest.parse(ipad(extra)), extra)
            XCTAssertNil(parsed.rollout, extra)
            XCTAssertEqual(parsed.build, 320, extra)
        }
    }

    func testMissingGarbageOrNewerConfigIsTheDefaults() {
        let cases: [(String, String)] = [
            ("", "missing"), (#","config":null"#, "missing"), (#","config":"1""#, "invalid"),
            (#","config":[]"#, "invalid"), (#","config":{}"#, "invalid"), (#","config":{"schema":"1"}"#, "invalid"),
            (#","config":{"schema":0}"#, "invalid"), (#","config":{"schema":true}"#, "invalid"),
            (#","config":{"schema":2,"first_frame_timeout_s":12}"#, "schema"),
        ]
        for (extra, reason) in cases {
            XCTAssertEqual(RemoteConfig.parse(ipad(extra)), .defaults(reason), extra)
        }
        XCTAssertEqual(RemoteConfig.parse(Data("<html>".utf8)), .defaults("missing"))
        XCTAssertEqual(RemoteConfig.parse(Data(#"{"schema":2,"ipad":{"config":{"schema":1}}}"#.utf8)), .defaults("missing"))
    }

    func testTheFirstPictureWaitIsClamped() {
        // Never shorter than today's 10 s.
        for (value, applied) in [("10", 10), ("15", 15), ("20", 20), ("9", nil), ("8", nil), ("7", nil), ("21", nil),
                                 ("0", nil), ("-12", nil),
                                 ("12.5", nil), (#""12""#, nil), ("true", nil), ("null", nil)] as [(String, Int?)] {
            let settings = RemoteConfig.parse(ipad(#","config":{"schema":1,"first_frame_timeout_s":\#(value)}"#))
            XCTAssertEqual(settings, RemoteConfig.Settings(firstFrameTimeoutSeconds: applied, reason: nil), value)
        }
    }

    func testTheSavedConfigAppliesForTwoWeeks() throws {
        let suite = "RemoteConfigTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        XCTAssertEqual(RemoteConfig.read(from: defaults, now: start), .defaults("missing"))
        RemoteConfig.save(RemoteConfig.parse(try example()), to: defaults, now: start)
        let applied = RemoteConfig.Settings(firstFrameTimeoutSeconds: 12, reason: nil)
        XCTAssertEqual(RemoteConfig.read(from: defaults, now: start.addingTimeInterval(14 * day)), applied)
        XCTAssertEqual(RemoteConfig.read(from: defaults, now: start.addingTimeInterval(14 * day + 1)), .defaults("stale"))
        XCTAssertEqual(RemoteConfig.read(from: defaults, now: start.addingTimeInterval(-1)), .defaults("stale"))
        XCTAssertEqual(RemoteConfig.summary(applied, rollout: RemoteConfig.Rollout(start: start, days: 3)),
                       "remote_config=applied rollout=3d first_frame_s=12")
        // The next manifest has none: defaults, said once.
        RemoteConfig.save(RemoteConfig.parse(ipad("")), to: defaults, now: start)
        XCTAssertEqual(RemoteConfig.read(from: defaults, now: start), .defaults("missing"))
        XCTAssertEqual(RemoteConfig.summary(.defaults("missing"), rollout: nil), "remote_config=defaults reason=missing rollout=none")
        // A saved value out of range, as a newer build might write.
        RemoteConfig.save(RemoteConfig.Settings(firstFrameTimeoutSeconds: 60, reason: nil), to: defaults, now: start)
        XCTAssertEqual(RemoteConfig.read(from: defaults, now: start), RemoteConfig.Settings(firstFrameTimeoutSeconds: nil, reason: nil))
        XCTAssertEqual(RemoteConfig.summary(RemoteConfig.read(from: defaults, now: start), rollout: nil),
                       "remote_config=applied rollout=none first_frame_s=default")
    }

    /// Notes with separators or invisible format characters are dropped, as
    /// the PC and check-versions.sh refuse them.
    func testANoteWithHiddenCharactersIsDropped() throws {
        for note in ["Two\u{2028}lines", "Two\u{2029}paragraphs", "Zero\u{200B}width", "Reversed \u{202E}text",
                     "Soft\u{00AD}hyphen", "\u{FEFF}Fixes audio.", "Bell\u{07}"] {
            let escaped = String(data: try JSONSerialization.data(withJSONObject: [note], options: []), encoding: .utf8)!
            let parsed = try XCTUnwrap(UpdateManifest.parse(ipad(#","note":"# + escaped.dropFirst().dropLast())), note)
            XCTAssertNil(parsed.note, note.debugDescription)
        }
        XCTAssertEqual(UpdateManifest.parse(ipad(#","note":"Fixes Pencil pressure — and more.""#))?.note,
                       "Fixes Pencil pressure — and more.")
    }

    /// 0.3.3's decoder, copied here, and this one read the example manifest
    /// with every new field: old apps see the same update.
    func testTodaysParserStillReadsAManifestWithTheNewFields() throws {
        struct Manifest033: Decodable {
            struct IPad: Decodable {
                let version: String
                let build: Int
                let testflight: String?
                var recommended: Bool? = nil
                var note: String? = nil
                private enum CodingKeys: String, CodingKey { case version, build, testflight, recommended, note }
            }
            let schema: Int
            let ipad: IPad?
        }
        let data = try example()
        let old = try JSONDecoder().decode(Manifest033.self, from: data)
        XCTAssertEqual(old.schema, 1)
        XCTAssertEqual(old.ipad?.version, "0.3.4")
        XCTAssertEqual(old.ipad?.build, 320)
        XCTAssertEqual(old.ipad?.note, "Connects faster to PCs that start their second screen slowly.")

        let parsed = try XCTUnwrap(UpdateManifest.parse(data))
        XCTAssertEqual(parsed.build, 320)
        XCTAssertEqual(parsed.rollout, RemoteConfig.Rollout(start: start, days: 3))
        XCTAssertEqual(parsed.note, "Connects faster to PCs that start their second screen slowly.")
    }

    /// Review round 2: a start 15 days out was a typo (shown today), then a
    /// valid start the next day (hidden). Once shown, it stays for that
    /// build, through a restart and a clock set back; the next build waits
    /// for its own turn.
    @MainActor
    func testAPromptThatShowedKeepsShowingForThatBuild() throws {
        let suite = "RemoteConfigTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let installed = InstalledApp(version: "0.3.3", build: 316)
        let id = ids(1)[0]
        let today = start.addingTimeInterval(-15 * day)
        let checker = UpdateChecker(defaults: defaults, source: .injected(try example()), installed: installed,
                                    now: today, installationID: { id })
        checker.checkIfDue(now: today)
        XCTAssertNotNil(checker.popup(now: today))
        for later in [today.addingTimeInterval(day), today.addingTimeInterval(5 * day), today.addingTimeInterval(-day)] {
            XCTAssertNotNil(checker.prompted(now: later), "\(later)")
        }
        let relaunched = UpdateChecker(defaults: defaults, source: .disabled, installed: installed,
                                       now: today, installationID: { id })
        XCTAssertNotNil(relaunched.prompted(now: today.addingTimeInterval(day)))

        let next = Data(#"{"schema":1,"ipad":{"version":"0.3.5","build":330,"rollout":{"start":"2026-11-02T00:00:00Z","days":3}}}"#.utf8)
        let newer = UpdateChecker(defaults: defaults, source: .injected(next), installed: installed,
                                  now: today, installationID: { id })
        newer.checkIfDue(now: start.addingTimeInterval(-day))
        XCTAssertEqual(newer.available?.build, 330)
        XCTAssertNil(newer.prompted(now: start.addingTimeInterval(-day)), "before its start")
    }

    /// A saved rollout is held to the manifest's limits: an out-of-range one
    /// is no rollout, and the update still shows.
    @MainActor
    func testASavedRolloutOutOfRangeIsNoRollout() throws {
        let installed = InstalledApp(version: "0.3.3", build: 316)
        for days in [15, 365, -1, Int.max] {
            let suite = "RemoteConfigTests-\(UUID().uuidString)"
            let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
            defer { defaults.removePersistentDomain(forName: suite) }
            defaults.set("0.3.4", forKey: "updateAvailableVersion")
            defaults.set(320, forKey: "updateAvailableBuild")
            defaults.set(start, forKey: "updateAvailableRolloutStart")
            defaults.set(days, forKey: "updateAvailableRolloutDays")
            let checker = UpdateChecker(defaults: defaults, source: .disabled, installed: installed,
                                        now: start, installationID: { "x" })
            XCTAssertEqual(checker.available?.build, 320, "\(days)")
            XCTAssertNil(checker.available?.rollout, "\(days)")
            XCTAssertNotNil(checker.prompted(now: start), "\(days)")
        }
        XCTAssertNil(RemoteConfig.Rollout(start: start, days: 15))
        XCTAssertNil(RemoteConfig.Rollout(start: Date.distantFuture, days: Int.max))
        let far = try XCTUnwrap(RemoteConfig.Rollout(start: Date.distantFuture, days: 14))
        XCTAssertTrue(far.shows(id: "x", version: "0.3.4", now: start))
    }

    /// The banner and popup wait for this iPad's turn; Settings shows the
    /// update at once, and a relaunch keeps the rollout.
    @MainActor
    func testAStagedBuildWaitsOnlyForTheBannerAndPopup() throws {
        let suite = "RemoteConfigTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let installed = InstalledApp(version: "0.3.3", build: 316)
        let id = try XCTUnwrap(ids(10_000).first {
            let share = Double(RemoteConfig.bucket(id: $0, version: "0.3.4")) / Double(UInt64.max)
            return abs(share - 0.5) < 0.01
        })
        let turn = start.addingTimeInterval(1.5 * day)
        let checker = UpdateChecker(defaults: defaults, source: .injected(try example()), installed: installed,
                                    now: turn.addingTimeInterval(-day), installationID: { id })
        checker.checkIfDue(now: turn.addingTimeInterval(-day))
        XCTAssertEqual(checker.available?.build, 320, "Settings shows it")
        XCTAssertNil(checker.prompted(now: turn.addingTimeInterval(-day)))
        XCTAssertNil(checker.popup(now: turn.addingTimeInterval(-day)))

        // A relaunch keeps the rollout, and the turn.
        let relaunched = UpdateChecker(defaults: defaults, source: .disabled, installed: installed,
                                       now: turn.addingTimeInterval(-day), installationID: { id })
        XCTAssertEqual(relaunched.available?.rollout, RemoteConfig.Rollout(start: start, days: 3))
        XCTAssertNil(relaunched.popup(now: turn.addingTimeInterval(-day)))
        XCTAssertNotNil(relaunched.prompted(now: turn.addingTimeInterval(60)))
        XCTAssertNotNil(relaunched.popup(now: turn.addingTimeInterval(60)))
        // Once shown, it stays for this build.
        XCTAssertNotNil(checker.popup(now: turn.addingTimeInterval(-day)))
        XCTAssertEqual(RemoteConfig.read(from: defaults, now: turn), RemoteConfig.Settings(firstFrameTimeoutSeconds: 12, reason: nil))
    }
}
