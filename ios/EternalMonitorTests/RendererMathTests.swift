import XCTest
import MetalKit
@testable import EternalMonitor

final class RendererMathTests: XCTestCase {
    func testWideSourceLetterboxes() {
        // 16:9 video on a 4:3 drawable: full width, reduced height.
        let scale = MetalRenderer.aspectFitScale(
            video: CGSize(width: 1920, height: 1080),
            drawable: CGSize(width: 2048, height: 1536)
        )
        XCTAssertEqual(scale.x, 1, accuracy: 0.0001)
        XCTAssertEqual(scale.y, Float((2048.0 / 1536.0) / (1920.0 / 1080.0)), accuracy: 0.0001)
        XCTAssertLessThan(scale.y, 1)
    }

    func testTallSourcePillarboxes() {
        // 9:16 video on a 4:3 drawable: full height, reduced width.
        let scale = MetalRenderer.aspectFitScale(
            video: CGSize(width: 1080, height: 1920),
            drawable: CGSize(width: 2048, height: 1536)
        )
        XCTAssertEqual(scale.y, 1, accuracy: 0.0001)
        XCTAssertLessThan(scale.x, 1)
    }

    func testExactMatchIsUnityScale() {
        let scale = MetalRenderer.aspectFitScale(
            video: CGSize(width: 1280, height: 720),
            drawable: CGSize(width: 2560, height: 1440)
        )
        XCTAssertEqual(scale.x, 1, accuracy: 0.0001)
        XCTAssertEqual(scale.y, 1, accuracy: 0.0001)
    }

    func testDegenerateSizesAreSafe() {
        let scale = MetalRenderer.aspectFitScale(video: .zero, drawable: .zero)
        XCTAssertEqual(scale, SIMD2<Float>(1, 1))
    }

    private func rendererHoldingAFrame() throws -> (MetalRenderer, MTKView) {
        let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
        let view = MTKView(frame: CGRect(x: 0, y: 0, width: 64, height: 64), device: device)
        view.colorPixelFormat = .bgra8Unorm
        let renderer = MetalRenderer(device: device, view: view)
        renderer.draw(in: view, pixelBuffer: try XCTUnwrap(makeBuffer()))
        XCTAssertTrue(renderer.holdsFrame)
        return (renderer, view)
    }

    func testMemoryWarningReleasesTheHeldFrame() throws {
        let (renderer, view) = try rendererHoldingAFrame()
        let coordinator = MetalView.Coordinator()
        coordinator.renderer = renderer
        coordinator.attach(view: view)
        defer { coordinator.detach() }
        NotificationCenter.default.post(name: UIApplication.didReceiveMemoryWarningNotification, object: nil)
        XCTAssertFalse(renderer.holdsFrame, "a memory warning must not keep a multi-megabyte frame")
        // A new frame is held again.
        renderer.draw(in: view, pixelBuffer: try XCTUnwrap(makeBuffer()))
        XCTAssertTrue(renderer.holdsFrame)
    }

    /// The picture closes while a draw's command buffer is still finishing.
    /// Its completion handler must release that draw's textures before their
    /// cache goes: CoreVideo crashed when the main thread destroyed the cache
    /// while the handler was releasing them.
    func testTexturesAreReleasedBeforeTheirCacheWhenThePictureCloses() throws {
        try closeWithADrawOutstanding(afterMemoryWarning: true)
        try closeWithADrawOutstanding(afterMemoryWarning: false)
    }

    private func closeWithADrawOutstanding(afterMemoryWarning: Bool) throws {
        let entered = DispatchSemaphore(value: 0)
        let release = DispatchSemaphore(value: 0)
        // Every exit lets the held handler go, so a slow or failed run can't
        // strand the frame, its cache and later Metal work.
        var released = false
        defer { if !released { release.signal() } }
        weak var cache: CVMetalTextureCache?
        var completed = false
        try autoreleasepool {
            let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
            let view = MTKView(frame: CGRect(x: 0, y: 0, width: 64, height: 64), device: device)
            view.colorPixelFormat = .bgra8Unorm
            let renderer = MetalRenderer(device: device, view: view)
            renderer.completedHandlerGate = { entered.signal(); _ = release.wait(timeout: .now() + 30) }
            renderer.draw(in: view, pixelBuffer: try XCTUnwrap(makeBuffer()))
            cache = renderer.textureCache
            completed = entered.wait(timeout: .now() + 10) == .success
            if afterMemoryWarning { renderer.releaseCachedFrame() }
        }
        guard completed else { return XCTFail("the draw's command buffer never completed") }
        // The renderer is gone and the held handler still has the textures.
        XCTAssertNotNil(cache, "the texture cache went before its textures (memory warning first: \(afterMemoryWarning))")
        released = true
        release.signal()
        // Once the handler lets them go, the cache goes too.
        let deadline = Date().addingTimeInterval(10)
        while cache != nil, Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        XCTAssertNil(cache, "the texture cache must not outlive its textures")
    }

    private func makeBuffer() -> CVPixelBuffer? {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(kCFAllocatorDefault, 64, 64, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, [
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as [String: Any],
        ] as CFDictionary, &buffer)
        return buffer
    }

    func testMemoryFootprintIsMeasured() {
        XCTAssertGreaterThan(MemoryFootprint.megabytes() ?? 0, 0)
    }
}
