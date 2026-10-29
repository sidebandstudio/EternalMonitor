//! Time limits for the Windows and driver calls that can block for good.
//!
//! A stuck graphics driver used to freeze a session: each capture generation
//! the watchdog restarted put one more thread into the same D3D11CreateDevice,
//! and none came back for 15 s (PR #97, N2). Windows cannot stop a thread
//! safely, so each of these calls runs on a thread of its own while the caller
//! waits with a deadline. Past it the caller gets [`Stall::TimedOut`] and takes
//! the path that call's error takes: the pipeline restarts, or the next encoder
//! in the order opens. The thread is abandoned. Whatever it returns later is
//! dropped on that thread and never used. While an abandoned call of a kind
//! still runs, the next one of that kind is refused at once instead of adding
//! another thread behind the same stuck driver.
//!
//! Every stall logs one line with `stall=<call>`, from the closed set of
//! [`Call::name`]s, so the daily diagnostics check can count them.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tracing::{info, warn};

/// A call that runs under a time limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    D3d11CreateDevice,
    DuplicateOutput,
    /// Opening an NVENC, AMF or QSV encoder, libx265, or libopus for PC audio.
    /// They share one lane: FFmpeg opens all of them under one process-wide
    /// lock (FF_CODEC_CAP_NOT_INIT_THREADSAFE), so after one hangs the others
    /// would only wait behind it. Each has its own limit, from
    /// [`encoder_open_limits`]. libx264 does not take that lock and opens
    /// without a limit.
    EncoderOpen,
    /// The Enable and Disable driver tasks: a PowerShell process, killed at
    /// the limit (see `vdd::run_task`).
    VddEnable,
    VddDisable,
}

const CALLS: usize = 5;

/// When a call is slow enough to log, and when it is given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub warn: Duration,
    pub limit: Duration,
}

impl Limits {
    const fn ms(warn: u64, limit: u64) -> Self {
        Self {
            warn: Duration::from_millis(warn),
            limit: Duration::from_millis(limit),
        }
    }
}

impl Call {
    /// Stable log value; no names, addresses or paths.
    pub const fn name(self) -> &'static str {
        match self {
            Self::D3d11CreateDevice => "d3d11_create_device",
            Self::DuplicateOutput => "duplicate_output",
            Self::EncoderOpen => "encoder_open",
            Self::VddEnable => "vdd_enable",
            Self::VddDisable => "vdd_disable",
        }
    }

    /// Normal durations are from the logs 18 Windows PCs uploaded on
    /// 25-28 Oct 2026. A limit must never trip on a slow PC that works, so
    /// each is at least four times the slowest call seen (an encoder open,
    /// twice: see [`ENCODER_OPENS`]), and none is below the 3 s after which
    /// the watchdog already restarted a stuck capture or encoder. The
    /// caller's heartbeats keep the watchdog off meanwhile.
    pub const fn limits(self) -> Limits {
        match self {
            // 235 device creations on 15 PCs: median 29 ms, p99 607 ms,
            // slowest 775 ms. 4 s is five times the slowest.
            Self::D3d11CreateDevice => Limits::ms(1_000, 4_000),
            // 64 on 14 PCs: median 1 ms, slowest 116 ms. It also runs in the
            // access-lost retries, which the logs do not time, so it keeps
            // the 3 s the watchdog gave it.
            Self::DuplicateOutput => Limits::ms(500, 3_000),
            // The hardware encoders'; libx265 and libopus have their own.
            Self::EncoderOpen => HARDWARE_OPEN,
            // 24 runs on 11 PCs: Enable median 2.9 s, slowest 6.3 s; Disable
            // median 3.0 s, slowest 7.1 s. vdd_task.ps1 waits up to 10 s for
            // an earlier run and 15 s for its own; 30 s is four times the
            // slowest and was the limit before.
            Self::VddEnable | Self::VddDisable => Limits::ms(10_000, 30_000),
        }
    }
}

/// NVENC, AMF and QSV opens. Slower than the other calls, so the limit is
/// set from a larger sample: "Opening encoder session" to "Encoder opened" in
/// the logs 31 Windows PCs uploaded from 0.3.3 over 7 h on 29 Oct 2026. The
/// first limit, 6 s, assumed QSV took at most 1.4 s (18 PCs, 25-28 Oct), and
/// left the slowest QSV open seen since only 1.4 times as long.
const HARDWARE_OPEN: Limits = Limits::ms(5_000, 15_000);

