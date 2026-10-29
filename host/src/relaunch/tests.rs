use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use super::*;
use crate::single_instance::{claim, Claim, NamedMutex};

fn secs(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds)
}

/// One PC during a restart, on a fake clock: the single-instance mutex as
/// Windows keeps it (it lives while any handle is open and belongs to one
/// process at a time), and when the old host exits once it quits.
#[derive(Default)]
struct Pc {
    now: Duration,
    owner: Option<&'static str>,
    handles: u32,
    /// How long the old host takes to exit after its window closes.
    old_exit_takes: Option<Duration>,
    old_exits_at: Option<Duration>,
}

type Shared = Rc<RefCell<Pc>>;

/// The old host is running and owns the mutex.
fn pc(old_exit_takes: f64) -> Shared {
    Rc::new(RefCell::new(Pc {
        owner: Some("old"),
        handles: 1,
        old_exit_takes: Some(secs(old_exit_takes)),
        ..Pc::default()
    }))
}

/// The old window closes now; the old host exits when its shutdown is done.
fn old_quits(pc: &Shared) {
    let mut pc = pc.borrow_mut();
    pc.old_exits_at = pc.old_exit_takes.map(|takes| pc.now + takes);
}

/// One process's view of the mutex.
struct Mutex {
    pc: Shared,
    process: &'static str,
}

impl NamedMutex for Mutex {
    type Handle = ();

    fn create(&mut self) -> Result<((), bool), String> {
        let mut pc = self.pc.borrow_mut();
        pc.handles += 1;
        if pc.handles == 1 {
            pc.owner = Some(self.process);
            return Ok(((), false));
        }
        Ok(((), true))
    }

    fn wait(&mut self, _: &(), timeout: Duration) -> Result<bool, String> {
        let mut pc = self.pc.borrow_mut();
        let exits_at = match pc.owner {
            None => Some(pc.now),
            Some("old") => pc.old_exits_at,
            // Any other host runs on.
            Some(_) => None,
        };
        match exits_at {
            Some(at) if at <= pc.now + timeout => {
                pc.now = pc.now.max(at);
                // Abandoned with its owner's exit, then ours.
                if pc.owner == Some("old") {
                    pc.handles -= 1;
                }
                pc.owner = Some(self.process);
                Ok(true)
            }
            _ => {
                pc.now += timeout;
                Ok(false)
            }
        }
    }

    fn close(&mut self, _: ()) {
        let mut pc = self.pc.borrow_mut();
        pc.handles -= 1;
        if pc.handles == 0 {
            pc.owner = None;
        }
    }
}

/// A launch from the Start menu: the ordinary check, which never waits.
fn manual_launch(pc: &Shared) -> bool {
    let started = pc.borrow().now;
    let mut mutex = Mutex {
        pc: pc.clone(),
        process: "manual",
    };
    let won = matches!(claim(&mut mutex, None), Claim::Owned(()));
    assert_eq!(pc.borrow().now, started, "a manual launch waited");
    won
}

/// What the copy does once started: signal after `ready_after`, or end
/// first at `ends_after` (it couldn't start, or crashed early).
#[derive(Clone, Copy)]
struct CopyPlan {
    ready_after: Option<f64>,
    ends_after: Option<f64>,
}

const READY: CopyPlan = CopyPlan {
    ready_after: Some(0.4),
    ends_after: None,
};

/// The old window's view: starting the copy and waiting for its signal.
struct OldWindow {
    pc: Shared,
    plan: CopyPlan,
    start_error: Option<String>,
    starts: u32,
    stopped: bool,
}

impl OldWindow {
    fn new(pc: &Shared, plan: CopyPlan) -> Self {
        Self {
            pc: pc.clone(),
            plan,
            start_error: None,
            starts: 0,
            stopped: false,
        }
    }
}

impl Launch for OldWindow {
    fn start(&mut self, _reason: Reason) -> Result<(), String> {
        if let Some(error) = &self.start_error {
            return Err(error.clone());
        }
        self.starts += 1;
        Ok(())
    }

    fn wait_ready(&mut self, timeout: Duration) -> Ready {
        let mut pc = self.pc.borrow_mut();
        let within = |at: Option<f64>| at.map(secs).filter(|at| *at <= timeout);
        if let Some(at) = within(self.plan.ready_after) {
            pc.now += at;
            Ready::Signalled
        } else if let Some(at) = within(self.plan.ends_after) {
            pc.now += at;
            Ready::Exited
        } else {
            pc.now += timeout;
            Ready::TimedOut
        }
    }

    fn stop(&mut self) {
        self.stopped = true;
    }
}

/// The copy's view: its signal (already counted in the old window's wait),
/// the mutex, and its message box.
struct NewCopy {
    mutex: Mutex,
    told: Vec<String>,
    crashes_while_waiting: bool,
}

