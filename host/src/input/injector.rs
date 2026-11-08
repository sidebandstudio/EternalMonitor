//! Input injection on its own thread.
//!
//! Windows' input calls (`SendInput`, `InjectSyntheticPointerInput`,
//! `GetSystemMetrics`, creating the pen device) can wait while Windows
//! reconfigures its displays. They ran on the transport loop, which then
//! answered nobody: no heartbeats, no HELLO answers, and a stopped stream
//! kept its port. Now the transport only queues input, and one thread for
//! the whole app hands it to Windows in order.
//!
//! Each stream generation queues on the same thread, behind the one before
//! it. A generation's end releases what its input still holds before the
//! next one's input lands, however long Windows kept the thread waiting,
//! and a restart never starts a second thread that could inject alongside
//! a stuck one.
//!
//! The queue is bounded. While Windows takes nothing, a pointer or Pencil
//! move replaces the newest queued move of its kind that no press or
//! release follows, so a backlog never replays a path the iPad already
//! left. Input that waited longer than
//! [`INPUT_LATE`] goes to the relay as late, which lands only a release of
//! what the PC holds, as for input that arrived late from the iPad. Ends,
//! key releases and session resets are never merged, and never lost: at
//! the bound, one becomes a release of everything after the last command
//! queued.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eternal_wire::v2::control::InputEvent;
use parking_lot::{Condvar, Mutex};
use tracing::{info, warn};

use super::{
    CaptureGeometry, InputRelay, KIND_HOVER, KIND_MOUSE_ABS, KIND_PENCIL, KIND_TOUCH, PHASE_MOVED,
};
use crate::transport::session::INPUT_LATE;

/// Commands kept at most, besides one start and one end per stream
/// generation. A second of a Pencil at 240 Hz with every other kind of
/// input. While Windows is stuck, each kind of move keeps only its newest
/// after the last press or release, so the queue grows by presses and
/// releases.
const CAPACITY: usize = 1024;

/// From this many waiting commands, a move replaces the newest move of its
/// kind. Below it every sample goes to Windows, as the Pencil's ink needs.
const MERGE_FROM: usize = 32;

/// How often a native pen contact is refreshed; Windows expires a still one.
const PEN_TICK: Duration = Duration::from_millis(50);

/// A Windows call this slow is logged: it would have stalled the stream.
const SLOW_CALL: Duration = Duration::from_millis(250);

/// A Windows call held this long is logged while it is still held, from
/// the transport's next input: one that never returns would log nothing.
const HELD_LOGGED: Duration = Duration::from_secs(5);

/// Waits this short are timed precisely, for pen frames held back to keep
/// them `FRAME_SPACING` apart; a condition variable wakes in whole
/// milliseconds or worse on Windows.
const PRECISE_WAIT: Duration = Duration::from_millis(2);

/// How long the first stream waits for the thread to say whether Windows
/// gave it a pen device. Later streams know at once.
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

type Geometry = Arc<Mutex<Option<CaptureGeometry>>>;

#[derive(Debug, Clone)]
enum Command {
    /// A stream generation starts: what the one before it holds is
    /// released first. `geometry` is the capture rect a still Pencil
    /// follows.
    Begin { geometry: Geometry },
    Relay {
        session_id: u32,
        event: InputEvent,
        output: CaptureGeometry,
        late: bool,
    },
    /// Release what the PC holds: the session ended, expired or is on hold.
    Reset,
    /// The generation's stream stopped: release what it holds.
    End,
    /// The app quits: release what the PC holds, whichever stream it is
    /// from, and take no more input.
    ReleaseAll,
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

    /// A press, release, reset or generation change: what a move must not
    /// pass.
    fn is_edge(&self) -> bool {
        match self {
            Command::Relay { event, .. } => event.phase != PHASE_MOVED,
            Command::Begin { .. } | Command::Reset | Command::End | Command::ReleaseAll => true,
        }
    }

    /// An end or reset: what keeps the PC from holding a key or button.
    fn releases(&self) -> bool {
        match self {
            Command::Reset | Command::End | Command::ReleaseAll => true,
            Command::Begin { .. } => false,
            Command::Relay { event, .. } => event.phase > PHASE_MOVED,
        }
    }
}

