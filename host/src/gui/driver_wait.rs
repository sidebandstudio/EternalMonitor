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

/// Waits until none of `free` is stuck in `lanes` and `attempt` succeeds,
/// with the notice up from the first time either fails. False when the
/// user quit, also by a Cancel that came as the driver answered.
fn wait_until(
    lanes: &Lanes,
    free: &[Call],
    mut attempt: impl FnMut() -> bool,
    prompt: &Prompt,
    poll: Duration,
) -> bool {
    let started = Instant::now();
    loop {
        if free.iter().all(|&call| lanes.abandoned(call) == 0) && attempt() {
            if prompt.phase(Instant::now()) == Phase::Quit {
                break;
            }
            if prompt.raised() {
                info!(
                    waited_ms = started.elapsed().as_millis() as u64,
                    "The graphics driver answered; opening the window"
                );
            }
            return true;
        }
        prompt.raise(Instant::now());
        if prompt.phase(Instant::now()) == Phase::Quit {
            break;
        }
        std::thread::sleep(poll);
    }
    info!(
        waited_ms = started.elapsed().as_millis() as u64,
        "Quit while the window waited for the graphics driver"
    );
    false
}

/// The steps before the window's thread makes its first graphics call:
/// fresh lists from `list` run in [`Call::GpuDetect`] under `list_limits`
/// until one returns in time, which `configure` gets; then fresh probes from
/// `probe`, over what `configure` returned, run in [`Call::GpuProbe`] until
/// one returns in time. False when the user quit from the notice.
#[allow(clippy::too_many_arguments)]
pub(super) fn before_window<T, L, S, F>(
    lanes: &'static Lanes,
    list_limits: Limits,
    probe_limits: Limits,
    prompt: &Prompt,
    poll: Duration,
    mut list: impl FnMut() -> L,
    configure: impl FnOnce(T) -> S,
    probe: impl Fn(&S) -> F,
) -> bool
where
    T: Send + 'static,
    L: FnOnce() -> T + Send + 'static,
    F: FnOnce() + Send + 'static,
{
    let mut listed = None;
    let got_list = wait_until(
        lanes,
        &[Call::GpuDetect],
        || match crate::stall::run_on(lanes, Call::GpuDetect, list_limits, list(), || {}) {
            Ok(adapters) => {
                listed = Some(adapters);
                true
            }
            Err(_) => false,
        },
        prompt,
        poll,
    );
    let Some(adapters) = listed.filter(|_| got_list) else {
        return false;
    };
    let spec = configure(adapters);
    wait_until(
        lanes,
        &[Call::GpuDetect, Call::GpuProbe],
        || crate::stall::run_on(lanes, Call::GpuProbe, probe_limits, probe(&spec), || {}).is_ok(),
        prompt,
        poll,
    )
}

