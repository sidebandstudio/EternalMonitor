//! The window waits for a graphics driver that answers, and says so.
//!
//! The window's thread lists adapters, creates its device and draws through
//! the graphics driver, and Windows cannot stop a thread stuck inside it.
//! So before that thread makes its first graphics call ([`before_window`]):
//! - the startup graphics lane ([`Call::GpuDetect`]) must be free: a GPU
//!   detection or screen list given up on may still be stuck there, and the
//!   renderer's crash avoidance reads the GPU list through the same lane;
//! - then the same wgpu calls the window's thread makes first are made once
//!   on a thread of their own ([`Call::GpuProbe`]), under its limit.
//!
//! Past that, the window's thread can still stall in calls that cannot be
//! moved off it (its own device and swap chain), so [`watch_first_paint`]
//! watches from another thread until its first frame is presented.
//!
//! Whenever the window waits on the driver, a notice drawn by Windows
//! without the graphics card says why ([`Prompt`]). It stays until the
//! first frame. OK hides it for a while; Cancel quits.
// Only Windows waits; the logic is tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::atomic::{AtomicBool, Ordering};
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

/// The notice while startup waits on the driver, shared by the steps on
/// the window's thread and the first-paint watcher: up from the first wait,
/// back [`REMIND_AFTER`] after OK, down for good at the first frame.
#[derive(Clone)]
pub(super) struct Prompt(Arc<Mutex<PromptState>>);

struct PromptState {
    notice: Box<dyn Notice>,
    up: bool,
    raised: bool,
    hidden_at: Option<Instant>,
    quit: bool,
    done: bool,
}

impl Prompt {
    pub(super) fn new(notice: impl Notice + 'static) -> Self {
        Self(Arc::new(Mutex::new(PromptState {
            notice: Box::new(notice),
            up: false,
            raised: false,
            hidden_at: None,
            quit: false,
            done: false,
        })))
    }

    /// Show the notice at `now`, unless it is up, OK hid it less than
    /// [`REMIND_AFTER`] ago, or the wait is over.
    pub(super) fn raise(&self, now: Instant) {
        let mut state = self.0.lock();
        state.read_answer(now);
        if state.up || state.quit || state.done {
            return;
        }
        if state
            .hidden_at
            .is_some_and(|at| now.saturating_duration_since(at) < REMIND_AFTER)
        {
            return;
        }
        state.notice.show();
        state.up = true;
        state.raised = true;
        state.hidden_at = None;
    }

    /// The user chose Cancel, by `now`.
    pub(super) fn quit(&self, now: Instant) -> bool {
        let mut state = self.0.lock();
        state.read_answer(now);
        state.quit
    }

    /// The notice was ever shown.
    pub(super) fn raised(&self) -> bool {
        self.0.lock().raised
    }

