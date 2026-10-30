import SwiftUI
import XCTest
@testable import EternalMonitor

final class LocalizationTests: XCTestCase {
    override func tearDown() {
        LanguageStore.shared.choice = nil
        super.tearDown()
    }

    private func table(_ code: String) throws -> [String: String] {
        let url = try XCTUnwrap(Bundle.main.url(forResource: "Localizable", withExtension: "strings",
                                                subdirectory: nil, localization: code), code)
        return try XCTUnwrap(NSDictionary(contentsOf: url) as? [String: String], code)
    }

    private func specifiers(_ text: String) -> [String] {
        let pattern = try! NSRegularExpression(pattern: #"%(?:\d+\$)?(lld|@|d)"#)
        return pattern.matches(in: text, range: NSRange(text.startIndex..., in: text))
            .map { String(text[Range($0.range(at: 1), in: text)!]) }.sorted()
    }

    /// Every language has exactly English's strings, none empty, each with
    /// the same placeholders.
    func testEveryLanguageHasEveryString() throws {
        let english = try table("en")
        XCTAssertGreaterThan(english.count, 100)
        for language in AppLanguage.allCases where language != .en {
            let strings = try table(language.rawValue)
            XCTAssertEqual(Set(strings.keys), Set(english.keys), language.rawValue)
            for (key, value) in strings {
                XCTAssertFalse(value.trimmingCharacters(in: .whitespaces).isEmpty, "\(language.rawValue): \(key)")
                XCTAssertEqual(specifiers(value), specifiers(key), "\(language.rawValue): \(key)")
            }
        }
    }

    /// The device's language when the app has it, otherwise English.
    func testSystemLanguageMatching() {
        XCTAssertEqual(AppLanguage.matching(["de-DE", "en-US"]), .de)
        XCTAssertEqual(AppLanguage.matching(["it-IT", "fr-FR"]), .fr)
        XCTAssertEqual(AppLanguage.matching(["it-IT"]), .en)
        XCTAssertEqual(AppLanguage.matching([]), .en)
        XCTAssertEqual(AppLanguage.matching(["zh-Hans-CN"]), .zhHans)
        XCTAssertEqual(AppLanguage.matching(["zh-CN"]), .zhHans)
        XCTAssertEqual(AppLanguage.matching(["zh-Hant-TW", "ja-JP"]), .ja)
        XCTAssertEqual(AppLanguage.matching(["pt-PT"]), .ptBR)
        XCTAssertEqual(AppLanguage.matching(["pt-BR"]), .ptBR)
        XCTAssertEqual(AppLanguage.matching(["es-419"]), .es)
        XCTAssertEqual(AppLanguage.matching(["ar-SA"]), .ar)
        XCTAssertEqual(AppLanguage.matching(["hi-IN"]), .hi)
        XCTAssertEqual(AppLanguage.matching(["en-GB", "de-DE"]), .en)
    }

    /// A choice changes every lookup at once, strings built in code
    /// included, and Arabic lays the app out right to left.
    func testChoosingALanguageSwitchesLookupsAtOnce() {
        let store = LanguageStore.shared
        store.choice = .de
        XCTAssertEqual(L("Settings"), "Einstellungen")
        XCTAssertEqual(L("Reaching \("192.168.1.20")"), "Verbinde mit 192.168.1.20")
        XCTAssertEqual(store.layoutDirection, .leftToRight)
        store.choice = .ja
        XCTAssertEqual(L("Step \(1) of \(3)"), "ステップ 1/3")
        store.choice = .ko
        XCTAssertEqual(L("Step \(1) of \(3)"), "3단계 중 1단계")
        store.choice = .ar
        XCTAssertEqual(L("Settings"), "الإعدادات")
        XCTAssertEqual(store.layoutDirection, .rightToLeft)
        XCTAssertEqual(UserDefaults.standard.string(forKey: LanguageStore.defaultsKey), "ar")
        // Kept English: not in the catalog, so every language shows the key.
        XCTAssertEqual(L("Connection details"), "Connection details")
        store.choice = .en
        XCTAssertEqual(L("Settings"), "Settings")
        XCTAssertEqual(L("Found \(2) PCs"), "Found 2 PCs")
        store.choice = nil
        XCTAssertEqual(store.current, AppLanguage.matching(Locale.preferredLanguages))
        XCTAssertEqual(UserDefaults.standard.string(forKey: LanguageStore.defaultsKey), "")
    }

    /// A saved choice (or a launch argument) is read, not saved again.
    func testReadingTheChoiceSavesNothing() throws {
        let suite = "LocalizationTests.\(UUID())"
        let saved = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { saved.removePersistentDomain(forName: suite) }
        saved.set("ko", forKey: LanguageStore.defaultsKey)
        let before = UserDefaults.standard.string(forKey: LanguageStore.defaultsKey)
        let store = LanguageStore(defaults: saved)
        XCTAssertEqual(store.choice, .ko)
        XCTAssertEqual(store.current, .ko)
        XCTAssertEqual(UserDefaults.standard.string(forKey: LanguageStore.defaultsKey), before)
    }

    /// Only a fresh install sees setup. What the app saves by itself at
    /// launch doesn't count; anything a person's use saved does, a single
    /// changed setting included.
    func testSetupIsForFreshInstallsOnly() {
        let launchWrites: [String: Any] = [
            "diagnosticsInstallationID": "A1B2", ForegroundMarker.key: ["version": "0.3.3"],
            "remoteConfigFetchedAt": Date(), "updateCheckedAt": Date(), "updateAvailableBuild": 40,
            LanguageStore.defaultsKey: "",
        ]
        XCTAssertTrue(FirstRun.isFreshInstall(saved: [:], hasPairings: false))
        XCTAssertTrue(FirstRun.isFreshInstall(saved: launchWrites, hasPairings: false))
        XCTAssertFalse(FirstRun.isFreshInstall(saved: launchWrites, hasPairings: true))
        let upgrades: [String: Any] = [
            "playPCaudio": false, "targetFPS": 120, "pictureFit": "fill", "allowUSB": false,
            "didSeeOnboarding": false, "lastHost": "192.168.1.20", "recentConnections": Data(),
            "deviceId": "42", "checkForUpdates": false, DiagnosticsUpload.enabledKey: true,
            FeedbackAsk.doneKey: true, "preferUSB": false, "promotionEnabled": false,
        ]
        for (key, value) in upgrades {
            XCTAssertFalse(FirstRun.isFreshInstall(saved: launchWrites.merging([key: value]) { $1 }, hasPairings: false), key)
        }
    }

    /// The cable card's usual statuses follow the language; its problem
    /// stays English.
    func testUSBStatusesFollowTheLanguage() {
        LanguageStore.shared.choice = .de
        XCTAssertEqual(USBStatusText.describe("USB: connecting"), "Verbinden per Kabel…")
        XCTAssertEqual(USBStatusText.describe("USB: connected to Studio PC"), "Per Kabel mit Studio PC verbunden.")
        XCTAssertEqual(USBStatusText.describe("USB: off"), "Aus. Schalte es ein, um per Kabel zu verbinden.")
        XCTAssertEqual(USBStatusText.describe("USB: unavailable"), "The cable can't be used right now. Wi-Fi still works.")
        LanguageStore.shared.choice = .ja
        XCTAssertEqual(USBStatusText.describe("USB: connected to Studio PC"), "ケーブルで Studio PC に接続済み。")
    }
}

/// The streamed picture and the touch layer stay left to right under
/// Arabic, so a touch maps to the same PC point in every language.
@MainActor
final class RightToLeftMappingTests: XCTestCase {
    private struct Probe: UIViewRepresentable {
        let view: RelayTouchUIView
        func makeUIView(context: Context) -> RelayTouchUIView { view }
        func updateUIView(_ view: RelayTouchUIView, context: Context) {}
    }