#[derive(Debug)]
struct Queued {
    generation: u64,
    command: Command,
    at: Instant,
    /// A release came while the queue was full: once this command has
    /// landed, release everything its generation holds.
    then_reset: bool,
}

#[derive(Debug, Default)]
struct State {
    commands: VecDeque<Queued>,
    /// Since when the thread has been in Windows, with a command or a pen
    /// refresh.
    in_windows_since: Option<Instant>,
    /// That call was logged as held.
    held_logged: bool,
    #[cfg(test)]
    held_logs: u64,
    merged: u64,
    dropped: u64,
    /// Releases that came while the queue was full, each now part of a
    /// release of everything.
    folded: u64,
    /// Input events queued since the thread started, for tests that must
    /// know one is behind another.
    #[cfg(any(test, feature = "test-hooks"))]
    relays_queued: u64,
}

impl State {
    fn leave_windows(&mut self) {
        self.in_windows_since = None;
        self.held_logged = false;
    }
}

/// What the transport's handles and the thread share.
#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// The queue is empty and the thread waits: for `InputThread::finish`.
    idle: Condvar,
    limits: Limits,
    next_generation: AtomicU64,
    /// Whether the relay has a native pen device: 0 not known yet, 1 no,
    /// 2 yes.
    pen: AtomicU8,
}

impl Shared {
    fn push(&self, generation: u64, command: Command, now: Instant) {
        let mut state = self.state.lock();
        let waiting = state.commands.len();
        if let Some(since) = state.in_windows_since {
            let held = now.saturating_duration_since(since);
            if held >= HELD_LOGGED && !state.held_logged {
                state.held_logged = true;
                #[cfg(test)]
                {
                    state.held_logs += 1;
                }
                warn!(
                    held_s = held.as_secs(),
                    waiting, "Windows has held input for seconds and has not returned"
                );
            }
        }
        if waiting >= self.limits.merge_from {
            // The newest move of this kind, unless a press or release, or
            // another generation, came after it.
            let merges = state
                .commands
                .iter()
                .enumerate()
                .rev()
                .take_while(|(_, queued)| {
                    queued.generation == generation
                        && !queued.then_reset
                        && !queued.command.is_edge()
                })
                .find(|(_, queued)| command.merges_with(&queued.command))
                .map(|(index, _)| index);
            if let Some(index) = merges {
                // It goes last, where it came: the other kinds' newest
                // moves keep their order before it.
                let earlier = state.commands.remove(index).expect("found above");
                state.commands.push_back(Queued { command, ..earlier });
                state.merged += 1;
                return;
            }
        }
        // A generation's start and end always go in: there is one of each
        // per stream.
        let boundary = matches!(
            command,
            Command::Begin { .. } | Command::End | Command::ReleaseAll
        );
        if waiting >= self.limits.capacity && !boundary {
            let barrier = state
                .commands
                .back_mut()
                .filter(|last| command.releases() && last.generation == generation);
            match barrier {
                Some(last) => {
                    last.then_reset = true;
                    state.folded += 1;
                }
                None => state.dropped += 1,
            }
            return;
        }
        #[cfg(any(test, feature = "test-hooks"))]
        {
            state.relays_queued += u64::from(matches!(command, Command::Relay { .. }));
        }
        state.commands.push_back(Queued {
            generation,
            command,
            at: now,
            then_reset: false,
        });
        drop(state);
        self.wake.notify_one();
    }
}

/// The app's one input thread, started with the first stream that takes
/// input. `SharedControl` keeps it across stream generations.
#[derive(Default)]
pub struct InputThread {
    shared: OnceLock<Arc<Shared>>,
}

impl std::fmt::Debug for InputThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputThread")
            .field("started", &self.shared.get().is_some())
            .finish()
    }
}

impl InputThread {
    /// A new stream generation's handle; starts the thread with the relay
    /// the first time.
    pub fn begin(&self, geometry: Geometry) -> Injector {
        let shared = self.shared.get_or_init(|| {
            #[cfg(all(test, not(windows)))]
            let recorder = super::recorder::current();
            spawn(Limits::default(), move || {
                #[cfg(all(test, not(windows)))]
                super::recorder::adopt(recorder);
                InputRelay::default()
            })
        });
        Injector::begin(Arc::clone(shared), geometry)
    }

