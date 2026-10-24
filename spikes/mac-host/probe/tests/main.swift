// Safe tests for Sources/Safety.swift and Sources/Measure.swift: no display
// is created, no event is posted, nothing in the session changes. Built and
// run by tests/run.sh.
import CoreGraphics
import Foundation

if CommandLine.arguments.count > 2, CommandLine.arguments[1] == "--watchdog-child" {
    // Arms a short self-deadline and then never finishes on its own.
    Watchdog.arm(seconds: Double(CommandLine.arguments[2]) ?? 0.5) {}
    while true { sleep(60) }
}

var failures = 0
var checks = 0
func expect(_ ok: Bool, _ what: String, file: String = #file, line: Int = #line) {
    checks += 1
    if !ok { failures += 1 }
    print("\(ok ? "PASS" : "FAIL") \(what)\(ok ? "" : "  (tests/main.swift:\(line))")")
}

// MARK: ProbeTag

let run = ProbeTag.newRun()
expect(run > 0 && run < 1 << 48, "a run tag fits in 48 bits and is never 0")
expect(ProbeTag.seq(ProbeTag.make(run: run, seq: 7), run: run) == 7, "tag round trip")
expect(ProbeTag.seq(ProbeTag.make(run: run ^ 1, seq: 7), run: run) == nil, "another run's tag is not ours")
expect(ProbeTag.seq(0, run: run) == nil, "untagged is not ours")

// MARK: TapPolicy

let display = CGRect(x: 3440, y: 0, width: 1335, height: 939)
let inside = CGPoint(x: 3440 + 400, y: 500)
let outside = CGPoint(x: 1000, y: 500)
let systemRun = ProbeTag.newRun(), pidRun = ProbeTag.newRun()
let me: Int32 = 4242
let safe = TapState(systemRun: systemRun, pidRun: pidRun, display: display, frontmost: true, ownPID: me)
func decide(_ type: CGEventType, _ tag: Int64, _ at: CGPoint, flags: CGEventFlags = [], target: Int32 = 0,
            _ state: TapState = safe) -> TapDecision {
    TapPolicy.decide(type: type, userData: tag, location: at, flags: flags, targetPID: target, state: state)
}
let sys = ProbeTag.make(run: systemRun, seq: 1), pid = ProbeTag.make(run: pidRun, seq: 1)
func isSwallow(_ d: TapDecision) -> Bool { if case .swallow = d { true } else { false } }