    private func host(_ root: some View) -> UIWindow {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 1024, height: 768))
        window.rootViewController = UIHostingController(rootView: root)
        window.makeKeyAndVisible()
        window.layoutIfNeeded()
        return window
    }

    /// Where a touch at `point` in the window lands on the PC (0...1), the
    /// way RelayTouchUIView maps it.
    private func pcPoint(_ point: CGPoint, on relay: RelayTouchUIView) -> CGPoint? {
        ContentRectMapper(viewSize: relay.bounds.size, videoSize: CGSize(width: 1920, height: 1080), fit: .fit)
            .normalize(relay.convert(point, from: nil))
            .map { CGPoint(x: Double($0.x) / 65535, y: Double($0.y) / 65535) }
    }

    func testStreamSurfaceKeepsLeftToRightUnderArabic() throws {
        let relay = RelayTouchUIView()
        let window = host(
            ZStack { Probe(view: relay).ignoresSafeArea() }
                .streamSurfaceDirection()
                .environment(\.layoutDirection, .rightToLeft)
                .environment(\.locale, Locale(identifier: "ar"))
        )
        defer { window.isHidden = true }
        XCTAssertGreaterThan(relay.bounds.width, 0)
        XCTAssertEqual(relay.effectiveUserInterfaceLayoutDirection, .leftToRight)
        let y = window.bounds.midY
        let left = try XCTUnwrap(pcPoint(CGPoint(x: window.bounds.width * 0.1, y: y), on: relay))
        let right = try XCTUnwrap(pcPoint(CGPoint(x: window.bounds.width * 0.9, y: y), on: relay))
        XCTAssertEqual(left.x, 0.1, accuracy: 0.01, "a tap on the left must click the left of the PC")
        XCTAssertEqual(right.x, 0.9, accuracy: 0.01)
    }
}