/// Each encoder in the [`Call::EncoderOpen`] lane by name suffix, its
/// limits, and the slowest open of it seen. A limit stays at least twice
/// that, so a slow PC that works keeps its encoder; the warning comes after
/// it, so a `slow=encoder_open` line marks an open slower than any seen.
const ENCODER_OPENS: [(&str, Limits, Duration); 5] = [
    // 87 opens on 3 PCs: median 428 ms, p99 3.2 s, slowest 4.4 s at
    // 1080p. One 0.3.2 Intel PC's opens were slow enough, again and
    // again, to trip the 3 s watchdog this replaced.
    ("_qsv", HARDWARE_OPEN, Duration::from_millis(4_376)),
    // 392 opens: median 102 ms, p99 522 ms, slowest 2.6 s at 2560x1600.
    ("_nvenc", HARDWARE_OPEN, Duration::from_millis(2_603)),
    // 98 opens: slowest 1.2 s.
    ("_amf", HARDWARE_OPEN, Duration::from_millis(1_167)),
    // CPU-bound, so slowest on a busy laptop. No PC opened it in the
    // window. On a 16-thread test PC with every core busy and the Power
    // saver plan: 110-366 ms at 1080p to 4K (50 ms idle).
    (
        "libx265",
        Limits::ms(10_000, 20_000),
        Duration::from_millis(366),
    ),
    // Not logged in the field. A small CPU-only open that holds up only
    // audio, which retries; it keeps #110's 6 s. On the same loaded PC:
    // 9-44 ms.
    (
        "libopus",
        Limits::ms(2_000, 6_000),
        Duration::from_millis(44),
    ),
];

/// The limits for opening FFmpeg encoder `encoder` in the
/// [`Call::EncoderOpen`] lane.
pub fn encoder_open_limits(encoder: &str) -> Limits {
    ENCODER_OPENS
        .iter()
        .find(|(suffix, ..)| encoder.ends_with(suffix))
        .map_or(HARDWARE_OPEN, |&(_, limits, _)| limits)
}

/// Abandoned calls of one kind that may still run. One: calls that hang wait
/// on the same driver, so a second thread would only wait behind the first.
const MAX_ABANDONED: usize = 1;

/// How often a waiting caller runs its `progress` and checks the deadline.
/// A result wakes it at once.
const WAIT_SLICE: Duration = Duration::from_millis(50);

/// Why a time-limited call returned no result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stall {
    /// It did not return within its limit and was abandoned.
    TimedOut { call: Call, after: Duration },
    /// It was not started: an earlier one of its kind is still stuck, or no
    /// thread could be started for it.
    Refused { call: Call },
}

impl std::fmt::Display for Stall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut { call, after } => write!(
                f,
                "{} did not return within {} ms",
                call.name(),
                after.as_millis()
            ),
            Self::Refused { call } => write!(
                f,
                "{} not started: an earlier one is still stuck",
                call.name()
            ),
        }
    }
}

impl std::error::Error for Stall {}

/// Calls of each kind that were abandoned and still run.
pub(crate) struct Lanes([AtomicUsize; CALLS]);

impl Lanes {
    pub(crate) const fn new() -> Self {
        Self([const { AtomicUsize::new(0) }; CALLS])
    }

    pub(crate) fn abandoned(&self, call: Call) -> usize {
        self.0[call as usize].load(SeqCst)
    }
}

static LANES: Lanes = Lanes::new();

/// The process's lanes, for callers that pass them on to [`run_on`].
pub(crate) fn lanes() -> &'static Lanes {
    &LANES
}

/// Abandoned calls of this kind still running, process-wide: while one
/// runs, the next of its kind is refused.
pub fn abandoned(call: Call) -> usize {
    LANES.abandoned(call)
}

/// The deadline's clock, injectable for tests.
pub(crate) struct Wait<N> {
    pub limits: Limits,
    pub slice: Duration,
    pub now: N,
}

