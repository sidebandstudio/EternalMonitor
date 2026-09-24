import XCTest
@testable import EternalMonitor

final class KeyboardRelayTests: XCTestCase {
    func testHIDCoverageAndCommandMapping() {
        for usage in UInt16(0)...255 {
            XCTAssertEqual(KeyboardRelayMachine.isSupported(usage),
                (4...0x65).contains(usage) || (0xe0...0xe7).contains(usage))
        }
        XCTAssertEqual(KeyboardRelayMachine.mappedUsage(0xe3, commandAsControl: true), 0xe0)
        XCTAssertEqual(KeyboardRelayMachine.mappedUsage(0xe7, commandAsControl: true), 0xe4)
        XCTAssertEqual(KeyboardRelayMachine.mappedUsage(0xe7, commandAsControl: false), 0xe7)
        XCTAssertEqual(KeyboardRelayMachine.mappedUsage(0x04, commandAsControl: true), 0x04)
    }

    func testHardwareChordsSynthesiseFlagsAndReleaseAfterMappingChanges() {
        var machine = KeyboardRelayMachine()
        let down = machine.hardware(usage: 0x06, down: true, modifiers: 8, commandAsControl: true)
        XCTAssertEqual(down.map(\.keycode), [0xe0, 0x06])
        XCTAssertTrue(down.allSatisfy { $0.phase == 0 })
        let up = machine.hardware(usage: 0x06, down: false, modifiers: 0, commandAsControl: false)
        XCTAssertEqual(up.map(\.keycode), [0xe0, 0x06])
        XCTAssertTrue(up.allSatisfy { $0.phase == 2 })
        XCTAssertTrue(machine.cancel().isEmpty)
        let shift = machine.hardware(usage: 0x04, down: true, modifiers: 1, commandAsControl: true)
        XCTAssertEqual(shift.map(\.keycode), [0xe1, 0x04])
        XCTAssertEqual(Set(machine.cancel().map(\.keycode)), [0xe1, 0x04])
    }

    func testCommandAndControlDoNotReleaseEachOther() {
        var machine = KeyboardRelayMachine()
        XCTAssertEqual(machine.hardware(usage: 0xe0, down: true, modifiers: 2, commandAsControl: true).count, 1)
        XCTAssertTrue(machine.hardware(usage: 0xe3, down: true, modifiers: 10, commandAsControl: true).isEmpty)
        XCTAssertTrue(machine.hardware(usage: 0xe0, down: false, modifiers: 8, commandAsControl: true).isEmpty)
        let last = machine.hardware(usage: 0xe3, down: false, modifiers: 0, commandAsControl: true)
        XCTAssertEqual(last.map(\.keycode), [0xe0])
        XCTAssertEqual(last.first?.phase, 2)
    }

    func testLeftAndRightModifiersReleaseIndependently() {
        var machine = KeyboardRelayMachine()
        XCTAssertEqual(machine.hardware(usage: 0xe1, down: true, modifiers: 1, commandAsControl: true).map(\.keycode), [0xe1])
        XCTAssertEqual(machine.hardware(usage: 0xe5, down: true, modifiers: 1, commandAsControl: true).map(\.keycode), [0xe5])
        XCTAssertEqual(machine.hardware(usage: 0xe1, down: false, modifiers: 1, commandAsControl: true).map(\.keycode), [0xe1])
        XCTAssertEqual(machine.cancel().map(\.keycode), [0xe5])
        // Flags may arrive before the physical modifier's own press event.
        _ = machine.hardware(usage: 0x04, down: true, modifiers: 1, commandAsControl: true)
        let right = machine.hardware(usage: 0xe5, down: true, modifiers: 1, commandAsControl: true)
        XCTAssertEqual(right.map(\.keycode), [0xe1, 0xe5])
        XCTAssertEqual(right.map(\.phase), [2, 0])
        XCTAssertEqual(Set(machine.cancel().map(\.keycode)), [0x04, 0xe5])
    }

