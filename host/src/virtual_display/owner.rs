//! The thread that owns the second-screen display.
//!
//! Displays outlive pipeline generations (a codec or settings change
//! restarts capture, not the display), and the private API's objects are
//! not thread-safe, so one long-lived thread holds the [`SecondScreen`] and
//! capture asks it for a display. The requests are plain messages: if spike
//! c shows macOS 26 needs a restartable helper process to own displays, the
//! same requests go over a pipe to it instead of a channel to this thread.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{DisplayError, Keeper, LiveClient, VirtualDisplay, VirtualDisplayManager, Want};
use crate::display_profile::DisplayProfile;

/// Covers a recreation: removal (2 s) and creation (3 s) with margin.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the thread sleeps when there are no displays at all.
const IDLE_WAKE: Duration = Duration::from_secs(60);
/// How often the thread observes the session state.
const PRESENCE_POLL: Duration = Duration::from_millis(500);

enum Request {
    Want {
        want: Want,
        reply: mpsc::Sender<Result<Option<VirtualDisplay>, DisplayError>>,
    },
    Release {
        reply: mpsc::Sender<Result<(), DisplayError>>,
    },
}

pub struct DisplayOwner {
    requests: mpsc::Sender<Request>,
}

impl DisplayOwner {
    /// Starts the owner thread. `make` runs on it, so the manager never
    /// crosses threads. If it fails, every request reports why.
    ///
    /// `live` reads the transport's session as it is now: the connected
    /// client and its saved profile, or `None`. The thread is the single
    /// source of truth for which device it serves: it reads `live` at the
    /// start of every turn (every request, and every `PRESENCE_POLL`
    /// otherwise), and requests only carry a device hint. It removes a
    /// display whose session has been gone for the grace period, whether or
    /// not anything asks.
    ///
    /// `save_first` saves the default profile of a device that has none,
    /// without replacing one saved meanwhile. It is the only profile write
    /// the owner makes, and capture makes none. It runs after the request
    /// that called for it is answered; a save that did not happen is tried
    /// again on a later turn (`Keeper::save_first_profile`).
    ///
    /// Neither `live` nor `save_first` may touch the disk, or every answer
    /// after them waits for it: the host reads a `ProfileView` and hands
    /// saves to a `ProfileIo` thread (`capture::mac_screen::spawn_owner`).
    pub fn spawn<M, F>(
        make: F,
        mut live: impl FnMut() -> Option<LiveClient> + Send + 'static,
        save_first: impl FnMut(u64, DisplayProfile) + Send + 'static,
    ) -> std::io::Result<Self>
    where
        M: VirtualDisplayManager + 'static,
        F: FnOnce() -> Result<M, DisplayError> + Send + 'static,
    {
        let (requests, incoming) = mpsc::channel();
        std::thread::Builder::new()
            .name("display-owner".into())
            .spawn(move || {
                let mut keeper =
                    make().map(|manager| Keeper::new(manager).saving_first_profiles(save_first));
                if let Err(error) = &keeper {
                    tracing::warn!(%error, "Second-screen displays are unavailable");
                }
                loop {
                    let wait = match &keeper {
                        Ok(keeper) => keeper.expires_at().map_or(PRESENCE_POLL, |at| {
                            at.saturating_duration_since(Instant::now())
                                .min(PRESENCE_POLL)
                        }),
                        Err(_) => IDLE_WAKE,
                    };
                    let request = match incoming.recv_timeout(wait) {
                        Ok(request) => Some(request),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let session = live();
                    match request {
                        Some(Request::Want { want, reply }) => {
                            let result = match &mut keeper {
                                Ok(keeper) => keeper
                                    .serve(session.as_ref(), Some(want), Instant::now())
                                    .expect("a request is answered"),
                                Err(error) if want.enabled && want.device.is_some() => {
                                    Err(error.clone())
                                }
                                Err(_) => Ok(None),
                            };
                            let _ = reply.send(result);
                        }
                        Some(Request::Release { reply }) => {
                            let result = match &mut keeper {
                                Ok(keeper) => keeper.release(),
                                Err(_) => Ok(()),
                            };
                            let _ = reply.send(result);
                        }
                        None => {
                            if let Ok(keeper) = &mut keeper {
                                keeper.serve(session.as_ref(), None, Instant::now());
                            }
                        }
                    }
                    // Answered first: no reply waits for the store.
                    if let Ok(keeper) = &mut keeper {
                        keeper.save_first_profile(session.as_ref(), Instant::now());
                    }
                }
                if let Ok(keeper) = &mut keeper {
                    let _ = keeper.release();
                }
            })?;
        Ok(Self { requests })
    }

    fn ask<T>(
        &self,
        request: impl FnOnce(mpsc::Sender<Result<T, DisplayError>>) -> Request,
    ) -> Result<T, DisplayError> {
        let (reply, answer) = mpsc::channel();
        let gone = || DisplayError::CreateFailed("the display owner is not answering".into());
        self.requests.send(request(reply)).map_err(|_| gone())?;
        answer.recv_timeout(REPLY_TIMEOUT).map_err(|_| gone())?
    }

    /// The display to capture now, for the live session (see
    /// `Keeper::serve`); `None` to mirror.
    pub fn want(&self, want: Want) -> Result<Option<VirtualDisplay>, DisplayError> {
        self.ask(|reply| Request::Want { want, reply })
    }

    /// Removes the display now: the host is quitting.
    pub fn release(&self) -> Result<(), DisplayError> {
        self.ask(|reply| Request::Release { reply })
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{FakeDisplays, Op};
    use super::*;
    use crate::display_profile::{ClientScreen, ProfileIo, ProfileStore, ProfileView};
    use std::sync::{Arc, Mutex};

    const DUO: u64 = 0xd0;

    fn duo() -> LiveClient {
        LiveClient {
            client: ClientScreen {
                device_id: DUO,
                screen_px: (1398, 2034),
                refresh_hz: 120,
            },
            device_name: "Duo".into(),
            saved: None,
        }
    }

    fn want(saw_client: bool, grace: Duration) -> Want {
        Want {
            enabled: true,
            grace,
            device: saw_client.then_some(DUO),
        }
    }

    /// FakeDisplays that reports its operations as they happen.
    struct Reporting(FakeDisplays, mpsc::Sender<Op>);

    impl VirtualDisplayManager for Reporting {
        fn create(
            &mut self,
            spec: &super::super::DisplaySpec,
        ) -> Result<VirtualDisplay, DisplayError> {
            let _ = self.1.send(Op::Create(spec.clone()));
            self.0.create(spec)
        }
        fn change_mode(
            &mut self,
            display: &VirtualDisplay,
            profile: crate::display_profile::DisplayProfile,
        ) -> Result<VirtualDisplay, DisplayError> {
            self.0.change_mode(display, profile)
        }
        fn remove(&mut self, display: &VirtualDisplay) -> Result<(), DisplayError> {
            let _ = self.1.send(Op::Remove(display.id));
            self.0.remove(display)
        }
        fn bounds(
            &self,
            display: &VirtualDisplay,
        ) -> Option<crate::input::mac::display::DisplayBounds> {
            self.0.bounds(display)
        }
    }

    /// An owner over reporting fakes whose live session is `session`.
    fn owner(session: Arc<Mutex<Option<LiveClient>>>) -> (DisplayOwner, mpsc::Receiver<Op>) {
        let (ops_tx, ops) = mpsc::channel();
        let owner = DisplayOwner::spawn(
            move || Ok(Reporting(FakeDisplays::default(), ops_tx)),
            move || session.lock().unwrap().clone(),
            |_, _| {},
        )
        .unwrap();
        (owner, ops)
    }

    #[test]
    fn the_owner_keeps_the_display_across_requests_and_releases_it_on_quit() {
        let (owner, ops) = owner(Arc::new(Mutex::new(Some(duo()))));
        let grace = Duration::from_secs(60);
        let first = owner.want(want(true, grace)).unwrap().unwrap();
        // A pass that saw no client keeps capturing the same display.
        assert_eq!(owner.want(want(false, grace)), Ok(Some(first)));
        assert_eq!(owner.want(want(true, grace)), Ok(Some(first)));
        owner.release().unwrap();
        owner.release().unwrap();
        let history: Vec<_> = ops.try_iter().collect();
        assert_eq!(history.len(), 2, "one create, one remove: {history:?}");
        assert_eq!(history[1], Op::Remove(first.id));
    }

    #[test]
    fn the_owner_keeps_a_connected_display_and_removes_it_after_the_session_ends() {
        let session = Arc::new(Mutex::new(Some(duo())));
        let (owner, ops) = owner(session.clone());
        let tiny = Duration::from_millis(100);
        let first = owner.want(want(true, tiny)).unwrap().unwrap();
        assert!(matches!(ops.recv().unwrap(), Op::Create(_)));
        // Nothing asks for longer than the grace period, but the session
        // stays up: nothing is removed, and the next request gets the same
        // display.
        assert!(ops
            .recv_timeout(super::super::MIN_GRACE + Duration::from_secs(1))
            .is_err());
        assert_eq!(owner.want(want(false, tiny)), Ok(Some(first)));
        assert_eq!(owner.want(want(true, tiny)), Ok(Some(first)));
        // The session ends; nothing asks, and the display goes once the
        // (minimum) grace period has passed.
        let gone = std::time::Instant::now();
        *session.lock().unwrap() = None;
        assert_eq!(
            ops.recv_timeout(Duration::from_secs(10)),
            Ok(Op::Remove(first.id))
        );
        assert!(gone.elapsed() >= super::super::MIN_GRACE);
    }

    #[test]
    fn a_request_naming_another_device_than_the_live_one_changes_nothing() {
        let session = Arc::new(Mutex::new(Some(duo())));
        let (owner, ops) = owner(session.clone());
        let grace = Duration::from_secs(60);
        let first = owner.want(want(true, grace)).unwrap().unwrap();
        assert!(matches!(ops.recv().unwrap(), Op::Create(_)));
        let other = Want {
            device: Some(7),
            ..want(true, grace)
        };
        assert_eq!(owner.want(other), Ok(Some(first)));
        assert!(ops.try_recv().is_err(), "no create, remove or change");
    }

    /// An owner wired as the host wires it (`capture::mac_screen`), over
    /// `dir`'s profiles: the live session's saved profile comes from the
    /// profile thread's view, and first saves are handed to that thread.
    /// The live device is `session`'s, and `asked` counts first saves.
    fn owner_with_profiles(
        dir: &std::path::Path,
        session: Arc<Mutex<LiveClient>>,
    ) -> (
        DisplayOwner,
        Arc<std::sync::atomic::AtomicUsize>,
        ProfileView,
    ) {
        let io =
            ProfileIo::spawn(ProfileStore::at(Some(dir.join("display-profiles.json")))).unwrap();
        let (view, live) = (io.view(), io.view());
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = asked.clone();
        let owner = DisplayOwner::spawn(
            || Ok(FakeDisplays::default()),
            move || {
                let session = session.lock().unwrap().clone();
                Some(LiveClient {
                    saved: live.get(session.client.device_id),
                    ..session
                })
            },
            move |device_id, profile| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                io.save_if_absent(device_id, profile);
            },
        )
        .unwrap();
        (owner, asked, view)
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("eternal-owner-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_busy_profile_store_never_slows_the_answers_and_the_first_save_follows_once() {
        use std::sync::atomic::Ordering;
        let dir = temp_dir("busy");
        let path = dir.join("display-profiles.json");
        // Another writer, in any process, holds the profiles' lock.
        let held = std::fs::File::create(dir.join("display-profiles.json.lock")).unwrap();
        held.lock().unwrap();
        let (owner, asked, view) = owner_with_profiles(&dir, Arc::new(Mutex::new(duo())));
        let grace = Duration::from_secs(60);
        let mut display = None;
        for pass in 0..10 {
            let started = Instant::now();
            let answer = owner.want(want(true, grace)).unwrap();
            assert!(
                started.elapsed() < Duration::from_millis(50),
                "pass {pass} took {:?}",
                started.elapsed()
            );
            assert!(answer.is_some());
            display = display.or(answer);
            assert_eq!(answer, display);
        }
        assert_eq!(asked.load(Ordering::SeqCst), 1, "tried once, then waits");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!path.exists(), "nothing written while the lock is held");
        assert_eq!(view.get(DUO), None);
        // The lock is released: the owner's next try saves, within
        // SAVE_RETRY and a poll, whether or not capture asks meanwhile.
        let released = Instant::now();
        drop(held);
        let deadline = released + super::super::SAVE_RETRY + Duration::from_secs(2);
        while view.get(DUO).is_none() {
            assert!(Instant::now() < deadline, "never saved");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(released.elapsed() <= super::super::SAVE_RETRY + PRESENCE_POLL * 2);
        assert_eq!(
            ProfileStore::at(Some(path.clone())).get(DUO),
            display.map(|display| display.profile)
        );
        // Saved once: the view now has it, so later turns try nothing.
        let saved = std::fs::metadata(&path).unwrap().modified().unwrap();
        for _ in 0..5 {
            assert_eq!(owner.want(want(true, grace)).unwrap(), display);
            std::thread::sleep(PRESENCE_POLL / 2);
        }
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), saved);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn first_saves_never_delay_the_answers() {
        use std::sync::atomic::Ordering;
        const PASSES: u64 = 250;
        const PER_DEVICE: u64 = 5;
        let dir = temp_dir("timing");
        let session = Arc::new(Mutex::new(duo()));
        let (owner, asked, view) = owner_with_profiles(&dir, session.clone());
        let grace = Duration::from_secs(60);
        // A fresh device every few passes: each one's first pass hands a
        // save over, which the profile thread writes and syncs while the
        // passes go on, a couple of milliseconds apart.
        let mut waits = Vec::new();
        for pass in 0..PASSES {
            let device = 0x100 + pass / PER_DEVICE;
            session.lock().unwrap().client.device_id = device;
            let started = Instant::now();
            let answer = owner.want(Want {
                device: Some(device),
                ..want(true, grace)
            });
            waits.push(started.elapsed());
            assert!(matches!(answer, Ok(Some(_))), "{answer:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
        let devices = PASSES / PER_DEVICE;
        let saved_during = (0..devices)
            .filter(|n| view.get(0x100 + n).is_some())
            .count();
        let deadline = Instant::now() + Duration::from_secs(10);
        while (0..devices).any(|n| view.get(0x100 + n).is_none()) {
            assert!(Instant::now() < deadline, "not every device was saved");
            std::thread::sleep(Duration::from_millis(10));
        }
        waits.sort();
        let at = |q: usize| waits[(waits.len() - 1) * q / 100];
        let (p50, p95, max) = (at(50), at(95), at(100));
        println!(
            "{PASSES} answers with {devices} first saves ({saved_during} written by the last): \
             p50 {p50:?}, p95 {p95:?}, max {max:?}"
        );
        assert_eq!(
            asked.load(Ordering::SeqCst) as u64,
            devices,
            "one save each"
        );
        // A save that delayed the next answer (about 10 ms each, Sol's
        // round 9) would put one pass in five over p95. The bound on the
        // maximum leaves room for the scheduler on a loaded machine, which
        // stalls a pass as long with no saves at all.
        assert!(p50 < Duration::from_millis(2), "p50 {p50:?}");
        assert!(p95 < Duration::from_millis(10), "p95 {p95:?}");
        assert!(max < Duration::from_millis(50), "max {max:?}");
        let mut on_disk = ProfileStore::at(Some(dir.join("display-profiles.json")));
        assert!((0..devices).all(|n| on_disk.get(0x100 + n).is_some()));
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unavailable_api_is_reported_when_a_display_is_wanted() {
        let owner = DisplayOwner::spawn(
            || -> Result<FakeDisplays, _> {
                Err(DisplayError::Unavailable("no CGVirtualDisplay".into()))
            },
            || Some(duo()),
            |_, _| {},
        )
        .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                owner.want(want(true, Duration::from_secs(60))),
                Err(DisplayError::Unavailable(_))
            ));
        }
        assert_eq!(owner.want(want(false, Duration::from_secs(60))), Ok(None));
        assert_eq!(owner.release(), Ok(()));
    }
}
