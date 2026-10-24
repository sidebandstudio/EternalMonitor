import XCTest
@testable import EternalMonitor

/// iPhone Duo's two screens, open (inner) and folded (outer). Apple publishes
/// pixels only: inner 1878 × 2670, outer 1398 × 2034. The point sizes assume
/// a scale of 3, like other recent iPhones, which divides both exactly:
/// inner 626 × 890 pt, outer 466 × 678 pt. The mapping is plain proportion,
/// so a different scale moves no touch; check `display` in Diagnostics on a
/// real Duo for the actual numbers.
@MainActor
final class DuoGeometryTests: XCTestCase {
    static let scale: CGFloat = 3
    static let inner = CGSize(width: 1878, height: 2670)
    static let outer = CGSize(width: 1398, height: 2034)

    /// Both screens in portrait and landscape, in pixels.
    static var screens: [(name: String, pixels: CGSize)] {
        [("inner", inner), ("outer", outer)].flatMap { name, pixels in
            [(name + " portrait", pixels), (name + " landscape", CGSize(width: pixels.height, height: pixels.width))]
        }
    }

    private static func points(_ pixels: CGSize) -> CGSize {
        CGSize(width: pixels.width / scale, height: pixels.height / scale)
    }

    /// Where the renderer draws the picture's normalized point `u`, in screen
    /// pixels. The same shader math as PictureFitTests.
    private func rendered(_ u: CGPoint, scale: SIMD2<Float>, drawable: CGSize) -> CGPoint {
        let ndcX = (2 * u.x - 1) * CGFloat(scale.x)
        let ndcY = (1 - 2 * u.y) * CGFloat(scale.y)
        return CGPoint(x: (ndcX + 1) / 2 * drawable.width, y: (1 - ndcY) / 2 * drawable.height)
    }

    func testThePointSizesAreWholeAtScaleThree() {
        XCTAssertEqual(Self.points(Self.inner), CGSize(width: 626, height: 890))
        XCTAssertEqual(Self.points(Self.outer), CGSize(width: 466, height: 678))
    }

    /// A touch anywhere on the picture reaches the PC pixel drawn under it,
    /// within half a screen pixel, in Fit and Fill.
    func testTouchesRoundTripOnBothScreensInBothModes() {
        for fit in [PictureFit.fit, .fill] {
            for screen in Self.screens {
                let points = Self.points(screen.pixels)
                for video in PictureFitTests.streams {
                    let label = "\(fit) \(screen.name) \(video)"
                    let mapper = ContentRectMapper(viewSize: points, videoSize: video, fit: fit)
                    let scale = MetalRenderer.scale(fit, video: video, drawable: screen.pixels)
                    let visible = mapper.visibleRect
                    XCTAssertGreaterThan(visible.width * visible.height, 0, label)
                    for i in 0...16 {
                        for j in 0...16 {
                            let point = CGPoint(x: visible.minX + visible.width * CGFloat(i) / 16,
                                                y: visible.minY + visible.height * CGFloat(j) / 16)
                            guard let wire = mapper.normalize(point) else {
                                XCTFail("\(label): \(point) on the picture was rejected")
                                continue
                            }
                            let u = CGPoint(x: CGFloat(wire.x) / 65535, y: CGFloat(wire.y) / 65535)
                            let drawn = rendered(u, scale: scale, drawable: screen.pixels)
                            XCTAssertEqual(drawn.x, point.x * Self.scale, accuracy: 0.5, label)
                            XCTAssertEqual(drawn.y, point.y * Self.scale, accuracy: 0.5, label)
                        }
                    }
                }
            }
        }
    }

    /// Fit letterboxes a 16:9 PC on every Duo shape, and bars never click.
    /// Fill covers the whole screen without stretching.
    func testSixteenByNineOnTheInnerScreen() {
        let video = CGSize(width: 1920, height: 1080)
        let portrait = CGSize(width: 626, height: 890)
        let fit = ContentRectMapper(viewSize: portrait, videoSize: video, fit: .fit)
        XCTAssertEqual(fit.contentRect.width, 626, accuracy: 1e-9)
        XCTAssertEqual(fit.contentRect.height, 626 * 9 / 16, accuracy: 1e-9)
        XCTAssertNil(fit.normalize(CGPoint(x: 313, y: 20)))
        let fill = ContentRectMapper(viewSize: portrait, videoSize: video, fit: .fill)
        XCTAssertEqual(fill.visibleRect, CGRect(origin: .zero, size: portrait))
        XCTAssertEqual(fill.contentRect.height, 890, accuracy: 1e-9)
        XCTAssertEqual(fill.contentRect.width, 890 * 16 / 9, accuracy: 1e-9)
        XCTAssertNotNil(fill.normalize(CGPoint(x: 313, y: 20)))
        let landscape = ContentRectMapper(viewSize: CGSize(width: 890, height: 626), videoSize: video, fit: .fit)
        XCTAssertEqual(landscape.contentRect.width, 890, accuracy: 1e-9)
        XCTAssertEqual(landscape.contentRect.minY, (626 - 890 * 9 / 16) / 2, accuracy: 1e-9)
    }

