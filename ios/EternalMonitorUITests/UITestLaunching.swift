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
    /// terminated, whole: "Failed to terminate com.eternal.monitor:20016:
    /// Failed to terminate com.eternal.monitor:0" on CI, where the app exited
    /// 70 ms after SIGTERM but testmanagerd never passed that on, and the
    /// launch that had asked for it failed after 60 s.
    private static let missedExit = try! NSRegularExpression(
        pattern: #"^Failed to terminate com\.eternal\.monitor:\d+(: Failed to terminate com\.eternal\.monitor:\d+)?$"#)

    /// Whether `issue` is that fault. XCTest records its runner's launch
    /// faults as assertion failures with no source location; every XCTAssert
    /// and XCTFail in test code has one, so none of them can pass for it.
    static func isMissedExit(_ issue: XCTIssue) -> Bool {
        let text = issue.compactDescription
        return issue.type == .assertionFailure
            && issue.sourceCodeContext.location == nil
            && missedExit.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) != nil
    }

    /// Launches `app` with nothing left for XCTest to terminate first. If
    /// XCTest still misses an exit, that one failure is expected and the app
    /// is launched once more. A crash of the app reports a different issue,
    /// so it, any other launch failure and a second miss fail the test.
    /// The app follows the device's language unless the test chooses one:
    /// a test that failed after choosing one in the app leaves it saved.
    func launchCleanly(_ app: XCUIApplication) {
        if !app.launchArguments.contains("-appLanguage") {
            app.launchArguments += ["-appLanguage", ""]
        }
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
            let matches = Self.isMissedExit(issue)
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
    private static let fault = "Failed to terminate com.eternal.monitor:20016: Failed to terminate com.eternal.monitor:0"

    /// An issue as XCTest's runner raises it: an assertion failure with no
    /// source location. (Recording one from here would give it this file's
    /// location, so the matcher is checked on the issue itself.)
    private func issue(_ text: String, type: XCTIssue.IssueType = .assertionFailure,
                       line: Int? = nil) -> XCTIssue {
        let context = line.map {
            XCTSourceCodeContext(location: XCTSourceCodeLocation(filePath: #filePath, lineNumber: $0))
        } ?? XCTSourceCodeContext()
        return XCTIssue(type: type, compactDescription: text, sourceCodeContext: context)
    }

    /// Only the runner's missed exit, whole, is that fault: not the same text
    /// from an assertion in test code, another launch fault, the text inside
    /// another message, another issue type, or a crash of the app.
    func testOnlyTheRunnersMissedExitMatches() {
        XCTAssertTrue(Self.isMissedExit(issue(Self.fault)))
        XCTAssertTrue(Self.isMissedExit(issue("Failed to terminate com.eternal.monitor:20016")))
        XCTAssertFalse(Self.isMissedExit(issue(Self.fault, line: 7)))
        XCTAssertFalse(Self.isMissedExit(issue("Failed to launch com.eternal.monitor: The request to open failed.")))
        XCTAssertFalse(Self.isMissedExit(issue("Setup failed: \(Self.fault)")))
        XCTAssertFalse(Self.isMissedExit(issue("\(Self.fault) (then the app crashed)")))
        XCTAssertFalse(Self.isMissedExit(issue(Self.fault, type: .system)))
        XCTAssertFalse(Self.isMissedExit(issue("com.eternal.monitor crashed in ConnectionManager")))
    }

    /// Through the helper, failures in test code are still failures, even
    /// with the fault's text, and no failure means no relaunch.
    func testFailuresInTestCodeStillFail() {
        XCTAssertFalse(expectingMissedExit {})
        for text in [Self.fault, "com.eternal.monitor crashed in ConnectionManager"] {
            // Strict, and only for this failure: if the helper swallowed it,
            // this expectation goes unmet and the test fails.
            let options = XCTExpectedFailure.Options()
            options.issueMatcher = { $0.compactDescription.hasSuffix(text) }
            XCTExpectFailure("\(text) is not the runner's missed exit", options: options) {
                XCTAssertFalse(expectingMissedExit { XCTFail(text) }, "the helper expected a failure in test code")
            }
        }
    }
}
