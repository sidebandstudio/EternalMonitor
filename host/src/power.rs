//! Windows putting the PC to sleep, shutting it down or signing the user
//! out while an iPad streams.
//!
//! The host said goodbye only when its window closed. A PC that slept or
//! shut down mid-stream went silent instead: the iPad showed a lost signal,
//! then tried an address that no longer answered, and no session summary
//! was written, so these drops never reached the diagnostics either. Now the
//! session ends when Windows says the PC is going down: its summary is
//! written first, and an iPad that understands it hears why.
//!
//! Windows waits for none of this for long: about two seconds after a
//! suspend message, and a few seconds after the end-of-session message before
//! it lists the app as holding up shutdown. Each step here is bounded well
//! inside that, and nothing ever asks Windows to wait.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tracing::info;

use crate::control::SharedControl;
use crate::transport::session::PowerEvent;

/// Longest a suspend message waits for the goodbye to leave.
pub const SLEEP_WAIT: Duration = Duration::from_millis(500);
/// Longest the end-of-session message waits for it.
pub const END_SESSION_WAIT: Duration = Duration::from_millis(1500);

/// Wakes the transport to send a goodbye the PC going down left, and counts
/// the goodbyes it sent, so the message can wait for its own.
#[derive(Debug, Default)]
pub struct Signal {
    wake: tokio::sync::Notify,
    said: AtomicU64,
}

impl Signal {
    /// The transport sent a goodbye.
    pub(crate) fn said(&self) {
        self.said.fetch_add(1, Ordering::SeqCst);
    }

    /// Resolves when a goodbye waits for the transport; a wake that came
    /// while the transport was busy is kept for its next look.
    pub(crate) async fn woken(&self) {
        self.wake.notified().await;
    }

    fn count(&self) -> u64 {
        self.said.load(Ordering::SeqCst)
    }
}

/// What Windows told the app, from its window messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// WM_POWERBROADCAST PBT_APMSUSPEND.
    Suspend,
    /// WM_POWERBROADCAST PBT_APMRESUMEAUTOMATIC or PBT_APMRESUMESUSPEND.
    Resume,
    /// WM_QUERYENDSESSION: Windows asks whether the session may end. Always
    /// yes, and nothing happens yet: another app can still call it off.
    QueryEndSession,
    /// WM_ENDSESSION: `ending` false when the shutdown was called off.
    EndSession { ending: bool, logoff: bool },
    /// WM_WTSSESSION_CHANGE WTS_SESSION_LOGOFF.
    SessionLogoff,
}

/// Acts on `message` and returns what the window answers Windows: 1 (TRUE)
/// for WM_POWERBROADCAST and WM_QUERYENDSESSION, 0 otherwise.
pub fn handle(shared: &SharedControl, message: Message) -> isize {
    match message {
        Message::Suspend => {
            going_down(shared, PowerEvent::Sleep, SLEEP_WAIT);
            1
        }
        Message::Resume => {
            back_up(shared, "The PC woke");
            1
        }
        Message::QueryEndSession => 1,
        Message::EndSession {
            ending: true,
            logoff,
        } => {
            let event = if logoff {
                PowerEvent::Logoff
            } else {
                PowerEvent::Shutdown
            };
            going_down(shared, event, END_SESSION_WAIT);
            0
        }
        Message::EndSession { ending: false, .. } => {
            back_up(shared, "The shutdown was called off");
            0
        }
        Message::SessionLogoff => {
            going_down(shared, PowerEvent::Logoff, END_SESSION_WAIT);
            0
        }
    }
}

/// Ends the session for `event`, writes its summary, and waits at most
/// `wait` for the transport to send the goodbye, if there is one.
fn going_down(shared: &SharedControl, event: PowerEvent, wait: Duration) {
    let started = Instant::now();
    let before = shared.power.count();
    let (end, told) = {
        let mut session = shared.session.lock();
        let end = session.power_down(started, event);
        (end, session.power_goodbye_pending())
    };
    // Only a running transport can send it.
    let sending = told && shared.transport_beat.read().is_some();
    if sending {
        shared.power.wake.notify_one();
    }
    let Some(end) = end else {
        info!(
            power = event.name(),
            "Windows is taking the PC down; no iPad was streaming"
        );
        return;
    };
    info!(
        power = event.name(),
        told, "Windows is taking the PC down — ended the iPad's session"
    );
    // A PC that sleeps comes back with this process and its log writers;
    // one that shuts down or signs out may end it any moment now.
    match event {
        PowerEvent::Sleep => crate::diagnostics::log_session_end(&end, shared),
        PowerEvent::Shutdown | PowerEvent::Logoff => {
            crate::diagnostics::log_session_end_now(&end, shared)
        }
    }
    if !sending {
        return;
    }
    while shared.power.count() == before && started.elapsed() < wait {
        std::thread::sleep(Duration::from_millis(5));
    }
    let sent = shared.power.count() != before;
    info!(
        power = event.name(),
        sent,
        waited_ms = started.elapsed().as_millis() as u64,
        "Goodbye before the PC goes down"
    );
}

fn back_up(shared: &SharedControl, why: &'static str) {
    shared.session.lock().power_up();
    info!("{why} — iPads may connect again");
}

