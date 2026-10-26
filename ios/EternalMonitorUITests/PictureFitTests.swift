import XCTest

/// The HUD's picture button switches the live stream between Fit and Fill,
/// and the choice is still there after the app reopens.
final class PictureFitTests: XCTestCase {
    func testHUDButtonSwitchesFitAndFillAndPersists() {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let environment = ProcessInfo.processInfo.environment
        let app = XCUIApplication()
        // VoiceOver keeps the HUD up, so its button stays where it was.
        let arguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "NO",
                         "-playPCaudio", "NO", "-assumeVoiceOver", "YES"]
        // Start from Fit. Only this launch passes it: the relaunch must read
        // what the button saved.
        app.launchArguments = arguments + ["-pictureFit", "fit"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        app.launch()
        defer { app.terminate() }

        let fill = button(app, "Fill the screen")
        XCTAssertTrue(fill.waitForExistence(timeout: 15))
        // The synthetic 16:9 picture sits between bars in portrait.
        XCTAssertTrue(waitForPicture(filled: false), "Fit did not show bars above and below the picture")
        capture("picture-fit", app: app)

        fill.tap()
        let fit = button(app, "Show the whole picture")
        XCTAssertTrue(fit.waitForExistence(timeout: 5))
        XCTAssertTrue(waitForPicture(filled: true), "Fill did not cover the bars")
        capture("picture-fill", app: app)

        app.terminate()
        app.launchArguments = arguments
        app.launch()
        XCTAssertTrue(button(app, "Show the whole picture").waitForExistence(timeout: 15), "Fill was not saved")
        XCTAssertTrue(waitForPicture(filled: true), "Fill was saved but not drawn after reopening")

        button(app, "Show the whole picture").tap()
        XCTAssertTrue(button(app, "Fill the screen").waitForExistence(timeout: 5))
        XCTAssertTrue(waitForPicture(filled: false), "Fit did not come back")
    }

    private func button(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "identifier == 'display.pictureFit' AND label == %@", label)).firstMatch
    }

    /// A quarter of the way down a portrait iPad is a bar in Fit (theme
    /// black) and the moving test pattern in Fill (its green never drops
    /// below a quarter).
    private func waitForPicture(filled: Bool) -> Bool {
        let deadline = Date().addingTimeInterval(10)
        repeat {
            let shot = XCUIScreen.main.screenshot().image
            let green = self.green(shot, at: CGPoint(x: shot.size.width / 2, y: shot.size.height / 4))
            if filled ? green > 48 : green < 24 { return true }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < deadline
        return false
    }

    private func green(_ image: UIImage, at point: CGPoint) -> Int {
        guard let cgImage = image.cgImage else { return -1 }
        let scale = CGFloat(cgImage.width) / image.size.width
        var pixel = [UInt8](repeating: 0, count: 4)
        let drawn = pixel.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: 1, height: 1, bitsPerComponent: 8,
                                          bytesPerRow: 4, space: CGColorSpaceCreateDeviceRGB(),
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            // Shift the image so the sampled pixel lands on the context's
            // only pixel (Core Graphics counts rows from the bottom).
            context.draw(cgImage, in: CGRect(x: -(point.x * scale).rounded(.down),
                                             y: -(CGFloat(cgImage.height) - 1 - (point.y * scale).rounded(.down)),
                                             width: CGFloat(cgImage.width), height: CGFloat(cgImage.height)))
            return true
        }
        return drawn ? Int(pixel[1]) : -1
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
