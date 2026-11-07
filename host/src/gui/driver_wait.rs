//! The window waits for a graphics driver that answers before it opens.
//!
//! The window's thread lists adapters, creates its device and draws through
//! the graphics driver, and Windows cannot stop a thread stuck inside it. So
//! before that thread makes its first graphics call, the same calls are made
//! once on a thread of their own, in the startup graphics lane
//! ([`Call::GpuDetect`]), under its limit. While they do not return, or an
//! earlier startup call in that lane is still stuck (GPU detection, the
//! screen list), a notice drawn by Windows without the graphics card says
//! why the window is not there yet, and the window opens once they return.
// Only Windows waits; the logic is tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::stall::{Call, Lanes, Limits};

/// How often a waiting launch looks whether the stuck call has returned.
pub(super) const POLL: Duration = Duration::from_millis(250);

/// What the user sees while the window waits.
pub(super) trait Notice {
    fn show(&mut self);
    /// The user chose to quit instead of waiting.
    fn quit(&mut self) -> bool;
    fn close(&mut self);
}

/// Runs a fresh `probe` in `lanes` until one returns within `limits`, with
/// `notice` up from the first one that does not. False when the user quit
/// from the notice.
pub(super) fn wait_for_driver<F>(
    lanes: &'static Lanes,
    limits: Limits,
    mut probe: impl FnMut() -> F,
    notice: &mut impl Notice,
    poll: Duration,
) -> bool
where
    F: FnOnce() + Send + 'static,
{
    let started = Instant::now();
    let mut shown = false;
    loop {
        // While an earlier call is stuck, a new one would only be refused.
        if lanes.abandoned(Call::GpuDetect) == 0
            && crate::stall::run_on(lanes, Call::GpuDetect, limits, probe(), || {}).is_ok()
        {
            if shown {
                notice.close();
                info!(
                    waited_ms = started.elapsed().as_millis() as u64,
                    "The graphics driver answered; opening the window"
                );
            }
            return true;
        }
        if !shown {
            shown = true;
            notice.show();
            warn!("The graphics driver is not answering; the window opens once it does");
        }
        if notice.quit() {
            info!(
                waited_ms = started.elapsed().as_millis() as u64,
                "Quit while the window waited for the graphics driver"
            );
            return false;
        }
        std::thread::sleep(poll);
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
    thread: std::sync::Arc<std::sync::atomic::AtomicU32>,
    answer: std::thread::JoinHandle<windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT>,
}

#[cfg(windows)]
const NOTICE: &str = "The graphics driver is not responding, so the EternalMonitor window can't open yet. It opens on its own as soon as the driver answers.\n\nIf this stays, restart the PC or update the graphics driver from NVIDIA, AMD or Intel.\n\nClick OK to keep waiting, or Cancel to quit EternalMonitor.";

#[cfg(windows)]
impl Notice for MessageNotice {
    fn show(&mut self) {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
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

    fn quit(&mut self) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IDCANCEL;
        // OK, or a box that could not be shown, keeps waiting.
        if !self.shown.as_ref().is_some_and(|s| s.answer.is_finished()) {
            return false;
        }
        let shown = self.shown.take().expect("checked above");
        shown.answer.join().is_ok_and(|answer| answer == IDCANCEL)
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
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::{mpsc, Arc};

    const LIMITS: Limits = Limits {
        warn: Duration::from_millis(100),
        limit: Duration::from_millis(300),
    };
    const SLICE: Duration = Duration::from_millis(10);

    fn lanes() -> &'static Lanes {
        Box::leak(Box::new(Lanes::new()))
    }

    /// When the notice was shown and closed, from the start of the test.
    #[derive(Default)]
    struct Recorded {
        started: Option<Instant>,
        shown: Option<Duration>,
        closed: Option<Duration>,
        quit_after_shown: bool,
    }

    impl Notice for Recorded {
        fn show(&mut self) {
            self.shown = Some(self.started.unwrap().elapsed());
        }
        fn quit(&mut self) -> bool {
            self.quit_after_shown
        }
        fn close(&mut self) {
            self.closed = Some(self.started.unwrap().elapsed());
        }
    }

    fn notice() -> Recorded {
        Recorded {
            started: Some(Instant::now()),
            ..Default::default()
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

    #[test]
    fn a_driver_that_answers_opens_the_window_without_a_notice() {
        let mut notice = notice();
        let probes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&probes);
        let opened = wait_for_driver(
            lanes(),
            LIMITS,
            || {
                let count = Arc::clone(&count);
                move || {
                    count.fetch_add(1, SeqCst);
                }
            },
            &mut notice,
            SLICE,
        );
        assert!(opened);
        assert_eq!(probes.load(SeqCst), 1);
        assert!(notice.shown.is_none() && notice.closed.is_none());
    }

    #[test]
    fn while_detection_is_stuck_the_notice_shows_at_once_and_the_window_follows_it() {
        let lanes = lanes();
        let release = stuck_detection(lanes);
        let probes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&probes);
        let waiter = std::thread::spawn(move || {
            let mut notice = notice();
            let opened = wait_for_driver(
                lanes,
                LIMITS,
                || {
                    let count = Arc::clone(&count);
                    move || {
                        count.fetch_add(1, SeqCst);
                    }
                },
                &mut notice,
                SLICE,
            );
            (opened, notice)
        });
        // No probe joins the stuck driver.
        std::thread::sleep(LIMITS.limit * 2);
        assert_eq!(probes.load(SeqCst), 0);
        release.send(()).unwrap();
        let (opened, notice) = waiter.join().unwrap();
        assert!(opened);
        // Shown without waiting out another limit.
        let shown = notice.shown.expect("no notice");
        assert!(shown < LIMITS.limit, "{shown:?}");
        // Closed once the driver answered, and the window's probe ran then.
        assert!(notice.closed.expect("not closed") >= LIMITS.limit);
        assert_eq!(probes.load(SeqCst), 1);
    }

    #[test]
    fn a_probe_that_hangs_shows_the_notice_at_its_limit() {
        let lanes = lanes();
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(parking_lot::Mutex::new(Some(held)));
        let waiter = std::thread::spawn(move || {
            let mut notice = notice();
            let opened = wait_for_driver(
                lanes,
                LIMITS,
                || {
                    // The first probe blocks in the driver; the next answers.
                    let held = held.lock().take();
                    move || {
                        if let Some(held) = held {
                            let _ = held.recv();
                        }
                    }
                },
                &mut notice,
                SLICE,
            );
            (opened, notice)
        });
        std::thread::sleep(LIMITS.limit * 3);
        release.send(()).unwrap();
        let (opened, notice) = waiter.join().unwrap();
        assert!(opened);
        let shown = notice.shown.expect("no notice");
        assert!(
            shown >= LIMITS.limit && shown < LIMITS.limit + Duration::from_secs(1),
            "{shown:?}"
        );
        assert!(notice.closed.expect("not closed") >= LIMITS.limit * 2);
    }

    #[test]
    fn quitting_from_the_notice_opens_no_window() {
        let lanes = lanes();
        let _release = stuck_detection(lanes);
        let mut notice = Recorded {
            quit_after_shown: true,
            ..notice()
        };
        assert!(!wait_for_driver(
            lanes,
            LIMITS,
            || || {},
            &mut notice,
            SLICE
        ));
        assert!(notice.shown.is_some());
        assert!(notice.closed.is_none());
    }
}
