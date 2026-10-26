import XCTest
@testable import EternalMonitor

/// Fill must scale evenly, cover the screen, and map every touch to the PC
/// pixel drawn under it. The renderer and the touch mapper compute the
/// picture's rectangle separately, so these tests hold them against each other.
final class PictureFitTests: XCTestCase {
    /// iPad screens in pixels and their point scale.
    static let iPads: [(name: String, pixels: CGSize, scale: CGFloat)] = [
        ("iPad Air 5th gen / iPad Air 11-inch", CGSize(width: 2360, height: 1640), 2),
        ("iPad Pro 12.9-inch", CGSize(width: 2732, height: 2048), 2),
        ("iPad Pro 11-inch", CGSize(width: 2388, height: 1668), 2),
        ("iPad mini 6th gen", CGSize(width: 2266, height: 1488), 2),
        ("iPad 9th gen", CGSize(width: 2160, height: 1620), 2),
        ("iPad Pro 13-inch (M4)", CGSize(width: 2752, height: 2064), 2),
    ]
    /// 16:9, 21:9, 4:3 and 3:2 PC pictures.
    static let streams: [CGSize] = [
        CGSize(width: 1920, height: 1080), CGSize(width: 2560, height: 1080),
        CGSize(width: 3440, height: 1440), CGSize(width: 1600, height: 1200),
        CGSize(width: 2160, height: 1440), CGSize(width: 2360, height: 1640),
    ]

    /// Every iPad in landscape and portrait.
    static var screens: [(name: String, pixels: CGSize, scale: CGFloat)] {
        iPads.flatMap { iPad in
            [(iPad.name + " landscape", iPad.pixels, iPad.scale),
             (iPad.name + " portrait", CGSize(width: iPad.pixels.height, height: iPad.pixels.width), iPad.scale)]
        }
    }

    /// Where the renderer draws the PC picture's normalized point `u`, in
    /// screen pixels. Follows the shader: NDC x = (2u − 1)·scale.x, Metal's
    /// viewport puts NDC y = +1 at the top row and texture v = 0 there.
    private func rendered(_ u: CGPoint, scale: SIMD2<Float>, drawable: CGSize) -> CGPoint {
        let ndcX = (2 * u.x - 1) * CGFloat(scale.x)
        let ndcY = (1 - 2 * u.y) * CGFloat(scale.y)
        return CGPoint(x: (ndcX + 1) / 2 * drawable.width, y: (1 - ndcY) / 2 * drawable.height)
    }

    func testFillScaleCoversTheScreenEvenly() {
        for screen in Self.screens {
            for video in Self.streams {
                let label = "\(screen.name) \(video)"
                let fill = MetalRenderer.aspectFillScale(video: video, drawable: screen.pixels)
                // One axis exactly fills, the other overflows: no bars.
                XCTAssertEqual(min(fill.x, fill.y), 1, label)
                XCTAssertGreaterThanOrEqual(max(fill.x, fill.y), 1, label)
                // Never stretched: the drawn shape keeps the PC's aspect.
                let drawnAspect = CGFloat(fill.x) * screen.pixels.width / (CGFloat(fill.y) * screen.pixels.height)
                XCTAssertEqual(drawnAspect, video.width / video.height, accuracy: 1e-5, label)
                // Fill is Fit scaled up evenly.
                let fit = MetalRenderer.aspectFitScale(video: video, drawable: screen.pixels)
                XCTAssertEqual(fill.x / fit.x, fill.y / fit.y, accuracy: 1e-5, label)
                XCTAssertEqual(MetalRenderer.scale(.fill, video: video, drawable: screen.pixels), fill, label)
                XCTAssertEqual(MetalRenderer.scale(.fit, video: video, drawable: screen.pixels), fit, label)
            }
        }
    }

