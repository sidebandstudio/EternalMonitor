//! Input injection on its own thread.
//!
//! Windows' input calls (`SendInput`, `InjectSyntheticPointerInput`,
//! `GetSystemMetrics`, creating the pen device) can wait while Windows
//! reconfigures its displays. They ran on the transport loop, which then
//! answered nobody: no heartbeats, no HELLO answers, and a stopped stream
//! kept its port. Now the transport only queues input, and this thread
//! hands it to Windows in order.
//!
//! The queue is bounded. While Windows takes nothing, a pointer or Pencil
//! move replaces the move queued just before it, so a backlog never replays
//! a path the iPad already left. Input that waited longer than
//! [`INPUT_LATE`] goes to the relay as late, which lands only a release of
//! what the PC holds, as for input that arrived late from the iPad. Ends,
//! key releases and session resets are never dropped or merged.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::InputEvent;
use parking_lot::{Condvar, Mutex};
use tracing::{info, warn};

use super::{
    CaptureGeometry, InputRelay, KIND_HOVER, KIND_MOUSE_ABS, KIND_PENCIL, KIND_TOUCH, PHASE_MOVED,
};
use crate::transport::session::INPUT_LATE;

/// Commands kept at most. A second of a Pencil at 240 Hz with every other
/// kind of input; while Windows is stuck, moves merge and the queue grows
/// only by presses and releases.
const CAPACITY: usize = 1024;

/// From this many waiting commands, a move replaces the move before it.
/// Below it every sample goes to Windows, as the Pencil's ink needs.
const MERGE_FROM: usize = 32;

/// How often a native pen contact is refreshed; Windows expires a still one.
const PEN_TICK: Duration = Duration::from_millis(50);

/// A Windows call this slow is logged: it would have stalled the stream.
const SLOW_CALL: Duration = Duration::from_millis(250);

/// Waits this short are timed precisely, for pen frames held back to keep
/// them `FRAME_SPACING` apart; a condition variable wakes in whole
/// milliseconds or worse on Windows.
const PRECISE_WAIT: Duration = Duration::from_millis(2);

/// How long the transport waits at its start for the thread to say whether
/// Windows gave it a pen device.
const START_WAIT: Duration = Duration::from_secs(1);

/// What the injection thread drives: the input relay, or a test's fake.
pub(crate) trait Sink {
    fn relay(&mut self, session_id: u32, event: &InputEvent, output: CaptureGeometry, late: bool);
    fn reset(&mut self);
    fn keep_alive(&mut self, output: Option<CaptureGeometry>);
    fn pen_available(&self) -> bool;
    fn spaced_until(&self) -> Option<Instant>;
    fn tick_needed(&self) -> bool;
}

impl Sink for InputRelay {
    fn relay(&mut self, session_id: u32, event: &InputEvent, output: CaptureGeometry, late: bool) {
        if late {
            self.relay_late(session_id, event, output);
        } else {
            InputRelay::relay(self, session_id, event, output);
        }
    }
    fn reset(&mut self) {
        InputRelay::reset(self);
    }
    fn keep_alive(&mut self, output: Option<CaptureGeometry>) {
        self.keep_pen_alive(output);
    }
    fn pen_available(&self) -> bool {
        InputRelay::pen_available(self)
    }
    fn spaced_until(&self) -> Option<Instant> {
        self.pen_spaced_until()
    }
    fn tick_needed(&self) -> bool {
        self.pen_tick_needed()
    }
}

#[derive(Debug, Clone, Copy)]
enum Command {
    Relay {
        session_id: u32,
        event: InputEvent,
        output: CaptureGeometry,
        late: bool,
    },
    /// Release what the PC holds: the session ended, expired or is on hold.
    Reset,
}

