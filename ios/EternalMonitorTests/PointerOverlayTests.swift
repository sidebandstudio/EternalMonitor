import XCTest
import os
@testable import EternalMonitor

final class PointerOverlayTests: XCTestCase {
    /// A 40x40 color image split as the PC splits it (1358 bytes a chunk).
    private func chunks(id: UInt32, side: UInt16 = 40, fill: UInt8 = 7) -> [PointerShapeChunk] {
        let total = PointerShapeChunk.imageLength(width: side, height: side, flags: 0)
        let bytes = Data(repeating: fill, count: total)
        return stride(from: 0, to: total, by: 1358).map { offset in
            PointerShapeChunk(shapeId: id, sourceType: 2, flags: 0, width: side, height: side, hotX: 0, hotY: 0,
                              totalLen: UInt32(total), offset: UInt32(offset),
                              data: bytes.subdata(in: offset..<min(offset + 1358, total)))
        }
    }

    private func state(_ x: Int32, visible: Bool = true, shape: UInt32 = 1) -> PointerState {
        PointerState(flags: visible ? PointerState.visibleFlag : 0, shapeId: shape, x: x, y: 2, hostTimeUs: 0)
    }

    func testAnImageCompletesFromChunksInAnyOrderOnce() {
        let overlay = PointerOverlay()
        var changes = 0
        overlay.onChange = { changes += 1 }
        let parts = chunks(id: 1)
        XCTAssertEqual(parts.count, 5)
        for chunk in parts.dropFirst().reversed() { overlay.handle(chunk) }
        overlay.handle(parts[4])  // a repeat adds nothing
        XCTAssertNil(overlay.snapshot().image, "one chunk still missing")
        XCTAssertEqual(overlay.completeShapeId, 0)
        overlay.handle(parts[0])
        let image = try? XCTUnwrap(overlay.snapshot().image)
        XCTAssertEqual(image?.id, 1)
        XCTAssertEqual(image?.pixels.count, 40 * 40 * 4)
        XCTAssertNil(image?.invertMask)
        XCTAssertEqual(overlay.completeShapeId, 1)
        XCTAssertEqual(changes, 1)
        // The PC's resend of a held image changes nothing.
        for chunk in parts { overlay.handle(chunk) }
        XCTAssertEqual(overlay.counters.shapes, 1)
    }

    func testANewImageReplacesTheOldButALateOlderChunkDoesNot() {
        let overlay = PointerOverlay()
        for chunk in chunks(id: 1) { overlay.handle(chunk) }
        let newer = chunks(id: 2, fill: 9)
        overlay.handle(newer[0])
        overlay.handle(chunks(id: 1)[1])  // a held image's late chunk
        XCTAssertEqual(overlay.snapshot().image?.id, 1, "the old image stays until the new one is whole")
        for chunk in newer.dropFirst() { overlay.handle(chunk) }
        XCTAssertEqual(overlay.snapshot().image?.id, 2)
        XCTAssertEqual(overlay.snapshot().image?.pixels.first, 9)
        let third = chunks(id: 4)
        overlay.handle(third[0])
        overlay.handle(chunks(id: 3)[0])  // older than the one assembling
        for chunk in third.dropFirst() { overlay.handle(chunk) }
        XCTAssertEqual(overlay.snapshot().image?.id, 4)
    }

    func testThePositionIsLatestWinsByMessageNumber() {
        let overlay = PointerOverlay()
        overlay.handle(state(10), msgSeq: 5)
        overlay.handle(state(9), msgSeq: 4)  // reordered on the way
        XCTAssertEqual(overlay.snapshot().x, 10)
        overlay.handle(state(11, visible: false), msgSeq: 6)
        XCTAssertFalse(overlay.snapshot().visible)
        overlay.handle(state(12), msgSeq: 6 &+ 0x7FFF_FFFF)
        XCTAssertEqual(overlay.snapshot().x, 12, "numbers compare across a wrap")
        XCTAssertEqual(overlay.counters.states, 3)
        XCTAssertEqual(overlay.counters.staleStates, 1)
        overlay.reset()
        overlay.handle(state(1), msgSeq: 1)
        XCTAssertEqual(overlay.snapshot().x, 1, "a new session numbers from 1")
        XCTAssertNil(overlay.snapshot().image)
    }

    func testTheInvertMaskExpandsToABytePerPixel() {
        // 9x1: two mask bytes per row, bits 0 and 8 set.
        let flags = PointerShapeChunk.invertFlag
        let total = PointerShapeChunk.imageLength(width: 9, height: 1, flags: flags)
        XCTAssertEqual(total, 9 * 4 + 2)
        var bytes = [UInt8](repeating: 0, count: total)
        bytes[36] = 0x80
        bytes[37] = 0x80
        let chunk = PointerShapeChunk(shapeId: 1, sourceType: 1, flags: flags, width: 9, height: 1, hotX: 0,
                                      hotY: 0, totalLen: UInt32(total), offset: 0, data: Data(bytes))
        let mask = [UInt8](PointerOverlay.image(chunk, bytes: bytes).invertMask ?? Data())
        XCTAssertEqual(mask, [255, 0, 0, 0, 0, 0, 0, 0, 255])
    }

    // MARK: Geometry

