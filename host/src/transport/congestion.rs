//! Windows refuses a UDP send with WSAENOBUFS (10055) when it has no buffer
//! space for the datagram: a queue in the network adapter, its driver or a
//! filter driver is full. macOS and Linux say ENOBUFS. The socket's own send
//! buffer (4 MiB) is not the one that ran out. In the field each refused send
//! logged its own line, and the stream lowered its bitrate only after the
//! iPad reported the losses. A refused send now counts as congestion: video
//! backs off for a moment, the bitrate steps down at once, and the log says
//! one line when an episode starts and one when it ends.

use std::io;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// How long a frame's sending pauses after the system refused a datagram,
/// before that datagram is sent once more. A frame pauses once at most.
pub const BACKOFF: Duration = Duration::from_millis(5);

/// How long a refused control reply waits before its one retry.
pub const REPLY_RETRY: Duration = Duration::from_millis(2);

/// An episode ends once sends go this long without a refusal.
pub const EPISODE_QUIET: Duration = Duration::from_secs(1);

#[cfg(windows)]
const NO_BUFFER_SPACE: i32 = 10055; // WSAENOBUFS
#[cfg(not(windows))]
const NO_BUFFER_SPACE: i32 = libc::ENOBUFS;

/// Whether the system refused a send for lack of buffer space.
pub fn is_no_buffer_space(error: &io::Error) -> bool {
    error.raw_os_error() == Some(NO_BUFFER_SPACE)
}

/// The error a send the system had no buffer space for returns.
pub fn no_buffer_space() -> io::Error {
    io::Error::from_raw_os_error(NO_BUFFER_SPACE)
}

/// Refused sends from the first until sends go [`EPISODE_QUIET`] without one.
struct Episode {
    started: Instant,
    last: Instant,
    refused: u64,
}

/// Every refused UDP send of one transport, by episode.
#[derive(Default)]
pub struct SendCongestion {
    episode: Option<Episode>,
    step_down: bool,
}

impl SendCongestion {
    /// Notes a send refused for lack of buffer space. The first of an
    /// episode is logged, and asks for one step down.
    pub fn refused(&mut self, error: &io::Error, now: Instant) {
        self.settle(now);
        match &mut self.episode {
            Some(episode) => {
                episode.refused += 1;
                episode.last = now;
            }
            None => {
                warn!(link = %"wifi", error = %error, "UDP send failed: no buffer space, backing off");
                self.episode = Some(Episode {
                    started: now,
                    last: now,
                    refused: 1,
                });
                self.step_down = true;
            }
        }
    }

    /// Whether an episode started since the last call, once per episode.
    pub fn take_step_down(&mut self) -> bool {
        std::mem::take(&mut self.step_down)
    }

    /// Ends an episode with no refusal for [`EPISODE_QUIET`], logging how
    /// many sends it refused and how long it lasted.
    pub fn settle(&mut self, now: Instant) {
        if self
            .episode
            .as_ref()
            .is_some_and(|episode| now.saturating_duration_since(episode.last) >= EPISODE_QUIET)
        {
            self.end();
        }
    }

    fn end(&mut self) {
        if let Some(episode) = self.episode.take() {
            info!(
                link = %"wifi",
                refused = episode.refused,
                took_ms = episode.last.duration_since(episode.started).as_millis() as u64,
                "UDP sends have buffer space again"
            );
        }
    }
}

/// A transport that stops during an episode still says how it went.
impl Drop for SendCongestion {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logged(run: impl FnOnce()) -> Vec<String> {
        let out = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
        struct Sink(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);
        impl io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let writer = std::sync::Arc::clone(&out);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(move || Sink(std::sync::Arc::clone(&writer)))
            .finish();
        crate::logging::prepare_log_capture();
        tracing::subscriber::with_default(subscriber, run);
        let text = String::from_utf8(out.lock().clone()).unwrap();
        text.lines().map(|line| line.trim().to_owned()).collect()
    }

    #[test]
    fn only_no_buffer_space_counts_as_congestion() {
        assert!(is_no_buffer_space(&no_buffer_space()));
        #[cfg(windows)]
        assert!(is_no_buffer_space(&io::Error::from_raw_os_error(10055)));
        #[cfg(target_os = "macos")]
        assert!(is_no_buffer_space(&io::Error::from_raw_os_error(55)));
        #[cfg(target_os = "linux")]
        assert!(is_no_buffer_space(&io::Error::from_raw_os_error(105)));
        for other in [
            io::Error::from(io::ErrorKind::ConnectionReset),
            io::Error::from(io::ErrorKind::WouldBlock),
            io::Error::from_raw_os_error(10054),
        ] {
            assert!(!is_no_buffer_space(&other), "{other}");
        }
    }

    /// Hundreds of refused sends close together are one episode: one line
    /// when it starts, one step down, and one summary once it has been quiet.
    #[test]
    fn an_episode_logs_one_line_and_its_end() {
        let start = Instant::now();
        let mut congestion = SendCongestion::default();
        let lines = logged(|| {
            for n in 0..300u32 {
                congestion.refused(&no_buffer_space(), start + Duration::from_millis(n.into()));
                congestion.settle(start + Duration::from_millis(n.into()));
            }
            assert!(congestion.take_step_down());
            assert!(!congestion.take_step_down(), "one step per episode");
            congestion.settle(start + Duration::from_millis(299) + EPISODE_QUIET / 2);
            congestion.settle(start + Duration::from_millis(299) + EPISODE_QUIET);
        });
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(
            lines[0]
                .starts_with("WARN UDP send failed: no buffer space, backing off link=wifi error="),
            "{lines:#?}"
        );
        assert_eq!(
            lines[1],
            "INFO UDP sends have buffer space again link=wifi refused=300 took_ms=299"
        );

        // A refusal after the quiet starts a new episode, with its own step.
        let later = start + Duration::from_secs(5);
        let lines = logged(|| {
            congestion.refused(&no_buffer_space(), later);
            assert!(congestion.take_step_down());
            drop(congestion);
        });
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(lines[0].starts_with("WARN UDP send failed: no buffer space"));
        assert_eq!(
            lines[1], "INFO UDP sends have buffer space again link=wifi refused=1 took_ms=0",
            "a transport that stops ends its episode"
        );
    }
}
