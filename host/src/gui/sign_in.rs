//! A launch Windows started at sign-in ("Start with Windows"): the window
//! goes to the taskbar once it is confirmed drawing, unless it shows the
//! user something it needs (first-run setup, the update popup, a warning or
//! error) or the user takes it first. For [`GRACE`] after that, such a
//! state that appears brings it back once; after that it is like any window
//! the user minimised. The feedback ask and update news are not such states.

use std::time::{Duration, Instant};

use tracing::info;

/// How long after going to the taskbar a new warning brings the window back.
pub(super) const GRACE: Duration = Duration::from_secs(30);

/// The window's own activation, just after its first frame (eframe shows it
/// only then), is no user's: focus gained this soon after does not count.
const FOCUS_SETTLE: Duration = Duration::from_secs(1);

/// True once for each request from a later launch to show the window
/// (`single_instance::show_requests`).
pub type ShowRequests = Box<dyn FnMut() -> bool>;

/// What the window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Minimise,
    Restore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Not confirmed drawing yet: the graphics driver watch needs that proof
    /// from a visible window.
    Pending,
    /// Sent to the taskbar at this time.
    Grace(Instant),
    /// Like any window: nothing more happens on its own.
    Done,
}

pub(super) struct SignIn {
    phase: Phase,
    show_requests: ShowRequests,
    focused: Option<bool>,
    minimized: Option<bool>,
    /// The first pass after the window's first frame.
    shown_at: Option<Instant>,
}

impl SignIn {
    pub(super) fn new(show_requests: ShowRequests) -> Self {
        Self {
            phase: Phase::Pending,
            show_requests,
            focused: None,
            minimized: None,
            shown_at: None,
        }
    }

    pub(super) fn done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Sent to the taskbar, and a new notice would still bring it back.
    pub(super) fn in_grace(&self) -> bool {
        matches!(self.phase, Phase::Grace(_))
    }

    /// At every pass, and while minimised: the window's state (`frame` is
    /// egui's frame number) and requests from later launches. The user
    /// taking the window (a later launch, a restore, focus gained) ends it.
    pub(super) fn observe(
        &mut self,
        now: Instant,
        frame: u64,
        focused: Option<bool>,
        minimized: Option<bool>,
    ) {
        if self.done() {
            return;
        }
        let asked = (self.show_requests)();
        if frame >= 1 {
            self.shown_at.get_or_insert(now);
        }
        let settled = self
            .shown_at
            .is_some_and(|at| now.duration_since(at) >= FOCUS_SETTLE);
        let focus_gained = settled && self.focused == Some(false) && focused == Some(true);
        let restored = self.minimized == Some(true) && minimized == Some(false);
        self.focused = focused;
        self.minimized = minimized;
        if asked {
            self.end("EternalMonitor was started again");
        } else if restored {
            self.end("the window was restored");
        } else if focus_gained {
            // Windows may also activate a window late at sign-in, when
            // another startup app's window closes; the field log tells.
            self.end("the window was activated after its first second");
        }
    }

    /// A click or key press in the window.
    pub(super) fn user_input(&mut self) {
        if !self.done() {
            self.end("the window was used");
        }
    }

    /// Whether the window goes to the taskbar or comes back now. `drawing`:
    /// confirmed drawing. `needs_user`: first-run setup, the update popup or
    /// a warning or error banner is on screen (or would be, while minimised).
    pub(super) fn decide(
        &mut self,
        now: Instant,
        drawing: bool,
        needs_user: bool,
    ) -> Option<Command> {
        match self.phase {
            Phase::Pending if !drawing => None,
            Phase::Pending if needs_user => {
                self.end("it shows setup, the update popup or a warning");
                None
            }
            Phase::Pending => {
                info!("Started at sign-in: window minimised to the taskbar");
                self.phase = Phase::Grace(now);
                Some(Command::Minimise)
            }
            Phase::Grace(at) if now.duration_since(at) >= GRACE => {
                self.phase = Phase::Done;
                None
            }
            Phase::Grace(_) if needs_user => {
                info!("Started at sign-in: a warning appeared, so the window comes back");
                self.phase = Phase::Done;
                Some(Command::Restore)
            }
            Phase::Grace(_) | Phase::Done => None,
        }
    }

    fn end(&mut self, why: &str) {
        match self.phase {
            Phase::Pending => info!(why, "Started at sign-in: window stays on screen"),
            Phase::Grace(_) => info!(why, "Started at sign-in: the user took the window back"),
            Phase::Done => {}
        }
        self.phase = Phase::Done;
    }
}