    func testTextPreservesUTF16AndConvertsNewlineToEnter() {
        var machine = KeyboardRelayMachine()
        let events = machine.text("Hi!😀\r\n")
        XCTAssertEqual(events.map(\.keycode), [72, 105, 33, 0xd83d, 0xde00, 0x28, 0x28])
        XCTAssertEqual(events.map(\.kind), [5, 5, 5, 5, 5, 4, 4])
        XCTAssertEqual(events.map(\.phase), [0, 0, 0, 0, 0, 0, 2])
        XCTAssertEqual(machine.tap(0x2a).map(\.phase), [0, 2])
    }

    func testStickyModifiersApplyToOneKeyAndCanBeCancelled() {
        var machine = KeyboardRelayMachine()
        XCTAssertEqual(machine.toggleSticky(0xe0).first?.phase, 0)
        XCTAssertEqual(machine.sticky, [0xe0])
        let shortcut = machine.text("cv")
        XCTAssertEqual(shortcut.map(\.keycode), [0x06, 0x06, 0xe0, 118])
        XCTAssertEqual(shortcut.map(\.phase), [0, 2, 2, 0])
        XCTAssertTrue(machine.sticky.isEmpty)
        _ = machine.toggleSticky(0xe2)
        XCTAssertEqual(machine.toggleSticky(0xe2).first?.phase, 2)
        _ = machine.toggleSticky(0xe3)
        XCTAssertEqual(machine.cancel().map(\.keycode), [0xe3])
        XCTAssertTrue(machine.cancel().isEmpty)
    }

    func testOneSequenceCoversEveryInputSourceAndDuplicatesOnlyEdges() {
        var machine = KeyboardRelayMachine()
        var sequence = InputEventSequencer()
        let first = sequence.packets(machine.text("a"), timeUs: 100)
        XCTAssertEqual(first.count, 2)
        XCTAssertEqual(first[0], first[1])
        var hover = first[0]
        hover.kind = 6; hover.phase = 1
        let second = sequence.packets([hover], timeUs: 200)
        XCTAssertEqual(second.count, 1)
        XCTAssertEqual(second[0].eventId, first[0].eventId + 1)
        XCTAssertEqual(second[0].clientTimeUs, 200)
        let third = sequence.packets(machine.tap(0x28), timeUs: 300)
        XCTAssertEqual(third.map(\.eventId), [3, 3, 4, 4])
    }

    func testHardwareKeyConsumesStickyAndRightShiftIsNotReleasedByText() {
        var machine = KeyboardRelayMachine()
        _ = machine.toggleSticky(0xe0)
        XCTAssertEqual(machine.hardware(usage: 0x06, down: true, modifiers: 0, commandAsControl: true).map(\.keycode), [0x06])
        XCTAssertEqual(machine.hardware(usage: 0x06, down: false, modifiers: 0, commandAsControl: true).map(\.keycode), [0x06, 0xe0])
        XCTAssertTrue(machine.sticky.isEmpty)
        _ = machine.hardware(usage: 0xe5, down: true, modifiers: 1, commandAsControl: true)
        _ = machine.toggleSticky(0xe0)
        XCTAssertEqual(machine.text("A").map(\.keycode), [0x04, 0x04, 0xe0])
        XCTAssertEqual(machine.cancel().map(\.keycode), [0xe5])
    }

    func testPointerButtonsReleaseOutsideVideoAndHoverIsBounded() {
        var pointer = PointerRelayMachine()
        let point = TouchRelayMachine.Point(x: 100, y: 200)
        XCTAssertTrue(pointer.began(at: nil, mask: 1).isEmpty)
        XCTAssertEqual(pointer.began(at: point, mask: 6).map(\.buttons), [2, 4])
        XCTAssertEqual(pointer.ended(at: nil, remainingMask: 4).map(\.buttons), [2])
        let release = pointer.ended(at: nil)
        XCTAssertEqual(release.first?.buttons, 4)
        XCTAssertEqual(release.first?.xNorm, 100)
        XCTAssertTrue(pointer.ended(at: nil).isEmpty)
        XCTAssertEqual(pointer.hover(at: point, timeUs: 0).count, 1)
        XCTAssertTrue(pointer.hover(at: point, timeUs: 16_666).isEmpty)
        XCTAssertEqual(pointer.hover(at: point, timeUs: 16_667).first?.kind, 6)
        XCTAssertEqual(pointer.scroll(at: point, delta: CGSize(width: 0, height: -10)).first?.scrollDy, -10)
    }
}
