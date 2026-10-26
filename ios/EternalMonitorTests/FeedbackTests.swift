import UIKit
import XCTest
@testable import EternalMonitor

final class FeedbackTests: XCTestCase {
    private let context = Feedback.Context(
        appVersion: "0.3.2", build: "7", systemVersion: "26.2", model: "iPad16,3",
        pcVersion: "0.3.2", connection: "USB"
    )

    private func defaults() -> UserDefaults {
        let name = "feedback-tests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        return defaults
    }

    func testSubjectCarriesTheKindAndVersionForSorting() {
        XCTAssertEqual(Feedback.subject(.general, context), "EternalMonitor 0.3.2 (7) for iPad: Feedback")
        XCTAssertEqual(Feedback.subject(.problem("Your PC didn't answer"), context),
                       "EternalMonitor 0.3.2 (7) for iPad: Problem")
    }

    func testBodyLeavesRoomToWriteThenListsTheDetails() {
        let body = Feedback.body(.general, context)
        XCTAssertTrue(body.hasPrefix("\n\n\n--\n"), body)
        XCTAssertEqual(body.components(separatedBy: "--\n").last, """
            App: EternalMonitor 0.3.2 (7) for iPad
            iPadOS: 26.2
            iPad: iPad16,3
            PC app: EternalMonitor 0.3.2
            Connection: USB
            """)
        let unknown = Feedback.Context(appVersion: "0.3.2", build: "7", systemVersion: "18.6",
                                       model: "iPad14,1", pcVersion: nil, connection: nil)
        let text = Feedback.body(.general, unknown)
        XCTAssertTrue(text.contains("PC app: unknown"), text)
        XCTAssertTrue(text.contains("Connection: not connected"), text)
        XCTAssertFalse(text.contains("Problem:"), text)
    }

    func testAProblemCardNamesItsProblem() {
        let body = Feedback.body(.problem(ConnectionProblem.noReply(usb: false).title), context)
        XCTAssertTrue(body.contains("--\nProblem: Your PC didn't answer\nApp:"), body)
    }

    func testReportThisProblemOnlyShowsForFailures() {
        let informational: [ConnectionProblem] = [
            .reconnecting(attempt: 1, of: 3), .pcUpdating(.reconnecting), .pcUpdating(.manual),
            .lost(.cableUnplugged), .lost(.usbTurnedOff), .pcBusy, .pairingCodeAsAddress,
        ]
        for problem in informational {
            XCTAssertFalse(problem.canReport, problem.title)
        }
        let failures: [ConnectionProblem] = [
            .noReply(usb: false), .noReply(usb: true), .noPicture, .receiverNotReady, .badAddress("pc"),
            .routerAddress, .noNetwork, .localNetworkBlocked, .couldNotConnect, .usbFailed,
            .lost(.pcStoppedResponding), .reconnectFailed, .pcEndedSession, .pcUpdating(.notBack),
        ]
        for problem in failures {
            XCTAssertTrue(problem.canReport, problem.title)
        }
    }

    func testMailSaysTheDiagnosticsAreAttachedAndHowToRemoveThem() {
        let attached = Feedback.body(.problem("Your PC didn't answer"), context, attachesDiagnostics: true)
        XCTAssertTrue(attached.hasPrefix(Feedback.attachmentNotice + "\n\n\n\n--\n"), attached)
        XCTAssertTrue(Feedback.attachmentNotice.contains("PC's and iPad's names and local network addresses"))
        XCTAssertTrue(Feedback.attachmentNotice.contains("Remove it before sending"))
        // Without an attachment (the copy sheet) it says nothing about one.
        XCTAssertFalse(Feedback.body(.general, context).contains("diagnostics"))
        XCTAssertFalse(Feedback.fullText(.general, context).contains("diagnostics"))
    }

