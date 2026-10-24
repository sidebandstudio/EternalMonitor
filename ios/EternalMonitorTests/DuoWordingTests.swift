import XCTest
@testable import EternalMonitor

/// On iPhone the app says "iPhone" where an iPad says "iPad". On iPad every
/// message is unchanged: the guides quote that form.
final class DuoWordingTests: XCTestCase {
    func testTheNounFollowsTheModel() {
        for model in ["iPhone19,4", "iPhone14,4", "iPhone18,2"] {
            XCTAssertEqual(DeviceNoun.noun(forModel: model), "iPhone", model)
        }
        // Anything else, including iPad apps on a Mac, stays iPad.
        for model in ["iPad16,3", "iPad14,1", "arm64", "MacBookPro18,2", ""] {
            XCTAssertEqual(DeviceNoun.noun(forModel: model), "iPad", model)
        }
        let simulated = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] ?? ""
        XCTAssertEqual(DeviceNoun.current, simulated.hasPrefix("iPhone") ? "iPhone" : "iPad")
    }

    func testFeedbackFromIPhoneDuoNamesTheDevice() {
        let duo = Feedback.Context(appVersion: "0.3.2", build: "7", systemVersion: "27.2", model: "iPhone19,4",
                                   pcVersion: "0.3.2", connection: "Wi-Fi")
        XCTAssertEqual(Feedback.subject(.general, duo), "EternalMonitor 0.3.2 (7) for iPhone: Feedback")
        XCTAssertEqual(Feedback.body(.general, duo).components(separatedBy: "--\n").last, """
            App: EternalMonitor 0.3.2 (7) for iPhone
            iOS: 27.2
            iPhone: iPhone Duo (iPhone19,4)
            PC app: EternalMonitor 0.3.2
            Connection: Wi-Fi
            """)
        // Other iPhones give the identifier alone.
        let mini = Feedback.Context(appVersion: "0.3.2", build: "7", systemVersion: "26.2", model: "iPhone14,4",
                                    pcVersion: nil, connection: nil)
        XCTAssertTrue(Feedback.body(.general, mini).contains("\niOS: 26.2\niPhone: iPhone14,4\n"))
    }

    func testMessagesUseThisDevicesName() {
        let noun = DeviceNoun.current
        XCTAssertEqual(ConnectionProblem.noNetwork.title, "This \(noun) isn't connected to a network")
        XCTAssertEqual(ConnectionProblem.receiverNotReady.detail, "Close EternalMonitor on this \(noun) and open it again.")
        XCTAssertEqual(USBStatusText.describe("USB: waiting for a cable"),
                       "Plug this \(noun) into your PC. Keep Apple Devices open on the PC.")
        XCTAssertEqual(VersionMismatch.iPadOlder.message,
                       "This \(noun) app is older than EternalMonitor on your PC, so they can't connect. "
                       + "Update the \(noun) app: open TestFlight and tap Update.")
        // The PC's own words, which say iPad, are quoted as they are.
        XCTAssertTrue(ConnectionProblem.noReply(usb: false).detail.contains("says Ready for your iPad,"))
        XCTAssertEqual(ConnectionProblem.pcBusy.detail, DeviceNoun.isPhone
            ? "Disconnect the other device first, then try again."
            : "Disconnect the other iPad first, then try again.")
    }
}
