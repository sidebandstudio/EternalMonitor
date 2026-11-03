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
    /// the test. e2e_keyboard.sh checks the PC's commands and pictures from
    /// the clean run, and reports the retries.
    func testKeyboardRelay() throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["EM_INPUT_HOST_LOG"])
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES", "-playPCaudio", "NO",
                               "-pictureFit", "fit", "-uiTestProbe", "YES"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        let started = Date()
        for run in 1...3 {
            var shots: [(name: String, screenshot: XCUIScreenshot)] = []
            let start = relayKeys(app: app, hostLog: path, shots: &shots)
            for shot in shots {
                let attachment = XCTAttachment(screenshot: shot.screenshot)
                attachment.name = start == nil ? "lost-signal-run\(run)-\(shot.name)" : shot.name
                attachment.lifetime = .keepAlways
                add(attachment)
            }
            if let start {
                let clean = XCTAttachment(string: "run=\(run) offset=\(start)")
                clean.name = "keyboard-clean-run"
                clean.lifetime = .keepAlways
                add(clean)
                return
            }
            XCTContext.runActivity(named: "The signal was lost: start again in a fresh app") { _ in stopIfRunning(app) }
            // A run takes about 45 s on a starved CI runner, and System E2E
            // jobs on base left 2.5 to 6.5 of their 30 min. Starting no run
            // after 90 s keeps the retries to about 1.5 min.
            if Date().timeIntervalSince(started) > 90 { break }
        }
        XCTFail("The signal was lost, and no clean run followed")
    }

    /// One run in a fresh app. Returns the host log's size when the run's
    /// commands began if every check passed with no lost signal, or nil if
    /// the signal was lost. Adds the run's screenshots to `shots`.
    private func relayKeys(app: XCUIApplication, hostLog path: String,
                           shots: inout [(name: String, screenshot: XCUIScreenshot)]) -> Int? {
        launchCleanly(app)
        let keyboard = app.buttons["display.keyboard"]
        // The app is fresh, so any lost signal it counts is this run's.
        // A failed check only fails the test if the signal was not lost.
        func check(_ passed: Bool, _ message: @autoclosure () -> String, line: UInt = #line) -> Bool {
            if passed { return true }
            if signalLosses(app) > 0 { return false }
            XCTFail(message(), line: line)
            return false
        }
        // The HUD can come and go while a starved runner waits for the app
        // to idle; the probe still shows that it appeared.
        let hud = app.displayAndHUDProblem()
        guard check(hud == nil, hud ?? "") else { return nil }
        // The PC's commands count from here. The host has taken this app's
        // session, so a lost run's late key releases came before.
        let before = (try? Data(contentsOf: URL(fileURLWithPath: path)).count) ?? 0
        guard check(app.triedHUDControl("display.keyboard", until: app.keyboards.firstMatch, reveal: .threeFingersTogether),
                    "The Keyboard button did not open the keyboard") else { return nil }
        app.typeText("Hi!\n")
        let commands = ["Unicode(72)", "Unicode(105)", "Unicode(33)", "KeyDown { scan: 28, extended: false }", "KeyUp { scan: 28, extended: false }"]
        let relayed = NSPredicate { _, _ in
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)), data.count >= before else { return false }
            let log = String(decoding: data.dropFirst(before), as: UTF8.self)
            return commands.allSatisfy { log.contains($0) }
        }
        guard check(XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: relayed, object: nil)], timeout: 10) == .completed,
                    "Hi! and Enter did not reach the PC") else { return nil }
        shots.append(("keyboard-relay", app.screenshot()))
        let control = app.buttons["keyboard.key.224"]
        guard check(app.tapKeyboardKey(control, until: "On", hostLog: path) == "On",
                    "Ctrl did not turn on: \(value(of: control))") else { return nil }
        guard check(app.tapKeyboardKey(app.buttons["keyboard.key.80"], until: "Off", on: control, hostLog: path) == "Off",
                    "← did not release Ctrl: \(value(of: control))") else { return nil }
        guard check(tap(app.keyboards.buttons["Hide keyboard"], app: app) && closes(app.keyboards.firstMatch),
                    "Hide keyboard did not close the keyboard") else { return nil }
        guard verifyDismissalAndRestoreHUD(app: app, keyboard: keyboard, check: check) else { return nil }
        shots.append(("keyboard-dismissed", app.screenshot()))
        // A slow screenshot can use up the HUD's 5 s timer (CI took 2.3 s
        // here while the simulator's decoder stalled), and on a starved runner
        // XCTest's own idle wait and hit test outlasted it (5 s on #75), so the
        // button vanished mid-tap. Let the HUD hide, then reveal and tap in
        // one step, again if it hides first.
        guard check(closes(keyboard, within: 6), "The controls did not hide") else { return nil }
        let done = app.buttons["keyboard.done"]
        guard check(app.triedHUDControl("display.keyboard", until: done, reveal: .threeFingersTogether),
                    "The Keyboard button did not open the keyboard again") else { return nil }
        guard check(tap(done, app: app) && closes(app.keyboards.firstMatch),
                    "Done did not close the keyboard") else { return nil }
        guard verifyDismissalAndRestoreHUD(app: app, keyboard: keyboard, check: check) else { return nil }
        // A lost signal can close the keyboard too, so a run with one proves
        // nothing even if every check passed.
        guard signalLosses(app) == 0 else { return nil }
        app.tapHUDControl("display.disconnect", until: app.textFields["connect.host"], reveal: .threeFingersTogether)
        stopIfRunning(app)
        return before
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

    /// The signal losses the app counted, Connect screen or not.
    private func signalLosses(_ app: XCUIApplication) -> Int {
        app.probe()?.signalLosses ?? 0
    }

    /// Touches `key` where a snapshot found it. False if it was gone: an
    /// element tap would fail the test outright.
    private func tap(_ key: XCUIElement, app: XCUIApplication) -> Bool {
        guard let frame = try? key.snapshot().frame else { return false }
        app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
        return true
    }

    private func value(of key: XCUIElement) -> String {
        (try? key.snapshot())?.value as? String ?? "gone"
    }

    // A waiter can time out without looking again when the runner stalls
    // past its deadline, so this looks once more before giving up.
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
}
