//! "Restart EternalMonitor": the window starts the new copy first, and only
//! leaves once that copy says it is running.
//!
//! The old window starts the new copy with [`WAIT_ARG`] and a random nonce.
//! Before anything else, the copy signals a named event made from the nonce,
//! then waits on the single-instance mutex until the old host has exited
//! (it never shows the old window and quits, as an ordinary second launch
//! does). The old window waits for that event, at most [`READY_TIMEOUT`],
//! then quits normally. Whatever the restart is for (a graphics card
//! preference, the window drawing method) is saved before the copy starts,
//! so any launch from then on, this copy or one the user starts meanwhile,
//! already uses it.
//!
//! Every way this can end without a window says so:
//! - The copy never signals (it could not start, or ended early): the old
//!   window stays, ends the copy if it still runs, and says to quit and open
//!   EternalMonitor again ([`restart_failed_message`]).
//! - The old host takes more than [`LOCK_TIMEOUT`] to exit: the copy says so
//!   and exits ([`still_closing_message`]).
//! - The copy fails between its signal and owning the mutex: a panic there
//!   shows the same restart message. That stretch runs no host code; a
//!   process killed in it ends silently, as one killed at any point would.
//!   From the mutex on, the copy is an ordinary host: a crash while it starts
//!   is shown by its failure reporter, as after any launch.

use std::sync::mpsc;
use std::time::Duration;

/// Added to the copy's arguments, with the nonce after it.
pub const WAIT_ARG: &str = "--relaunch-wait";
/// Added with what the restart applies ([`Reason::word`]), which only
/// changes what the messages say.
pub const REASON_ARG: &str = "--relaunch-for";

/// How long the old window waits for the copy to signal.
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the copy waits for the old host to exit. Closing can take 40 s
/// (the virtual display's Disable task may run 35 s, then its worker 5 s).
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(120);

/// What the restart applies, for the messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A graphics card preference, already saved in Windows.
    GraphicsCard,
    /// A Troubleshooting choice, already saved in settings.json.
    Settings,
    /// Nothing EternalMonitor changed (after the user changed something in
    /// another app).
    Plain,
}

impl Reason {
    fn word(self) -> &'static str {
        match self {
            Self::GraphicsCard => "gpu",
            Self::Settings => "settings",
            Self::Plain => "plain",
        }
    }

    fn from_word(word: &str) -> Self {
        match word {
            "gpu" => Self::GraphicsCard,
            "settings" => Self::Settings,
            _ => Self::Plain,
        }
    }

    /// The sentence that says the change is kept, with a space after it.
    fn saved(self) -> &'static str {
        match self {
            Self::GraphicsCard => "The graphics card setting is saved. ",
            Self::Settings => "Your change is saved. ",
            Self::Plain => "",
        }
    }
}

/// The title of the old window's notice when the copy never said it was
/// running.
pub const RESTART_FAILED_TITLE: &str = "EternalMonitor couldn't restart by itself";

/// The body of that notice, which offers Quit.
pub fn restart_failed_body(reason: Reason) -> String {
    format!("{}Quit EternalMonitor and open it again.", reason.saved())
}

/// The copy's message when the old host still runs after [`LOCK_TIMEOUT`].
pub fn still_closing_message(reason: Reason) -> String {
    format!(
        "EternalMonitor is still closing. {}Open EternalMonitor again in a minute.",
        reason.saved()
    )
}

/// The copy's message when it failed before it could start.
pub fn copy_failed_message(reason: Reason) -> String {
    format!(
        "EternalMonitor couldn't restart by itself. {}Open EternalMonitor again.",
        reason.saved()
    )
}

/// What a restart copy was started for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub nonce: String,
    pub reason: Reason,
}

/// A nonce is 32 lowercase hex digits; anything else isn't a restart.
fn valid_nonce(nonce: &str) -> bool {
    nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The readiness event for `nonce`, in this Windows session.
pub fn event_name(nonce: &str) -> String {
    format!(r"Local\EternalMonitor-ready-{nonce}")
}

/// The user's own arguments (a custom port is the first one), without the
/// ones a restart adds.
pub fn own_args<T: PartialEq<str>>(args: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut kept = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == *WAIT_ARG || arg == *REASON_ARG {
            args.next();
        } else {
            kept.push(arg);
        }
    }
    kept
}

/// The copy's arguments: the user's own, then the restart's.
pub fn copy_args(
    args: impl IntoIterator<Item = String>,
    nonce: &str,
    reason: Reason,
) -> Vec<String> {
    let mut kept = own_args(args);
    kept.extend([
        WAIT_ARG.to_string(),
        nonce.to_string(),
        REASON_ARG.to_string(),
        reason.word().to_string(),
    ]);
    kept
}