impl NewCopy {
    fn new(pc: &Shared) -> Self {
        Self {
            mutex: Mutex {
                pc: pc.clone(),
                process: "copy",
            },
            told: Vec::new(),
            crashes_while_waiting: false,
        }
    }
}

impl Takeover for NewCopy {
    type Guard = ();

    fn signal_ready(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn wait_for_lock(&mut self, timeout: Duration) -> Option<()> {
        assert!(!self.crashes_while_waiting, "simulated crash in the copy");
        match claim(&mut self.mutex, Some(timeout)) {
            Claim::Owned(()) | Claim::Unchecked(..) => Some(()),
            Claim::Running => None,
        }
    }

    fn tell(&mut self, body: &str) {
        self.told.push(body.to_string());
    }
}

/// The old window hands over and quits; returns the copy, which has not
/// waited yet.
fn handed_over(pc: &Shared) -> NewCopy {
    let mut old = OldWindow::new(pc, READY);
    assert_eq!(hand_over(&mut old, Reason::GraphicsCard), Ok(()));
    assert_eq!(old.starts, 1);
    assert!(!old.stopped);
    old_quits(pc);
    NewCopy::new(pc)
}

#[test]
fn a_restart_starts_the_copy_first_and_the_old_host_leaves() {
    let pc = pc(2.0);
    let mut copy = handed_over(&pc);
    // The old window waited only for the signal.
    assert_eq!(pc.borrow().now, secs(0.4));
    assert_eq!(take_over(&mut copy, Reason::GraphicsCard), Some(()));
    assert!(copy.told.is_empty(), "{:?}", copy.told);
    let pc = pc.borrow();
    assert_eq!(pc.owner, Some("copy"));
    assert_eq!(pc.now, secs(2.4));
}

#[test]
fn a_copy_that_never_signals_leaves_the_old_window_open_with_the_reason() {
    // Started but stuck (or an antivirus holds it): the old window stops it,
    // so it can't take over later, and stays.
    let pc = pc(2.0);
    let mut old = OldWindow::new(
        &pc,
        CopyPlan {
            ready_after: None,
            ends_after: None,
        },
    );
    let error = hand_over(&mut old, Reason::GraphicsCard).unwrap_err();
    assert_eq!(error, "the new copy wasn't ready after 15 s");
    assert!(old.stopped);
    assert_eq!(pc.borrow().now, READY_TIMEOUT);
    assert_eq!(pc.borrow().owner, Some("old"));
    // Ended before it signalled: no need to wait out the 15 s.
    let pc = self::pc(2.0);
    let mut old = OldWindow::new(
        &pc,
        CopyPlan {
            ready_after: None,
            ends_after: Some(0.2),
        },
    );
    assert!(hand_over(&mut old, Reason::Settings).is_err());
    assert!(!old.stopped);
    assert_eq!(pc.borrow().now, secs(0.2));
    // Windows wouldn't start it at all.
    let mut old = OldWindow::new(&pc, READY);
    old.start_error = Some("could not start EternalMonitor again: denied".into());
    assert_eq!(
        hand_over(&mut old, Reason::GraphicsCard),
        Err("could not start EternalMonitor again: denied".into())
    );
    // What the window says, with a Quit button, in each case.
    assert_eq!(
        format!(
            "{RESTART_FAILED_TITLE}. {}",
            restart_failed_body(Reason::GraphicsCard)
        ),
        "EternalMonitor couldn't restart by itself. The graphics card setting is saved. Quit EternalMonitor and open it again."
    );
    assert_eq!(
        restart_failed_body(Reason::Settings),
        "Your change is saved. Quit EternalMonitor and open it again."
    );
    assert_eq!(
        restart_failed_body(Reason::Plain),
        "Quit EternalMonitor and open it again."
    );
}

#[test]
fn a_copy_that_fails_after_its_signal_says_so_itself() {
    // The old window has already quit when the copy fails while it waits.
    // No host code runs there; a panic shows the restart message, and from
    // the mutex on the copy's failure reporter covers it as any start.
    let pc = pc(2.0);
    let mut copy = handed_over(&pc);
    copy.crashes_while_waiting = true;
    assert_eq!(take_over(&mut copy, Reason::GraphicsCard), None);
    assert_eq!(
        copy.told,
        ["EternalMonitor couldn't restart by itself. The graphics card setting is saved. Open EternalMonitor again."]
    );
    // The old host exits as planned, and with it the mutex: opening
    // EternalMonitor again works.
    {
        let mut pc = pc.borrow_mut();
        pc.now = pc.old_exits_at.unwrap();
        pc.handles -= 1;
        pc.owner = None;
    }
    assert!(manual_launch(&pc));
}

#[test]
fn a_slow_shutdown_is_waited_out() {
    // The virtual display's Disable task at its 35 s limit, then more.
    let pc = pc(45.0);
    let mut copy = handed_over(&pc);
    assert_eq!(take_over(&mut copy, Reason::GraphicsCard), Some(()));
    assert!(copy.told.is_empty());
    assert_eq!(pc.borrow().owner, Some("copy"));
    assert_eq!(pc.borrow().now, secs(45.4));
}

#[test]
fn a_stuck_shutdown_is_explained_and_the_setting_kept() {
    let pc = pc(130.0);
    let mut copy = handed_over(&pc);
    assert_eq!(take_over(&mut copy, Reason::GraphicsCard), None);
    assert_eq!(
        copy.told,
        ["EternalMonitor is still closing. The graphics card setting is saved. Open EternalMonitor again in a minute."]
    );
    assert_eq!(pc.borrow().now, secs(0.4) + LOCK_TIMEOUT);
    // The copy let go of the mutex; the old host still owns it, so a launch
    // now shows its window, and one after it exits starts normally.
    assert_eq!(pc.borrow().owner, Some("old"));
    assert!(!manual_launch(&pc));
    assert_eq!(
        still_closing_message(Reason::Settings),
        "EternalMonitor is still closing. Your change is saved. Open EternalMonitor again in a minute."
    );
}

#[test]
fn a_manual_launch_during_the_handoff_shows_the_running_window() {
    let pc = pc(45.0);
    // Before the old window quits, and while it shuts down: the ordinary
    // check finds the mutex, shows the window that runs, and exits at once.
    let mut old = OldWindow::new(&pc, READY);
    assert!(!manual_launch(&pc));
    hand_over(&mut old, Reason::GraphicsCard).unwrap();
    old_quits(&pc);
    assert!(!manual_launch(&pc));
    // The copy still takes over when the old host exits; a launch after
    // that finds the copy.
    let mut copy = NewCopy::new(&pc);
    assert_eq!(take_over(&mut copy, Reason::GraphicsCard), Some(()));
    assert_eq!(pc.borrow().owner, Some("copy"));
    assert!(!manual_launch(&pc));
    assert_eq!(pc.borrow().owner, Some("copy"));
}

#[test]
fn two_quick_clicks_restart_once() {
    let (release, released) = mpsc::channel::<()>();
    let runs = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut restart = Restart::default();
    let counted = runs.clone();
    assert!(restart.begin(Reason::GraphicsCard, move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        released.recv().unwrap();
        Ok(())
    }));
    assert!(restart.busy());
    // The second click while the first copy starts does nothing.
    let counted = runs.clone();
    assert!(!restart.begin(Reason::GraphicsCard, move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }));
    assert!(!restart.poll());
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !restart.poll() {
        assert!(std::time::Instant::now() < deadline, "never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!restart.busy());
    assert_eq!(restart.failed(), None);
}

#[test]
fn a_failed_restart_is_remembered_and_can_be_tried_again() {
    let mut restart = Restart::default();
    assert!(restart.begin(Reason::Settings, |_| Err("no".into())));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while restart.busy() {
        assert!(!restart.poll());
        assert!(std::time::Instant::now() < deadline, "never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(restart.failed(), Some(Reason::Settings));
    // The copy was stopped, so another try can't start a second one.
    assert!(restart.begin(Reason::Plain, |_| Ok(())));
    assert_eq!(restart.failed(), None);
}

#[test]
fn a_copy_finds_its_request_and_keeps_the_users_arguments() {
    let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let nonce = "0123456789abcdef0123456789abcdef";
    assert_eq!(request(&args(&["19876"])), None);
    // A second restart from a copy replaces the earlier restart's arguments.
    let copy = copy_args(
        args(&["19876", WAIT_ARG, "old", REASON_ARG, "plain"]),
        nonce,
        Reason::GraphicsCard,
    );
    assert_eq!(copy, args(&["19876", WAIT_ARG, nonce, REASON_ARG, "gpu"]));
    assert_eq!(
        request(&copy),
        Some(Request {
            nonce: nonce.into(),
            reason: Reason::GraphicsCard,
        })
    );
    assert_eq!(own_args(copy), args(&["19876"]));
    assert_eq!(
        request(&copy_args(args(&[]), nonce, Reason::Settings))
            .unwrap()
            .reason,
        Reason::Settings
    );
    // Only a 32-digit hex nonce names an event.
    assert_eq!(request(&args(&[WAIT_ARG, r"x\..\Global\y"])), None);
    assert_eq!(request(&args(&[WAIT_ARG])), None);
    assert_eq!(
        event_name(nonce),
        r"Local\EternalMonitor-ready-0123456789abcdef0123456789abcdef"
    );
    // The update helper reopens with the user's arguments only.
    let os: Vec<std::ffi::OsString> = vec!["9877".into(), WAIT_ARG.into(), nonce.into()];
    assert_eq!(own_args(os), vec![std::ffi::OsString::from("9877")]);
}