    private func relay(_ size: CGSize, fit: PictureFit) -> RelayTouchUIView {
        let view = RelayTouchUIView(frame: CGRect(origin: .zero, size: size))
        view.videoSize = CGSize(width: 1920, height: 1080)
        view.pictureFit = fit
        view.layoutIfNeeded()
        return view
    }

    /// Unfolding mid-drag moves the picture under the finger. The drag ends
    /// once, where it was last sent, and nothing more goes out for it. Folding
    /// back, and the same in landscape, behave the same way.
    func testADragHeldAcrossAFoldOrUnfoldEndsWhereItWas() {
        let outer = Self.points(Self.outer), inner = Self.points(Self.inner)
        let changes: [(String, CGSize, CGSize)] = [
            ("unfold", outer, inner), ("fold", inner, outer),
            ("unfold landscape", CGSize(width: outer.height, height: outer.width),
             CGSize(width: inner.height, height: inner.width)),
        ]
        for fit in [PictureFit.fit, .fill] {
            for (name, from, to) in changes {
                let label = "\(fit) \(name)"
                let view = relay(from, fit: fit)
                var events: [WireInputEvent] = []
                view.onEvents = { events += $0 }
                let finger = DuoTouch(.direct)
                finger.point = CGPoint(x: from.width / 2, y: from.height / 2)
                view.touchesBegan([finger], with: nil)
                finger.point.x += 60
                view.touchesMoved([finger], with: nil)
                XCTAssertEqual(events.map(\.phase), [0, 1], label)
                let lastSent = events.last.map { ($0.xNorm, $0.yNorm) }

                view.frame.size = to
                view.layoutIfNeeded()
                finger.point = CGPoint(x: to.width / 3, y: to.height / 3)
                view.touchesMoved([finger], with: nil)
                view.touchesEnded([finger], with: nil)
                XCTAssertEqual(events.map(\.phase), [0, 1, 3], label)
                XCTAssertEqual(events.last?.xNorm, lastSent?.0, label)
                XCTAssertEqual(events.last?.yNorm, lastSent?.1, label)

                // The next tap maps through the new screen's geometry.
                let tap = DuoTouch(.direct)
                tap.point = CGPoint(x: to.width / 2, y: to.height / 2)
                view.touchesBegan([tap], with: nil)
                view.touchesEnded([tap], with: nil)
                let expected = ContentRectMapper(viewSize: to, videoSize: view.videoSize, fit: fit).normalize(tap.point)
                XCTAssertEqual(events.dropFirst(3).map(\.phase), [0, 2], label)
                XCTAssertEqual(events.last?.xNorm, expected?.x, label)
                XCTAssertEqual(events.last?.yNorm, expected?.y, label)
            }
        }
    }

    /// A Pencil stroke across an unfold is cancelled where it was, never
    /// dragged to a point mapped the new way.
    func testAPencilStrokeAcrossAnUnfoldIsCancelledInPlace() {
        let view = relay(Self.points(Self.outer), fit: .fit)
        view.nativePenSupported = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = DuoTouch(.pencil)
        tip.point = CGPoint(x: 200, y: 339)
        view.touchesBegan([tip], with: nil)
        view.frame.size = Self.points(Self.inner)
        view.layoutIfNeeded()
        tip.sampleTime = 2
        tip.point = CGPoint(x: 300, y: 445)
        view.touchesMoved([tip], with: nil)
        view.touchesEnded([tip], with: nil)
        XCTAssertEqual(events.map(\.phase), [0, 3])
        XCTAssertEqual(Set(events.map(\.xNorm)).count, 1)
        XCTAssertEqual(Set(events.map(\.yNorm)).count, 1)
    }
}

private final class DuoTouch: UITouch {
    let touchType: UITouch.TouchType
    var sampleTime: TimeInterval = 1
    var point = CGPoint.zero
    init(_ type: UITouch.TouchType) { touchType = type; super.init() }
    override var type: UITouch.TouchType { touchType }
    override var timestamp: TimeInterval { sampleTime }
    override var force: CGFloat { 2 }
    override var maximumPossibleForce: CGFloat { 4 }
    override var altitudeAngle: CGFloat { .pi / 4 }
    override func azimuthAngle(in view: UIView?) -> CGFloat { 0 }
    override func location(in view: UIView?) -> CGPoint { point }
    override func preciseLocation(in view: UIView?) -> CGPoint { point }
}