expect(decide(.mouseMoved, 0, inside) == .passThrough, "untagged (real) input passes untouched, inside")
expect(decide(.leftMouseDown, 0, outside) == .passThrough, "untagged (real) input passes untouched, outside")
expect(decide(.keyDown, ProbeTag.make(run: 99, seq: 1), outside) == .passThrough, "a foreign tag passes untouched")
expect(decide(.leftMouseDown, pid, outside) == .passOwnPid, "a postToPid event passes (addressed to this process)")
expect(decide(.flagsChanged, pid, outside, flags: .maskShift) == .passOwnPid, "postToPid modifiers pass (own process only)")
expect(decide(.leftMouseDown, sys, inside) == .deliver, "system pointer inside the display, frontmost: delivered")
expect(decide(.leftMouseDown, sys, inside, target: me) == .deliver, "system pointer annotated for this process: delivered")
expect(isSwallow(decide(.leftMouseDown, sys, inside, target: 999)), "system pointer annotated for another process: swallowed")
expect(isSwallow(decide(.leftMouseDown, sys, outside)), "system pointer outside the display: swallowed")
expect(isSwallow(decide(.mouseMoved, sys, CGPoint(x: display.maxX, y: 10))), "system pointer on the display's right edge (next display): swallowed")
expect(isSwallow(decide(.mouseMoved, sys, CGPoint(x: display.minX + 1, y: display.maxY))), "system pointer on the bottom edge: swallowed")
var noDisplay = safe
noDisplay.display = nil
expect(isSwallow(decide(.mouseMoved, sys, inside, noDisplay)), "system pointer with no virtual display: swallowed")
var background = safe
background.frontmost = false
expect(isSwallow(decide(.leftMouseDown, sys, inside, background)), "system pointer while the probe is not frontmost: swallowed")
expect(isSwallow(decide(.scrollWheel, sys, inside, background)), "system scroll while not frontmost: swallowed")
expect(decide(.tabletProximity, sys, inside) == .deliver, "pen proximity inside, frontmost: delivered")
expect(isSwallow(decide(.keyDown, sys, inside)), "a system-path key (never posted) is swallowed as a failure")
expect(isSwallow(decide(.keyUp, sys, outside, background)), "a system-path key up is swallowed as a failure")
expect(decide(.keyDown, pid, inside) == .passOwnPid, "keys go only to the probe's own process")
for (flag, name) in [(CGEventFlags.maskCommand, "⌘"), (.maskControl, "⌃"), (.maskAlternate, "⌥"), (.maskSecondaryFn, "fn")] {
    expect(isSwallow(decide(.keyDown, sys, inside, flags: flag)), "system key with \(name): swallowed as a failure")
    expect(isSwallow(decide(.leftMouseDown, sys, inside, flags: flag)), "system click with \(name): swallowed as a failure")
}
expect(isSwallow(decide(.flagsChanged, sys, inside)), "a system-path modifier key event: swallowed as a failure")
expect(TapPolicy.isInput(.mouseMoved) && TapPolicy.isInput(.keyDown) && TapPolicy.isInput(.flagsChanged), "real moves, keys and modifiers count as input")
expect(!TapPolicy.isInput(.null), "non-input events don't stop the run")
expect(TapPolicy.isInput(.scrollWheel) && TapPolicy.isInput(.leftMouseDown), "real clicks and scrolls count as input")
expect(!TapPolicy.isInput(.tabletPointer) && !TapPolicy.isInput(.tabletProximity), "tablet events (none are real here) don't stop the run")

// MARK: PointerSafety (what makes a system-path pointer event safe)

let shielding = 2_147_483_628
let ownWindow = 77
func win(_ number: Int, pid: Int32, layer: Int, _ bounds: CGRect, _ owner: String = "Other") -> WindowInfo {
    WindowInfo(number: number, pid: pid, layer: layer, bounds: bounds, owner: owner)
}
let probeWindow = win(ownWindow, pid: me, layer: shielding, display, "Probe")
let mainBounds = CGRect(x: 0, y: 0, width: 3440, height: 1440)
func cover(_ windows: [WindowInfo]) -> String? {
    PointerSafety.coverProblem(windows, display: display, ownPID: me, ownWindow: ownWindow, minLayer: shielding)
}
expect(cover([probeWindow, win(5, pid: 99, layer: 0, display)]) == nil, "topmost and exactly covering, others below it: safe")
expect(cover([win(1, pid: 99, layer: 25, mainBounds), probeWindow]) == nil, "a window above it on another display only: safe")
expect(cover([win(1, pid: 99, layer: shielding + 2, CGRect(x: display.minX + 100, y: 100, width: 50, height: 50)), probeWindow]) != nil,
       "another process's window above it on the display: not safe")
expect(cover([win(1, pid: 99, layer: shielding, CGRect(x: display.minX - 10, y: 0, width: 20, height: 20)), probeWindow]) != nil,
       "another window overlapping the display's edge from above: not safe")
expect(cover([win(1, pid: me, layer: shielding + 1, CGRect(x: display.minX, y: 0, width: 10, height: 10)), probeWindow]) == nil,
       "the probe's own window above it: safe")
