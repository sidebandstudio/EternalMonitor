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

use super::{DisplayError, Keeper, VirtualDisplay, VirtualDisplayManager, Want};

/// Covers a recreation: removal (2 s) and creation (3 s) with margin.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the thread sleeps with no display to expire.
const IDLE_WAKE: Duration = Duration::from_secs(60);

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
    /// crosses threads. If it fails, every request reports why. The thread
    /// also removes a display whose client stayed away for the grace
    /// period, whether or not anything asks.
    pub fn spawn<M, F>(make: F) -> std::io::Result<Self>
    where
        M: VirtualDisplayManager + 'static,
        F: FnOnce() -> Result<M, DisplayError> + Send + 'static,
    {
        let (requests, incoming) = mpsc::channel();
        std::thread::Builder::new()
            .name("display-owner".into())
            .spawn(move || {
                let mut keeper = make().map(Keeper::new);
                if let Err(error) = &keeper {
                    tracing::warn!(%error, "Second-screen displays are unavailable");
                }
                loop {
                    let wait = match &keeper {
                        Ok(keeper) => keeper
                            .expires_at()
                            .map_or(IDLE_WAKE, |at| at.saturating_duration_since(Instant::now())),
                        Err(_) => IDLE_WAKE,
                    };
                    let request = match incoming.recv_timeout(wait) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Ok(keeper) = &mut keeper {
                                if let Err(error) = keeper.expire(Instant::now()) {
                                    tracing::warn!(%error, "The second-screen display did not go away");
                                }
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match request {
                        Request::Want { want, reply } => {
                            let result = match &mut keeper {
                                Ok(keeper) => keeper.want(want, Instant::now()),
                                Err(error) if want.enabled && want.client.is_some() => {
                                    Err(error.clone())
                                }
                                Err(_) => Ok(None),
                            };
                            let _ = reply.send(result);
                        }
                        Request::Release { reply } => {
                            let result = match &mut keeper {
                                Ok(keeper) => keeper.release(),
                                Err(_) => Ok(()),
                            };
                            let _ = reply.send(result);
                        }
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

    /// The display to capture now (see `Keeper::want`); `None` to mirror.
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
    use super::super::Wanted;
    use super::*;
    use crate::display_profile::ClientScreen;

    fn want(client: bool, grace: Duration) -> Want {
        Want {
            enabled: true,
            client: client.then(|| Wanted {
                client: ClientScreen {
                    device_id: 0xd0,
                    screen_px: (1398, 2034),
                    refresh_hz: 120,
                },
                device_name: "Duo".into(),
                saved: None,
            }),
            grace,
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

    #[test]
    fn the_owner_keeps_the_display_across_requests_and_releases_it_on_quit() {
        let (ops_tx, ops) = mpsc::channel();
        let owner =
            DisplayOwner::spawn(move || Ok(Reporting(FakeDisplays::default(), ops_tx))).unwrap();
        let grace = Duration::from_secs(60);
        let first = owner.want(want(true, grace)).unwrap().unwrap();
        // A restart with no client yet keeps capturing the same display.
        assert_eq!(owner.want(want(false, grace)), Ok(Some(first)));
        assert_eq!(owner.want(want(true, grace)), Ok(Some(first)));
        owner.release().unwrap();
        owner.release().unwrap();
        let history: Vec<_> = ops.try_iter().collect();
        assert_eq!(history.len(), 2, "one create, one remove: {history:?}");
        assert_eq!(history[1], Op::Remove(first.id));
    }

    #[test]
    fn the_owner_removes_an_abandoned_display_on_its_own_timer() {
        let (ops_tx, ops) = mpsc::channel();
        let owner =
            DisplayOwner::spawn(move || Ok(Reporting(FakeDisplays::default(), ops_tx))).unwrap();
        let first = owner
            .want(want(true, Duration::from_millis(100)))
            .unwrap()
            .unwrap();
        assert!(matches!(ops.recv().unwrap(), Op::Create(_)));
        // Nothing asks again; the display still goes after the grace period.
        assert_eq!(
            ops.recv_timeout(Duration::from_secs(5)),
            Ok(Op::Remove(first.id))
        );
    }

    #[test]
    fn an_unavailable_api_is_reported_when_a_display_is_wanted() {
        let owner = DisplayOwner::spawn(|| -> Result<FakeDisplays, _> {
            Err(DisplayError::Unavailable("no CGVirtualDisplay".into()))
        })
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