/// A language switch relabels a keyboard bar that is already built:
/// TouchRelayView calls `updateLanguage()` when the language changes.
@MainActor
final class KeyboardBarLanguageTests: XCTestCase {
    override func tearDown() {
        LanguageStore.shared.choice = nil
        super.tearDown()
    }

    private func button(_ identifier: String, in view: UIView) -> UIButton? {
        if let button = view as? UIButton, button.accessibilityIdentifier == identifier { return button }
        return view.subviews.lazy.compactMap { self.button(identifier, in: $0) }.first
    }

    func testSwitchRelabelsTheBuiltBar() throws {
        LanguageStore.shared.choice = .en
        let relay = RelayTouchUIView()
        relay.keyboardSupported = true
        // Without a sink `onEvents?(…)` skips the toggle it would send.
        relay.onEvents = { _ in }
        relay.updateLanguage()
        let input = try XCTUnwrap(relay.subviews.lazy.compactMap { $0 as? RelayKeyboardInputView }.first)
        let bar = try XCTUnwrap(input.inputAccessoryView)
        let done = try XCTUnwrap(button("keyboard.done", in: bar))
        let control = try XCTUnwrap(button("keyboard.key.224", in: bar))
        let alt = try XCTUnwrap(button("keyboard.key.226", in: bar))
        relay.toggleRemoteModifier(0xe0)
        XCTAssertEqual(done.configuration?.title, "Done")
        XCTAssertEqual(control.accessibilityValue, "On")
        XCTAssertEqual(alt.accessibilityValue, "Off")

        LanguageStore.shared.choice = .ar
        relay.updateLanguage()
        XCTAssertEqual(done.configuration?.title, "تم")
        XCTAssertEqual(control.accessibilityValue, "قيد التشغيل")
        XCTAssertEqual(alt.accessibilityValue, "متوقف")

        LanguageStore.shared.choice = .en
        relay.updateLanguage()
        XCTAssertEqual(done.configuration?.title, "Done")
        XCTAssertEqual(control.accessibilityValue, "On")
        XCTAssertEqual(alt.accessibilityValue, "Off")
    }
}