expect(cover([win(ownWindow, pid: me, layer: 0, display)]) != nil, "below shielding level: not safe")
expect(cover([win(ownWindow, pid: me, layer: shielding, display.insetBy(dx: 2, dy: 0))]) != nil, "not covering the whole display: not safe")
expect(cover([win(ownWindow, pid: me, layer: shielding, display.offsetBy(dx: 0, dy: 3))]) != nil, "shifted off the display: not safe")
expect(cover([win(5, pid: 99, layer: 0, display)]) != nil, "probe window not on screen: not safe")
expect(cover([win(ownWindow, pid: 99, layer: shielding, display)]) != nil, "a window number owned by another pid: not safe")
expect(PointerSafety.inset(CGPoint(x: display.minX + 400, y: 500), display), "a point well inside: allowed")
expect(!PointerSafety.inset(CGPoint(x: display.minX + 3, y: 500), display), "a point 3 pt from the left edge: refused")
expect(!PointerSafety.inset(CGPoint(x: display.maxX - 1, y: 500), display), "a point 1 pt from the right edge (next display): refused")
expect(!PointerSafety.inset(CGPoint(x: display.minX + 400, y: display.maxY - 4), display), "a point 4 pt from the bottom: refused")

// MARK: SequenceMatcher

let a = CGPoint(x: 100, y: 200), b = CGPoint(x: 150, y: 210)
let posted = [
    PostedEvent(seq: 0, name: "move", type: "moved", point: a, keyCode: nil, chars: nil),
    PostedEvent(seq: 1, name: "left down", type: "leftDown", point: a, keyCode: nil, chars: nil),
    PostedEvent(seq: 2, name: "left drag", type: "leftDragged", point: b, keyCode: nil, chars: nil),
    PostedEvent(seq: 3, name: "key a", type: "keyDown", point: nil, keyCode: 0, chars: nil),
    PostedEvent(seq: 4, name: "é", type: "keyDown", point: nil, keyCode: 0, chars: "é"),
]
func seen(_ e: PostedEvent, seq: Int? = nil, point: CGPoint? = nil, type: String? = nil) -> SeenEvent {
    SeenEvent(seq: seq ?? e.seq, type: type ?? e.type, point: point ?? e.point, keyCode: e.keyCode, chars: e.chars)
}
let all = posted.map { seen($0) }
expect(SequenceMatcher.match(posted, all).passed, "the full sequence in order passes")
expect(!SequenceMatcher.match(posted, []).passed, "zero delivered fails")
expect(SequenceMatcher.match(posted, []).missing == [0, 1, 2, 3, 4], "zero delivered lists every event as missing")
let dropped = SequenceMatcher.match(posted, all.filter { $0.seq != 2 })
expect(!dropped.passed && dropped.missing == [2], "one missing event fails and is named")
let swapped = SequenceMatcher.match(posted, [all[0], all[2], all[1], all[3], all[4]])
expect(!swapped.passed && !swapped.outOfOrder.isEmpty, "reordered events fail")
let off = SequenceMatcher.match(posted, [all[0], seen(posted[1], point: CGPoint(x: a.x + 1.2, y: a.y)), all[2], all[3], all[4]])
expect(!off.passed && off.mismatched.count == 1, "an event 1.2 pt off fails")
let nearby = SequenceMatcher.match(posted, [all[0], seen(posted[1], point: CGPoint(x: a.x + 0.6, y: a.y - 0.6)), all[2], all[3], all[4]])
expect(nearby.passed && abs(nearby.maxErrorPt - 0.6 * 2.0.squareRoot()) < 1e-9, "an event 0.85 pt off passes and is measured")
let wrongType = SequenceMatcher.match(posted, [all[0], seen(posted[1], type: "rightDown"), all[2], all[3], all[4]])
expect(!wrongType.passed, "the wrong type fails")
let wrongChars = SequenceMatcher.match(posted, all.dropLast() + [SeenEvent(seq: 4, type: "keyDown", point: nil, keyCode: 0, chars: "e")])
expect(!wrongChars.passed, "the wrong characters fail")
let noLocation = SequenceMatcher.match(posted, [all[0], SeenEvent(seq: 1, type: "leftDown", point: nil, keyCode: nil, chars: nil), all[2], all[3], all[4]])
expect(!noLocation.passed, "a pointer event with no location fails")
let noisy = [SeenEvent(seq: nil, type: "moved", point: CGPoint(x: 5, y: 5), keyCode: nil, chars: nil)] + all
    + [seen(posted[2], point: CGPoint(x: 999, y: 999))]
