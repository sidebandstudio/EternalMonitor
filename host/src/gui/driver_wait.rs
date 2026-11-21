//! The window waits for a graphics driver that answers, and says so.
//!
//! The window's thread lists adapters, creates its device and draws through
//! the graphics driver, and Windows cannot stop a thread stuck inside it.
//! So before that thread makes its first graphics call ([`before_window`]):
//! - the graphics cards are listed through DXGI on a thread of their own,
//!   in the startup graphics lane ([`Call::GpuDetect`]), once nothing else
//!   is stuck there. The renderer's crash avoidance keys its history by that
//!   list, so it is configured only with a list that returned;
//! - then the same wgpu calls the window's thread makes first are made once
//!   on a thread of their own ([`Call::GpuProbe`]).
//!
//! Each is one call, waited for as long as it runs: past its limit the
//! notice goes up, and a driver that answers late still opens the window.
//!
//! Past that, the window's thread can still stall in calls that cannot be
//! moved off it (its own device and swap chain, a surface it recreates), so
//! [`watch_first_paint`] watches from another thread until the window is
//! confirmed drawing.
//!
//! Whenever the window waits on the driver, a notice drawn by Windows
//! without the graphics card says why ([`Prompt`]). OK hides it for a
//! while; Cancel quits. It goes for good when the wait is over.
// Only Windows waits; the logic is tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tracing::{info, warn};

use crate::stall::{Call, Lanes, Limits};

/// How often a waiting launch looks whether the stuck call has returned.
pub(super) const POLL: Duration = Duration::from_millis(250);

/// How long OK hides the notice while the wait goes on.
pub(super) const REMIND_AFTER: Duration = Duration::from_secs(15);

/// How long the window may run UI passes without being confirmed drawing.
/// A working one is confirmed about 3.5 s after its first pass (3 s and 10
/// frames of drawing, then the screenshot that proves a present); one whose
/// paints return without a surface keeps passing and never is.
pub(super) const PROOF_WITHIN: Duration = Duration::from_secs(12);

/// How the user closed the notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Answer {
    /// OK: keep waiting.
    Wait,
    /// Cancel: quit EternalMonitor.
    Quit,
}

/// What the user sees while the window waits.
pub(super) trait Notice: Send {
    fn show(&mut self);
    /// How the user closed the shown notice, once.
    fn answer(&mut self) -> Option<Answer>;
    /// Takes the notice down, if it is up.
    fn close(&mut self);
}

/// Where the wait stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Waiting,
    /// The wait is over and the user did not quit.
    Over,
    /// The user chose Cancel, also one that came as the wait ended.
    Quit,
}

/// The notice while startup waits on the driver, shared by the steps on
/// the window's thread, the first-paint watcher and the renderer: up from
/// the first wait, back [`REMIND_AFTER`] after OK, down for good once the
/// wait is over. Every change happens under one lock, so the end of the
/// wait, the user's answer and showing the notice never cross.
#[derive(Clone)]
pub(super) struct Prompt(Arc<Mutex<PromptState>>);

struct PromptState {
    notice: Box<dyn Notice>,
    up: bool,
    raised: bool,
    hidden_at: Option<Instant>,
    phase: Phase,
}

impl Prompt {
    pub(super) fn new(notice: impl Notice + 'static) -> Self {
        Self(Arc::new(Mutex::new(PromptState {
            notice: Box::new(notice),
            up: false,
            raised: false,
            hidden_at: None,
            phase: Phase::Waiting,
        })))
    }

    /// Show the notice at `now`, unless it is up, OK hid it less than
    /// [`REMIND_AFTER`] ago, or the wait is over.
    pub(super) fn raise(&self, now: Instant) {
        let mut state = self.0.lock();
        state.read_answer(now);
        if state.up || state.phase != Phase::Waiting {
            return;
        }
        if state
            .hidden_at
            .is_some_and(|at| now.saturating_duration_since(at) < REMIND_AFTER)
        {
            return;
        }
        if !state.raised {
            warn!("The graphics driver is not answering; the window opens once it does");
        }
        state.notice.show();
        state.up = true;
        state.raised = true;
        state.hidden_at = None;
    }