impl Command {
    /// A pointer or Pencil move or hover, which a later one can stand in for.
    fn merges_with(&self, earlier: &Command) -> bool {
        let (
            Command::Relay {
                session_id,
                event,
                late,
                ..
            },
            Command::Relay {
                session_id: earlier_session,
                event: earlier_event,
                late: earlier_late,
                ..
            },
        ) = (self, earlier)
        else {
            return false;
        };
        let moves = |event: &InputEvent| {
            event.phase == PHASE_MOVED
                && matches!(
                    event.kind,
                    KIND_TOUCH | KIND_MOUSE_ABS | KIND_HOVER | KIND_PENCIL
                )
        };
        session_id == earlier_session
            && late == earlier_late
            && moves(event)
            && moves(earlier_event)
            && event.kind == earlier_event.kind
            && event.input_ver == earlier_event.input_ver
            && event.buttons == earlier_event.buttons
    }

    /// An end or reset: what keeps the PC from holding a key or button.
    fn must_land(&self) -> bool {
        match self {
            Command::Reset => true,
            Command::Relay { event, .. } => event.phase > PHASE_MOVED,
        }
    }
}

#[derive(Debug)]
struct Queued {
    command: Command,
    at: Instant,
}

#[derive(Debug, Default)]
struct State {
    commands: VecDeque<Queued>,
    closed: bool,
    merged: u64,
    dropped: u64,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    capacity: usize,
    merge_from: usize,
    stale_after: Duration,
}

impl Shared {
    fn push(&self, command: Command, now: Instant) {
        let mut state = self.state.lock();
        let waiting = state.commands.len();
        if waiting >= self.merge_from {
            if let Some(last) = state.commands.back_mut() {
                if command.merges_with(&last.command) {
                    // The newer sample, in the older one's place: it has
                    // waited as long.
                    last.command = command;
                    state.merged += 1;
                    return;
                }
            }
        }
        // Ends and resets may pass the bound, by as many as were pressed.
        if waiting >= self.capacity && !(command.must_land() && waiting < 2 * self.capacity) {
            state.dropped += 1;
            return;
        }
        state.commands.push_back(Queued { command, at: now });
        drop(state);
        self.wake.notify_one();
    }
}

/// The transport's handle on the injection thread. Dropping it lets the
/// thread hand over what is queued and release what the PC holds, without
/// waiting for Windows.
#[derive(Debug)]
pub struct Injector {
    shared: Arc<Shared>,
    pen_available: bool,
}

impl Injector {
    /// Starts the thread with a new relay. `geometry` is the capture rect a
    /// still Pencil follows.
    pub fn start(geometry: Arc<Mutex<Option<CaptureGeometry>>>) -> Self {
        #[cfg(all(test, not(windows)))]
        let recorder = super::recorder::current();
        Self::spawn(geometry, Limits::default(), move || {
            #[cfg(all(test, not(windows)))]
            super::recorder::adopt(recorder);
            InputRelay::default()
        })
    }

    fn spawn<S: Sink>(
        geometry: Arc<Mutex<Option<CaptureGeometry>>>,
        limits: Limits,
        sink: impl FnOnce() -> S + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            capacity: limits.capacity,
            merge_from: limits.merge_from,
            stale_after: limits.stale_after,
        });
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("input-inject".into())
            .spawn(move || {
                let mut sink = sink();
                let _ = ready_tx.send(sink.pen_available());
                run(&worker, &mut sink, &geometry);
            });
        let pen_available = match spawned {
            Ok(_) => ready.recv_timeout(START_WAIT).unwrap_or_else(|_| {
                warn!(
                    wait_ms = START_WAIT.as_millis() as u64,
                    "Windows did not set up input in time; the Pencil uses the mouse this stream"
                );
                false
            }),
            Err(error) => {
                warn!(%error, "Could not start the input thread; input is ignored this stream");
                shared.state.lock().closed = true;
                false
            }
        };
        Self {
            shared,
            pen_available,
        }
    }

    /// Whether the relay has a native pen device, for the HELLO answer.
    pub fn pen_available(&self) -> bool {
        self.pen_available
    }

    /// Queue one session-validated event. Never waits for Windows.
    pub fn relay(&self, session_id: u32, event: InputEvent, output: CaptureGeometry, late: bool) {
        self.shared.push(
            Command::Relay {
                session_id,
                event,
                output,
                late,
            },
            Instant::now(),
        );
    }

    /// Queue a release of every key, button and pen contact the PC holds.
    pub fn reset(&self) {
        self.shared.push(Command::Reset, Instant::now());
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.shared.state.lock().closed = true;
        self.shared.wake.notify_one();
    }
}

