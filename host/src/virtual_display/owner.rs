//! The thread that owns the second-screen display.
//!
//! Displays outlive pipeline generations (a codec or settings change
//! restarts capture, not the display), and the private API's objects are
//! not thread-safe, so one long-lived thread holds the [`SecondScreen`] and
//! capture asks it for a display. The requests are plain messages: if spike
//! c shows macOS 26 needs a restartable helper process to own displays, the
//! same requests go over a pipe to it instead of a channel to this thread.

use std::sync::mpsc;
use std::time::Duration;

use super::{DisplayError, SecondScreen, VirtualDisplay, VirtualDisplayManager};
use crate::display_profile::{ClientScreen, DisplayProfile};

/// Covers a recreation: removal (2 s) and creation (3 s) with margin.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

enum Request {
    Ensure {
        client: ClientScreen,
        device_name: String,
        saved: Option<DisplayProfile>,
        reply: mpsc::Sender<Result<VirtualDisplay, DisplayError>>,
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
    pub fn spawn<M, F>(make: F) -> std::io::Result<Self>
    where
        M: VirtualDisplayManager + 'static,
        F: FnOnce() -> Result<M, DisplayError> + Send + 'static,
    {
        let (requests, incoming) = mpsc::channel();
        std::thread::Builder::new()
            .name("display-owner".into())
            .spawn(move || {
                let mut screen = make().map(SecondScreen::new);
                if let Err(error) = &screen {
                    tracing::warn!(%error, "Second-screen displays are unavailable");
                }
                for request in incoming {
                    match request {
                        Request::Ensure {
                            client,
                            device_name,
                            saved,
                            reply,
                        } => {
                            let result = match &mut screen {
                                Ok(screen) => screen.ensure(client, &device_name, saved),
                                Err(error) => Err(error.clone()),
                            };
                            let _ = reply.send(result);
                        }
                        Request::Release { reply } => {
                            let result = match &mut screen {
                                Ok(screen) => screen.release(),
                                Err(_) => Ok(()),
                            };
                            let _ = reply.send(result);
                        }
                    }
                }
                if let Ok(screen) = &mut screen {
                    let _ = screen.release();
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

    /// The display for `client`: its current one if it has one.
    pub fn ensure(
        &self,
        client: ClientScreen,
        device_name: &str,
        saved: Option<DisplayProfile>,
    ) -> Result<VirtualDisplay, DisplayError> {
        self.ask(|reply| Request::Ensure {
            client,
            device_name: device_name.to_string(),
            saved,
            reply,
        })
    }

    pub fn release(&self) -> Result<(), DisplayError> {
        self.ask(|reply| Request::Release { reply })
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{FakeDisplays, Op};
    use super::*;

    fn duo(screen_px: (u16, u16)) -> ClientScreen {
        ClientScreen {
            device_id: 0xd0,
            screen_px,
            refresh_hz: 120,
        }
    }

    #[test]
    fn the_owner_thread_keeps_the_display_across_requests_and_releases_it() {
        let (ops_tx, ops) = mpsc::channel();
        struct Reporting(FakeDisplays, mpsc::Sender<Vec<Op>>);
        impl VirtualDisplayManager for Reporting {
            fn create(
                &mut self,
                spec: &super::super::DisplaySpec,
            ) -> Result<VirtualDisplay, DisplayError> {
                let result = self.0.create(spec);
                let _ = self.1.send(self.0.ops.clone());
                result
            }
            fn change_mode(
                &mut self,
                display: &VirtualDisplay,
                profile: DisplayProfile,
            ) -> Result<VirtualDisplay, DisplayError> {
                self.0.change_mode(display, profile)
            }
            fn remove(&mut self, display: &VirtualDisplay) -> Result<(), DisplayError> {
                let result = self.0.remove(display);
                let _ = self.1.send(self.0.ops.clone());
                result
            }
            fn bounds(
                &self,
                display: &VirtualDisplay,
            ) -> Option<crate::input::mac::display::DisplayBounds> {
                self.0.bounds(display)
            }
        }
        let owner =
            DisplayOwner::spawn(move || Ok(Reporting(FakeDisplays::default(), ops_tx))).unwrap();
        let first = owner.ensure(duo((1398, 2034)), "Duo", None).unwrap();
        assert_eq!(owner.ensure(duo((1878, 2670)), "Duo", None), Ok(first));
        owner.release().unwrap();
        owner.release().unwrap();
        let history: Vec<_> = ops.try_iter().collect();
        assert_eq!(history.len(), 2, "one create, one remove: {history:?}");
        assert_eq!(history[1].last(), Some(&Op::Remove(first.id)));
    }

    #[test]
    fn an_unavailable_api_is_reported_on_every_request() {
        let owner = DisplayOwner::spawn(|| -> Result<FakeDisplays, _> {
            Err(DisplayError::Unavailable("no CGVirtualDisplay".into()))
        })
        .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                owner.ensure(duo((2670, 1878)), "", None),
                Err(DisplayError::Unavailable(_))
            ));
        }
        assert_eq!(owner.release(), Ok(()));
    }
}