    func testIPadAirFillValues() {
        // The customer's case: a 16:9 PC on an iPad Air (2360 × 1640).
        let landscape = MetalRenderer.aspectFillScale(
            video: CGSize(width: 1920, height: 1080), drawable: CGSize(width: 2360, height: 1640))
        XCTAssertEqual(landscape.y, 1)
        XCTAssertEqual(landscape.x, Float((1920.0 / 1080.0) / (2360.0 / 1640.0)), accuracy: 1e-6)
        let mapper = ContentRectMapper(viewSize: CGSize(width: 1180, height: 820),
                                       videoSize: CGSize(width: 1920, height: 1080), fit: .fill)
        // 820 pt tall → 1457.8 pt wide, 138.9 pt trimmed off each side.
        XCTAssertEqual(mapper.contentRect.height, 820, accuracy: 1e-9)
        XCTAssertEqual(mapper.contentRect.width, 820 * 16 / 9, accuracy: 1e-9)
        XCTAssertEqual(mapper.contentRect.minX, -(820 * 16 / 9 - 1180) / 2, accuracy: 1e-9)
        XCTAssertEqual(mapper.visibleRect, CGRect(x: 0, y: 0, width: 1180, height: 820))

        let portrait = MetalRenderer.aspectFillScale(
            video: CGSize(width: 1920, height: 1080), drawable: CGSize(width: 1640, height: 2360))
        XCTAssertEqual(portrait.y, 1)
        XCTAssertGreaterThan(portrait.x, 2.4)
    }

    func testRendererAndMapperAgreeOnThePictureRect() {
        for fit in [PictureFit.fit, .fill] {
            for screen in Self.screens {
                let points = CGSize(width: screen.pixels.width / screen.scale, height: screen.pixels.height / screen.scale)
                for video in Self.streams {
                    let label = "\(fit) \(screen.name) \(video)"
                    let scale = MetalRenderer.scale(fit, video: video, drawable: screen.pixels)
                    let topLeft = rendered(.zero, scale: scale, drawable: screen.pixels)
                    let bottomRight = rendered(CGPoint(x: 1, y: 1), scale: scale, drawable: screen.pixels)
                    let rect = ContentRectMapper(viewSize: points, videoSize: video, fit: fit).contentRect
                    XCTAssertEqual(rect.minX * screen.scale, topLeft.x, accuracy: 0.01, label)
                    XCTAssertEqual(rect.minY * screen.scale, topLeft.y, accuracy: 0.01, label)
                    XCTAssertEqual(rect.maxX * screen.scale, bottomRight.x, accuracy: 0.01, label)
                    XCTAssertEqual(rect.maxY * screen.scale, bottomRight.y, accuracy: 0.01, label)
                }
            }
        }
    }

    /// A touch at screen position P must reach the PC coordinate the
    /// renderer draws at P, within half a pixel, everywhere a touch counts:
    /// a grid across the screen plus every corner and edge.
    func testTouchesRoundTripThroughTheRendererInBothModes() {
        for fit in [PictureFit.fit, .fill] {
            for screen in Self.screens {
                let points = CGSize(width: screen.pixels.width / screen.scale, height: screen.pixels.height / screen.scale)
                for video in Self.streams {
                    let label = "\(fit) \(screen.name) \(video)"
                    let mapper = ContentRectMapper(viewSize: points, videoSize: video, fit: fit)
                    let scale = MetalRenderer.scale(fit, video: video, drawable: screen.pixels)
                    let visible = mapper.visibleRect
                    var samples: [CGPoint] = []
                    for i in 0...24 {
                        for j in 0...24 {
                            samples.append(CGPoint(x: visible.minX + visible.width * CGFloat(i) / 24,
                                                   y: visible.minY + visible.height * CGFloat(j) / 24))
                        }
                    }
                    var mapped = 0
                    for point in samples {
                        guard let wire = mapper.normalize(point) else {
                            XCTFail("\(label): \(point) on the picture was rejected")
                            continue
                        }
                        mapped += 1
                        let u = CGPoint(x: CGFloat(wire.x) / 65535, y: CGFloat(wire.y) / 65535)
                        let drawn = rendered(u, scale: scale, drawable: screen.pixels)
                        XCTAssertEqual(drawn.x, point.x * screen.scale, accuracy: 0.5, label)
                        XCTAssertEqual(drawn.y, point.y * screen.scale, accuracy: 0.5, label)
                    }
                    XCTAssertEqual(mapped, samples.count, label)
                }
            }
        }
    }

