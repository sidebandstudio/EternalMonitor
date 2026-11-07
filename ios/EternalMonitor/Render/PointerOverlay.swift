import CoreGraphics
import Foundation
import os

/// The PC's pointer, which this app draws over the video when the PC sends
/// it apart (this app sets `Hello2.featureDrawsPointer`, the PC answers
/// with `HelloAck.hostCapPointer`). Pointer motion then never waits for a
/// video frame. Messages arrive on the control queue; the renderer reads a
/// snapshot on the main thread.
final class PointerOverlay {
    /// A complete pointer image: premultiplied BGRA rows, and one byte per
    /// pixel (0 or 255) where the picture under it is inverted.
    struct Image: Equatable {
        let id: UInt32
        let width: Int
        let height: Int
        let pixels: Data
        let invertMask: Data?
    }

    struct Snapshot: Equatable {
        var image: Image?
        var x: Int32 = 0
        var y: Int32 = 0
        var visible = false
    }

    struct Counters: Equatable {
        var states = 0
        var staleStates = 0
        var shapes = 0
        /// Draws with the pointer, and how many of them moved it.
        var drawn = 0
        var drawnMoves = 0
        var lastDrawn: (x: Int32, y: Int32)?

        static func == (a: Counters, b: Counters) -> Bool {
            (a.states, a.staleStates, a.shapes, a.drawn, a.drawnMoves)
                == (b.states, b.staleStates, b.shapes, b.drawn, b.drawnMoves)
        }
    }

    /// A position applied or an image completed: redraw. Called on the
    /// control queue.
    var onChange: (() -> Void)?

    private struct Assembly {
        let chunk: PointerShapeChunk
        var bytes: [UInt8]
        var offsets: Set<UInt32> = []
        var received = 0
    }

    private struct State {
        var assembly: Assembly?
        var snapshot = Snapshot()
        var lastSeq: UInt32?
        var counters = Counters()
    }

    private let state = OSAllocatedUnfairLock(initialState: State())

    /// A new session: nothing from the last one applies.
    func reset() {
        state.withLock { $0 = State() }
        onChange?()
    }

    /// The image the receiver reports name, so the PC sends it again when
    /// a chunk was lost.
    var completeShapeId: UInt32 { state.withLock { $0.snapshot.image?.id ?? 0 } }

    var counters: Counters { state.withLock { $0.counters } }

    func snapshot() -> Snapshot { state.withLock { $0.snapshot } }

    /// The renderer drew the pointer here.
    func noteDrawn(x: Int32, y: Int32) {
        state.withLock { s in
            s.counters.drawn += 1
            if let last = s.counters.lastDrawn, last != (x, y) { s.counters.drawnMoves += 1 }
            s.counters.lastDrawn = (x, y)
        }
    }

    /// Latest wins by the control header's `msgSeq`, which counts every
    /// control message the PC sends.
    func handle(_ pointer: PointerState, msgSeq: UInt32) {
        let applied = state.withLock { s -> Bool in
            if let last = s.lastSeq, Int32(bitPattern: msgSeq &- last) <= 0 {
                s.counters.staleStates += 1
                return false
            }
            s.lastSeq = msgSeq
            s.counters.states += 1
            s.snapshot.x = pointer.x
            s.snapshot.y = pointer.y
            s.snapshot.visible = pointer.visible
            return true
        }
        if applied { onChange?() }
    }

    func handle(_ chunk: PointerShapeChunk) {
        let completed = state.withLock { s -> Bool in
            if s.snapshot.image?.id == chunk.shapeId { return false }
            if s.assembly?.chunk.shapeId != chunk.shapeId {
                // A late chunk of an older image doesn't replace a newer one.
                if let pending = s.assembly, Int32(bitPattern: chunk.shapeId &- pending.chunk.shapeId) < 0 {
                    return false
                }
                s.assembly = Assembly(chunk: chunk, bytes: [UInt8](repeating: 0, count: Int(chunk.totalLen)))
            }
            guard var assembly = s.assembly,
                  assembly.chunk.width == chunk.width, assembly.chunk.height == chunk.height,
                  assembly.chunk.flags == chunk.flags,
                  !assembly.offsets.contains(chunk.offset)
            else { return false }
            let start = Int(chunk.offset)
            assembly.bytes.replaceSubrange(start..<start + chunk.data.count, with: chunk.data)
            assembly.offsets.insert(chunk.offset)
            assembly.received += chunk.data.count
            guard assembly.received >= assembly.bytes.count else {
                s.assembly = assembly
                return false
            }
            s.assembly = nil
            s.snapshot.image = Self.image(assembly.chunk, bytes: assembly.bytes)
            s.counters.shapes += 1
            return true
        }
        if completed { onChange?() }
    }

    static func image(_ chunk: PointerShapeChunk, bytes: [UInt8]) -> Image {
        let (w, h) = (Int(chunk.width), Int(chunk.height))
        let pixels = Data(bytes[0..<w * h * 4])
        var mask: Data?
        if chunk.flags & PointerShapeChunk.invertFlag != 0 {
            let rowBytes = (w + 7) / 8
            var expanded = Data(count: w * h)
            for y in 0..<h {
                for x in 0..<w where bytes[w * h * 4 + y * rowBytes + x / 8] & (0x80 >> (x % 8)) != 0 {
                    expanded[y * w + x] = 255
                }
            }
            mask = expanded
        }
        return Image(id: chunk.shapeId, width: w, height: h, pixels: pixels, invertMask: mask)
    }
}

/// Where the pointer is drawn, from the same content rect the touch relay
/// normalizes against (`ContentRectMapper`), so a tap on the drawn hotspot
/// lands where the PC has the pointer. Pure and unit-tested.
enum PointerGeometry {
    /// The pointer image's rect in view coordinates. `x`/`y` and the image
    /// size are video pixels; the image scales with the picture.
    static func rect(x: Int32, y: Int32, width: Int, height: Int, video: CGSize, view: CGSize,
                     fit: PictureFit) -> CGRect {
        let content = ContentRectMapper(viewSize: view, videoSize: video, fit: fit).contentRect
        guard content.width > 0, content.height > 0 else { return .zero }
        let sx = content.width / video.width
        let sy = content.height / video.height
        return CGRect(x: content.minX + CGFloat(x) * sx, y: content.minY + CGFloat(y) * sy,
                      width: CGFloat(width) * sx, height: CGFloat(height) * sy)
    }

    /// `rect` in Metal's normalized device coordinates: (minX, minY, maxX,
    /// maxY) with y up, as the renderer's quad takes it.
    static func ndc(_ rect: CGRect, view: CGSize) -> SIMD4<Float> {
        guard view.width > 0, view.height > 0 else { return .zero }
        let x0 = rect.minX / view.width * 2 - 1
        let x1 = rect.maxX / view.width * 2 - 1
        let y0 = 1 - rect.maxY / view.height * 2
        let y1 = 1 - rect.minY / view.height * 2
        return SIMD4<Float>(Float(x0), Float(y0), Float(x1), Float(y1))
    }
}
