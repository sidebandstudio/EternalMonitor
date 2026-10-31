import UIKit
import XCTest
@testable import EternalMonitor

/// iPadOS shrinks the software keyboard to its accessory bar in two cases:
/// the user taps the Hide Keyboard key, or a hardware keyboard starts
/// typing. Only the first one closes the remote keyboard.
@MainActor
final class KeyboardCollapseTests: XCTestCase {
    /// A key from a hardware keyboard. With no code the relay sends nothing
    /// for it.
    private final class HardwareKey: UIKey {
        private let usage: UIKeyboardHIDUsage
        init(_ usage: UIKeyboardHIDUsage) {
            self.usage = usage
            super.init()
        }
        required init?(coder: NSCoder) { nil }
        override var keyCode: UIKeyboardHIDUsage { usage }
    }

    /// A key press from a hardware keyboard.
    private final class HardwarePress: UIPress {
        private let hardwareKey: UIKey
        init(_ usage: UIKeyboardHIDUsage = .keyboardErrorRollOver) {
            hardwareKey = HardwareKey(usage)
            super.init()
        }
        override var key: UIKey? { hardwareKey }
    }

    private var window: UIWindow!
    private var relay: RelayTouchUIView!
    private var input: RelayKeyboardInputView!
    private var events: [WireInputEvent] = []
    private var dismissals = 0

    override func setUp() async throws {
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        window = UIWindow(windowScene: scene)
        relay = RelayTouchUIView(frame: window.bounds)
        relay.keyboardSupported = true
        relay.onEvents = { [unowned self] in events += $0 }
        relay.onKeyboardDismiss = { [unowned self] in dismissals += 1 }
        window.addSubview(relay)
        window.makeKeyAndVisible()
        input = try XCTUnwrap(relay.subviews.compactMap { $0 as? RelayKeyboardInputView }.first)
        relay.keyboardVisible = true
        relay.updateKeyboardPresentation()
        XCTAssertTrue(input.isFirstResponder)
        settleRealKeyboard()
    }

    override func tearDown() async throws {
        input.resignFirstResponder()
        window.isHidden = true
        window = nil
    }

    /// The simulator's own keyboard animates in and posts frames too. Let it
    /// finish so only this test's frames reach the relay afterwards.
    private func settleRealKeyboard() {
        var last = Date()
        let observer = NotificationCenter.default.addObserver(
            forName: UIResponder.keyboardDidChangeFrameNotification, object: nil, queue: nil) { _ in last = Date() }
        defer { NotificationCenter.default.removeObserver(observer) }
        let deadline = Date().addingTimeInterval(8)
        while Date() < deadline && Date().timeIntervalSince(last) < 1 {
            RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        }
    }

    /// Posts a keyboard frame of the given height at the bottom of the screen.
    private func keyboardFrame(height: CGFloat) {
        let screen = window.screen.bounds
        let frame = CGRect(x: 0, y: screen.height - height, width: screen.width, height: height)
        NotificationCenter.default.post(name: UIResponder.keyboardDidChangeFrameNotification, object: nil,
            userInfo: [UIResponder.keyboardFrameEndUserInfoKey: frame])
    }

    private var barHeight: CGFloat { input.inputAccessoryView?.bounds.height ?? 0 }

    private func spin(_ seconds: TimeInterval) {
        RunLoop.main.run(until: Date().addingTimeInterval(seconds))
    }

    func testHideKeyboardKeyStillCloses() {
        keyboardFrame(height: 400)
        keyboardFrame(height: barHeight)
        spin(0.3)
        XCTAssertFalse(input.isFirstResponder)
        XCTAssertFalse(relay.keyboardVisible)
        XCTAssertEqual(dismissals, 1)
    }

    func testHardwareKeyCollapseKeepsTheKeyboard() {
        keyboardFrame(height: 400)
        relay.pressesBegan([HardwarePress()], with: nil)
        keyboardFrame(height: barHeight)
        spin(0.3)
        XCTAssertTrue(input.isFirstResponder, "The bar must stay, as with a keyboard attached from the start")
        XCTAssertTrue(relay.keyboardVisible)
        XCTAssertEqual(dismissals, 0)
        // A key straight after the collapse still reaches the PC.
        input.insertText("\n")
        XCTAssertTrue(events.contains { $0.kind == 4 && $0.phase == 0 && $0.keycode == 0x28 }, "Enter was lost")

        // The user brings the software keyboard back, then hides it.
        keyboardFrame(height: 400)
        keyboardFrame(height: barHeight)
        spin(0.3)
        XCTAssertFalse(input.isFirstResponder)
        XCTAssertEqual(dismissals, 1)
    }

    func testAnOldHardwareKeyDoesNotStopHideKeyboard() {
        keyboardFrame(height: 400)
        relay.pressesBegan([HardwarePress()], with: nil)
        spin(1.2)
        keyboardFrame(height: barHeight)
        spin(0.3)
        XCTAssertFalse(input.isFirstResponder)
        XCTAssertEqual(dismissals, 1)
    }

    /// Only a key that types counts, so a modifier held near Hide Keyboard
    /// doesn't stop it.
    func testHideKeyboardJustAfterAModifierStillCloses() {
        keyboardFrame(height: 400)
        let shift = HardwarePress(.keyboardLeftShift)
        XCTAssertEqual(shift.key?.keyCode, .keyboardLeftShift)
        relay.pressesBegan([shift], with: nil)
        keyboardFrame(height: barHeight)
        spin(0.3)
        XCTAssertFalse(input.isFirstResponder)
        XCTAssertFalse(relay.keyboardVisible)
        XCTAssertEqual(dismissals, 1)
    }

    /// A key's collapse follows it within milliseconds. One that didn't
    /// collapse the keyboard doesn't stop Hide Keyboard a moment later.
    func testHideKeyboardSoonAfterAKeyStillCloses() {
        keyboardFrame(height: 400)
        relay.pressesBegan([HardwarePress(.keyboardA)], with: nil)
        spin(0.5)
        keyboardFrame(height: barHeight)
        spin(0.3)
        XCTAssertFalse(input.isFirstResponder)
        XCTAssertFalse(relay.keyboardVisible)
        XCTAssertEqual(dismissals, 1)
    }

    /// The simulators often post the collapse a few milliseconds before the
    /// key press that caused it reaches the app.
    func testAKeyJustAfterTheCollapseKeepsTheKeyboard() {
        keyboardFrame(height: 400)
        keyboardFrame(height: barHeight)
        relay.pressesBegan([HardwarePress(.keyboardA)], with: nil)
        spin(0.3)
        XCTAssertTrue(input.isFirstResponder)
        XCTAssertTrue(relay.keyboardVisible)
        XCTAssertEqual(dismissals, 0)

        keyboardFrame(height: 400)
        keyboardFrame(height: barHeight)
        spin(0.3)
        XCTAssertFalse(input.isFirstResponder)
        XCTAssertEqual(dismissals, 1)
    }
}