    /// Where the wait stands at `now`, with the user's answer read.
    pub(super) fn phase(&self, now: Instant) -> Phase {
        let mut state = self.0.lock();
        state.read_answer(now);
        state.phase
    }

    /// The notice was ever shown.
    pub(super) fn raised(&self) -> bool {
        self.0.lock().raised
    }

    /// The wait is over: the answer given by now is kept, and the notice
    /// goes down and stays down. Where it stands after.
    pub(super) fn finish(&self) -> Phase {
        let mut state = self.0.lock();
        state.read_answer(Instant::now());
        if state.phase == Phase::Waiting {
            state.phase = Phase::Over;
        }
        state.up = false;
        state.notice.close();
        state.phase
    }
}

impl PromptState {
    fn read_answer(&mut self, now: Instant) {
        if !self.up {
            return;
        }
        match self.notice.answer() {
            Some(Answer::Wait) => {
                self.up = false;
                self.hidden_at = Some(now);
            }
            Some(Answer::Quit) => {
                self.up = false;
                if self.phase == Phase::Waiting {
                    self.phase = Phase::Quit;
                }
            }
            None => {}
        }
    }
}

/// A limit no startup call reaches: it is waited for as long as it runs.
const NEVER: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Starts a call from `make` in `call`'s lane once none of `free` is stuck,
/// and waits for it as long as it runs, with the notice up while a lane is
/// stuck or once the call has run for `limits`' limit. Its result, also a
/// late one; None when the user quit, also by a Cancel that came as the
/// driver answered.
fn await_call<T, F>(
    lanes: &'static Lanes,
    call: Call,
    limits: Limits,
    free: &[Call],
    mut make: impl FnMut() -> F,
    prompt: &Prompt,
    poll: Duration,
) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let started = Instant::now();
    let unlimited = Limits {
        warn: limits.warn,
        limit: NEVER,
    };
    let quit = |prompt: &Prompt| {
        let quit = prompt.phase(Instant::now()) == Phase::Quit;
        if quit {
            info!(
                waited_ms = started.elapsed().as_millis() as u64,
                "Quit while the window waited for the graphics driver"
            );
        }
        quit
    };
    let mut pending = loop {
        if free.iter().all(|&stuck| lanes.abandoned(stuck) == 0) {
            if let Ok(pending) = crate::stall::start_on(lanes, call, unlimited, make()) {
                break pending;
            }
        }
        prompt.raise(Instant::now());
        if quit(prompt) {
            return None;
        }
        std::thread::sleep(poll);
    };
    let mut up_to = limits.limit;
    loop {
        match pending.wait_up_to(up_to, &mut || {}) {
            Some(Ok(value)) => {
                if quit(prompt) {
                    return None;
                }
                if prompt.raised() {
                    info!(
                        call = call.name(),
                        waited_ms = started.elapsed().as_millis() as u64,
                        "The graphics driver answered"
                    );
                }
                return Some(value);
            }
            // Only past a limit no call reaches.
            Some(Err(_)) => return None,
            None => {
                prompt.raise(Instant::now());
                // Dropped, a call still in the driver keeps its lane taken
                // until it returns.
                if quit(prompt) {
                    return None;
                }
                up_to += poll;
            }
        }
    }
}

/// The steps before the window's thread makes its first graphics call:
/// a list from `list`, in [`Call::GpuDetect`] under `list_limits`, which
/// `configure` gets; then a probe from `probe`, over what `configure`
/// returned, in [`Call::GpuProbe`] under `probe_limits`. Each is waited for
/// as long as it runs. False when the user quit from the notice.
#[allow(clippy::too_many_arguments)]
pub(super) fn before_window<T, L, S, F>(
    lanes: &'static Lanes,
    list_limits: Limits,
    probe_limits: Limits,
    prompt: &Prompt,
    poll: Duration,
    list: impl FnMut() -> L,
    configure: impl FnOnce(T) -> S,
    probe: impl Fn(&S) -> F,
) -> bool
where
    T: Send + 'static,
    L: FnOnce() -> T + Send + 'static,
    F: FnOnce() + Send + 'static,
{
    let Some(adapters) = await_call(
        lanes,
        Call::GpuDetect,
        list_limits,
        &[Call::GpuDetect],
        list,
        prompt,
        poll,
    ) else {
        return false;
    };
    let spec = configure(adapters);
    await_call(
        lanes,
        Call::GpuProbe,
        probe_limits,
        &[Call::GpuDetect, Call::GpuProbe],
        || probe(&spec),
        prompt,
        poll,
    )
    .is_some()
}