    /// The wait is over: take the notice down and keep it down.
    pub(super) fn close(&self) {
        let mut state = self.0.lock();
        state.done = true;
        state.up = false;
        state.notice.close();
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
                self.quit = true;
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
            if prompt.quit(Instant::now()) {
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
        if !prompt.raised() {
            warn!("The graphics driver is not answering; the window opens once it does");
        }
        prompt.raise(Instant::now());
        if prompt.quit(Instant::now()) {
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

/// The steps before the window's thread makes its first graphics call.
/// First the startup lane frees, so `configure` reads the GPU list crash
/// avoidance keys its history by. Then fresh probes from `probe`, over what
/// `configure` returned, run in [`Call::GpuProbe`] under `limits` until one
/// returns in time. False when the user quit from the notice.
pub(super) fn before_window<S, F>(
    lanes: &'static Lanes,
    limits: Limits,
    prompt: &Prompt,
    poll: Duration,
    configure: impl FnOnce() -> S,
    probe: impl Fn(&S) -> F,
) -> bool
where
    F: FnOnce() + Send + 'static,
{
    if !wait_until(lanes, &[Call::GpuDetect], || true, prompt, poll) {
        return false;
    }
    let spec = configure();
    wait_until(
        lanes,
        &[Call::GpuDetect, Call::GpuProbe],
        || crate::stall::run_on(lanes, Call::GpuProbe, limits, probe(&spec), || {}).is_ok(),
        prompt,
        poll,
    )
}

/// Watches the window's first frame from another thread, from now: the
/// notice goes up if `painted` is still false after `limit` (or already
/// was), and down for good once it is true. Cancel runs `quit`, which ends
/// the process: the window's thread may never return.
pub(super) fn watch_first_paint(
    prompt: Prompt,
    limit: Duration,
    poll: Duration,
    now: impl Fn() -> Instant + Send + 'static,
    painted: impl Fn() -> bool + Send + 'static,
    quit: impl FnOnce() + Send + 'static,
) -> FirstPaintWatch {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let started = now();
    let watch = move || {
        let mut warned = false;
        loop {
            if painted() || stopped.load(Ordering::SeqCst) {
                prompt.close();
                return;
            }
            let at = now();
            if prompt.raised() || at.saturating_duration_since(started) >= limit {
                if !warned && !prompt.raised() {
                    warn!(
                        limit_ms = limit.as_millis() as u64,
                        "The window has not drawn its first frame; the graphics driver may not be answering"
                    );
                }
                warned = true;
                prompt.raise(at);
            }
            if prompt.quit(at) {
                info!("Quit while the window waited for its first frame");
                quit();
                return;
            }
            std::thread::sleep(poll);
        }
    };
    let thread = std::thread::Builder::new()
        .name("first-paint-watch".into())
        .spawn(watch)
        .map_err(|error| warn!(%error, "Could not watch the window's first frame"))
        .ok();
    FirstPaintWatch { stop, thread }
}

/// A running [`watch_first_paint`].
pub(super) struct FirstPaintWatch {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FirstPaintWatch {
    /// The window ended without a first frame: take the notice down.
    pub(super) fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
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
        use std::sync::atomic::AtomicU32;
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
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
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
    struct Events(Arc<Mutex<Vec<&'static str>>>);

    impl Events {
        fn push(&self, event: &'static str) {
            self.0.lock().push(event);
        }
        fn all(&self) -> Vec<&'static str> {
            self.0.lock().clone()
        }
        fn has(&self, event: &str) -> bool {
            self.0.lock().contains(&event)
        }
        /// Backstop only; each wait ends as soon as the event happens.
        fn wait_for(&self, event: &str) {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !self.has(event) {
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

    /// [`before_window`] on its own thread, with `configure` and each probe
    /// recorded.
    fn start_before_window(
        lanes: &'static Lanes,
        fake: &Fake,
        probe: impl Fn() -> Box<dyn FnOnce() + Send> + Send + 'static,
    ) -> std::thread::JoinHandle<bool> {
        let prompt = Prompt::new(fake.clone());
        let events = fake.events.clone();
        std::thread::spawn(move || {
            let configured = events.clone();
            before_window(
                lanes,
                LIMITS,
                &prompt,
                SLICE,
                move || {
                    // Crash avoidance reads the GPU list only once the
                    // startup lane is free.
                    assert_eq!(lanes.abandoned(Call::GpuDetect), 0);
                    configured.push("configure");
                },
                |_| probe(),
            )
        })
    }

    #[test]
    fn a_driver_that_answers_opens_the_window_without_a_notice() {
        let fake = Fake::default();
        let events = fake.events.clone();
        let opened = start_before_window(lanes(), &fake, move || {
            let events = events.clone();
            Box::new(move || events.push("probe"))
        });
        assert!(opened.join().unwrap());
        assert_eq!(fake.events.all(), ["configure", "probe"]);
    }

    #[test]
    fn while_detection_is_stuck_nothing_reads_the_gpus_and_the_notice_is_up() {
        let lanes = lanes();
        let release = stuck_detection(lanes);
        let fake = Fake::default();
        let events = fake.events.clone();
        let opened = start_before_window(lanes, &fake, move || {
            let events = events.clone();
            Box::new(move || events.push("probe"))
        });
        fake.events.wait_for("shown");
        // Nothing went near the stuck driver.
        assert_eq!(fake.events.all(), ["shown"]);
        release.send(()).unwrap();
        assert!(opened.join().unwrap());
        // The notice stays up for the first-paint watcher to take down.
        assert_eq!(fake.events.all(), ["shown", "configure", "probe"]);
    }

    #[test]
    fn a_probe_that_hangs_raises_the_notice_and_the_next_one_opens_the_window() {
        let lanes = lanes();
        let fake = Fake::default();
        let (started, probe_started) = mpsc::channel::<()>();
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(Mutex::new(Some(held)));
        let events = fake.events.clone();
        let opened = start_before_window(lanes, &fake, move || {
            let events = events.clone();
            let started = started.clone();
            // The first probe blocks in the driver; the next answers.
            let held = held.lock().take();
            Box::new(move || {
                events.push("probe");
                let _ = started.send(());
                if let Some(held) = held {
                    let _ = held.recv();
                    events.push("probe returned");
                }
            })
        });
        probe_started.recv().unwrap();
        fake.events.wait_for("shown");
        assert!(!fake.events.has("probe returned"));
        release.send(()).unwrap();
        assert!(opened.join().unwrap());
        let events = fake.events.all();
        assert_eq!(events[..3], ["configure", "probe", "shown"], "{events:?}");
        // The second probe ran once the first had returned and freed its lane.
        assert_eq!(events.iter().filter(|e| **e == "probe").count(), 2);
        let returned = events.iter().position(|e| *e == "probe returned").unwrap();
        assert_eq!(events[returned + 1], "probe", "{events:?}");
    }

    #[test]
    fn cancel_opens_no_window() {
        let lanes = lanes();
        let _release = stuck_detection(lanes);
        let fake = Fake::default();
        *fake.answer.lock() = Some(Answer::Quit);
        let opened = start_before_window(lanes, &fake, || Box::new(|| {}));
        assert!(!opened.join().unwrap());
        assert_eq!(fake.events.all(), ["shown", "cancel"]);
    }

    #[test]
    fn a_cancel_as_the_driver_answers_still_quits() {
        let lanes = lanes();
        let release = stuck_detection(lanes);
        let fake = Fake::default();
        let answer = Arc::clone(&fake.answer);
        // The user clicks Cancel while the probe runs, the moment before it
        // returns in time.
        let opened = start_before_window(lanes, &fake, move || {
            let answer = Arc::clone(&answer);
            Box::new(move || *answer.lock() = Some(Answer::Quit))
        });
        fake.events.wait_for("shown");
        release.send(()).unwrap();
        assert!(!opened.join().unwrap());
        assert_eq!(fake.events.all(), ["shown", "configure", "cancel"]);
    }

    #[test]
    fn ok_hides_the_notice_until_the_reminder_and_closing_ends_it() {
        let fake = Fake::default();
        let prompt = Prompt::new(fake.clone());
        let start = Instant::now();
        prompt.raise(start);
        prompt.raise(start + Duration::from_secs(1));
        assert_eq!(fake.events.all(), ["shown"]);
        *fake.answer.lock() = Some(Answer::Wait);
        let ok_at = start + Duration::from_secs(2);
        assert!(!prompt.quit(ok_at));
        prompt.raise(ok_at + REMIND_AFTER - Duration::from_millis(1));
        assert_eq!(fake.events.all(), ["shown", "ok"]);
        prompt.raise(ok_at + REMIND_AFTER);
        assert_eq!(fake.events.all(), ["shown", "ok", "shown"]);
        // Cancel on the second showing quits.
        *fake.answer.lock() = Some(Answer::Quit);
        assert!(prompt.quit(ok_at + REMIND_AFTER));
        // Once the wait is over, nothing shows it again.
        let fake = Fake::default();
        let prompt = Prompt::new(fake.clone());
        prompt.raise(start);
        prompt.close();
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

    #[test]
    fn a_window_that_paints_in_time_shows_no_notice() {
        let fake = Fake::default();
        let (_clock, now) = manual_clock();
        let painted = Arc::new(AtomicBool::new(true));
        let seen = Arc::clone(&painted);
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            LIMITS.limit,
            SLICE,
            now,
            move || seen.load(SeqCst),
            || panic!("quit"),
        );
        watch.thread.unwrap().join().unwrap();
        assert!(fake.events.all().is_empty());
    }

    #[test]
    fn a_window_stuck_before_its_first_frame_gets_the_notice_until_it_paints() {
        let fake = Fake::default();
        let (clock, now) = manual_clock();
        let painted = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&painted);
        let watch = watch_first_paint(
            Prompt::new(fake.clone()),
            LIMITS.limit,
            SLICE,
            now,
            move || seen.load(SeqCst),
            || panic!("quit"),
        );
        // Just under the limit: nothing yet.
        clock.store(LIMITS.limit.as_millis() as u64 - 1, SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        assert!(fake.events.all().is_empty());
        clock.store(LIMITS.limit.as_millis() as u64, SeqCst);
        fake.events.wait_for("shown");
        // OK, then the reminder.
        *fake.answer.lock() = Some(Answer::Wait);
        fake.events.wait_for("ok");
        clock.fetch_add(REMIND_AFTER.as_millis() as u64, SeqCst);
        let deadline = Instant::now() + Duration::from_secs(60);
        while fake.events.all().len() < 3 {
            assert!(Instant::now() < deadline, "{:?}", fake.events.all());
            std::thread::sleep(Duration::from_millis(1));
        }
        // The first frame takes it down.
        painted.store(true, SeqCst);
        watch.thread.unwrap().join().unwrap();
        assert_eq!(fake.events.all(), ["shown", "ok", "shown", "closed"]);
    }

    #[test]
    fn cancel_while_the_first_frame_is_stuck_quits_from_the_watcher() {
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
    fn a_notice_raised_before_the_window_stays_until_its_first_frame() {
        let fake = Fake::default();
        let prompt = Prompt::new(fake.clone());
        prompt.raise(Instant::now());
        let (_clock, now) = manual_clock();
        let painted = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&painted);
        let watch = watch_first_paint(
            prompt,
            REMIND_AFTER * 4,
            SLICE,
            now,
            move || seen.load(SeqCst),
            || panic!("quit"),
        );
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(fake.events.all(), ["shown"]);
        painted.store(true, SeqCst);
        watch.thread.unwrap().join().unwrap();
        assert_eq!(fake.events.all(), ["shown", "closed"]);
    }
}