/// Run `f` on its own thread and wait for it within `call`'s limit, calling
/// `progress` every 50 ms meanwhile (the caller's watchdog heartbeat).
pub fn run<T, F>(call: Call, f: F, progress: impl FnMut()) -> Result<T, Stall>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    run_on(&LANES, call, call.limits(), f, progress)
}

/// [`run`] counting abandoned calls in `lanes`, under `limits` (an encoder's
/// own, from [`encoder_open_limits`]).
pub(crate) fn run_on<T, F>(
    lanes: &'static Lanes,
    call: Call,
    limits: Limits,
    f: F,
    progress: impl FnMut(),
) -> Result<T, Stall>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let wait = Wait {
        limits,
        slice: WAIT_SLICE,
        now: Instant::now,
    };
    let delay = simulated_delay(call, limits);
    let hooks = Hooks {
        before: move || {
            if let Some(delay) = delay {
                std::thread::sleep(delay);
            }
        },
        handoff: || {},
    };
    run_in(lanes, call, wait, f, progress, hooks)
}

/// Points on the call's thread: `before` the call (a simulated stall), and
/// `handoff` after it returned, holding the hand-over lock, just before the
/// result is handed over or released.
pub(crate) struct Hooks<B, H> {
    pub before: B,
    pub handoff: H,
}

#[cfg(test)]
pub(crate) fn no_hooks() -> Hooks<fn(), fn()> {
    Hooks {
        before: || {},
        handoff: || {},
    }
}

/// What the call's thread hands over: its result, or its panic.
type Outcome<T> = std::thread::Result<T>;

/// [`run`] with its lanes, clock and hooks given.
pub(crate) fn run_in<T, F>(
    lanes: &'static Lanes,
    call: Call,
    wait: Wait<impl Fn() -> Instant>,
    f: F,
    mut progress: impl FnMut(),
    hooks: Hooks<impl FnOnce() + Send + 'static, impl FnOnce() + Send + 'static>,
) -> Result<T, Stall>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    let lane = &lanes.0[call as usize];
    let abandoned = lane.load(SeqCst);
    if abandoned >= MAX_ABANDONED {
        warn!(
            stall_refused = %call.name(),
            abandoned,
            "An earlier Windows or driver call of this kind is still stuck — not starting another"
        );
        return Err(Stall::Refused { call });
    }
    // Set by the caller when it gives up. The call's thread holds the same
    // lock from reading it until its result is in the channel, and the
    // caller looks in the channel under it before giving up, so exactly one
    // of them owns the result, and the lane is freed whenever it was taken.
    // Never held across the call itself.
    let gave_up = Arc::new(Mutex::new(false));
    let (tx, rx) = mpsc::channel::<Outcome<T>>();
    let thread_gave_up = Arc::clone(&gave_up);
    let real_start = Instant::now();
    let Hooks { before, handoff } = hooks;
    let spawned = std::thread::Builder::new()
        .name(format!("call-{}", call.name()))
        .spawn(move || {
            // A panic is handed over like a result: the caller re-raises it,
            // or, given up on, it is dropped here and the lane still frees.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                before();
                f()
            }));
            let given_up = thread_gave_up.lock();
            handoff();
            if !*given_up {
                let _ = tx.send(outcome);
                return;
            }
            drop(given_up);
            let panicked = outcome.is_err();
            // Released here, before the lane lets a new call start. A drop
            // that panics must not keep the lane taken either.
            let _ = catch_unwind(AssertUnwindSafe(move || drop(outcome)));
            lanes.0[call as usize].fetch_sub(1, SeqCst);
            info!(
                stall_ended = %call.name(),
                after_ms = real_start.elapsed().as_millis() as u64,
                panicked,
                "A Windows or driver call that was given up on returned; its result was released"
            );
        });
    if let Err(error) = spawned {
        warn!(stall_refused = %call.name(), %error, "Could not start a thread for a Windows or driver call");
        return Err(Stall::Refused { call });
    }
    let started = (wait.now)();
    let finish = |outcome: Outcome<T>, took: Duration| match outcome {
        Ok(value) => {
            note_finished(call, wait.limits, took);
            Ok(value)
        }
        Err(panic) => resume_unwind(panic),
    };
    loop {
        match rx.recv_timeout(wait.slice) {
            Ok(outcome) => {
                return finish(outcome, (wait.now)().saturating_duration_since(started));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("{} ended without a result", call.name())
            }
        }
        let after = (wait.now)().saturating_duration_since(started);
        if after >= wait.limits.limit {
            let mut gave_up = gave_up.lock();
            // It may have returned since the last look.
            if let Ok(outcome) = rx.try_recv() {
                drop(gave_up);
                return finish(outcome, after);
            }
            *gave_up = true;
            lane.fetch_add(1, SeqCst);
            drop(gave_up);
            log_timed_out(call, wait.limits, after);
            return Err(Stall::TimedOut { call, after });
        }
        progress();
    }
}