let extras = SequenceMatcher.match(posted, noisy)
expect(extras.passed && extras.untagged == 1, "untagged extras and later duplicates are ignored")
let dupFirst = SequenceMatcher.match(posted, [all[0], seen(posted[1], type: "tabletPoint"), all[1], all[2], all[3], all[4]])
expect(dupFirst.passed, "an extra copy of an event (a tablet point beside the click) is allowed")
expect(!SequenceMatcher.match([], all).passed, "an empty expectation never passes")

// MARK: Deadlines

do {
    let start = Date()
    let silent = try ChildProcess.spawn("/bin/sleep", ["30"])
    let line = silent.readLine(timeout: 1.0)
    let waited = Date().timeIntervalSince(start)
    expect(line == nil && waited >= 0.95 && waited < 1.6, String(format: "a child that never prints: readLine gives up at its deadline (%.2f s)", waited))
    let stopStart = Date()
    let stopped = silent.stop(timeout: 1)
    expect(stopped && kill(silent.pid, 0) == -1 && errno == ESRCH,
           String(format: "it is killed by PID and reaped within the deadline (%.2f s)", Date().timeIntervalSince(stopStart)))

    let ready = try ChildProcess.spawn("/bin/sh", ["-c", "echo READY 42; exec sleep 30"])
    expect(ready.readLine(timeout: 2) == "READY 42", "a child that prints is read without waiting for the deadline")
    ready.stop()

    // A grandchild in the child's process group goes with it.
    let parent = try ChildProcess.spawn("/bin/sh", ["-c", "sleep 31 & echo $!; wait"])
    let grandchild = pid_t(parent.readLine(timeout: 2) ?? "") ?? 0
    expect(grandchild > 0 && kill(grandchild, 0) == 0, "the grandchild is running (pid \(grandchild))")
    parent.stop()
    var gone = false
    for _ in 0..<100 where !gone {
        gone = kill(grandchild, 0) == -1 && errno == ESRCH
        if !gone { usleep(10_000) }
    }
    expect(gone, "killing the child's group kills its grandchild too")

    let quiet = try ChildProcess.spawn("/bin/sh", ["-c", "printf partial; exit 0"])
    expect(quiet.readLine(timeout: 2) == nil && quiet.pending == "partial", "output closed without a newline is not a line")
    expect(quiet.waitExit(timeout: 2) != nil, "an exiting child is reaped")

    // The hard self-deadline: a process that never finishes exits by itself.
    let watchdogStart = Date()
    let watched = try ChildProcess.spawn(CommandLine.arguments[0], ["--watchdog-child", "0.5"])
    let exited = watched.waitExit(timeout: 3)
    let code = watched.exitStatus.map { ($0 >> 8) & 0xff } ?? -1
    expect(exited != nil && exited! < 1.5 && code == 124,
           String(format: "the watchdog exits a stuck process at its deadline (%.2f s, exit %d)", Date().timeIntervalSince(watchdogStart), code))
    if exited == nil { watched.stop() }
} catch {
    expect(false, "spawning test children: \(error)")
}

let semaphore = DispatchSemaphore(value: 0)
Task {
    let start = Date()
    do {
        _ = try await withDeadline(0.4) { () async throws -> Int in
            try await Task.sleep(nanoseconds: 60_000_000_000)
            return 1
        }
        expect(false, "withDeadline gives up on an operation that never returns")
    } catch {
        let waited = Date().timeIntervalSince(start)
        expect(error is DeadlineExceeded && waited < 1, String(format: "withDeadline gives up on an operation that never returns (%.2f s)", waited))
    }
    let quick = try? await withDeadline(2) { 7 }
    expect(quick == 7, "withDeadline returns a quick answer")
    semaphore.signal()
}
semaphore.wait()

// MARK: FrameRate