    /// A thread driving a test's fake instead of Windows.
    #[cfg(test)]
    pub(crate) fn with_fake<S: Sink>(sink: impl FnOnce() -> S + Send + 'static) -> Self {
        Self::with_sink(Limits::default(), sink)
    }

    #[cfg(test)]
    fn with_sink<S: Sink>(limits: Limits, sink: impl FnOnce() -> S + Send + 'static) -> Self {
        let thread = Self::default();
        let _ = thread.shared.set(spawn(limits, sink));
        thread
    }

    /// Input events queued so far, merged and dropped ones aside.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn relays_queued(&self) -> u64 {
        self.shared
            .get()
            .map_or(0, |shared| shared.state.lock().relays_queued)
    }

    /// At quit: release what the PC holds, after everything queued, and
    /// wait up to `limit` for it to reach Windows. Whether it did. A stream
    /// still running need not have queued its end.
    pub fn finish(&self, limit: Duration) -> bool {
        let Some(shared) = self.shared.get() else {
            return true;
        };
        shared.push(0, Command::ReleaseAll, Instant::now());
        let done = self.idle(limit);
        if !done {
            warn!(
                waiting = shared.state.lock().commands.len(),
                "Quitting before Windows took the last input"
            );
        }
        done
    }

    /// Wait up to `limit` for everything queued to reach Windows, without
    /// releasing anything.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn wait_idle(&self, limit: Duration) -> bool {
        self.idle(limit)
    }

    fn idle(&self, limit: Duration) -> bool {
        let Some(shared) = self.shared.get() else {
            return true;
        };
        let deadline = Instant::now() + limit;
        let mut state = shared.state.lock();
        while state.in_windows_since.is_some() || !state.commands.is_empty() {
            if shared.idle.wait_until(&mut state, deadline).timed_out() {
                return false;
            }
        }
        true
    }
}

fn spawn<S: Sink>(limits: Limits, sink: impl FnOnce() -> S + Send + 'static) -> Arc<Shared> {
    let shared = Arc::new(Shared {
        state: Mutex::new(State::default()),
        wake: Condvar::new(),
        idle: Condvar::new(),
        limits,
        next_generation: AtomicU64::new(1),
        pen: AtomicU8::new(0),
    });
    let worker = Arc::clone(&shared);
    let spawned = std::thread::Builder::new()
        .name("input-inject".into())
        .spawn(move || {
            let mut sink = sink();
            note_pen(&worker, &sink);
            run(&worker, &mut sink);
        });
    if let Err(error) = spawned {
        warn!(%error, "Could not start the input thread; input is ignored");
        shared.pen.store(1, Ordering::SeqCst);
    }
    shared
}

fn note_pen(shared: &Shared, sink: &impl Sink) {
    let pen = if sink.pen_available() { 2 } else { 1 };
    shared.pen.store(pen, Ordering::SeqCst);
}

/// One stream generation's handle on the input thread. Dropping it
/// releases what the generation's input holds, in turn, without waiting
/// for Windows.
#[derive(Debug)]
pub struct Injector {
    shared: Arc<Shared>,
    generation: u64,
    pen_available: bool,
}

impl Injector {
    fn begin(shared: Arc<Shared>, geometry: Geometry) -> Self {
        let generation = shared.next_generation.fetch_add(1, Ordering::SeqCst);
        shared.push(generation, Command::Begin { geometry }, Instant::now());
        let started = Instant::now();
        let pen_available = loop {
            match shared.pen.load(Ordering::SeqCst) {
                0 if started.elapsed() < START_WAIT => std::thread::sleep(Duration::from_millis(1)),
                0 => {
                    warn!(
                        wait_ms = START_WAIT.as_millis() as u64,
                        "Windows did not set up input in time; the Pencil uses the mouse this stream"
                    );
                    break false;
                }
                pen => break pen == 2,
            }
        };
        Self {
            shared,
            generation,
            pen_available,
        }
    }

    /// Whether the relay has a native pen device, for the HELLO answer.
    pub fn pen_available(&self) -> bool {
        self.pen_available
    }