#[derive(Debug, Clone, Copy)]
struct Limits {
    capacity: usize,
    merge_from: usize,
    stale_after: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            capacity: CAPACITY,
            merge_from: MERGE_FROM,
            stale_after: INPUT_LATE,
        }
    }
}

enum Next {
    Command(Queued),
    /// A pen frame's spacing or the refresh tick is due.
    Due,
    Closed,
}

/// Input that waited for Windows, counted until the queue empties.
#[derive(Debug, Default)]
struct Backlog {
    stale: u64,
    longest: Duration,
}

fn run<S: Sink>(shared: &Shared, sink: &mut S, geometry: &Mutex<Option<CaptureGeometry>>) {
    let mut timer = crate::capture::timing::FrameTimer::new();
    let mut next_tick = Instant::now() + PEN_TICK;
    let mut backlog = Backlog::default();
    loop {
        let next = {
            let mut state = shared.state.lock();
            loop {
                if let Some(queued) = state.commands.pop_front() {
                    break Next::Command(queued);
                }
                if state.closed {
                    break Next::Closed;
                }
                let (merged, dropped) = (
                    std::mem::take(&mut state.merged),
                    std::mem::take(&mut state.dropped),
                );
                if backlog.stale > 0 || merged > 0 || dropped > 0 {
                    info!(
                        stale = backlog.stale,
                        merged,
                        dropped,
                        longest_ms = backlog.longest.as_millis() as u64,
                        "Input waited for Windows: moves merged, and only releases sent of what waited too long"
                    );
                    backlog = Backlog::default();
                }
                let now = Instant::now();
                let spaced = sink.spaced_until();
                let due = spaced.or_else(|| sink.tick_needed().then_some(next_tick));
                match due {
                    Some(at) if at <= now => break Next::Due,
                    Some(at) if spaced.is_some() && at - now <= PRECISE_WAIT => {
                        drop(state);
                        timer.wait_until(at);
                        state = shared.state.lock();
                        if state.commands.is_empty() {
                            break Next::Due;
                        }
                    }
                    Some(at) => {
                        if shared.wake.wait_until(&mut state, at).timed_out() {
                            break Next::Due;
                        }
                    }
                    None => shared.wake.wait(&mut state),
                }
            }
        };
        match next {
            Next::Command(Queued { command, at }) => {
                let started = Instant::now();
                let waited = started.saturating_duration_since(at);
                backlog.longest = backlog.longest.max(waited);
                match command {
                    Command::Relay {
                        session_id,
                        event,
                        output,
                        late,
                    } => {
                        let stale = waited > shared.stale_after;
                        backlog.stale += u64::from(stale && !late);
                        sink.relay(session_id, &event, output, late || stale);
                    }
                    Command::Reset => sink.reset(),
                }
                note_slow(started, "input");
            }
            // The relay goes with the thread, releasing what the PC holds.
            Next::Closed => return,
            Next::Due => {}
        }
        // Pen frames held back for their spacing, and the refresh a still
        // contact needs, go between commands.
        let now = Instant::now();
        let spaced_due = sink.spaced_until().is_some_and(|at| at <= now);
        let tick_due = sink.spaced_until().is_none() && sink.tick_needed() && next_tick <= now;
        if spaced_due || tick_due {
            if tick_due {
                next_tick = now + PEN_TICK;
            }
            let started = Instant::now();
            sink.keep_alive(*geometry.lock());
            note_slow(started, "pen_refresh");
        }
    }
}