let at120 = (0..<960).map { Double($0) / 120 }
let window = FrameRate.window(first: at120[0], seconds: 8)
expect(window.start == 1 && window.length == 5, "the window is 5 s, starting 1 s in")
let fps120 = FrameRate.inWindow(at120, start: window.start, length: window.length)
expect(abs(fps120 - 120) < 0.5 && FrameRate.meets(fps120, target: 120), "120 fps delivery meets 120")
let at100 = (0..<800).map { Double($0) / 100 }
expect(!FrameRate.meets(FrameRate.inWindow(at100, start: 1, length: 5), target: 120), "100 fps does not meet 120")
expect(FrameRate.meets(108, target: 120) && !FrameRate.meets(107.9, target: 120), "the bar is 108 for 120")
expect(FrameRate.meets(FrameRate.inWindow(at120.filter { Int($0 * 120) % 2 == 0 }, start: 1, length: 5), target: 60), "60 fps meets 60")
let stalled = at120.filter { $0 < 2 || $0 > 4 } // a 2 s stall inside the window
expect(!FrameRate.meets(FrameRate.inWindow(stalled, start: 1, length: 5), target: 120), "a stall inside the window fails")
expect(FrameRate.inWindow([], start: 1, length: 5) == 0, "no frames is 0 fps")

// MARK: FramePacer

func paced(_ times: [Double], fps: Int) -> [Double] {
    var pacer = FramePacer(fps: fps)
    return times.filter { pacer.take($0) }
}
let steady120 = (0..<960).map { Double($0) / 120 }
// Sol's case: alternate gaps of 6.33 and 10.33 ms (±2 ms of callback jitter).
let jittered120: [Double] = (0..<960).map { (n: Int) -> Double in
    let delay: Double = n % 2 == 1 ? -0.002 : 0
    return Double(n) / 120 + delay
}
let wobbly120: [Double] = (0..<960).map { (n: Int) -> Double in Double(n) / 120 + 0.003 * sin(Double(n) * 1.7) }
expect(paced(steady120, fps: 120).count == 960, "steady 120 fps at a 120 target: every frame taken")
expect(paced(jittered120, fps: 120).count == 960, "120 fps with alternating 6.33/10.33 ms gaps at a 120 target: every frame taken")
expect(paced(wobbly120, fps: 120).count == 960, "120 fps with ±3 ms timestamp jitter: every frame taken")
let halved = paced(steady120, fps: 60)
expect(halved.count == 480 && FrameRate.meets(FrameRate.inWindow(halved, start: 1, length: 5), target: 60), "120 fps thinned to a 60 target: every other frame")
let halvedJittered = paced(jittered120, fps: 60)
expect(abs(halvedJittered.count - 480) <= 1, "jittered 120 fps thinned to 60: still half (\(halvedJittered.count))")
let gap = steady120.filter { $0 < 2 || $0 >= 3 } // a 1 s pause in delivery
let afterGap = paced(gap, fps: 60).filter { $0 >= 3 }
expect(abs(FrameRate.inWindow(afterGap, start: 3, length: 4) - 60) < 1, "after a pause the rate resumes at 60, with no burst")

// MARK: Marker

func luma(_ bits: [Bool], noise: Int = 0) -> [UInt8] {
    bits.enumerated().map { index, on in
        let jitter = noise == 0 ? 0 : (index * 37 % (2 * noise + 1)) - noise
        return UInt8(clamping: (on ? 235 : 16) + jitter)
    }
}
let id: UInt32 = 0x1234_5678
let decoded = Marker.decode(luma(Marker.bits(display: id, counter: 0xbeef)))
expect(decoded?.display == 0x5678 && decoded?.counter == 0xbeef, "a marker decodes to its display and counter")
let noisyDecode = Marker.decode(luma(Marker.bits(display: id, counter: 513), noise: 40))
expect(noisyDecode?.display == 0x5678 && noisyDecode?.counter == 513, "a marker survives ±40 luma of compression noise")
expect(Marker.decode([UInt8](repeating: 128, count: Marker.count)) == nil, "flat grey (no marker) is unreadable")
expect(Marker.decode(luma(Marker.bits(display: id, counter: 1).map { !$0 })) == nil, "an inverted marker is unreadable")
expect(Marker.decode(luma(Marker.bits(display: id, counter: 1)).dropLast().map { $0 }) == nil, "a short read is unreadable")