/// The restart this copy was started for, from its arguments. `None` for
/// an ordinary launch.
pub fn request(args: &[String]) -> Option<Request> {
    let after = |name: &str| {
        let index = args.iter().position(|arg| arg == name)?;
        args.get(index + 1)
    };
    let nonce = after(WAIT_ARG).filter(|nonce| valid_nonce(nonce))?;
    Some(Request {
        nonce: nonce.clone(),
        reason: after(REASON_ARG).map_or(Reason::Plain, |word| Reason::from_word(word)),
    })
}

/// How the wait for the copy's signal ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    Signalled,
    /// The copy ended without signalling.
    Exited,
    TimedOut,
}

/// What the old window needs from Windows. Tests fake it.
pub trait Launch {
    /// Starts the copy with its readiness event in place.
    fn start(&mut self, reason: Reason) -> Result<(), String>;
    fn wait_ready(&mut self, timeout: Duration) -> Ready;
    /// Ends a copy that never signalled, so it can't take over later.
    fn stop(&mut self);
}

/// The old window's side: start the copy and wait for its signal. `Ok`
/// means quit now; `Err` says why the window has to stay.
pub fn hand_over(launch: &mut impl Launch, reason: Reason) -> Result<(), String> {
    launch.start(reason)?;
    match launch.wait_ready(READY_TIMEOUT) {
        Ready::Signalled => Ok(()),
        Ready::Exited => Err("the new copy ended before it was ready".into()),
        Ready::TimedOut => {
            launch.stop();
            Err(format!(
                "the new copy wasn't ready after {} s",
                READY_TIMEOUT.as_secs()
            ))
        }
    }
}

/// What the copy needs from Windows. Tests fake it.
pub trait Takeover {
    type Guard;
    fn signal_ready(&mut self) -> Result<(), String>;
    /// The single-instance mutex once the old host has exited, or `None`
    /// when it still runs after `timeout`.
    fn wait_for_lock(&mut self, timeout: Duration) -> Option<Self::Guard>;
    /// Shows a message box and waits for OK.
    fn tell(&mut self, body: &str);
}

/// The copy's side: signal, then wait for the old host to exit. `None`
/// means exit now: the old window explains a failed signal, and this copy
/// explains anything after it.
pub fn take_over<T: Takeover>(host: &mut T, reason: Reason) -> Option<T::Guard> {
    host.signal_ready().ok()?;
    let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host.wait_for_lock(LOCK_TIMEOUT)
    }));
    match waited {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            host.tell(&still_closing_message(reason));
            None
        }
        Err(_) => {
            host.tell(&copy_failed_message(reason));
            None
        }
    }
}

/// The window's restart: at most one at a time, waited for off the UI
/// thread.
#[derive(Default)]
pub struct Restart {
    waiting: Option<(Reason, mpsc::Receiver<Result<(), String>>)>,
    failed: Option<Reason>,
}

impl Restart {
    /// A restart is waiting for its copy: every restart button does nothing.
    pub fn busy(&self) -> bool {
        self.waiting.is_some()
    }

    /// The last restart's copy never said it was running.
    pub fn failed(&self) -> Option<Reason> {
        self.failed
    }

    /// Runs `hand_over` on its own thread unless a restart already waits.
    /// Returns whether it started.
    pub fn begin(
        &mut self,
        reason: Reason,
        hand_over: impl FnOnce(Reason) -> Result<(), String> + Send + 'static,
    ) -> bool {
        if self.busy() {
            return false;
        }
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(hand_over(reason));
        });
        self.waiting = Some((reason, result));
        self.failed = None;
        true
    }

    /// True once the copy is running and the window should quit.
    pub fn poll(&mut self) -> bool {
        let Some((reason, result)) = &self.waiting else {
            return false;
        };
        let outcome = match result.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err("the restart stopped".into()),
        };
        let reason = *reason;
        self.waiting = None;
        match outcome {
            Ok(()) => {
                tracing::info!("Restart: the new copy is running; closing this window");
                true
            }
            Err(error) => {
                tracing::warn!(%error, "Restart: EternalMonitor couldn't restart by itself");
                self.failed = Some(reason);
                false
            }
        }
    }
}

#[cfg(windows)]
pub use windows_impl::{hand_over_now, take_over_if_requested};