/// A limited call returned after `took`: log it if that was slow.
pub(crate) fn note_finished(call: Call, limits: Limits, took: Duration) {
    if took >= limits.warn {
        warn!(
            slow = %call.name(),
            took_ms = took.as_millis() as u64,
            warn_ms = limits.warn.as_millis() as u64,
            "A Windows or driver call was slow but returned"
        );
    }
}

/// The one line per stall.
pub(crate) fn log_timed_out(call: Call, limits: Limits, after: Duration) {
    warn!(
        stall = %call.name(),
        after_ms = after.as_millis() as u64,
        limit_ms = limits.limit.as_millis() as u64,
        "A Windows or driver call did not return in time — continuing without it"
    );
}

/// Debug builds only, to see the recovery on a PC whose driver works:
/// `ETERNAL_SIMULATE_STALL=d3d11_create_device,encoder_open` holds the first
/// such call of this process for twice its limit before making it.
#[cfg(debug_assertions)]
fn simulated_delay(call: Call, limits: Limits) -> Option<Duration> {
    static USED: AtomicUsize = AtomicUsize::new(0);
    let wanted = std::env::var("ETERNAL_SIMULATE_STALL").ok()?;
    if !wanted.split(',').any(|name| name.trim() == call.name()) {
        return None;
    }
    let bit = 1 << call as usize;
    if USED.fetch_or(bit, SeqCst) & bit != 0 {
        return None;
    }
    warn!(call = %call.name(), "Simulating a stuck Windows or driver call");
    Some(limits.limit * 2)
}