// A marker drawn into a synthetic luma plane and sampled back at its block centres.
let width = 1200, height = 200, blockPx = 24.0
var plane = [UInt8](repeating: 60, count: width * height)
for (index, on) in Marker.bits(display: id, counter: 77).enumerated() {
    for y in 30..<(30 + Int(blockPx)) {
        for x in (40 + index * Int(blockPx))..<(40 + (index + 1) * Int(blockPx)) { plane[y * width + x] = on ? 235 : 16 }
    }
}
let sampled = Marker.centres(x: 40, y: 30, blockPx: blockPx).map { plane[$0.1 * width + $0.0] }
expect(Marker.decode(sampled)?.counter == 77, "a marker is read back from a plane at its block centres")
let elsewhere = Marker.centres(x: 400, y: 120, blockPx: blockPx).map { plane[min($0.1, height - 1) * width + min($0.0, width - 1)] }
expect(Marker.decode(elsewhere) == nil, "sampling where the marker isn't (another display's frame) is unreadable")

var stats = MarkerStats(display: id)
for counter in [65533, 65534, 65535, 0, 1, 2] as [UInt16] { stats.add((0x5678, counter)) }
expect(stats.proves && stats.changes == 5 && stats.backwards == 0, "counters that advance (and wrap) prove the content: \(stats.summary)")
var repeated = MarkerStats(display: id)
for counter in [1, 1, 2, 2, 3] as [UInt16] { repeated.add((0x5678, counter)) }
expect(repeated.proves && repeated.changes == 2, "repeated frames are valid but are not changes")
var wrong = stats
wrong.add((0x9999, 3))
expect(!wrong.proves && wrong.wrongDisplay == 1, "a frame of another display's marker fails")
var unreadable = stats
unreadable.add(nil)
expect(!unreadable.proves, "an unreadable frame fails")
var back = stats
back.add((0x5678, 1))
expect(!back.proves && back.backwards == 1, "a counter that goes back fails")
expect(!MarkerStats(display: id).proves, "no frames proves nothing")

// MARK: DecodedMatch

func markers(_ counters: [Int], display: UInt16 = 0x5678) -> [(display: UInt16, counter: UInt16)?] {
    counters.map { (display, UInt16(truncatingIfNeeded: $0)) }
}
let sent = markers(Array(0..<600))
expect(DecodedMatch(submitted: sent, decoded: sent, display: id).passes, "every submitted frame decoded, in order: passes")
let frozen = DecodedMatch(submitted: sent, decoded: markers(Array(repeating: 0, count: 600)), display: id)
expect(!frozen.passes && frozen.outOfSequence == 599, "a frozen decoder (600 identical markers) fails: \(frozen.summary)")
var swappedOrder = Array(0..<600)
swappedOrder.swapAt(100, 101)
expect(!DecodedMatch(submitted: sent, decoded: markers(swappedOrder), display: id).passes, "a reordering decoder fails")
let twoLost = DecodedMatch(submitted: sent, decoded: markers((0..<600).filter { $0 != 10 && $0 != 300 }), display: id)
expect(twoLost.passes && twoLost.missing == 2, "two frames lost of 600 is within the stated tolerance (\(twoLost.allowedMissing))")
let manyLost = DecodedMatch(submitted: sent, decoded: markers((0..<600).filter { $0 % 50 != 0 }), display: id)
expect(!manyLost.passes, "12 frames lost of 600 (2%) fails")
expect(!DecodedMatch(submitted: sent, decoded: sent + [nil], display: id).passes, "an unreadable decoded frame fails")
expect(!DecodedMatch(submitted: sent, decoded: markers(Array(0..<600), display: 0x9999), display: id).passes, "frames of another display fail")
expect(!DecodedMatch(submitted: sent, decoded: [], display: id).passes, "nothing decoded fails")
expect(!DecodedMatch(submitted: sent, decoded: markers(Array(0..<600) + [700]), display: id).passes, "a frame that was never submitted fails")

print("\(checks - failures)/\(checks) passed, failures=\(failures)")
exit(failures == 0 ? 0 : 1)