/// What the first-paint watcher has seen, and when it says the window is
/// stuck. Every look is made from the watcher's thread, in order.
pub(super) struct Watch {
    started: Instant,
    limit: Duration,
    proof_within: Duration,
    last_held: Option<Instant>,
    first_pass: Option<Instant>,
}

impl Watch {
    pub(super) fn new(started: Instant, limit: Duration, proof_within: Duration) -> Self {
        Self {
            started,
            limit,
            proof_within,
            last_held: None,
            first_pass: None,
        }
    }

    /// Whether the notice should be up at `at`, given the window thread's
    /// last UI pass and whether the window is held now (minimized, or its
    /// title bar or system menu held, when a working window runs no UI pass
    /// either). Counted from the latest of the start and the last time it
    /// was held: `limit` without a UI pass, or `proof_within` of UI passes
    /// without the window confirmed drawing.
    pub(super) fn look(&mut self, at: Instant, last_pass: Option<Instant>, held: bool) -> bool {
        if held {
            self.last_held = Some(at);
            return false;
        }
        if let Some(pass) = last_pass {
            self.first_pass.get_or_insert(pass);
        }
        let since = self
            .last_held
            .map_or(self.started, |held| held.max(self.started));
        let quiet = last_pass.map_or(since, |pass| pass.max(since));
        let silent = at.saturating_duration_since(quiet) >= self.limit;
        let unproven = self.first_pass.is_some_and(|first| {
            at.saturating_duration_since(first.max(since)) >= self.proof_within
        });
        silent || unproven
    }
}

/// Watches the window from another thread until the wait ends
/// ([`FirstPaintWatch::finisher`], called when the window is confirmed
/// drawing, or [`FirstPaintWatch::stop`]), and raises the notice when
/// [`Watch::look`] says so. Cancel runs `quit`, which ends the process: the
/// window's thread may never return.
#[allow(clippy::too_many_arguments)]
pub(super) fn watch_first_paint(
    prompt: Prompt,
    limit: Duration,
    proof_within: Duration,
    poll: Duration,
    now: impl Fn() -> Instant + Send + 'static,
    last_pass: impl Fn() -> Option<Instant> + Send + 'static,
    held: impl Fn() -> bool + Send + 'static,
    quit: impl FnOnce() + Send + 'static,
) -> FirstPaintWatch {
    let mut watch = Watch::new(now(), limit, proof_within);
    let watched = prompt.clone();
    let run = move || loop {
        match watched.phase(now()) {
            Phase::Quit => {
                info!("Quit while the window waited for the graphics driver");
                quit();
                return;
            }
            Phase::Over => return,
            Phase::Waiting => {}
        }
        let at = now();
        if watch.look(at, last_pass(), held()) {
            watched.raise(at);
        }
        std::thread::sleep(poll);
    };
    let thread = std::thread::Builder::new()
        .name("first-paint-watch".into())
        .spawn(run)
        .map_err(|error| warn!(%error, "Could not watch the window's first frames"))
        .ok();
    FirstPaintWatch { prompt, thread }
}