/// Watches the window from another thread until the wait ends
/// ([`FirstPaintWatch::finisher`], called when the window is confirmed
/// drawing, or [`FirstPaintWatch::stop`]). The notice goes up once the
/// window's thread has begun no UI pass (`last_pass`, with whether the
/// window was minimized then) for `limit`, counted from now before the
/// first; a minimized window draws nothing and is left alone. Cancel runs
/// `quit`, which ends the process: the window's thread may never return.
pub(super) fn watch_first_paint(
    prompt: Prompt,
    limit: Duration,
    poll: Duration,
    now: impl Fn() -> Instant + Send + 'static,
    last_pass: impl Fn() -> Option<(Instant, bool)> + Send + 'static,
    quit: impl FnOnce() + Send + 'static,
) -> FirstPaintWatch {
    let started = now();
    let watched = prompt.clone();
    let watch = move || loop {
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
        let (since, minimized) = last_pass().unwrap_or((started, false));
        if !minimized && at.saturating_duration_since(since.max(started)) >= limit {
            watched.raise(at);
        }
        std::thread::sleep(poll);
    };
    let thread = std::thread::Builder::new()
        .name("first-paint-watch".into())
        .spawn(watch)
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
    /// Cancel given by then still quits.
    pub(super) fn stop(self) {
        self.prompt.finish();
        if let Some(thread) = self.thread {
            let _ = thread.join();
        }
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
        // The box may not be up yet when the driver answers at once, so ask
        // again until its thread ends. Bounded: the window opens regardless.
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;

    const LIMITS: Limits = Limits {
        warn: Duration::from_millis(100),
        limit: Duration::from_millis(300),
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
            LIMITS,
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
        list: impl FnMut() -> ListStep + Send + 'static,
        probe: impl Fn() -> ProbeStep + Send + 'static,
    ) -> std::thread::JoinHandle<bool> {
        let prompt = Prompt::new(fake.clone());
        let events = fake.events.clone();
        std::thread::spawn(move || {
            let configured = events.clone();
            before_window(
                lanes,
                LIMITS,
                LIMITS,
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

    #[test]
    fn a_driver_that_answers_opens_the_window_without_a_notice() {
        let fake = Fake::default();
        let (list_events, probe_events) = (fake.events.clone(), fake.events.clone());
        let opened = start_before_window(
            lanes(),
            &fake,
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
    fn a_list_that_stalls_is_never_configured_and_the_next_one_is() {
        let lanes = lanes();
        let fake = Fake::default();
        let (started, list_started) = mpsc::channel::<()>();
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(Mutex::new(Some(held)));
        let (list_events, probe_events) = (fake.events.clone(), fake.events.clone());
        let opened = start_before_window(
            lanes,
            &fake,
            move || {
                let events = list_events.clone();
                let started = started.clone();
                // The first list blocks in the driver; the next answers.
                let held = held.lock().take();
                Box::new(move || {
                    let _ = started.send(());
                    match held {
                        Some(held) => {
                            let _ = held.recv();
                            events.push("stalled list returned");
                            "late".to_string()
                        }
                        None => {
                            events.push("list");
                            "gpus".to_string()
                        }
                    }
                })
            },
            move || probing(&probe_events),
        );
        list_started.recv().unwrap();
        fake.events.wait_for("shown", 1);
        assert_eq!(fake.events.all(), ["shown"]);
        release.send(()).unwrap();
        assert!(opened.join().unwrap());
        // The stalled list's result was dropped; the one that returned in
        // time configured the renderer, once.
        assert_eq!(
            fake.events.all(),
            [
                "shown",
                "stalled list returned",
                "list",
                "configure gpus",
                "probe"
            ]
        );
    }

    #[test]
    fn a_probe_that_hangs_raises_the_notice_and_the_next_one_opens_the_window() {
        let lanes = lanes();
        let fake = Fake::default();
        let (started, probe_started) = mpsc::channel::<()>();
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(Mutex::new(Some(held)));
        let (list_events, probe_events) = (fake.events.clone(), fake.events.clone());
        let opened = start_before_window(
            lanes,
            &fake,
            move || listing(&list_events, "gpus"),
            move || {
                let events = probe_events.clone();
                let started = started.clone();
                let held = held.lock().take();
                Box::new(move || {
                    events.push("probe");
                    let _ = started.send(());
                    if let Some(held) = held {
                        let _ = held.recv();
                        events.push("probe returned");
                    }
                })
            },
        );
        probe_started.recv().unwrap();
        fake.events.wait_for("shown", 1);
        release.send(()).unwrap();
        assert!(opened.join().unwrap());
        assert_eq!(
            fake.events.all(),
            [
                "list",
                "configure gpus",
                "probe",
                "shown",
                "probe returned",
                "probe"
            ]
        );
    }

    #[test]
    fn cancel_opens_no_window() {
        let lanes = lanes();
        let _release = stuck_detection(lanes);
        let fake = Fake::default();
        *fake.answer.lock() = Some(Answer::Quit);
        let (list_events, probe_events) = (fake.events.clone(), fake.events.clone());
        let opened = start_before_window(
            lanes,
            &fake,
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
        // returns in time.
        let opened = start_before_window(
            lanes,
            &fake,
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

    /// A clock the test moves on by hand.
    fn manual_clock() -> (Arc<AtomicU64>, impl Fn() -> Instant + Send + 'static) {
        let start = Instant::now();
        let ms = Arc::new(AtomicU64::new(0));
        let read = Arc::clone(&ms);
        (ms, move || start + Duration::from_millis(read.load(SeqCst)))
    }

    /// The renderer's view of the window: when its thread last began a UI
    /// pass, in the manual clock's time.
    #[derive(Clone)]
    struct Passes {
        start: Instant,
        last: Arc<Mutex<Option<(Instant, bool)>>>,
    }

    impl Passes {
        fn new(now: &impl Fn() -> Instant) -> Self {
            Self {
                start: now(),
                last: Arc::default(),
            }
        }
        fn pass_at(&self, ms: u64, minimized: bool) {
            *self.last.lock() = Some((self.start + Duration::from_millis(ms), minimized));
        }
        fn reader(&self) -> impl Fn() -> Option<(Instant, bool)> + Send + 'static {
            let last = Arc::clone(&self.last);
            move || *last.lock()
        }
    }

    fn never_quit() -> impl FnOnce() + Send + 'static {
        || panic!("quit")
    }

    #[test]
    fn a_window_that_draws_in_time_shows_no_notice() {
        let fake = Fake::default();
        let (clock, now) = manual_clock();
        let passes = Passes::new(&now);
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            LIMITS.limit,
            SLICE,
            now,
            passes.reader(),
            never_quit(),
        );
        // UI passes keep coming; then the window is confirmed drawing.
        for ms in [100, 300, 500, 700] {
            passes.pass_at(ms, false);
            clock.store(ms + 50, SeqCst);
            std::thread::sleep(Duration::from_millis(5));
        }
        (watch.finisher())();
        watch.stop();
        assert!(fake.events.all().is_empty(), "{:?}", fake.events.all());
    }

    #[test]
    fn a_failed_first_acquire_then_a_stuck_recreation_gets_the_notice_until_the_window_draws() {
        let fake = Fake::default();
        let (clock, now) = manual_clock();
        let passes = Passes::new(&now);
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            LIMITS.limit,
            SLICE,
            now,
            passes.reader(),
            never_quit(),
        );
        // The first pass ran and its frame's surface acquire failed, so the
        // renderer did not confirm the window; recreating the surface at the
        // start of the next paint never returns: no more passes.
        passes.pass_at(200, false);
        let stuck_at = 200 + LIMITS.limit.as_millis() as u64;
        clock.store(stuck_at - 1, SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        assert!(fake.events.all().is_empty(), "{:?}", fake.events.all());
        clock.store(stuck_at, SeqCst);
        fake.events.wait_for("shown", 1);
        // OK, then the reminder while it is still stuck.
        *fake.answer.lock() = Some(Answer::Wait);
        fake.events.wait_for("ok", 1);
        clock.fetch_add(REMIND_AFTER.as_millis() as u64, SeqCst);
        fake.events.wait_for("shown", 2);
        // It comes back and is confirmed drawing: the notice goes.
        (watch.finisher())();
        watch.thread.unwrap().join().unwrap();
        assert_eq!(fake.events.all(), ["shown", "ok", "shown", "closed"]);
    }

    #[test]
    fn a_minimized_window_gets_no_notice() {
        let fake = Fake::default();
        let (clock, now) = manual_clock();
        let passes = Passes::new(&now);
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            LIMITS.limit,
            SLICE,
            now,
            passes.reader(),
            never_quit(),
        );
        passes.pass_at(100, true);
        clock.store(100 + LIMITS.limit.as_millis() as u64 * 10, SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        watch.stop();
        assert!(fake.events.all().is_empty(), "{:?}", fake.events.all());
    }

    #[test]
    fn cancel_while_the_window_is_stuck_quits_from_the_watcher() {
        let fake = Fake::default();
        let (clock, now) = manual_clock();
        clock.store(LIMITS.limit.as_millis() as u64, SeqCst);
        let quits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&quits);
        *fake.answer.lock() = Some(Answer::Quit);
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            Duration::ZERO,
            SLICE,
            now,
            || None,
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
        let (_clock, now) = manual_clock();
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
            SLICE,
            now,
            || None,
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
            start + LIMITS.limit * 2
        };
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            LIMITS.limit,
            SLICE,
            now,
            || None,
            never_quit(),
        );
        clock_read.recv().unwrap();
        (watch.finisher())();
        go.send(()).unwrap();
        watch.thread.unwrap().join().unwrap();
        assert!(fake.events.all().is_empty(), "{:?}", fake.events.all());
    }
}
