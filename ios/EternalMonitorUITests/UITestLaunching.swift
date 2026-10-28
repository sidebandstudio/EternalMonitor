import XCTest

/// Every UI test case: leaves no app running for the next test's launch.
class UITestCase: XCTestCase {
    override func tearDown() {
        stopIfRunning(XCUIApplication())
        super.tearDown()
    }
}

extension XCTestCase {
    /// What XCTest reports when its runner misses the exit of an app it
    /// terminated. On CI's simulator the app exited 70 ms after SIGTERM,
    /// but testmanagerd never passed that on, and the launch that had asked
    /// for it failed after 60 s.
    private static let missedExit = "Failed to terminate com.eternal.monitor"

    /// Launches `app` with nothing left for XCTest to terminate first. If
    /// XCTest still misses an exit, that one failure is expected and the app
    /// is launched once more. A crash of the app reports a different issue,
    /// so it, any other launch failure and a second miss fail the test.
    func launchCleanly(_ app: XCUIApplication) {
        stopIfRunning(app)
        if expectingMissedExit({ app.launch() }) {
            print("runner failed to terminate app; relaunching")
            app.launch()
        }
    }

    /// Terminates `app` if it is running, and waits until XCTest sees it
    /// gone. The 30 s are only a backstop.
    func stopIfRunning(_ app: XCUIApplication) {
        guard app.state != .notRunning else { return }
        if expectingMissedExit({ app.terminate() }) {
            print("runner failed to terminate app; waiting for it to stop")
        }
        let gone = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "state == %d", XCUIApplication.State.notRunning.rawValue),
            object: app)
        if XCTWaiter().wait(for: [gone], timeout: 30) != .completed {
            print("runner still sees the app running after 30 s")
        }
    }

    /// Runs `body` with XCTest's missed-exit failure, and only that one,
    /// expected. Returns whether it happened.
    func expectingMissedExit(_ body: () -> Void) -> Bool {
        var missed = false
        let options = XCTExpectedFailure.Options()
        options.isStrict = false
        options.issueMatcher = { issue in
            let matches = issue.compactDescription.contains(Self.missedExit)
            missed = missed || matches
            return matches
        }
        // An expected failure must not stop the test before the relaunch.
        let continues = continueAfterFailure
        continueAfterFailure = true
        XCTExpectFailure("XCTest missed the app's exit", options: options, failingBlock: body)
        continueAfterFailure = continues
        return missed
    }
}

final class UITestLaunchingTests: XCTestCase {
    /// The runner's missed exit is expected, nothing else is: a crash of the
    /// app during a launch still fails the test.
    func testOnlyTheMissedExitIsExpected() {
        XCTAssertTrue(expectingMissedExit {
            XCTFail("Failed to terminate com.eternal.monitor:20016: Failed to terminate com.eternal.monitor:0")
        })
        XCTAssertFalse(expectingMissedExit {})
        XCTExpectFailure("a crash is not the runner missing an exit") {
            XCTAssertFalse(expectingMissedExit { XCTFail("com.eternal.monitor crashed in ConnectionManager") })
        }
    }
}