/// A running [`watch_first_paint`].
pub(super) struct FirstPaintWatch {
    prompt: Prompt,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FirstPaintWatch {
    /// Ends the wait, for the renderer to call when the window is
    /// confirmed drawing.
    pub(super) fn finisher(&self) -> Box<dyn FnOnce() + Send> {
        let prompt = self.prompt.clone();
        Box::new(move || {
            prompt.finish();
        })
    }

    /// The window ended: the wait is over, and the watcher is done. A
    /// Cancel given by then still quits. Err if the watcher panicked.
    pub(super) fn stop(self) -> std::thread::Result<()> {
        self.prompt.finish();
        self.thread.map_or(Ok(()), std::thread::JoinHandle::join)
    }
}

/// A message box: Windows draws it without the graphics card's driver.
#[cfg(windows)]
#[derive(Default)]
pub(super) struct MessageNotice {
    shown: Option<Shown>,
}

#[cfg(windows)]
struct Shown {
    /// The box's thread, to find its window by.
    thread: Arc<std::sync::atomic::AtomicU32>,
    answer: std::thread::JoinHandle<windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT>,
}

#[cfg(windows)]
const NOTICE: &str = "The graphics driver is not responding, so the EternalMonitor window can't open yet. It opens on its own as soon as the driver answers.\n\nIf this stays, restart the PC or update the graphics driver from NVIDIA, AMD or Intel.\n\nClick OK to keep waiting, or Cancel to quit EternalMonitor.";

#[cfg(windows)]
impl Notice for MessageNotice {
    fn show(&mut self) {
        use std::sync::atomic::{AtomicU32, Ordering};
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONWARNING, MB_OKCANCEL, MB_SETFOREGROUND,
        };
        let thread = Arc::new(AtomicU32::new(0));
        let id = Arc::clone(&thread);
        let spawned = std::thread::Builder::new()
            .name("driver-wait-notice".into())
            .spawn(move || unsafe {
                id.store(
                    windows::Win32::System::Threading::GetCurrentThreadId(),
                    Ordering::SeqCst,
                );
                MessageBoxW(
                    None,
                    &HSTRING::from(NOTICE),
                    &HSTRING::from("EternalMonitor"),
                    MB_OKCANCEL | MB_ICONWARNING | MB_SETFOREGROUND,
                )
            });
        match spawned {
            Ok(answer) => self.shown = Some(Shown { thread, answer }),
            Err(error) => warn!(%error, "Could not show the graphics driver notice"),
        }
    }

    fn answer(&mut self) -> Option<Answer> {
        use windows::Win32::UI::WindowsAndMessaging::IDCANCEL;
        if !self.shown.as_ref()?.answer.is_finished() {
            return None;
        }
        let shown = self.shown.take()?;
        // A box that could not be shown keeps waiting, as OK does.
        Some(match shown.answer.join() {
            Ok(answer) if answer == IDCANCEL => Answer::Quit,
            _ => Answer::Wait,
        })
    }