#[cfg(not(windows))]
pub fn hand_over_now(_reason: Reason) -> Result<(), String> {
    Err("restarting EternalMonitor needs Windows".into())
}

/// No restart copies outside Windows.
#[cfg(not(windows))]
pub fn take_over_if_requested() -> Option<Option<crate::single_instance::Guard>> {
    None
}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::time::Duration;

    use windows::core::HSTRING;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};

    use super::{Launch, Ready, Reason, Takeover};

    fn raw(handle: &OwnedHandle) -> HANDLE {
        HANDLE(handle.as_raw_handle())
    }

    /// The readiness event for `nonce`, created unsignalled if it doesn't
    /// exist yet.
    fn event(nonce: &str) -> Result<OwnedHandle, String> {
        let name = HSTRING::from(super::event_name(nonce));
        let handle = unsafe { CreateEventW(None, true, false, &name) }
            .map_err(|error| format!("could not make the restart signal: {error}"))?;
        Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
    }

    fn nonce() -> Result<String, String> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    #[derive(Default)]
    struct Old {
        event: Option<OwnedHandle>,
        copy: Option<OwnedHandle>,
    }

    impl Launch for Old {
        /// Starts the copy inheriting no handles (the host holds inheritable
        /// sockets and driver handles) and outside this process's job object
        /// where the job allows it, so it outlives this process.
        fn start(&mut self, reason: Reason) -> Result<(), String> {
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            let nonce = nonce()?;
            self.event = Some(event(&nonce)?);
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            let mut line = eternal_updater::quote_arg(&exe.display().to_string());
            for arg in super::copy_args(std::env::args().skip(1), &nonce, reason) {
                line.push(' ');
                line.push_str(&eternal_updater::quote_arg(&arg));
            }
            let flags = CREATE_NEW_PROCESS_GROUP;
            let copy = eternal_updater::win::start_suspended(
                &exe,
                &line,
                flags | CREATE_BREAKAWAY_FROM_JOB,
                None,
            )
            .or_else(|_| eternal_updater::win::start_suspended(&exe, &line, flags, None))
            .and_then(|started| started.resume())
            .map_err(|error| format!("could not start EternalMonitor again: {error}"))?;
            self.copy = Some(copy);
            Ok(())
        }

        fn wait_ready(&mut self, timeout: Duration) -> Ready {
            let (Some(event), Some(copy)) = (&self.event, &self.copy) else {
                return Ready::Exited;
            };
            let millis = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
            let handles = [raw(event), raw(copy)];
            match unsafe { WaitForMultipleObjects(&handles, false, millis) } {
                WAIT_OBJECT_0 => Ready::Signalled,
                WAIT_TIMEOUT => Ready::TimedOut,
                _ => Ready::Exited,
            }
        }

        fn stop(&mut self) {
            if let Some(copy) = &self.copy {
                eternal_updater::win::terminate(copy);
            }
        }
    }

    /// The old window's whole restart, on a thread of its own.
    pub fn hand_over_now(reason: Reason) -> Result<(), String> {
        super::hand_over(&mut Old::default(), reason)
    }

    struct NewCopy {
        nonce: String,
    }

    impl Takeover for NewCopy {
        type Guard = crate::single_instance::Guard;

        fn signal_ready(&mut self) -> Result<(), String> {
            if crate::startup::simulate_relaunch_without_ready() {
                return Err("simulated".into());
            }
            let event = super::event(&self.nonce)?;
            unsafe { SetEvent(raw(&event)) }.map_err(|error| error.to_string())
        }

        fn wait_for_lock(&mut self, timeout: Duration) -> Option<Self::Guard> {
            crate::single_instance::take_over(timeout)
        }

        fn tell(&mut self, body: &str) {
            use windows::Win32::UI::WindowsAndMessaging::{
                MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
            };
            unsafe {
                MessageBoxW(
                    None,
                    &HSTRING::from(body),
                    &HSTRING::from("EternalMonitor"),
                    MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
                );
            }
        }
    }

    /// In a restart copy: signal the old window, then wait for it to exit.
    /// `None` for an ordinary launch; `Some(None)` when this copy must exit
    /// (it said why, or the old window does); `Some(Some(guard))` once it
    /// holds the single-instance mutex. Runs before logging, so a copy that
    /// exits never touches the session log.
    pub fn take_over_if_requested() -> Option<Option<crate::single_instance::Guard>> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let request = super::request(&args)?;
        Some(super::take_over(
            &mut NewCopy {
                nonce: request.nonce,
            },
            request.reason,
        ))
    }
}

#[cfg(test)]
mod tests;
