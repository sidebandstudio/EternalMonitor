import XCTest
@testable import EternalMonitor

final class UpdateCheckerTests: XCTestCase {
    private func manifest(_ version: String, _ build: Int, schema: Int = 1) -> Data {
        Data("""
        {"schema":\(schema),"windows":{"version":"\(version)","installer":"x","sha256":"y"},
         "ipad":{"version":"\(version)","build":\(build),"testflight":"https://testflight.apple.com/join/ppc9QfXc"},
         "future":true}
        """.utf8)
    }

    func testManifestParsing() {
        let ipad = UpdateManifest.parse(manifest("0.3.2", 170))
        XCTAssertEqual(ipad, UpdateManifest.IPad(version: "0.3.2", build: 170, testflight: "https://testflight.apple.com/join/ppc9QfXc"))
        XCTAssertNil(UpdateManifest.parse(manifest("0.3.2", 170, schema: 2)))
        XCTAssertNil(UpdateManifest.parse(manifest("0.3.2-rc.1", 170)))
        XCTAssertNil(UpdateManifest.parse(manifest("0.3.2", 0)))
        XCTAssertNil(UpdateManifest.parse(Data(#"{"schema":1,"windows":{}}"#.utf8)))
        XCTAssertNil(UpdateManifest.parse(Data("<html>".utf8)))
    }

    private func ipadManifest(_ extra: String) -> Data {
        Data("""
        {"schema":1,"windows":{"version":"0.3.2","installer":"x","sha256":"y","note":"PC only"},
         "ipad":{"version":"0.3.2","build":170,"channel":"beta"\(extra)},"future":{"a":1}}
        """.utf8)
    }

    func testManifestReadsTheOptionalRecommendedFlagAndNote() throws {
        // Missing: recommended, no note. Unknown keys are ignored.
        let plain = try XCTUnwrap(UpdateManifest.parse(ipadManifest("")))
        XCTAssertEqual(plain, UpdateManifest.IPad(version: "0.3.2", build: 170, testflight: nil))
        XCTAssertTrue(plain.isRecommended)
        XCTAssertNil(plain.note)

        let soft = try XCTUnwrap(UpdateManifest.parse(ipadManifest(#","recommended":false,"note":"  Fixes Pencil pressure.  ""#)))
        XCTAssertFalse(soft.isRecommended)
        XCTAssertEqual(soft.note, "Fixes Pencil pressure.")

        // A note that isn't one short line, or a field of the wrong type, is
        // dropped instead of hiding the update.
        let long = String(repeating: "x", count: 201)
        for extra in [#","note":"""#, #","note":"   ""#, #","note":"Two\nlines""#, #","note":"\#(long)""#,
                      #","note":7,"recommended":"yes""#, #","note":null,"recommended":null"#] {
            let parsed = try XCTUnwrap(UpdateManifest.parse(ipadManifest(extra)), extra)
            XCTAssertNil(parsed.note, extra)
            XCTAssertTrue(parsed.isRecommended, extra)
        }
        let longest = String(repeating: "x", count: 200)
        XCTAssertEqual(UpdateManifest.parse(ipadManifest(#","note":"\#(longest)""#))?.note?.count, 200)
    }

    func testPopupShowsOncePerBuildThenAtMostEveryThreeDays() {
        let installed = InstalledApp(version: "0.3.1", build: 166)
        let available = UpdateManifest.IPad(version: "0.3.2", build: 170, testflight: nil)
        let now = Date(timeIntervalSince1970: 1_000_000)
        let day: TimeInterval = 86_400
        let make = { (available: UpdateManifest.IPad?, closed: (build: Int, at: Date)?, at: Date) in
            UpdatePopupModel.make(available: available, installed: installed, automatic: true, closed: closed, now: at)
        }
        let popup = make(available, nil, now)
        XCTAssertEqual(popup, UpdatePopupModel(version: "0.3.2", build: 170, recommended: true, note: nil))
        XCTAssertEqual(popup?.title, "A new version of EternalMonitor is ready")
        XCTAssertEqual(popup?.message, "Updating is highly recommended.")

        let closed = (build: 170, at: now)
        XCTAssertNil(make(available, closed, now.addingTimeInterval(3 * day - 1)))
        XCTAssertNotNil(make(available, closed, now.addingTimeInterval(3 * day)))
        // A clock set back counts as closed now: not at once, and back three days from this clock.
        let rolledBack = now.addingTimeInterval(-1)
        XCTAssertNil(make(available, closed, rolledBack), "a clock set back must not show it at once")
        XCTAssertNil(make(available, closed, now.addingTimeInterval(-60)))
        let next = UpdateManifest.IPad(version: "0.3.2", build: 171, testflight: nil)
        XCTAssertNotNil(make(next, closed, now), "a newer build shows at once")
        XCTAssertNil(make(nil, nil, now))
        XCTAssertNil(make(UpdateManifest.IPad(version: "0.3.1", build: 166, testflight: nil), nil, now))
        XCTAssertNil(UpdatePopupModel.make(available: available, installed: installed, automatic: false, closed: nil, now: now))

        let soft = UpdateManifest.IPad(version: "0.3.2", build: 170, testflight: nil, recommended: false, note: "Fixes Pencil pressure.")
        XCTAssertEqual(make(soft, nil, now)?.message, "An update is available.")
        XCTAssertEqual(make(soft, nil, now)?.note, "Fixes Pencil pressure.")
    }

    @MainActor
    func testCheckerRemembersThePopupAndTheNote() throws {
        let suite = "UpdateCheckerTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let installed = InstalledApp(version: "0.3.1", build: 166)
        let data = Data(#"{"schema":1,"ipad":{"version":"0.3.2","build":170,"recommended":false,"note":"Fixes Pencil pressure."}}"#.utf8)
        let checker = UpdateChecker(defaults: defaults, source: .injected(data), installed: installed)
        checker.checkIfDue()
        let now = Date()
        XCTAssertEqual(checker.popup(now: now)?.note, "Fixes Pencil pressure.")
        checker.closePopup(now: now)
        XCTAssertNil(checker.popup(now: now))
        XCTAssertNotNil(checker.banner, "Later on the popup leaves the banner")

        let relaunched = UpdateChecker(defaults: defaults, source: .disabled, installed: installed)
        XCTAssertNil(relaunched.popup(now: now.addingTimeInterval(60)))
        let later = relaunched.popup(now: now.addingTimeInterval(UpdatePopupModel.closeInterval))
        XCTAssertEqual(later?.message, "An update is available.")
        XCTAssertEqual(later?.note, "Fixes Pencil pressure.")
    }

    @MainActor
    func testAClockSetBackOrFarAheadWaitsThreeDaysFromNow() throws {
        let suite = "UpdateCheckerTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let installed = InstalledApp(version: "0.3.1", build: 166)
        let data = Data(#"{"schema":1,"ipad":{"version":"0.3.2","build":170}}"#.utf8)
        let now = Date(timeIntervalSince1970: 2_000_000_000)
        let checker = UpdateChecker(defaults: defaults, source: .injected(data), installed: installed, now: now)
        checker.checkIfDue(now: now)

        // Closed, then the clock goes back one second.
        checker.closePopup(now: now)
        let back = now.addingTimeInterval(-1)
        checker.checkIfDue(now: back)
        XCTAssertNil(checker.popup(now: back))
        XCTAssertNil(checker.popup(now: back.addingTimeInterval(UpdatePopupModel.closeInterval - 1)))
        XCTAssertNotNil(checker.popup(now: back.addingTimeInterval(UpdatePopupModel.closeInterval)))

        // Closed while the clock was ten years ahead, then corrected: three days at most.
        checker.closePopup(now: now.addingTimeInterval(10 * 365 * 86_400))
        let relaunched = UpdateChecker(defaults: defaults, source: .disabled, installed: installed, now: now)
        XCTAssertEqual(relaunched.popupClosed?.at, now, "rewritten as closed now")
        XCTAssertEqual(defaults.object(forKey: "updatePopupClosedAt") as? Date, now)
        XCTAssertNil(relaunched.popup(now: now.addingTimeInterval(UpdatePopupModel.closeInterval - 1)))
        XCTAssertNotNil(relaunched.popup(now: now.addingTimeInterval(UpdatePopupModel.closeInterval)))
    }

    func testThisAppTellsThePCItChecksForUpdates() {
        // 0.3.1 sent only bits 0 to 2, so the PC reads a missing bit 4 as an older app.
        XCTAssertEqual(Hello2.featureChecksUpdates, 1 << 4)
        for bit in [Hello2.featureWantsInput, Hello2.featureWantsAudio, Hello2.featureSupportsNack] {
            XCTAssertEqual(bit & Hello2.featureChecksUpdates, 0)
        }
        for input in [false, true] {
            for audio in [false, true] {
                for usb in [false, true] {
                    let caps = ConnectionManager.featureCaps(wantsInput: input, wantsAudio: audio, isUSB: usb)
                    XCTAssertNotEqual(caps & Hello2.featureChecksUpdates, 0)
                    XCTAssertEqual(caps & Hello2.featureWantsInput != 0, input)
                    XCTAssertEqual(caps & Hello2.featureWantsAudio != 0, audio)
                    XCTAssertEqual(caps & Hello2.featureSupportsNack != 0, !usb)
                }
            }
        }
    }

    func testVersionCompare() {
        XCTAssertLessThan(AppVersion("0.3.9")!, AppVersion("0.3.10")!)
        XCTAssertLessThan(AppVersion("0.9.0")!, AppVersion("1.0.0")!)
        XCTAssertEqual(AppVersion("1.2.3"), AppVersion("1.2.3"))
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "1.2.3-rc.1", "v1.2.3", "1..3", "１.2.3"] {
            XCTAssertNil(AppVersion(bad), bad)
        }
        let installed = InstalledApp(version: "0.3.1", build: 166)
        let newer = { (version: String, build: Int) in
            installed.isOlder(than: UpdateManifest.IPad(version: version, build: build, testflight: nil))
        }
        XCTAssertTrue(newer("0.3.2", 1), "a newer version wins whatever its build")
        XCTAssertTrue(newer("0.3.1", 170), "same version, newer TestFlight build")
        XCTAssertFalse(newer("0.3.1", 166))
        XCTAssertFalse(newer("0.3.1", 150))
        XCTAssertFalse(newer("0.3.0", 999))
        XCTAssertFalse(InstalledApp(version: "dev", build: 1).isOlder(than: .init(version: "9.9.9", build: 9, testflight: nil)))
    }

    func testBannerModel() {
        let installed = InstalledApp(version: "0.3.1", build: 166)
        let available = UpdateManifest.IPad(version: "0.3.2", build: 170, testflight: nil)
        let now = Date(timeIntervalSince1970: 1_000_000)
        let banner = UpdateBannerModel.make(available: available, installed: installed, automatic: true, dismissed: nil, now: now)
        XCTAssertEqual(banner, UpdateBannerModel(version: "0.3.2", build: 170))
        XCTAssertEqual(banner?.title, "A new version is ready in TestFlight")
        XCTAssertTrue(banner?.detail.contains("Automatic Updates") == true)
        XCTAssertNil(UpdateBannerModel.make(available: available, installed: installed, automatic: false, dismissed: nil, now: now))
        XCTAssertNil(UpdateBannerModel.make(available: nil, installed: installed, automatic: true, dismissed: nil, now: now))
        let current = UpdateManifest.IPad(version: "0.3.1", build: 166, testflight: nil)
        XCTAssertNil(UpdateBannerModel.make(available: current, installed: installed, automatic: true, dismissed: nil, now: now))
        // Hidden for a day for the dismissed build only.
        let hidden = (build: 170, at: now)
        XCTAssertNil(UpdateBannerModel.make(available: available, installed: installed, automatic: true, dismissed: hidden, now: now.addingTimeInterval(3600)))
        XCTAssertNotNil(UpdateBannerModel.make(available: available, installed: installed, automatic: true, dismissed: hidden, now: now.addingTimeInterval(86_400)))
        let next = UpdateManifest.IPad(version: "0.3.2", build: 171, testflight: nil)
        XCTAssertNotNil(UpdateBannerModel.make(available: next, installed: installed, automatic: true, dismissed: hidden, now: now))
    }

    @MainActor
    func testCheckerRemembersTheUpdateAndRespectsTheSetting() throws {
        let suite = "UpdateCheckerTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let installed = InstalledApp(version: "0.3.1", build: 166)

        let checker = UpdateChecker(defaults: defaults, source: .injected(manifest("0.3.2", 170)), installed: installed)
        XCTAssertTrue(checker.automatic)
        XCTAssertNil(checker.available)
        checker.checkIfDue()
        XCTAssertEqual(checker.available?.build, 170)
        XCTAssertNotNil(checker.banner)
        XCTAssertEqual(checker.joinURL.absoluteString, "https://testflight.apple.com/join/ppc9QfXc")

        // Remembered across launches, even offline.
        let relaunched = UpdateChecker(defaults: defaults, source: .disabled, installed: installed)
        XCTAssertEqual(relaunched.available?.version, "0.3.2")
        relaunched.dismissBanner()
        XCTAssertNil(relaunched.banner)
        XCTAssertNil(UpdateChecker(defaults: defaults, source: .disabled, installed: installed).banner)

        // Once installed, the stored update no longer counts.
        let updated = UpdateChecker(defaults: defaults, source: .disabled, installed: InstalledApp(version: "0.3.2", build: 170))
        XCTAssertNil(updated.available)

        // Turning automatic checks off hides it and stops checking.
        let off = UpdateChecker(defaults: defaults, source: .injected(manifest("0.3.3", 180)), installed: installed)
        off.automatic = false
        XCTAssertNil(off.banner)
        XCTAssertFalse(UpdateChecker(defaults: defaults, source: .disabled, installed: installed).automatic)
    }

    @MainActor
    func testUntrustedTestFlightLinkFallsBackToTheInvite() throws {
        let suite = "UpdateCheckerTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let data = Data(#"{"schema":1,"ipad":{"version":"0.4.0","build":200,"testflight":"https://evil.example/join"}}"#.utf8)
        let checker = UpdateChecker(defaults: defaults, source: .injected(data), installed: InstalledApp(version: "0.3.1", build: 166))
        checker.checkIfDue()
        XCTAssertEqual(checker.joinURL, UpdateChecker.joinURL)
    }

    func testVersionMismatchNamesTheOlderSide() {
        XCTAssertNil(VersionMismatch.classify(hostProtocol: 2, iPadMin: 2, iPadMax: 2))
        XCTAssertEqual(VersionMismatch.classify(hostProtocol: 3, iPadMin: 2, iPadMax: 2), .iPadOlder)
        XCTAssertEqual(VersionMismatch.classify(hostProtocol: 2, iPadMin: 3, iPadMax: 4), .pcOlder)
        XCTAssertTrue(VersionMismatch.iPadOlder.opensTestFlight)
        XCTAssertFalse(VersionMismatch.pcOlder.opensTestFlight)
        XCTAssertFalse(VersionMismatch.pcLegacy.opensTestFlight)
        XCTAssertTrue(VersionMismatch.iPadOlder.message.contains("Update the iPad app: open TestFlight"))
        XCTAssertTrue(VersionMismatch.pcOlder.message.contains("click Update now on the PC"))
        XCTAssertTrue(VersionMismatch.pcLegacy.message.contains("eternalmonitor.dev"))
    }
}