    fn close(&mut self) {
        use std::sync::atomic::Ordering;
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, PostMessageW, WM_CLOSE};
        unsafe extern "system" fn close_window(hwnd: HWND, _: LPARAM) -> BOOL {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            true.into()
        }
        let Some(shown) = self.shown.take() else {
            return;
        };
        // On a thread of its own: this may run on the window's thread, under
        // the prompt's lock. The box may not be up yet when the driver
        // answers at once, so ask again until its thread ends, for 2 s.
        let closing = move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !shown.answer.is_finished() && Instant::now() < deadline {
                let thread = shown.thread.load(Ordering::SeqCst);
                if thread != 0 {
                    unsafe {
                        let _ = EnumThreadWindows(thread, Some(close_window), LPARAM(0));
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        if let Err(error) = std::thread::Builder::new()
            .name("driver-wait-notice-close".into())
            .spawn(closing)
        {
            warn!(%error, "Could not close the graphics driver notice");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;

    /// For a call that hangs until the test releases it.
    const SHORT: Limits = Limits {
        warn: Duration::from_millis(100),
        limit: Duration::from_millis(300),
    };
    /// For a call that answers: a slow machine never reaches it, so the
    /// events stay exactly what the test lists.
    const LONG: Limits = Limits {
        warn: Duration::from_secs(30),
        limit: Duration::from_secs(60),
    };
    const SLICE: Duration = Duration::from_millis(1);

    fn lanes() -> &'static Lanes {
        Box::leak(Box::new(Lanes::new()))
    }

    /// What happened, in order.
    #[derive(Clone, Default)]
    struct Events(Arc<Mutex<Vec<String>>>);

    impl Events {
        fn push(&self, event: impl Into<String>) {
            self.0.lock().push(event.into());
        }
        fn all(&self) -> Vec<String> {
            self.0.lock().clone()
        }
        fn count(&self, event: &str) -> usize {
            self.0.lock().iter().filter(|e| *e == event).count()
        }
        /// Backstop only; each wait ends as soon as the event happens.
        fn wait_for(&self, event: &str, times: usize) {
            let deadline = Instant::now() + Duration::from_secs(60);
            while self.count(event) < times {
                assert!(Instant::now() < deadline, "no {event}: {:?}", self.all());
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// A notice the test answers by setting `answer` while it is up.
    #[derive(Clone, Default)]
    struct Fake {
        events: Events,
        answer: Arc<Mutex<Option<Answer>>>,
        up: Arc<AtomicBool>,
    }

    impl Notice for Fake {
        fn show(&mut self) {
            self.up.store(true, SeqCst);
            self.events.push("shown");
        }
        fn answer(&mut self) -> Option<Answer> {
            if !self.up.load(SeqCst) {
                return None;
            }
            let answer = self.answer.lock().take()?;
            self.up.store(false, SeqCst);
            self.events.push(match answer {
                Answer::Wait => "ok",
                Answer::Quit => "cancel",
            });
            Some(answer)
        }
        fn close(&mut self) {
            if self.up.swap(false, SeqCst) {
                self.events.push("closed");
            }
        }
    }

    /// Something that blocks in `lanes`' startup lane until the returned
    /// sender sends, already given up on: a stuck GPU detection.
    fn stuck_detection(lanes: &'static Lanes) -> mpsc::Sender<()> {
        let (release, held) = mpsc::channel::<()>();
        let result = crate::stall::run_on(
            lanes,
            Call::GpuDetect,
            SHORT,
            move || {
                let _ = held.recv();
            },
            || {},
        );
        assert!(result.is_err());
        release
    }

    type ListStep = Box<dyn FnOnce() -> String + Send>;
    type ProbeStep = Box<dyn FnOnce() + Send>;

    /// [`before_window`] on its own thread: each list's result and each
    /// probe come from the test, and `configure` records what it got.
    fn start_before_window(
        lanes: &'static Lanes,
        fake: &Fake,
        (list_limits, probe_limits): (Limits, Limits),
        list: impl FnMut() -> ListStep + Send + 'static,
        probe: impl Fn() -> ProbeStep + Send + 'static,
    ) -> std::thread::JoinHandle<bool> {
        let prompt = Prompt::new(fake.clone());
        let events = fake.events.clone();
        std::thread::spawn(move || {
            let configured = events.clone();
            before_window(
                lanes,
                list_limits,
                probe_limits,
                &prompt,
                SLICE,
                list,
                move |gpus: String| {
                    // Crash avoidance is configured only with a list that
                    // returned, and nothing is stuck in the startup lane.
                    assert_eq!(lanes.abandoned(Call::GpuDetect), 0);
                    configured.push(format!("configure {gpus}"));
                },
                |_| probe(),
            )
        })
    }

    fn listing(events: &Events, gpus: &'static str) -> ListStep {
        let events = events.clone();
        Box::new(move || {
            events.push("list");
            gpus.to_string()
        })
    }

    fn probing(events: &Events) -> ProbeStep {
        let events = events.clone();
        Box::new(move || events.push("probe"))
    }

    /// A step that blocks until the test sends on the returned sender, and
    /// says when it started.
    fn held_step(
        events: &Events,
        name: &'static str,
    ) -> (
        mpsc::Sender<()>,
        mpsc::Receiver<()>,
        Box<dyn FnOnce() + Send>,
    ) {
        let (release, held) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        let events = events.clone();
        let step = Box::new(move || {
            events.push(name);
            let _ = started.send(());
            let _ = held.recv();
            events.push(format!("{name} returned"));
        });
        (release, running, step)
    }

    #[test]
    fn a_driver_that_answers_opens_the_window_without_a_notice() {
        let fake = Fake::default();
        let (list_events, probe_events) = (fake.events.clone(), fake.events.clone());
        let opened = start_before_window(
            lanes(),
            &fake,
            (LONG, LONG),
            move || listing(&list_events, "10de:2c02 32.0"),
            move || probing(&probe_events),
        );
        assert!(opened.join().unwrap());
        assert_eq!(
            fake.events.all(),
            ["list", "configure 10de:2c02 32.0", "probe"]
        );
    }

    #[test]
    fn while_detection_is_stuck_nothing_lists_and_the_notice_is_up() {
        let lanes = lanes();
        let release = stuck_detection(lanes);
        let fake = Fake::default();
        let (list_events, probe_events) = (fake.events.clone(), fake.events.clone());
        let opened = start_before_window(
            lanes,
            &fake,
            (LONG, LONG),
            move || listing(&list_events, "gpus"),
            move || probing(&probe_events),
        );
        fake.events.wait_for("shown", 1);
        // Nothing went near the stuck driver.
        assert_eq!(fake.events.all(), ["shown"]);
        release.send(()).unwrap();
        assert!(opened.join().unwrap());
        // The notice stays up for the first-paint watcher to take down.
        assert_eq!(
            fake.events.all(),
            ["shown", "list", "configure gpus", "probe"]
        );
    }

    #[test]
    fn a_list_that_answers_after_its_limit_is_waited_for_and_configures_once() {
        let lanes = lanes();
        let fake = Fake::default();
        let (release, list_running, step) = held_step(&fake.events, "list");
        let step = Arc::new(Mutex::new(Some(step)));
        let probe_events = fake.events.clone();
        let opened = start_before_window(
            lanes,
            &fake,
            (SHORT, LONG),
            move || {
                let step = step.lock().take().expect("listed once");
                Box::new(move || {
                    step();
                    "late".to_string()
                })
            },
            move || probing(&probe_events),
        );
        list_running.recv().unwrap();
        fake.events.wait_for("shown", 1);
        release.send(()).unwrap();
        assert!(opened.join().unwrap());
        // One list, past its limit; its late result configured the renderer.
        assert_eq!(
            fake.events.all(),
            ["list", "shown", "list returned", "configure late", "probe"]
        );
        assert_eq!(lanes.abandoned(Call::GpuDetect), 0);
    }

    #[test]
    fn a_probe_that_answers_after_its_limit_opens_the_window() {
        let lanes = lanes();
        let fake = Fake::default();
        let (release, probe_running, step) = held_step(&fake.events, "probe");
        let step = Arc::new(Mutex::new(Some(step)));
        let list_events = fake.events.clone();
        let opened = start_before_window(
            lanes,
            &fake,
            (LONG, SHORT),
            move || listing(&list_events, "gpus"),
            move || step.lock().take().expect("probed once"),
        );
        probe_running.recv().unwrap();
        fake.events.wait_for("shown", 1);
        release.send(()).unwrap();
        assert!(opened.join().unwrap());
        assert_eq!(
            fake.events.all(),
            ["list", "configure gpus", "probe", "shown", "probe returned"]
        );
        assert_eq!(lanes.abandoned(Call::GpuProbe), 0);
    }

    #[test]
    fn cancel_while_a_call_hangs_opens_no_window_and_leaves_its_lane_taken() {
        let lanes = lanes();
        let fake = Fake::default();
        let (release, probe_running, step) = held_step(&fake.events, "probe");
        let step = Arc::new(Mutex::new(Some(step)));
        let list_events = fake.events.clone();
        let opened = start_before_window(
            lanes,
            &fake,
            (LONG, SHORT),
            move || listing(&list_events, "gpus"),
            move || step.lock().take().expect("probed once"),
        );
        probe_running.recv().unwrap();
        fake.events.wait_for("shown", 1);
        *fake.answer.lock() = Some(Answer::Quit);
        assert!(!opened.join().unwrap());
        assert_eq!(
            fake.events.all(),
            ["list", "configure gpus", "probe", "shown", "cancel"]
        );
        // The probe still runs in the driver; its lane says so until it
        // returns.
        assert_eq!(lanes.abandoned(Call::GpuProbe), 1);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while lanes.abandoned(Call::GpuProbe) != 0 {
            assert!(Instant::now() < deadline, "the lane did not free");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn cancel_while_detection_is_stuck_opens_no_window() {
        let lanes = lanes();
        let _release = stuck_detection(lanes);
        let fake = Fake::default();
        *fake.answer.lock() = Some(Answer::Quit);
        let (list_events, probe_events) = (fake.events.clone(), fake.events.clone());
        let opened = start_before_window(
            lanes,
            &fake,
            (LONG, LONG),
            move || listing(&list_events, "gpus"),
            move || probing(&probe_events),
        );
        assert!(!opened.join().unwrap());
        assert_eq!(fake.events.all(), ["shown", "cancel"]);
    }

    #[test]
    fn a_cancel_as_the_driver_answers_still_quits() {
        let lanes = lanes();
        let release = stuck_detection(lanes);
        let fake = Fake::default();
        let answer = Arc::clone(&fake.answer);
        let list_events = fake.events.clone();
        // The user clicks Cancel while the probe runs, the moment before it
        // returns.
        let opened = start_before_window(
            lanes,
            &fake,
            (LONG, LONG),
            move || listing(&list_events, "gpus"),
            move || {
                let answer = Arc::clone(&answer);
                Box::new(move || *answer.lock() = Some(Answer::Quit))
            },
        );
        fake.events.wait_for("shown", 1);
        release.send(()).unwrap();
        assert!(!opened.join().unwrap());
        assert_eq!(
            fake.events.all(),
            ["shown", "list", "configure gpus", "cancel"]
        );
    }

    #[test]
    fn ok_hides_the_notice_until_the_reminder_and_the_end_of_the_wait_ends_it() {
        let fake = Fake::default();
        let prompt = Prompt::new(fake.clone());
        let start = Instant::now();
        prompt.raise(start);
        prompt.raise(start + Duration::from_secs(1));
        assert_eq!(fake.events.all(), ["shown"]);
        *fake.answer.lock() = Some(Answer::Wait);
        let ok_at = start + Duration::from_secs(2);
        assert_eq!(prompt.phase(ok_at), Phase::Waiting);
        prompt.raise(ok_at + REMIND_AFTER - Duration::from_millis(1));
        assert_eq!(fake.events.all(), ["shown", "ok"]);
        prompt.raise(ok_at + REMIND_AFTER);
        assert_eq!(fake.events.all(), ["shown", "ok", "shown"]);
        // Cancel on the second showing quits.
        *fake.answer.lock() = Some(Answer::Quit);
        assert_eq!(prompt.phase(ok_at + REMIND_AFTER), Phase::Quit);
        // Once the wait is over, nothing shows it again.
        let fake = Fake::default();
        let prompt = Prompt::new(fake.clone());
        prompt.raise(start);
        assert_eq!(prompt.finish(), Phase::Over);
        prompt.raise(start + REMIND_AFTER * 2);
        assert_eq!(fake.events.all(), ["shown", "closed"]);
    }

    const LIMIT: Duration = Duration::from_secs(4);
    const PROOF: Duration = Duration::from_secs(12);

    /// Looks at the window as the watcher does, each at a time in ms from
    /// the start: (ms, last UI pass in ms, held). The times the notice
    /// would be raised at.
    fn looks(script: &[(u64, Option<u64>, bool)]) -> Vec<u64> {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut watch = Watch::new(start, LIMIT, PROOF);
        script
            .iter()
            .filter(|(ms, pass, held)| watch.look(at(*ms), pass.map(at), *held))
            .map(|(ms, _, _)| *ms)
            .collect()
    }

    /// UI passes every 200 ms from `from` to `to` ms, looked at each 250 ms.
    fn passing(from: u64, to: u64) -> Vec<(u64, Option<u64>, bool)> {
        (from..=to)
            .step_by(250)
            .map(|ms| (ms, Some(ms / 200 * 200), false))
            .collect()
    }

    #[test]
    fn a_window_that_passes_and_draws_in_time_shows_no_notice() {
        // Confirmed after about 3.5 s, well before either bound.
        assert!(looks(&passing(0, 3_500)).is_empty());
    }

    #[test]
    fn a_failed_first_acquire_then_a_stuck_recreation_raises_the_notice_4_s_after_the_last_pass() {
        // One pass at 200 ms, whose frame's surface acquire failed; the
        // recreation at the start of the next paint never returns.
        let script: Vec<_> = (250..=4_500)
            .step_by(50)
            .map(|ms| (ms, Some(200), false))
            .collect();
        assert_eq!(looks(&script)[0], 4_200);
    }

    #[test]
    fn passes_without_a_proof_of_a_presented_frame_raise_the_notice_at_the_bound() {
        // The surface is gone and every paint returns before drawing: UI
        // passes go on, the screenshots are never answered.
        let raised = looks(&passing(0, 12_500));
        assert_eq!(raised.first(), Some(&12_000), "{raised:?}");
    }

    #[test]
    fn held_windows_suspend_both_bounds_and_count_again_from_when_they_are_let_go() {
        // One pass, then minimized: eframe runs no pass for a minimized
        // window, so only the live check sees it. Held for a minute: nothing.
        let mut script = vec![(100, Some(100), false)];
        script.extend((250..=60_000).step_by(250).map(|ms| (ms, Some(100), true)));
        assert!(looks(&script).is_empty());
        // Restored at 60 s, and no pass comes: the notice 4 s later.
        let mut stuck = script.clone();
        stuck.extend(
            (60_250..=64_500)
                .step_by(250)
                .map(|ms| (ms, Some(100), false)),
        );
        assert_eq!(looks(&stuck).first(), Some(&64_000));
        // Restored and passing, but never confirmed: the proof bound counts
        // from the release, not from the first pass.
        let mut unproven = script.clone();
        unproven.extend(passing(60_250, 72_500));
        assert_eq!(looks(&unproven).first(), Some(&72_000));
        // The title bar held while passes stop counts the same way.
        let mut dragged = passing(0, 2_000);
        dragged.extend(
            (2_250..=20_000)
                .step_by(250)
                .map(|ms| (ms, Some(2_000), true)),
        );
        assert!(looks(&dragged).is_empty());
    }

    #[test]
    fn cancel_while_the_window_is_stuck_quits_from_the_watcher() {
        let fake = Fake::default();
        let quits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&quits);
        *fake.answer.lock() = Some(Answer::Quit);
        // Nothing passes: the notice goes up at once with a zero limit.
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            Duration::ZERO,
            PROOF,
            SLICE,
            Instant::now,
            || None,
            || false,
            move || {
                count.fetch_add(1, SeqCst);
            },
        );
        watch.thread.unwrap().join().unwrap();
        assert_eq!(quits.load(SeqCst), 1);
        assert_eq!(fake.events.all(), ["shown", "cancel"]);
    }

    #[test]
    fn a_cancel_that_comes_with_the_window_drawing_still_quits() {
        let fake = Fake::default();
        let quits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&quits);
        let prompt = Prompt::new(fake.clone());
        prompt.raise(Instant::now());
        // The user clicks Cancel; before the watcher reads it, the renderer
        // confirms the window and ends the wait.
        *fake.answer.lock() = Some(Answer::Quit);
        assert_eq!(prompt.finish(), Phase::Quit);
        let watch = watch_first_paint(
            prompt,
            REMIND_AFTER,
            PROOF,
            SLICE,
            Instant::now,
            || None,
            || false,
            move || {
                count.fetch_add(1, SeqCst);
            },
        );
        watch.thread.unwrap().join().unwrap();
        assert_eq!(quits.load(SeqCst), 1);
        assert_eq!(fake.events.all(), ["shown", "cancel"]);
    }

    #[test]
    fn a_window_that_draws_while_the_watcher_decides_to_raise_shows_no_notice() {
        let fake = Fake::default();
        let start = Instant::now();
        // The watcher reads the clock just before it raises the notice. The
        // test holds that read until the renderer has ended the wait.
        let (reading, clock_read) = mpsc::channel::<()>();
        let (go, proceed) = mpsc::channel::<()>();
        let proceed = std::sync::Mutex::new(proceed);
        let reads = AtomicUsize::new(0);
        let now = move || {
            // Read 0 is the watcher's start, 1 its phase check; 2 is the
            // time it raises at.
            if reads.fetch_add(1, SeqCst) == 2 {
                let _ = reading.send(());
                let _ = proceed.lock().unwrap().recv();
            }
            start + SHORT.limit * 2
        };
        let never_quit = || panic!("quit");
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            SHORT.limit,
            PROOF,
            SLICE,
            now,
            || None,
            || false,
            never_quit,
        );
        clock_read.recv().unwrap();
        (watch.finisher())();
        go.send(()).unwrap();
        watch.stop().unwrap();
        assert!(fake.events.all().is_empty(), "{:?}", fake.events.all());
    }
}