fn note_slow(started: Instant, what: &'static str) {
    let took = started.elapsed();
    if took >= SLOW_CALL {
        warn!(
            what,
            took_ms = took.as_millis() as u64,
            "Windows took a long time to take input"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KIND_KEY, PHASE_BEGAN, PHASE_ENDED};
    use std::sync::mpsc;

    const OUTPUT: CaptureGeometry = CaptureGeometry {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Relay {
            kind: u8,
            phase: u8,
            id: u32,
            late: bool,
        },
        Reset,
        KeepAlive,
    }

    /// Records calls, and can hold the first relay until told to go on,
    /// as Windows does while it reconfigures displays.
    struct Fake {
        calls: Arc<Mutex<Vec<Call>>>,
        hold: Option<mpsc::Receiver<()>>,
        entered: Option<mpsc::Sender<()>>,
    }

    impl Sink for Fake {
        fn relay(&mut self, _: u32, event: &InputEvent, _: CaptureGeometry, late: bool) {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            if let Some(hold) = self.hold.take() {
                let _ = hold.recv();
            }
            self.calls.lock().push(Call::Relay {
                kind: event.kind,
                phase: event.phase,
                id: event.event_id,
                late,
            });
        }
        fn reset(&mut self) {
            self.calls.lock().push(Call::Reset);
        }
        fn keep_alive(&mut self, _: Option<CaptureGeometry>) {
            self.calls.lock().push(Call::KeepAlive);
        }
        fn pen_available(&self) -> bool {
            true
        }
        fn spaced_until(&self) -> Option<Instant> {
            None
        }
        fn tick_needed(&self) -> bool {
            false
        }
    }

    struct Harness {
        injector: Option<Injector>,
        calls: Arc<Mutex<Vec<Call>>>,
        go: mpsc::Sender<()>,
        entered: mpsc::Receiver<()>,
    }

    fn harness(limits: Limits) -> Harness {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (go, hold) = mpsc::channel();
        let (entered_tx, entered) = mpsc::channel();
        let recorded = Arc::clone(&calls);
        let injector = Injector::spawn(Arc::new(Mutex::new(Some(OUTPUT))), limits, move || Fake {
            calls: recorded,
            hold: Some(hold),
            entered: Some(entered_tx),
        });
        Harness {
            injector: Some(injector),
            calls,
            go,
            entered,
        }
    }

    impl Harness {
        fn send(&self, kind: u8, phase: u8, id: u32) {
            self.injector
                .as_ref()
                .unwrap()
                .relay(1, event(kind, phase, id), OUTPUT, false);
        }

        /// Holds Windows in the first event's call until `go`.
        fn stuck(&self) {
            self.send(KIND_KEY, PHASE_BEGAN, 0);
            self.entered
                .recv_timeout(Duration::from_secs(5))
                .expect("the thread takes the first event");
        }

        fn wait_for(&self, count: usize) -> Vec<Call> {
            let started = Instant::now();
            while self.calls.lock().len() < count {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "{:?}",
                    self.calls.lock()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            self.calls.lock().clone()
        }
    }

    fn event(kind: u8, phase: u8, id: u32) -> InputEvent {
        InputEvent {
            input_ver: 1,
            kind,
            phase,
            buttons: 0,
            event_id: id,
            x_norm: id as u16,
            y_norm: 0,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: 0x04,
            modifiers: 0,
            client_time_us: 0,
            tilt_x: 0,
            tilt_y: 0,
        }
    }

    fn relayed(kind: u8, phase: u8, id: u32) -> Call {
        Call::Relay {
            kind,
            phase,
            id,
            late: false,
        }
    }

    #[test]
    fn queueing_never_waits_for_a_stuck_windows() {
        let harness = harness(Limits::default());
        harness.stuck();
        let started = Instant::now();
        for id in 1..=5000 {
            harness.send(KIND_MOUSE_ABS, PHASE_MOVED, id);
        }
        harness.injector.as_ref().unwrap().reset();
        assert!(started.elapsed() < Duration::from_millis(500));
        harness.go.send(()).unwrap();
    }

    #[test]
    fn every_command_reaches_windows_in_the_order_it_came() {
        let harness = harness(Limits::default());
        harness.stuck();
        let sent = [
            (KIND_MOUSE_ABS, PHASE_BEGAN, 1),
            (KIND_MOUSE_ABS, PHASE_MOVED, 2),
            (KIND_PENCIL, PHASE_MOVED, 3),
            (KIND_MOUSE_ABS, PHASE_MOVED, 4),
            (KIND_KEY, PHASE_ENDED, 5),
            (KIND_MOUSE_ABS, PHASE_ENDED, 6),
        ];
        for (kind, phase, id) in sent {
            harness.send(kind, phase, id);
        }
        harness.injector.as_ref().unwrap().reset();
        harness.send(KIND_KEY, PHASE_BEGAN, 7);
        harness.go.send(()).unwrap();
        let mut expected = vec![relayed(KIND_KEY, PHASE_BEGAN, 0)];
        expected.extend(sent.map(|(kind, phase, id)| relayed(kind, phase, id)));
        expected.push(Call::Reset);
        expected.push(relayed(KIND_KEY, PHASE_BEGAN, 7));
        assert_eq!(harness.wait_for(expected.len()), expected);
    }

    #[test]
    fn a_backed_up_queue_merges_moves_but_keeps_every_edge_in_place() {
        let harness = harness(Limits {
            merge_from: 4,
            ..Limits::default()
        });
        harness.stuck();
        // Four wait as they came; then each move stands in for the one
        // before it, up to the next edge.
        let mut id = 0;
        for _ in 0..4 {
            id += 1;
            harness.send(KIND_HOVER, PHASE_MOVED, id);
        }
        for _ in 0..50 {
            id += 1;
            harness.send(KIND_HOVER, PHASE_MOVED, id);
        }
        harness.send(KIND_MOUSE_ABS, PHASE_BEGAN, 100);
        for move_id in 101..150 {
            harness.send(KIND_MOUSE_ABS, PHASE_MOVED, move_id);
        }
        // A Pencil move never stands in for a mouse move.
        harness.send(KIND_PENCIL, PHASE_MOVED, 150);
        harness.send(KIND_MOUSE_ABS, PHASE_ENDED, 151);
        harness.injector.as_ref().unwrap().reset();
        harness.go.send(()).unwrap();
        let expected = vec![
            relayed(KIND_KEY, PHASE_BEGAN, 0),
            relayed(KIND_HOVER, PHASE_MOVED, 1),
            relayed(KIND_HOVER, PHASE_MOVED, 2),
            relayed(KIND_HOVER, PHASE_MOVED, 3),
            relayed(KIND_HOVER, PHASE_MOVED, 54),
            relayed(KIND_MOUSE_ABS, PHASE_BEGAN, 100),
            relayed(KIND_MOUSE_ABS, PHASE_MOVED, 149),
            relayed(KIND_PENCIL, PHASE_MOVED, 150),
            relayed(KIND_MOUSE_ABS, PHASE_ENDED, 151),
            Call::Reset,
        ];
        assert_eq!(harness.wait_for(expected.len()), expected);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(harness.calls.lock().len(), expected.len());
    }

    #[test]
    fn input_that_waited_too_long_goes_as_late_and_releases_still_land() {
        let harness = harness(Limits {
            stale_after: Duration::from_millis(50),
            ..Limits::default()
        });
        harness.stuck();
        harness.send(KIND_KEY, PHASE_ENDED, 1);
        harness.send(KIND_MOUSE_ABS, PHASE_BEGAN, 2);
        std::thread::sleep(Duration::from_millis(100));
        harness.go.send(()).unwrap();
        // Fresh input after Windows came back is not late.
        harness.wait_for(3);
        harness.send(KIND_MOUSE_ABS, PHASE_ENDED, 3);
        let late = |kind, phase, id| Call::Relay {
            kind,
            phase,
            id,
            late: true,
        };
        assert_eq!(
            harness.wait_for(4),
            [
                relayed(KIND_KEY, PHASE_BEGAN, 0),
                late(KIND_KEY, PHASE_ENDED, 1),
                late(KIND_MOUSE_ABS, PHASE_BEGAN, 2),
                relayed(KIND_MOUSE_ABS, PHASE_ENDED, 3),
            ]
        );
    }

    #[test]
    fn a_full_queue_drops_new_moves_and_presses_but_keeps_releases() {
        let harness = harness(Limits {
            capacity: 3,
            merge_from: 100,
            stale_after: INPUT_LATE,
        });
        harness.stuck();
        for id in 1..=3 {
            harness.send(KIND_KEY, PHASE_BEGAN, id);
        }
        harness.send(KIND_KEY, PHASE_BEGAN, 4);
        harness.send(KIND_MOUSE_ABS, PHASE_MOVED, 5);
        harness.send(KIND_KEY, PHASE_ENDED, 6);
        harness.injector.as_ref().unwrap().reset();
        harness.go.send(()).unwrap();
        assert_eq!(
            harness.wait_for(6),
            [
                relayed(KIND_KEY, PHASE_BEGAN, 0),
                relayed(KIND_KEY, PHASE_BEGAN, 1),
                relayed(KIND_KEY, PHASE_BEGAN, 2),
                relayed(KIND_KEY, PHASE_BEGAN, 3),
                relayed(KIND_KEY, PHASE_ENDED, 6),
                Call::Reset,
            ]
        );
    }

    #[test]
    fn a_dropped_handle_returns_at_once_and_its_input_still_lands() {
        let mut harness = harness(Limits::default());
        harness.stuck();
        harness.send(KIND_KEY, PHASE_ENDED, 1);
        let started = Instant::now();
        drop(harness.injector.take());
        assert!(started.elapsed() < Duration::from_millis(100));
        harness.go.send(()).unwrap();
        assert_eq!(
            harness.wait_for(2),
            [
                relayed(KIND_KEY, PHASE_BEGAN, 0),
                relayed(KIND_KEY, PHASE_ENDED, 1),
            ]
        );
    }

    /// The real relay, with a native pen device that takes every frame
    /// (Windows' own off Windows). A stall delivers 64 samples at once; the
    /// spacing holds back all but the first burst, and the thread sends the
    /// rest within a few times the spacing, not at the 50 ms refresh.
    #[cfg(not(windows))]
    #[test]
    fn the_thread_sends_a_held_pen_backlog_at_its_spacing() {
        use crate::input::{native_pen, PenPhase, PenSample};
        const SAMPLES: usize = 64;
        let (taken_tx, taken_rx) = mpsc::channel();
        let delivered = Arc::new(Mutex::new(None));
        let at = Arc::clone(&delivered);
        let injector = Injector::spawn(
            Arc::new(Mutex::new(Some(OUTPUT))),
            Limits::default(),
            move || {
                let taken = native_pen::install();
                *at.lock() = Some(Instant::now());
                native_pen::with(|device| {
                    for x in 0..SAMPLES as u16 {
                        let sample = PenSample {
                            phase: if x == 0 {
                                PenPhase::Down
                            } else {
                                PenPhase::Move
                            },
                            x: i32::from(x),
                            y: 10,
                            x_norm: x,
                            y_norm: 10,
                            pressure: 512,
                            tilt_x: 0,
                            tilt_y: 0,
                        };
                        device.deliver(sample, native_pen::SCREEN, Some(OUTPUT), Instant::now());
                    }
                });
                assert!(
                    taken.lock().len() < SAMPLES / 2,
                    "the spacing must hold the backlog back"
                );
                taken_tx.send(taken).unwrap();
                InputRelay::default()
            },
        );
        let taken = taken_rx.recv().unwrap();
        let started = Instant::now();
        while taken.lock().len() < SAMPLES && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(injector);
        let taken = taken.lock();
        let order: Vec<_> = taken.iter().map(|(frame, _)| frame.x_norm).collect();
        assert_eq!(order, (0..SAMPLES as u16).collect::<Vec<_>>());
        let drained = taken.last().unwrap().1 - delivered.lock().unwrap();
        // 58 held frames 250 µs apart take 15 ms; the 50 ms tick alone
        // would take 500 ms.
        assert!(drained < Duration::from_millis(250), "{drained:?}");
    }
}