    /// Queue one session-validated event. Never waits for Windows.
    pub fn relay(&self, session_id: u32, event: InputEvent, output: CaptureGeometry, late: bool) {
        self.push(Command::Relay {
            session_id,
            event,
            output,
            late,
        });
    }

    /// Queue a release of every key, button and pen contact the PC holds.
    pub fn reset(&self) {
        self.push(Command::Reset);
    }

    fn push(&self, command: Command) {
        self.shared.push(self.generation, command, Instant::now());
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.push(Command::End);
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
}

/// Input that waited for Windows, counted until the queue empties.
#[derive(Debug, Default)]
struct Backlog {
    stale: u64,
    longest: Duration,
}

/// The generation whose input the thread takes now.
struct Current {
    generation: u64,
    geometry: Geometry,
    /// Its input reached the relay, so its end has something to release.
    relayed: bool,
}

fn run<S: Sink>(shared: &Shared, sink: &mut S) {
    let mut timer = crate::capture::timing::FrameTimer::new();
    let mut next_tick = Instant::now() + PEN_TICK;
    let mut backlog = Backlog::default();
    let mut current: Option<Current> = None;
    loop {
        let next = {
            let mut state = shared.state.lock();
            state.leave_windows();
            loop {
                if let Some(queued) = state.commands.pop_front() {
                    state.in_windows_since = Some(Instant::now());
                    break Next::Command(queued);
                }
                shared.idle.notify_all();
                let (merged, dropped, folded) = (
                    std::mem::take(&mut state.merged),
                    std::mem::take(&mut state.dropped),
                    std::mem::take(&mut state.folded),
                );
                if backlog.stale > 0 || merged > 0 || dropped > 0 || folded > 0 {
                    info!(
                        stale = backlog.stale,
                        merged,
                        dropped,
                        folded,
                        longest_ms = backlog.longest.as_millis() as u64,
                        "Input waited for Windows: moves merged, only releases sent of what waited too long, and everything released for releases past the bound"
                    );
                    backlog = Backlog::default();
                }
                let now = Instant::now();
                let spaced = current.as_ref().and(sink.spaced_until());
                let ticks = current.is_some() && sink.tick_needed();
                let due = spaced.or_else(|| ticks.then_some(next_tick));
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
        if let Next::Command(Queued {
            generation,
            command,
            at,
            then_reset,
        }) = next
        {
            let started = Instant::now();
            let waited = started.saturating_duration_since(at);
            backlog.longest = backlog.longest.max(waited);
            let ours = current
                .as_ref()
                .is_some_and(|current| current.generation == generation);
            match command {
                Command::Begin { geometry } => {
                    // A generation that never said it ended releases what
                    // it holds before the new one's input.
                    if current.take().is_some_and(|current| current.relayed) {
                        sink.reset();
                    }
                    current = Some(Current {
                        generation,
                        geometry,
                        relayed: false,
                    });
                }
                Command::End if ours => {
                    if current.take().is_some_and(|current| current.relayed) {
                        sink.reset();
                    }
                }
                Command::Reset if ours => sink.reset(),
                Command::Relay {
                    session_id,
                    event,
                    output,
                    late,
                } if ours => {
                    let stale = waited > shared.limits.stale_after;
                    backlog.stale += u64::from(stale && !late);
                    sink.relay(session_id, &event, output, late || stale);
                    if let Some(current) = current.as_mut() {
                        current.relayed = true;
                    }
                }
                Command::ReleaseAll => {
                    current = None;
                    sink.reset();
                }
                // An ended generation's: its end released what it held.
                Command::End | Command::Reset | Command::Relay { .. } => {}
            }
            if then_reset
                && current
                    .as_ref()
                    .is_some_and(|current| current.generation == generation)
            {
                sink.reset();
            }
            note_slow(started, "input");
            note_pen(shared, sink);
        }
        // Pen frames held back for their spacing, and the refresh a still
        // contact needs, go between commands.
        let Some(current) = current.as_ref() else {
            continue;
        };
        let now = Instant::now();
        let spaced_due = sink.spaced_until().is_some_and(|at| at <= now);
        let tick_due = sink.spaced_until().is_none() && sink.tick_needed() && next_tick <= now;
        if spaced_due || tick_due {
            if tick_due {
                next_tick = now + PEN_TICK;
            }
            // Copied out first: the transport takes this lock for every
            // input datagram, and Windows can hold the refresh.
            let output = *current.geometry.lock();
            let started = Instant::now();
            shared.state.lock().in_windows_since = Some(started);
            sink.keep_alive(output);
            shared.state.lock().leave_windows();
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
        thread: InputThread,
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
        let thread = InputThread::with_sink(limits, move || Fake {
            calls: recorded,
            hold: Some(hold),
            entered: Some(entered_tx),
        });
        let injector = thread.begin(geometry());
        Harness {
            thread,
            injector: Some(injector),
            calls,
            go,
            entered,
        }
    }

    fn geometry() -> Geometry {
        Arc::new(Mutex::new(Some(OUTPUT)))
    }

    impl Harness {
        fn send(&self, kind: u8, phase: u8, id: u32) {
            self.injector
                .as_ref()
                .unwrap()
                .relay(1, event(kind, phase, id), OUTPUT, false);
        }

        /// Queued as though at `at`.
        fn send_at(&self, kind: u8, phase: u8, id: u32, at: Instant) {
            let injector = self.injector.as_ref().unwrap();
            injector.shared.push(
                injector.generation,
                Command::Relay {
                    session_id: 1,
                    event: event(kind, phase, id),
                    output: OUTPUT,
                    late: false,
                },
                at,
            );
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

        /// Nothing more reaches Windows.
        fn settled(&self, count: usize) {
            assert!(self.thread.wait_idle(Duration::from_secs(5)));
            assert_eq!(self.calls.lock().len(), count, "{:?}", self.calls.lock());
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
        harness.settled(expected.len());
    }

    /// A Pencil hovering while the pointer moves sends both kinds of move
    /// interleaved, about 250 a second. While Windows is stuck each kind
    /// keeps only its newest move after the last press or release, in the
    /// order those newest moves came.
    #[test]
    fn interleaved_moves_of_different_kinds_each_merge_up_to_the_last_edge() {
        let harness = harness(Limits {
            merge_from: 4,
            ..Limits::default()
        });
        harness.stuck();
        for id in 1..=104 {
            let kind = if id % 2 == 1 { KIND_PENCIL } else { KIND_HOVER };
            harness.send(kind, PHASE_MOVED, id);
        }
        harness.send(KIND_MOUSE_ABS, PHASE_BEGAN, 200);
        for id in 201..=204 {
            let kind = if id % 2 == 1 { KIND_PENCIL } else { KIND_HOVER };
            harness.send(kind, PHASE_MOVED, id);
        }
        harness.send(KIND_MOUSE_ABS, PHASE_ENDED, 300);
        harness.injector.as_ref().unwrap().reset();
        harness.go.send(()).unwrap();
        let expected = vec![
            relayed(KIND_KEY, PHASE_BEGAN, 0),
            relayed(KIND_PENCIL, PHASE_MOVED, 1),
            relayed(KIND_HOVER, PHASE_MOVED, 2),
            relayed(KIND_PENCIL, PHASE_MOVED, 103),
            relayed(KIND_HOVER, PHASE_MOVED, 104),
            // Moves never merge across a press or release.
            relayed(KIND_MOUSE_ABS, PHASE_BEGAN, 200),
            relayed(KIND_PENCIL, PHASE_MOVED, 203),
            relayed(KIND_HOVER, PHASE_MOVED, 204),
            relayed(KIND_MOUSE_ABS, PHASE_ENDED, 300),
            Call::Reset,
        ];
        assert!(harness.thread.wait_idle(Duration::from_secs(5)));
        assert_eq!(*harness.calls.lock(), expected);
    }

    #[test]
    fn input_that_waited_too_long_goes_as_late_and_releases_still_land() {
        // A limit no pick-up under load comes near, and input queued as if
        // long ago: no sleep decides which is late.
        let harness = harness(Limits {
            stale_after: Duration::from_secs(10),
            ..Limits::default()
        });
        harness.stuck();
        let long_ago = Instant::now() - Duration::from_secs(20);
        harness.send_at(KIND_KEY, PHASE_ENDED, 1, long_ago);
        harness.send_at(KIND_MOUSE_ABS, PHASE_BEGAN, 2, long_ago);
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
    fn a_full_queue_drops_new_moves_and_presses_and_turns_a_release_into_a_reset() {
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
        // The key-up and the reset after it become one release of
        // everything, after the last press queued.
        let expected = [
            relayed(KIND_KEY, PHASE_BEGAN, 0),
            relayed(KIND_KEY, PHASE_BEGAN, 1),
            relayed(KIND_KEY, PHASE_BEGAN, 2),
            relayed(KIND_KEY, PHASE_BEGAN, 3),
            Call::Reset,
        ];
        assert_eq!(harness.wait_for(5), expected);
        harness.settled(expected.len());
    }

    /// Windows stays stuck while the queue fills to its bound and then
    /// releases keep coming: the key-ups, and the disconnect's reset after
    /// them. However many pile up, what the PC holds is released once
    /// Windows answers, and nothing pressed lands after that.
    #[test]
    fn releases_past_the_hard_limit_still_release_everything() {
        let harness = harness(Limits::default());
        harness.stuck();
        for id in 1..=CAPACITY as u32 {
            harness.send(KIND_KEY, PHASE_BEGAN, id);
        }
        for id in 1..=CAPACITY as u32 {
            harness.send(KIND_KEY, PHASE_ENDED, 10_000 + id);
        }
        harness.send(KIND_KEY, PHASE_BEGAN, 20_000);
        harness.injector.as_ref().unwrap().reset();
        harness.go.send(()).unwrap();
        assert!(harness.thread.wait_idle(Duration::from_secs(5)));
        let calls = harness.calls.lock().clone();
        assert_eq!(
            calls.last(),
            Some(&Call::Reset),
            "{:?}",
            &calls[calls.len() - 3..]
        );
        assert!(calls.len() <= 2 * CAPACITY + 1, "{}", calls.len());
        let pressed: Vec<_> = calls[1..=CAPACITY].to_vec();
        assert_eq!(
            pressed,
            (1..=CAPACITY as u32)
                .map(|id| relayed(KIND_KEY, PHASE_BEGAN, id))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_dropped_handle_returns_at_once_and_its_input_still_lands_then_is_released() {
        let mut harness = harness(Limits::default());
        harness.stuck();
        harness.send(KIND_KEY, PHASE_ENDED, 1);
        let started = Instant::now();
        drop(harness.injector.take());
        assert!(started.elapsed() < Duration::from_millis(100));
        harness.go.send(()).unwrap();
        let expected = [
            relayed(KIND_KEY, PHASE_BEGAN, 0),
            relayed(KIND_KEY, PHASE_ENDED, 1),
            Call::Reset,
        ];
        assert_eq!(harness.wait_for(3), expected);
        harness.settled(expected.len());
    }

    /// A stream restarts while Windows holds the old one's input. The new
    /// generation's handle is ready at once, on the same thread, and its
    /// input lands only after the old one's release: a key it presses
    /// stays down. Restarts over and over start no thread of their own.
    #[test]
    fn a_restart_while_windows_is_stuck_releases_the_old_input_before_the_new() {
        let mut harness = harness(Limits::default());
        harness.stuck();
        drop(harness.injector.take());
        let started = Instant::now();
        let mut generations: Vec<_> = (0..20).map(|_| harness.thread.begin(geometry())).collect();
        assert!(started.elapsed() < Duration::from_millis(100));
        let newest = generations.pop().unwrap();
        generations.clear();
        newest.relay(1, event(KIND_KEY, PHASE_BEGAN, 1), OUTPUT, false);
        harness.go.send(()).unwrap();
        let expected = [
            relayed(KIND_KEY, PHASE_BEGAN, 0),
            Call::Reset,
            relayed(KIND_KEY, PHASE_BEGAN, 1),
        ];
        assert_eq!(harness.wait_for(3), expected);
        // The old generations' ends, queued behind the new one's start,
        // released nothing of it.
        harness.settled(expected.len());
        drop(newest);
        assert_eq!(harness.wait_for(4)[3], Call::Reset);
    }

    /// A Windows call that never returns is logged once, from the input
    /// that queues behind it, and again only for the next held call.
    #[test]
    fn a_call_windows_holds_for_seconds_is_logged_once() {
        let harness = harness(Limits::default());
        harness.stuck();
        let logs = || {
            harness
                .injector
                .as_ref()
                .unwrap()
                .shared
                .state
                .lock()
                .held_logs
        };
        let later = |seconds| Instant::now() + Duration::from_secs(seconds);
        harness.send(KIND_HOVER, PHASE_MOVED, 1);
        assert_eq!(logs(), 0);
        harness.send_at(KIND_HOVER, PHASE_MOVED, 2, later(6));
        harness.send_at(KIND_HOVER, PHASE_MOVED, 3, later(60));
        assert_eq!(logs(), 1);
        harness.go.send(()).unwrap();
        assert!(harness.thread.finish(Duration::from_secs(5)));
        assert_eq!(logs(), 1);
    }

    /// A quit while a stream still runs, its end not yet queued, still
    /// releases what the PC holds.
    #[test]
    fn quitting_releases_what_a_running_stream_holds() {
        let harness = harness(Limits::default());
        harness.stuck();
        harness.go.send(()).unwrap();
        assert!(harness.thread.finish(Duration::from_secs(5)));
        assert_eq!(
            *harness.calls.lock(),
            [relayed(KIND_KEY, PHASE_BEGAN, 0), Call::Reset]
        );
    }

    #[test]
    fn quitting_waits_for_queued_releases_but_not_past_its_limit() {
        let harness = harness(Limits::default());
        harness.stuck();
        harness.send(KIND_KEY, PHASE_ENDED, 1);
        let started = Instant::now();
        assert!(!harness.thread.finish(Duration::from_millis(50)));
        assert!(started.elapsed() < Duration::from_secs(1));
        harness.go.send(()).unwrap();
        assert!(harness.thread.wait_idle(Duration::from_secs(5)));
        assert_eq!(
            *harness.calls.lock(),
            [
                relayed(KIND_KEY, PHASE_BEGAN, 0),
                relayed(KIND_KEY, PHASE_ENDED, 1),
                Call::Reset,
            ]
        );
    }

    /// The hop the thread adds: from the transport queueing an event to the
    /// call that hands it to Windows, at a steady 1 kHz. Prints p50, p99 and
    /// the worst; the bound only catches a thread that waits for a timer.
    #[test]
    fn the_hop_to_the_input_thread_is_short() {
        struct Timed(Arc<Mutex<Vec<(u32, Instant)>>>);
        impl Sink for Timed {
            fn relay(&mut self, _: u32, event: &InputEvent, _: CaptureGeometry, _: bool) {
                self.0.lock().push((event.event_id, Instant::now()));
            }
            fn reset(&mut self) {}
            fn keep_alive(&mut self, _: Option<CaptureGeometry>) {}
            fn pen_available(&self) -> bool {
                false
            }
            fn spaced_until(&self) -> Option<Instant> {
                None
            }
            fn tick_needed(&self) -> bool {
                false
            }
        }
        const EVENTS: u32 = 2000;
        let taken = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&taken);
        let thread = InputThread::with_sink(Limits::default(), move || Timed(recorded));
        let injector = thread.begin(geometry());
        let mut queued = Vec::new();
        let mut next = Instant::now();
        for id in 0..EVENTS {
            next += Duration::from_millis(1);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            queued.push(Instant::now());
            injector.relay(1, event(KIND_HOVER, PHASE_MOVED, id), OUTPUT, false);
        }
        assert!(thread.finish(Duration::from_secs(5)));
        let mut hops: Vec<_> = taken
            .lock()
            .iter()
            .map(|(id, at)| *at - queued[*id as usize])
            .collect();
        assert_eq!(hops.len(), EVENTS as usize);
        hops.sort();
        let at = |q: f64| hops[((hops.len() - 1) as f64 * q) as usize];
        let (p50, p99, worst) = (at(0.5), at(0.99), *hops.last().unwrap());
        eprintln!(
            "input hop to the input thread: p50 {} us, p99 {} us, max {} us",
            p50.as_micros(),
            p99.as_micros(),
            worst.as_micros()
        );
        assert!(p99 < Duration::from_millis(50), "{p99:?}");
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
        let thread = InputThread::with_sink(Limits::default(), move || {
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
        });
        let injector = thread.begin(geometry());
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