#[cfg(not(debug_assertions))]
fn simulated_delay(_call: Call, _limits: Limits) -> Option<Duration> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn lanes() -> &'static Lanes {
        Box::leak(Box::new(Lanes::new()))
    }

    /// A clock that moves on 100 ms each time it is read, so a 4 s limit
    /// passes after 40 reads, however slowly the machine runs.
    fn stepping_clock() -> impl Fn() -> Instant {
        let start = Instant::now();
        let reads = AtomicU64::new(0);
        move || start + Duration::from_millis(100 * reads.fetch_add(1, SeqCst))
    }

    fn wait(call: Call) -> Wait<impl Fn() -> Instant> {
        Wait {
            limits: call.limits(),
            slice: Duration::from_millis(1),
            now: stepping_clock(),
        }
    }

    /// A call that blocks until the returned sender sends or is dropped.
    fn hang<T: Send + 'static>(value: T) -> (mpsc::Sender<()>, impl FnOnce() -> T + Send) {
        let (release, released) = mpsc::channel::<()>();
        (release, move || {
            let _ = released.recv();
            value
        })
    }

    /// Records the thread it is dropped on.
    struct Device(Arc<Mutex<Option<std::thread::ThreadId>>>);
    impl Drop for Device {
        fn drop(&mut self) {
            *self.0.lock() = Some(std::thread::current().id());
        }
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        // Backstop only; on a free machine this takes microseconds.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_call_that_hangs_times_out_at_its_limit() {
        let lanes = lanes();
        let (_release, call) = hang(());
        let mut progress = 0;
        let result = run_in(
            lanes,
            Call::D3d11CreateDevice,
            wait(Call::D3d11CreateDevice),
            call,
            || progress += 1,
            no_hooks(),
        );
        let Err(Stall::TimedOut { call, after }) = result else {
            panic!("{result:?}")
        };
        assert_eq!(call, Call::D3d11CreateDevice);
        // Within one clock step of the limit, never before it.
        assert!(after >= Duration::from_secs(4), "{after:?}");
        assert!(after <= Duration::from_millis(4_100), "{after:?}");
        // The caller's heartbeat ran while it waited.
        assert!(progress >= 30, "{progress} heartbeats");
        assert_eq!(lanes.abandoned(Call::D3d11CreateDevice), 1);
    }

    #[test]
    fn a_late_result_is_released_on_its_own_thread_and_never_returned() {
        let lanes = lanes();
        let dropped_on = Arc::new(Mutex::new(None));
        let (release, call) = hang(Device(Arc::clone(&dropped_on)));
        let result = run_in(
            lanes,
            Call::EncoderOpen,
            wait(Call::EncoderOpen),
            call,
            || {},
            no_hooks(),
        );
        assert!(matches!(result, Err(Stall::TimedOut { .. })));
        assert!(dropped_on.lock().is_none());
        // The driver comes back with the encoder after the caller left.
        release.send(()).unwrap();
        wait_until("the late result was not released", || {
            lanes.abandoned(Call::EncoderOpen) == 0
        });
        let thread = dropped_on.lock().expect("the late result was not dropped");
        assert_ne!(thread, std::thread::current().id());
    }

    #[test]
    fn while_one_call_is_stuck_the_next_of_its_kind_fails_fast() {
        let lanes = lanes();
        let (release, call) = hang(());
        let stuck = run_in(
            lanes,
            Call::DuplicateOutput,
            wait(Call::DuplicateOutput),
            call,
            || {},
            no_hooks(),
        );
        assert!(matches!(stuck, Err(Stall::TimedOut { .. })));
        // Refused without starting: the call never runs, and nothing waits.
        let ran = Arc::new(AtomicUsize::new(0));
        for _ in 0..10 {
            let counted = Arc::clone(&ran);
            let started = Instant::now();
            let result = run_in(
                lanes,
                Call::DuplicateOutput,
                wait(Call::DuplicateOutput),
                move || counted.fetch_add(1, SeqCst),
                || panic!("a refused call waited"),
                no_hooks(),
            );
            assert_eq!(
                result,
                Err(Stall::Refused {
                    call: Call::DuplicateOutput
                })
            );
            assert!(started.elapsed() < Duration::from_secs(1));
        }
        assert_eq!(ran.load(SeqCst), 0);
        assert_eq!(lanes.abandoned(Call::DuplicateOutput), 1);
        // Another kind of call is not held up by it.
        let other = run_in(
            lanes,
            Call::D3d11CreateDevice,
            wait(Call::D3d11CreateDevice),
            || 7,
            || {},
            no_hooks(),
        );
        assert_eq!(other, Ok(7));
        // Once the stuck call returns, its kind runs again.
        release.send(()).unwrap();
        wait_until("the stuck call's lane did not free", || {
            lanes.abandoned(Call::DuplicateOutput) == 0
        });
        let again = run_in(
            lanes,
            Call::DuplicateOutput,
            wait(Call::DuplicateOutput),
            || "duplication",
            || {},
            no_hooks(),
        );
        assert_eq!(again, Ok("duplication"));
    }

    #[test]
    fn a_call_that_fails_fast_returns_its_error_unchanged() {
        let lanes = lanes();
        let result = run_in(
            lanes,
            Call::EncoderOpen,
            wait(Call::EncoderOpen),
            || Err::<(), _>("h264_nvenc open failed: FFmpeg code=-40"),
            || {},
            no_hooks(),
        );
        assert_eq!(result, Ok(Err("h264_nvenc open failed: FFmpeg code=-40")));
        assert_eq!(lanes.abandoned(Call::EncoderOpen), 0);
    }

    #[test]
    fn a_result_wakes_the_caller_at_once_not_at_the_next_check() {
        let lanes = lanes();
        // A slice far longer than the call: waiting it out would show.
        let wait = Wait {
            limits: Call::D3d11CreateDevice.limits(),
            slice: Duration::from_secs(30),
            now: Instant::now,
        };
        let started = Instant::now();
        let result = run_in(
            lanes,
            Call::D3d11CreateDevice,
            wait,
            || {
                std::thread::sleep(Duration::from_millis(20));
                "device"
            },
            || panic!("no heartbeat is due within the slice"),
            no_hooks(),
        );
        assert_eq!(result, Ok("device"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_normal_call_adds_well_under_a_millisecond() {
        // The cost of the thread and the handover, against D3D11CreateDevice's
        // 29 ms median: the median of 50 calls that return at once.
        let lanes = lanes();
        let mut costs: Vec<Duration> = (0..50)
            .map(|_| {
                let started = Instant::now();
                let result = run_in(
                    lanes,
                    Call::D3d11CreateDevice,
                    Wait {
                        limits: Call::D3d11CreateDevice.limits(),
                        slice: WAIT_SLICE,
                        now: Instant::now,
                    },
                    || 1,
                    || {},
                    no_hooks(),
                );
                assert_eq!(result, Ok(1));
                started.elapsed()
            })
            .collect();
        costs.sort();
        let median = costs[costs.len() / 2];
        println!(
            "median cost {median:?}, slowest {:?}",
            costs[costs.len() - 1]
        );
        // Generous for a loaded shared machine; typically 20-100 µs.
        assert!(median < Duration::from_millis(5), "{median:?}");
    }

    #[test]
    fn a_result_handed_over_as_the_caller_gives_up_is_returned_and_frees_the_lane() {
        // The call returns just as the caller reads its deadline as passed:
        // the call's thread is inside the hand-over when the caller goes to
        // give up. Before, the caller found the channel empty and took the
        // lane, the thread then sent its result anyway, and the lane stayed
        // taken for the life of the process.
        for _ in 0..20 {
            let lanes = lanes();
            let (inside_tx, inside_rx) = mpsc::channel::<()>();
            let start = Instant::now();
            let reads = AtomicU64::new(0);
            let dropped_on = Arc::new(Mutex::new(None));
            let device = Device(Arc::clone(&dropped_on));
            let result = run_in(
                lanes,
                Call::D3d11CreateDevice,
                Wait {
                    limits: Call::D3d11CreateDevice.limits(),
                    slice: Duration::from_millis(1),
                    now: move || {
                        if reads.fetch_add(1, SeqCst) == 0 {
                            return start;
                        }
                        // The deadline passes once the thread is handing over.
                        let _ = inside_rx.recv_timeout(Duration::from_secs(60));
                        start + Duration::from_secs(60)
                    },
                },
                move || device,
                || {},
                Hooks {
                    before: || {},
                    handoff: move || {
                        let _ = inside_tx.send(());
                        // Give the caller time to reach the lock.
                        std::thread::sleep(Duration::from_millis(20));
                    },
                },
            );
            assert!(result.is_ok(), "{:?}", result.err());
            assert_eq!(lanes.abandoned(Call::D3d11CreateDevice), 0);
            assert!(
                dropped_on.lock().is_none(),
                "the returned result was released"
            );
        }
    }

    #[test]
    fn a_call_that_panics_after_it_was_given_up_on_frees_its_lane() {
        let lanes = lanes();
        let (release, released) = mpsc::channel::<()>();
        let result = run_in(
            lanes,
            Call::EncoderOpen,
            wait(Call::EncoderOpen),
            move || {
                let _ = released.recv();
                panic!("the driver crashed late");
            },
            || {},
            no_hooks(),
        );
        assert!(matches!(result, Err(Stall::TimedOut { .. })));
        assert_eq!(lanes.abandoned(Call::EncoderOpen), 1);
        release.send(()).unwrap();
        wait_until("a late panic kept the lane taken", || {
            lanes.abandoned(Call::EncoderOpen) == 0
        });
        // A panic while the caller still waits reaches the caller, as when
        // the call ran on its thread.
        let panicked = std::panic::catch_unwind(|| {
            run_in(
                lanes,
                Call::EncoderOpen,
                wait(Call::EncoderOpen),
                || -> u32 { panic!("the driver crashed") },
                || {},
                no_hooks(),
            )
        });
        let message = panicked.unwrap_err();
        assert_eq!(message.downcast_ref::<&str>(), Some(&"the driver crashed"));
        assert_eq!(lanes.abandoned(Call::EncoderOpen), 0);
    }

    #[test]
    fn stall_lines_use_a_closed_set_of_plain_tokens() {
        let names: Vec<_> = [
            Call::D3d11CreateDevice,
            Call::DuplicateOutput,
            Call::EncoderOpen,
            Call::VddEnable,
            Call::VddDisable,
        ]
        .map(Call::name)
        .into();
        assert_eq!(names.len(), CALLS);
        for name in &names {
            assert!(name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()));
        }
        // Every limit is at least the 3 s the watchdog allowed, and the
        // warning comes first.
        for call in [
            Call::D3d11CreateDevice,
            Call::DuplicateOutput,
            Call::EncoderOpen,
            Call::VddEnable,
        ] {
            let limits = call.limits();
            assert!(limits.limit >= Duration::from_secs(3), "{call:?}");
            assert!(limits.warn < limits.limit, "{call:?}");
        }
    }

    #[test]
    fn each_encoder_open_limit_is_at_least_twice_the_slowest_seen() {
        for (suffix, limits, slowest) in ENCODER_OPENS {
            assert!(
                slowest > Duration::ZERO,
                "{suffix}: no slowest open recorded"
            );
            assert!(
                limits.limit >= slowest * 2,
                "{suffix}: {limits:?}, slowest {slowest:?}"
            );
            // A slow line means slower than any open seen, and it comes
            // before the limit and after the watchdog's 3 s.
            assert!(
                limits.warn > slowest,
                "{suffix}: {limits:?}, slowest {slowest:?}"
            );
            assert!(limits.warn < limits.limit, "{suffix}: {limits:?}");
            assert!(
                limits.limit >= Duration::from_secs(3),
                "{suffix}: {limits:?}"
            );
        }
        for (names, limit) in [
            (
                &[
                    "h264_nvenc",
                    "hevc_nvenc",
                    "h264_amf",
                    "hevc_amf",
                    "h264_qsv",
                    "hevc_qsv",
                ][..],
                15,
            ),
            (&["libx265"][..], 20),
            (&["libopus"][..], 6),
        ] {
            for name in names {
                assert_eq!(
                    encoder_open_limits(name).limit,
                    Duration::from_secs(limit),
                    "{name}"
                );
            }
        }
        assert_eq!(Call::EncoderOpen.limits(), encoder_open_limits("h264_qsv"));
    }

    #[test]
    fn libopus_keeps_its_6_s_limit_in_the_lane_it_shares() {
        let lanes = lanes();
        let (release, call) = hang(());
        let wait = |encoder: &str| Wait {
            limits: encoder_open_limits(encoder),
            slice: Duration::from_millis(1),
            now: stepping_clock(),
        };
        let result = run_in(
            lanes,
            Call::EncoderOpen,
            wait("libopus"),
            call,
            || {},
            no_hooks(),
        );
        let Err(Stall::TimedOut { after, .. }) = result else {
            panic!("{result:?}")
        };
        assert!(after >= Duration::from_secs(6), "{after:?}");
        assert!(after <= Duration::from_millis(6_100), "{after:?}");
        // The lane is shared: a hardware open is refused behind it.
        let refused = run_in(
            lanes,
            Call::EncoderOpen,
            wait("h264_nvenc"),
            || (),
            || panic!("a refused call waited"),
            no_hooks(),
        );
        assert_eq!(
            refused,
            Err(Stall::Refused {
                call: Call::EncoderOpen
            })
        );
        release.send(()).unwrap();
        wait_until("the stuck open's lane did not free", || {
            lanes.abandoned(Call::EncoderOpen) == 0
        });
    }

    #[test]
    fn the_stall_line_is_unquoted_for_counting() {
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer = Arc::clone(&out);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Sink(Arc::clone(&writer)))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_timed_out(
                Call::D3d11CreateDevice,
                Call::D3d11CreateDevice.limits(),
                Duration::from_millis(4_012),
            )
        });
        let line = String::from_utf8(out.lock().clone()).unwrap();
        assert!(
            line.contains("stall=d3d11_create_device after_ms=4012 limit_ms=4000"),
            "{line}"
        );
        // An encoder's line gives that encoder's own limit.
        out.lock().clear();
        tracing::subscriber::with_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer({
                    let writer = Arc::clone(&out);
                    move || Sink(Arc::clone(&writer))
                })
                .finish(),
            || {
                log_timed_out(
                    Call::EncoderOpen,
                    encoder_open_limits("libx265"),
                    Duration::from_millis(20_012),
                )
            },
        );
        let line = String::from_utf8(out.lock().clone()).unwrap();
        assert!(
            line.contains("stall=encoder_open after_ms=20012 limit_ms=20000"),
            "{line}"
        );
    }
}