    @MainActor
    func testTheDetailsNeverNameTheIPadOrThePC() {
        PCVersions.record("0.3.2", name: "Jane's Gaming PC", address: "192.168.1.20")
        let current = Feedback.Context.current(
            pcVersion: PCVersions.version(address: "192.168.1.20", name: nil), connection: "Wi-Fi")
        XCTAssertEqual(current.pcVersion, "0.3.2")
        let text = Feedback.fullText(.general, current)
        // A simulator's name is just "iPad", which the text says anyway.
        let ipadName = UIDevice.current.name == "iPad" ? "Jane's iPad" : UIDevice.current.name
        for secret in [ipadName, "Jane", "192.168.1.20", ProcessInfo.processInfo.hostName] {
            XCTAssertFalse(text.contains(secret), "\(secret) in \(text)")
        }
        XCTAssertTrue(text.hasPrefix("To: hello@sideband.studio\nSubject: EternalMonitor "), text)
        XCTAssertFalse(current.model.isEmpty)
    }

    func testThePCVersionIsReadFromItsBonjourRecord() {
        let txt = NetService.data(fromTXTRecord: ["version": Data("0.3.2".utf8), "proto": Data("2".utf8)])
        XCTAssertEqual(PCVersions.version(fromTXT: txt), "0.3.2")
        XCTAssertNil(PCVersions.version(fromTXT: nil))
        XCTAssertNil(PCVersions.version(fromTXT: NetService.data(fromTXTRecord: ["proto": Data("2".utf8)])))
    }

    func testWithoutMailTheCopySheetShowsInstead() async {
        XCTAssertEqual(Feedback.route(canSendMail: true), .mail)
        XCTAssertEqual(Feedback.route(canSendMail: false), .copy)
        let copy = await FeedbackRequest.make(.general, context: context, canSendMail: false)
        XCTAssertEqual(copy.route, .copy)
        XCTAssertNil(copy.attachment)
        let mail = await FeedbackRequest.make(.problem("Lost the connection to your PC"), context: context, canSendMail: true)
        XCTAssertEqual(mail.route, .mail)
        let report = String(decoding: mail.attachment ?? Data(), as: UTF8.self)
        XCTAssertTrue(report.hasPrefix("EternalMonitor iPad support report"), report)
        // Report this problem and the one-time card both open Mail with the
        // report attached, and both say so at the top of the email.
        XCTAssertTrue(mail.mailBody.hasPrefix(Feedback.attachmentNotice), mail.mailBody)
        XCTAssertTrue(mail.mailBody.contains("Problem: Lost the connection to your PC"))
        let ask = await FeedbackRequest.make(.general, context: context, canSendMail: true)
        XCTAssertTrue(ask.mailBody.hasPrefix(Feedback.attachmentNotice), ask.mailBody)
        XCTAssertFalse(copy.mailBody.contains("diagnostics"), copy.mailBody)
    }

    func testTheAskComesAfterThreeSessionsOfFiveMinutes() {
        let store = defaults()
        var ask = FeedbackAsk(defaults: store)
        ask.sessionEnded(after: 299, defaults: store)
        ask.sessionEnded(after: 60, defaults: store)
        XCTAssertEqual(ask.longSessions, 0)
        ask.sessionEnded(after: 300, defaults: store)
        ask.sessionEnded(after: 3600, defaults: store)
        XCTAssertFalse(ask.due(streaming: false))
        ask.sessionEnded(after: 301, defaults: store)
        XCTAssertEqual(ask.longSessions, 3)
        // Never over the picture: only once the stream has ended.
        XCTAssertFalse(ask.due(streaming: true))
        XCTAssertTrue(ask.due(streaming: false))
        ask.sessionEnded(after: 900, defaults: store)
        XCTAssertEqual(ask.longSessions, 3)
        // The count survives a relaunch.
        XCTAssertEqual(FeedbackAsk(defaults: store).longSessions, 3)
    }

    func testOnceAnsweredTheAskNeverReturns() {
        let store = defaults()
        store.set(3, forKey: FeedbackAsk.sessionsKey)
        var ask = FeedbackAsk(defaults: store)
        XCTAssertTrue(ask.due(streaming: false))
        ask.finish(defaults: store)
        XCTAssertFalse(ask.due(streaming: false))
        var relaunched = FeedbackAsk(defaults: store)
        XCTAssertTrue(relaunched.done)
        XCTAssertFalse(relaunched.due(streaming: false))
        relaunched.sessionEnded(after: 600, defaults: store)
        XCTAssertFalse(relaunched.due(streaming: false))
    }
}