    /// Touches on the screen's edges, and just past them, never reach the
    /// part of the PC picture Fill trimmed away.
    func testFillNeverReachesTheTrimmedEdges() {
        for screen in Self.screens {
            let points = CGSize(width: screen.pixels.width / screen.scale, height: screen.pixels.height / screen.scale)
            for video in Self.streams {
                let label = "\(screen.name) \(video)"
                let mapper = ContentRectMapper(viewSize: points, videoSize: video, fit: .fill)
                let scale = MetalRenderer.aspectFillScale(video: video, drawable: screen.pixels)
                let w = points.width, h = points.height
                let edges = [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: 0, y: h), CGPoint(x: w, y: h),
                             CGPoint(x: w / 2, y: 0), CGPoint(x: w / 2, y: h), CGPoint(x: 0, y: h / 2), CGPoint(x: w, y: h / 2),
                             CGPoint(x: -0.9, y: -0.9), CGPoint(x: w + 0.9, y: h + 0.9),
                             CGPoint(x: -0.9, y: h / 2), CGPoint(x: w + 0.9, y: h / 2)]
                for point in edges {
                    guard let wire = mapper.normalize(point) else {
                        XCTFail("\(label): edge \(point) was rejected")
                        continue
                    }
                    let drawn = rendered(CGPoint(x: CGFloat(wire.x) / 65535, y: CGFloat(wire.y) / 65535),
                                         scale: scale, drawable: screen.pixels)
                    // Lands on screen: on the visible picture, not the trimmed part.
                    XCTAssertTrue((-0.5...(screen.pixels.width + 0.5)).contains(drawn.x), "\(label) \(point) → \(drawn)")
                    XCTAssertTrue((-0.5...(screen.pixels.height + 0.5)).contains(drawn.y), "\(label) \(point) → \(drawn)")
                }
                XCTAssertNil(mapper.normalize(CGPoint(x: -5, y: h / 2)), label)
                XCTAssertNil(mapper.normalize(CGPoint(x: w / 2, y: h + 5)), label)
            }
        }
    }

    func testFitIsTheDefaultAndUnchanged() {
        let view = CGSize(width: 1180, height: 820)
        let video = CGSize(width: 1920, height: 1080)
        let implicit = ContentRectMapper(viewSize: view, videoSize: video)
        let explicit = ContentRectMapper(viewSize: view, videoSize: video, fit: .fit)
        XCTAssertEqual(implicit.fit, .fit)
        XCTAssertEqual(implicit.contentRect, explicit.contentRect)
        XCTAssertEqual(implicit.visibleRect, implicit.contentRect)
        // The 16:9 picture sits between 78 pt bars; a bar touch does nothing.
        XCTAssertEqual(implicit.contentRect.minY, (820 - 1180 * 9 / 16) / 2, accuracy: 1e-9)
        XCTAssertNil(implicit.normalize(CGPoint(x: 590, y: 20)))
        XCTAssertNotNil(ContentRectMapper(viewSize: view, videoSize: video, fit: .fill).normalize(CGPoint(x: 590, y: 20)))
    }

    func testMatchingShapesLookTheSameInBothModes() {
        // Second screen with "Match the iPad's resolution".
        let view = CGSize(width: 1180, height: 820)
        let video = CGSize(width: 2360, height: 1640)
        XCTAssertEqual(ContentRectMapper(viewSize: view, videoSize: video, fit: .fill).contentRect,
                       ContentRectMapper(viewSize: view, videoSize: video, fit: .fit).contentRect)
        XCTAssertEqual(MetalRenderer.aspectFillScale(video: video, drawable: CGSize(width: 2360, height: 1640)),
                       SIMD2<Float>(1, 1))
    }

    func testNoVideoMeansNoFillMapping() {
        let empty = ContentRectMapper(viewSize: CGSize(width: 100, height: 100), videoSize: .zero, fit: .fill)
        XCTAssertEqual(empty.visibleRect, .zero)
        XCTAssertNil(empty.normalize(CGPoint(x: 50, y: 50)))
        XCTAssertEqual(MetalRenderer.aspectFillScale(video: .zero, drawable: .zero), SIMD2<Float>(1, 1))
    }
}

final class PictureFitSettingTests: XCTestCase {
    private var saved: Any?

    override func setUp() {
        saved = UserDefaults.standard.object(forKey: "pictureFit")
        UserDefaults.standard.removeObject(forKey: "pictureFit")
    }

    override func tearDown() {
        if let saved {
            UserDefaults.standard.set(saved, forKey: "pictureFit")
        } else {
            UserDefaults.standard.removeObject(forKey: "pictureFit")
        }
    }

    func testFitIsTheDefault() {
        XCTAssertEqual(AppSettings().pictureFit, .fit)
        UserDefaults.standard.set("stretch", forKey: "pictureFit")
        XCTAssertEqual(AppSettings().pictureFit, .fit)
    }

    func testChoicePersists() {
        let settings = AppSettings()
        settings.pictureFit = .fill
        XCTAssertEqual(UserDefaults.standard.string(forKey: "pictureFit"), "fill")
        XCTAssertEqual(AppSettings().pictureFit, .fill)
        settings.pictureFit = .fit
        XCTAssertEqual(AppSettings().pictureFit, .fit)
    }
}