/// Starts listening for Windows' sleep, shutdown and sign-out messages, on a
/// hidden window of its own. Later calls do nothing.
#[cfg(windows)]
pub fn watch(shared: SharedControl) {
    windows_impl::watch(shared);
}

/// The hidden window's handle, once it exists, for tests that send it
/// messages as Windows would.
#[cfg(windows)]
pub fn window() -> Option<isize> {
    windows_impl::window()
}

#[cfg(windows)]
mod windows_impl {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::OnceLock;

    use windows::core::w;
    use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Power::RegisterSuspendResumeNotification;
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        TranslateMessage, DEVICE_NOTIFY_WINDOW_HANDLE, ENDSESSION_LOGOFF, MSG,
        PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, WINDOW_EX_STYLE,
        WINDOW_STYLE, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_WTSSESSION_CHANGE,
        WNDCLASSW, WTS_SESSION_LOGOFF,
    };

    use super::Message;
    use crate::control::SharedControl;

    static SHARED: OnceLock<SharedControl> = OnceLock::new();
    static WINDOW: AtomicIsize = AtomicIsize::new(0);

    pub(super) fn watch(shared: SharedControl) {
        if SHARED.set(shared).is_err() {
            return;
        }
        let started = std::thread::Builder::new()
            .name("power-watch".into())
            .spawn(|| {
                if let Err(error) = unsafe { run() } {
                    tracing::warn!(
                        %error,
                        "Could not watch for sleep and shutdown; the iPad isn't told when the PC goes down"
                    );
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "Could not start watching for sleep and shutdown");
        }
    }

    pub(super) fn window() -> Option<isize> {
        Some(WINDOW.load(Ordering::SeqCst)).filter(|&hwnd| hwnd != 0)
    }

    unsafe fn run() -> windows::core::Result<()> {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("EternalMonitorPowerWatch");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&window_class);
        // A hidden top-level window: Windows sends the end-of-session
        // messages to top-level windows only, never to message-only ones.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("EternalMonitor power"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )?;
        // Sleep on a modern-standby PC reaches only registered windows.
        if let Err(error) =
            RegisterSuspendResumeNotification(HANDLE(hwnd.0), DEVICE_NOTIFY_WINDOW_HANDLE)
        {
            tracing::warn!(%error, "Could not register for sleep notices");
        }
        if let Err(error) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
            tracing::warn!(%error, "Could not register for sign-out notices");
        }
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        Ok(())
    }

    /// The message Windows sent, if it is one of those [`super::handle`] acts on.
    fn message(kind: u32, wparam: usize, lparam: isize) -> Option<Message> {
        Some(match kind {
            WM_POWERBROADCAST => match wparam as u32 {
                PBT_APMSUSPEND => Message::Suspend,
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => Message::Resume,
                _ => return None,
            },
            WM_QUERYENDSESSION => Message::QueryEndSession,
            WM_ENDSESSION => Message::EndSession {
                ending: wparam != 0,
                logoff: lparam as u32 & ENDSESSION_LOGOFF != 0,
            },
            WM_WTSSESSION_CHANGE if wparam as u32 == WTS_SESSION_LOGOFF => Message::SessionLogoff,
            _ => return None,
        })
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        kind: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match (message(kind, wparam.0, lparam.0), SHARED.get()) {
            (Some(message), Some(shared)) => LRESULT(super::handle(shared, message)),
            _ => DefWindowProcW(hwnd, kind, wparam, lparam),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn window_messages_map_to_what_windows_means() {
            let cases = [
                (
                    WM_POWERBROADCAST,
                    PBT_APMSUSPEND as usize,
                    0,
                    Some(Message::Suspend),
                ),
                (
                    WM_POWERBROADCAST,
                    PBT_APMRESUMEAUTOMATIC as usize,
                    0,
                    Some(Message::Resume),
                ),
                (
                    WM_POWERBROADCAST,
                    PBT_APMRESUMESUSPEND as usize,
                    0,
                    Some(Message::Resume),
                ),
                // A power setting change is someone else's business.
                (WM_POWERBROADCAST, 0x8013, 0, None),
                (WM_QUERYENDSESSION, 0, 0, Some(Message::QueryEndSession)),
                (
                    WM_ENDSESSION,
                    1,
                    0,
                    Some(Message::EndSession {
                        ending: true,
                        logoff: false,
                    }),
                ),
                (
                    WM_ENDSESSION,
                    1,
                    ENDSESSION_LOGOFF as i32 as isize,
                    Some(Message::EndSession {
                        ending: true,
                        logoff: true,
                    }),
                ),
                (
                    WM_ENDSESSION,
                    0,
                    0,
                    Some(Message::EndSession {
                        ending: false,
                        logoff: false,
                    }),
                ),
                (
                    WM_WTSSESSION_CHANGE,
                    WTS_SESSION_LOGOFF as usize,
                    1,
                    Some(Message::SessionLogoff),
                ),
                // A lock is not the PC going down.
                (WM_WTSSESSION_CHANGE, 7, 1, None),
            ];
            for (kind, wparam, lparam, expected) in cases {
                assert_eq!(
                    message(kind, wparam, lparam),
                    expected,
                    "{kind:#x} {wparam:#x}"
                );
            }
        }
    }
}
