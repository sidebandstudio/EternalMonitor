import XCTest

final class KeyboardRelayTests: UITestCase {
    /// When the PC's heartbeats stop for 3 s the app shows Reconnecting,
    /// closes the keyboard and releases held keys. On #164's starved runner
    /// the host restarted its stalled pipeline while XCTest spent 6 s finding
    /// Hide keyboard, so the key vanished mid-tap. A check that fails after
    /// the app counted a lost signal proves nothing either way, and neither
    /// does a run the signal dropped out of, so the whole run starts again in
    /// a fresh app: a keyboard opened again in the same app shows only its
    /// bar on the simulator, without Hide keyboard. Any other failure fails
    /// the test. e2e_keyboard.sh checks the PC's commands from the clean run.
    func testKeyboardRelay() throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["EM_INPUT_HOST_LOG"])
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES", "-playPCaudio", "NO",
                               "-pictureFit", "fit", "-uiTestProbe", "YES"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        for _ in 0..<3 {
            let before = (try? Data(contentsOf: URL(fileURLWithPath: path)).count) ?? 0
            if relayKeys(app: app, hostLog: path, from: before) {
                let start = XCTAttachment(string: String(before))
                start.name = "keyboard-host-log-start"
                start.lifetime = .keepAlways
                add(start)
                return
            }
            XCTContext.runActivity(named: "The signal was lost: start again in a fresh app") { _ in app.terminate() }
        }
        XCTFail("The signal was lost in every run")
    }

    /// One run in a fresh app: true if every check passed with no lost
    /// signal, false if the signal was lost.
    private func relayKeys(app: XCUIApplication, hostLog path: String, from before: Int) -> Bool {
        launchCleanly(app)
        let keyboard = app.buttons["display.keyboard"]
        // The HUD can come and go while a starved runner waits for the app
        // to idle; the probe still shows that it appeared.
        app.waitForDisplayAndHUD()
        let losses = signalLosses(app)
        // A failed check only fails the test if the signal was not lost.
        func check(_ passed: Bool, _ message: @autoclosure () -> String, line: UInt = #line) -> Bool {
            if passed { return true }
            if signalLosses(app) > losses { return false }
            XCTFail(message(), line: line)
            return false
        }
        guard check(app.triedHUDControl("display.keyboard", until: app.keyboards.firstMatch, reveal: .threeFingersTogether),
                    "The Keyboard button did not open the keyboard") else { return false }
        app.typeText("Hi!\n")
        let commands = ["Unicode(72)", "Unicode(105)", "Unicode(33)", "KeyDown { scan: 28, extended: false }", "KeyUp { scan: 28, extended: false }"]
        let relayed = NSPredicate { _, _ in
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)), data.count >= before else { return false }
            let log = String(decoding: data.dropFirst(before), as: UTF8.self)
            return commands.allSatisfy { log.contains($0) }
        }
        guard check(XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: relayed, object: nil)], timeout: 10) == .completed,
                    "Hi! and Enter did not reach the PC") else { return false }
        capture("keyboard-relay", app: app)
        let control = app.buttons["keyboard.key.224"]
        guard check(tap(control, app: app) && becomes(control, "On"),
                    "Ctrl did not turn on: \(value(of: control))") else { return false }
        guard check(tap(app.buttons["keyboard.key.80"], app: app) && becomes(control, "Off"),
                    "← did not release Ctrl: \(value(of: control))") else { return false }
        guard check(tap(app.keyboards.buttons["Hide keyboard"], app: app) && closes(app.keyboards.firstMatch),
                    "Hide keyboard did not close the keyboard") else { return false }
        guard verifyDismissalAndRestoreHUD(app: app, keyboard: keyboard, check: check) else { return false }
        capture("keyboard-dismissed", app: app)
        // A slow screenshot can use up the HUD's 5 s timer (CI took 2.3 s
        // here while the simulator's decoder stalled), and on a starved runner
        // XCTest's own idle wait and hit test outlasted it (5 s on #75), so the
        // button vanished mid-tap. Let the HUD hide, then reveal and tap in
        // one step, again if it hides first.
        guard check(closes(keyboard, within: 6), "The controls did not hide") else { return false }
        let done = app.buttons["keyboard.done"]
        guard check(app.triedHUDControl("display.keyboard", until: done, reveal: .threeFingersTogether),
                    "The Keyboard button did not open the keyboard again") else { return false }
        guard check(tap(done, app: app) && closes(app.keyboards.firstMatch),
                    "Done did not close the keyboard") else { return false }
        guard verifyDismissalAndRestoreHUD(app: app, keyboard: keyboard, check: check) else { return false }
        // The probe goes with the display, so look before disconnecting.
        guard signalLosses(app) == losses else { return false }
        app.tapHUDControl("display.disconnect", until: app.textFields["connect.host"], reveal: .threeFingersTogether)
        app.terminate()
        return true
    }

    private func verifyDismissalAndRestoreHUD(app: XCUIApplication, keyboard: XCUIElement,
                                              check: (Bool, @autoclosure () -> String, UInt) -> Bool) -> Bool {
        guard check(closes(app.keyboards.firstMatch), "The keyboard did not close", #line) else { return false }
        // Accessibility queries may outlast the normal HUD timer. Prove the
        // timeout first, then bring the controls back before checking state.
        guard check(closes(keyboard, within: 6), "The controls did not hide", #line) else { return false }
        revealHUD(app: app)
        guard check(keyboard.waitForExistence(timeout: 5), "The controls did not come back", #line) else { return false }
        let controls = app.triedHUDControls(["display.keyboard", "display.disconnect"], reveal: .threeFingersTogether)
        return check(controls["display.keyboard"]?.label == "Keyboard" && controls["display.disconnect"] != nil,
                     "The controls came back as \(controls.mapValues(\.label))", #line)
    }

    /// The signal losses the app counted. A reconnect shows the Connect
    /// screen for a moment, so wait for the display's probe.
    private func signalLosses(_ app: XCUIApplication) -> Int {
        _ = app.descendants(matching: .any)["debug.probe"].firstMatch.waitForExistence(timeout: 15)
        return app.probe()?.signalLosses ?? 0
    }

    /// Touches `key` where a snapshot found it. False if it was gone: an
    /// element tap would fail the test outright.
    private func tap(_ key: XCUIElement, app: XCUIApplication) -> Bool {
        guard let frame = try? key.snapshot().frame else { return false }
        app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
        return true
    }

    // A waiter can time out without looking again when the runner stalls
    // past its deadline, so these look once more before giving up.
    private func becomes(_ key: XCUIElement, _ value: String) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == true AND value == %@", value), object: key)
        return XCTWaiter().wait(for: [expectation], timeout: 5) == .completed || self.value(of: key) == value
    }

    private func value(of key: XCUIElement) -> String {
        (try? key.snapshot())?.value as? String ?? "gone"
    }

    private func closes(_ element: XCUIElement, within timeout: TimeInterval = 5) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: element)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed || !element.exists
    }

    /// XCTest spreads a three-finger tap from the top edge to the bottom
    /// edge. Its bottom finger lands where the sliding bar's Keyboard button
    /// is near the end of the hide animation, which opened the keyboard
    /// again in CI. Tap with three fingers together in the middle instead.
    private func revealHUD(app: XCUIApplication) {
        app.revealHUD(.threeFingersTogether)
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
