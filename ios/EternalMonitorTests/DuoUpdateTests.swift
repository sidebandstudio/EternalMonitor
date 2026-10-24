import XCTest
@testable import EternalMonitor

/// An iPhone Duo test build ships under its own bundle ID. The manifest's
/// update is the public app's, so the test app must never offer it.
@MainActor
final class DuoUpdateTests: XCTestCase {
    private let manifest = Data("""
        {"schema":1,"ipad":{"version":"0.4.0","build":200,"testflight":"https://testflight.apple.com/join/ppc9QfXc"}}
        """.utf8)
    private let available = UpdateManifest.IPad(version: "0.4.0", build: 200, testflight: nil)

    func testTheTestAppShowsNoUpdateBannerPopupOrSettingsRow() throws {
        let suite = "DuoUpdateTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let test = InstalledApp(version: "0.3.2", build: 7, bundleID: "com.eternal.monitor.duo")
        XCTAssertFalse(test.receivesPublicUpdates)
        XCTAssertFalse(test.isOlder(than: available))

        let checker = UpdateChecker(defaults: defaults, source: .injected(manifest), installed: test)
        checker.checkIfDue()
        // Settings shows its row only while `available` is set.
        XCTAssertNil(checker.available)
        XCTAssertNil(checker.banner)
        XCTAssertNil(checker.popup)
        XCTAssertNil(UpdateBannerModel.make(available: available, installed: test, automatic: true, dismissed: nil, now: Date()))
        XCTAssertNil(UpdatePopupModel.make(available: available, installed: test, automatic: true, closed: nil, now: Date()))
        // Nor from an update a public build stored before.
        let publicApp = UpdateChecker(defaults: defaults, source: .injected(manifest),
                                      installed: InstalledApp(version: "0.3.2", build: 7))
        publicApp.checkIfDue()
        XCTAssertNotNil(publicApp.available)
        XCTAssertNil(UpdateChecker(defaults: defaults, source: .disabled, installed: test).available)
    }

    /// The test app is never told to update the public app. The PC's side
    /// reads the same in both.
    func testTheTestAppsVersionMismatchNamesItsOwnBuild() {
        let noun = DeviceNoun.current
        let older = VersionMismatch.iPadOlder
        XCTAssertEqual(older.message(publicApp: false),
                       "This \(noun) app is older than EternalMonitor on your PC, so they can't connect. "
                       + "Install the newest build of this test app from TestFlight.")
        XCTAssertFalse(older.message(publicApp: false).contains("tap Update"))
        XCTAssertEqual(older.message(publicApp: true),
                       "This \(noun) app is older than EternalMonitor on your PC, so they can't connect. "
                       + "Update the \(noun) app: open TestFlight and tap Update.")
        // This test bundle runs inside the public app.
        XCTAssertEqual(older.message, older.message(publicApp: true))
        for pc in [VersionMismatch.pcOlder, .pcLegacy] {
            XCTAssertEqual(pc.message(publicApp: false), pc.message(publicApp: true))
            XCTAssertEqual(pc.message, pc.message(publicApp: true))
        }
    }

    func testThePublicAppStillOffersTheUpdate() {
        let installed = InstalledApp(version: "0.3.2", build: 7)
        XCTAssertEqual(installed.bundleID, "com.eternal.monitor")
        XCTAssertTrue(installed.isOlder(than: available))
        XCTAssertNotNil(UpdateBannerModel.make(available: available, installed: installed, automatic: true, dismissed: nil, now: Date()))
        // This test bundle runs inside the public app.
        XCTAssertTrue(InstalledApp.current.receivesPublicUpdates)
    }
}