    /// Taps on the drawn hotspot normalize to the hotspot's video pixel, in
    /// Fit and Fill, for a wider and a taller picture.
    func testThePointerIsDrawnWhereTouchesMapTo() {
        let view = CGSize(width: 1194, height: 834)
        for video in [CGSize(width: 3440, height: 1440), CGSize(width: 1080, height: 1920)] {
            for fit in [PictureFit.fit, .fill] {
                let (x, y, hotX, hotY): (Int32, Int32, CGFloat, CGFloat) = (500, 300, 3, 5)
                let rect = PointerGeometry.rect(x: x, y: y, width: 32, height: 32, video: video, view: view, fit: fit)
                let mapper = ContentRectMapper(viewSize: view, videoSize: video, fit: fit)
                let content = mapper.contentRect
                XCTAssertEqual(rect.width, 32 * content.width / video.width, accuracy: 1e-9)
                let hot = CGPoint(x: rect.minX + hotX * content.width / video.width,
                                  y: rect.minY + hotY * content.height / video.height)
                guard let normalized = mapper.normalize(hot) else {
                    // Fill trims edges: this pointer may be off screen.
                    XCTAssertFalse(mapper.visibleRect.contains(hot), "\(video) \(fit)")
                    continue
                }
                let expectX = (CGFloat(x) + hotX) / video.width * 65535
                let expectY = (CGFloat(y) + hotY) / video.height * 65535
                XCTAssertEqual(Double(normalized.x), Double(expectX), accuracy: 2, "\(video) \(fit)")
                XCTAssertEqual(Double(normalized.y), Double(expectY), accuracy: 2, "\(video) \(fit)")
            }
        }
    }

    /// A pointer image the size of the video covers exactly the video's
    /// quad, which the renderer scales by `MetalRenderer.scale`.
    func testTheQuadMatchesTheVideoQuad() {
        let view = CGSize(width: 2388, height: 1668)
        for video in [CGSize(width: 3440, height: 1440), CGSize(width: 1080, height: 1920)] {
            for fit in [PictureFit.fit, .fill] {
                let rect = PointerGeometry.rect(x: 0, y: 0, width: Int(video.width), height: Int(video.height),
                                                video: video, view: view, fit: fit)
                let ndc = PointerGeometry.ndc(rect, view: view)
                let scale = MetalRenderer.scale(fit, video: video, drawable: view)
                XCTAssertEqual(ndc.x, -scale.x, accuracy: 1e-4)
                XCTAssertEqual(ndc.z, scale.x, accuracy: 1e-4)
                XCTAssertEqual(ndc.y, -scale.y, accuracy: 1e-4)
                XCTAssertEqual(ndc.w, scale.y, accuracy: 1e-4)
            }
        }
        // Partly off the left edge: negative positions keep their offset.
        let rect = PointerGeometry.rect(x: -4, y: 0, width: 8, height: 8, video: CGSize(width: 1000, height: 500),
                                        view: CGSize(width: 1000, height: 500), fit: .fit)
        XCTAssertEqual(rect.minX, -4)
        XCTAssertEqual(PointerGeometry.rect(x: 0, y: 0, width: 8, height: 8, video: .zero, view: view, fit: .fit), .zero)
    }

    // MARK: Channel

    /// The channel hands pointer messages on only for its session, and only
    /// from a PC that said it sends the pointer.
    func testPointerMessagesNeedTheSessionAndTheCapability() async throws {
        for hostCaps in [UInt16(0), HelloAck.hostCapPointer] {
            let queue = DispatchQueue(label: "pointer-channel-test")
            let (stream, continuation) = AsyncStream<Data>.makeStream()
            let channel = ControlChannel(queue: queue) { continuation.yield($0) }
            let overlay = PointerOverlay()
            channel.pointer = overlay
            channel.startHandshake(listenPort: 50000, identity: ControlChannel.ClientIdentity(
                deviceName: "Test iPad", screenPxW: 640, screenPxH: 360, screenPtW: 640, screenPtH: 360,
                refreshHz: 60))
            var messages = stream.makeAsyncIterator()
            let first = await messages.next()
            guard case .hello2(let hello) = try XCTUnwrap(Wire.parseControl(XCTUnwrap(first))).message else {
                return XCTFail("missing handshake")
            }
            queue.sync {
                channel.handleControl(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .helloAck(HelloAck(
                    status: .ok, acceptedVersion: 2, clientNonce: hello.clientNonce, sessionId: 41,
                    heartbeatIntervalMs: 1000, reportIntervalMs: 500, livenessTimeoutMs: 3000,
                    streamConfig: StreamConfig(), hostName: "Test PC", authToken: Data(17...32),
                    hostCaps: hostCaps))))
                channel.handleControl(Wire.encodeControl(sessionId: 40, msgSeq: 2, message: .pointerState(state(1))))
                for chunk in chunks(id: 1) {
                    channel.handleControl(Wire.encodeControl(sessionId: 41, msgSeq: 3, message: .pointerShape(chunk)))
                }
                channel.handleControl(Wire.encodeControl(sessionId: 41, msgSeq: 4, message: .pointerState(state(7))))
            }
            channel.stop()
            continuation.finish()
            let snapshot = overlay.snapshot()
            if hostCaps == 0 {
                XCTAssertEqual(snapshot, PointerOverlay.Snapshot(), "a PC without the bit gets no pointer drawn")
            } else {
                XCTAssertEqual(snapshot.x, 7, "another session's state is ignored")
                XCTAssertEqual(snapshot.image?.id, 1)
                XCTAssertEqual(overlay.counters.states, 1)
            }
        }
    }

    func testThisAppAsksToDrawThePointer() {
        let caps = ConnectionManager.featureCaps(wantsInput: true, wantsAudio: false, isUSB: false)
        XCTAssertNotEqual(caps & Hello2.featureDrawsPointer, 0)
        XCTAssertEqual(Hello2.featureDrawsPointer & (Hello2.featureWantsInput | Hello2.featureWantsAudio
            | Hello2.featureSupportsNack | Hello2.featureChecksUpdates), 0)
    }
}
