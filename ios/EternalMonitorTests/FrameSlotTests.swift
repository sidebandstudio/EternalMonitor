import XCTest
import CoreVideo
import os
@testable import EternalMonitor

/// The slot shows decoder output in submission order, whatever order
/// VideoToolbox returns it in.
final class FrameSlotTests: XCTestCase {
    private func frame() -> CVPixelBuffer {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(kCFAllocatorDefault, 2, 2, kCVPixelFormatType_32BGRA, nil, &buffer)
        return buffer!
    }

    func testFrameOutputAfterANewerOneIsRefused() {
        let slot = FrameSlot()
        let newer = frame()
        XCTAssertTrue(slot.set(newer, sequence: 6))
        XCTAssertFalse(slot.set(frame(), sequence: 5))
        XCTAssertTrue(slot.take() === newer)
    }

    func testFrameOutputAfterANewerOneWasDrawnIsRefused() {
        // The renderer drew 6 and emptied the slot; 5 must not follow it.
        let slot = FrameSlot()
        slot.set(frame(), sequence: 6)
        XCTAssertNotNil(slot.take())
        XCTAssertFalse(slot.set(frame(), sequence: 5))
        XCTAssertNil(slot.take())
    }

    func testEqualOrNewerFrameIsStored() {
        let slot = FrameSlot()
        slot.set(frame(), sequence: 6)
        XCTAssertNotNil(slot.take())
        let same = frame()
        XCTAssertTrue(slot.set(same, sequence: 6))
        XCTAssertTrue(slot.take() === same)
        let newer = frame()
        XCTAssertTrue(slot.set(newer, sequence: 7))
        XCTAssertTrue(slot.take() === newer)
    }

    func testOnlyAStoredFrameAsksForARedraw() {
        let slot = FrameSlot()
        let redraws = OSAllocatedUnfairLock(initialState: 0)
        slot.onFrameStored = { redraws.withLock { $0 += 1 } }
        slot.set(frame(), sequence: 6)
        slot.set(frame(), sequence: 5)
        XCTAssertEqual(redraws.withLock { $0 }, 1)
    }

    func testConcurrentOutputKeepsTheNewest() {
        let slot = FrameSlot()
        let frames = (1...200).map { _ in frame() }
        DispatchQueue.concurrentPerform(iterations: frames.count) { index in
            slot.set(frames[index], sequence: UInt64(index + 1))
        }
        XCTAssertTrue(slot.take() === frames.last)
    }
}
