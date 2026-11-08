//! Wakes the encoder's graphics card when an iPad is probably about to
//! connect. A laptop's discrete card powers off entirely (D3cold) about 20 s
//! after its last use: its PCIe link and the ports above it go down too. On
//! an AMD hybrid laptop the next D3D11 device or AMF open on it then took
//! about 800 ms instead of 160, all of it inside the first session's first
//! picture. An iPad looks for PCs over mDNS on its connect screen, seconds
//! before it sends HELLO, so its query (see `discovery::watch_browsers`)
//! starts the card waking: one tiny D3D11 device on it, dropped at once.
//! Nothing is kept open, and nothing wakes it on a timer. The first AMF open
//! then took about 280 ms instead of 830; holding the device for 10 s
//! changed nothing measurable, so it isn't held.

use std::time::{Duration, Instant};

/// The shortest time between two wakes. A card woken this recently is still
/// in D3hot at worst, which wakes in about 160 ms.
pub const WAKE_EVERY: Duration = Duration::from_secs(15);
/// The longest time between two wakes after many wasted ones.
const WAKE_EVERY_AT_MOST: Duration = Duration::from_secs(600);

/// When a browse may wake the card. Devices browse without connecting too
/// (an iPad left on its connect screen, another app's scan), so each wake no
/// session followed doubles the time to the next, up to
/// [`WAKE_EVERY_AT_MOST`]: a few wakes an hour at most, never a keep-alive.
/// A session resets it.
#[derive(Debug)]
pub struct WakeGate {
    last: Option<Instant>,
    every: Duration,
    /// `Session::sessions_started` at the last wake.
    sessions_at_wake: Option<u32>,
}

impl Default for WakeGate {
    fn default() -> Self {
        Self {
            last: None,
            every: WAKE_EVERY,
            sessions_at_wake: None,
        }
    }
}

impl WakeGate {
    /// A device browsed for the service at `now`. Whether to wake the card:
    /// never while a viewer is connected (`in_session`, the card is in use).
    /// `sessions` is `Session::sessions_started`: a session since the last
    /// wake means the wake paid off.
    pub fn browse(&mut self, now: Instant, in_session: bool, sessions: u32) -> bool {
        if in_session {
            return false;
        }
        // A session since the last wake restores the short wait first: after
        // a long backoff, the next viewer's browse mustn't wait it out.
        let paid = self.sessions_at_wake.is_none_or(|at| at != sessions);
        if paid {
            self.every = WAKE_EVERY;
        }
        if let Some(last) = self.last {
            if now.saturating_duration_since(last) < self.every {
                return false;
            }
        }
        if !paid {
            self.every = (self.every * 2).min(WAKE_EVERY_AT_MOST);
        }
        self.sessions_at_wake = Some(sessions);
        self.last = Some(now);
        true
    }
}

/// The adapter to wake for `opened`, the encoder that opened in a pipeline
/// using `gpu`: the card detection chose, when `opened` is a hardware encoder
/// of its family (its HEVC sibling too). None for a CPU encoder, or for a
/// fallback to another card's encoder (NVENC or AMF failing over to Quick
/// Sync): the card detection chose is then unused, and waking it costs power
/// for nothing.
pub fn wake_adapter_for(opened: &str, gpu: &crate::gpu::GpuInfo) -> Option<u32> {
    let family = |name: &str| {
        name.strip_prefix("h264_")
            .or_else(|| name.strip_prefix("hevc_"))
            .map(str::to_owned)
    };
    (family(opened).is_some() && family(opened) == family(&gpu.encoder_name))
        .then_some(gpu.adapter_index)
}

/// Runs `touch` unless `still_wanted`, checked just before, says a session
/// has started since the wake was decided: the card is then the session's,
/// and a device of the wake's during encoder or capture startup would only
/// compete with it. Returns whether it ran.
#[cfg(any(windows, test))]
fn run_wake(still_wanted: impl FnOnce() -> bool, touch: impl FnOnce()) -> bool {
    if !still_wanted() {
        return false;
    }
    touch();
    true
}

/// Starts waking the graphics card at DXGI adapter `adapter_index`, on a
/// thread of its own, unless a wake is still running. The thread drops it
/// if `still_wanted` returns false when it starts (see [`run_wake`]).
#[cfg(windows)]
pub fn wake(adapter_index: u32, still_wanted: impl FnOnce() -> bool + Send + 'static) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("gpu-wake".into())
        .spawn(move || {
            let started = Instant::now();
            let ran = run_wake(still_wanted, || match touch(adapter_index) {
                Ok(()) => tracing::debug!(
                    adapter_index,
                    took_ms = started.elapsed().as_millis() as u64,
                    "Woke the encoder's graphics card for a viewer that may connect"
                ),
                Err(error) => {
                    tracing::debug!(adapter_index, %error, "Could not wake the graphics card")
                }
            });
            if !ran {
                tracing::debug!(
                    adapter_index,
                    "A session started first: not waking the graphics card"
                );
            }
            RUNNING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

#[cfg(not(windows))]
pub fn wake(_adapter_index: u32, _still_wanted: impl FnOnce() -> bool + Send + 'static) {}

/// A D3D11 device on the adapter, dropped at once: enough to power it up.
#[cfg(windows)]
fn touch(adapter_index: u32) -> windows::core::Result<()> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_SDK_VERSION};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let adapter: IDXGIAdapter = unsafe { factory.EnumAdapters1(adapter_index) }?.cast()?;
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            None,
            Default::default(),
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }?;
    drop(device);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, s: u64) -> Instant {
        start + Duration::from_secs(s)
    }

    #[test]
    fn a_browse_wakes_the_card_at_most_once_per_interval_and_never_in_a_session() {
        let start = Instant::now();
        let mut gate = WakeGate::default();
        assert!(gate.browse(at(start, 0), false, 7));
        assert!(
            !gate.browse(at(start, 5), false, 7),
            "a scan's burst of queries"
        );
        assert!(!gate.browse(at(start, 14), false, 7));
        assert!(!gate.browse(at(start, 40), true, 8), "the card is in use");
        // The iPad connected and left: the wake paid off.
        assert!(gate.browse(at(start, 60), false, 8));
        assert!(gate.browse(at(start, 75), false, 9));
    }

    fn gpu(encoder: &str, adapter_index: u32) -> crate::gpu::GpuInfo {
        crate::gpu::GpuInfo {
            encoder_name: encoder.to_string(),
            adapter_index,
            ..crate::gpu::GpuInfo::software_fallback()
        }
    }

    #[test]
    fn only_the_encoder_that_streams_on_the_detected_card_wakes_it() {
        let nvenc = gpu("h264_nvenc", 1);
        assert_eq!(wake_adapter_for("h264_nvenc", &nvenc), Some(1));
        assert_eq!(
            wake_adapter_for("hevc_nvenc", &nvenc),
            Some(1),
            "its HEVC sibling"
        );
        assert_eq!(
            wake_adapter_for("h264_qsv", &nvenc),
            None,
            "a Quick Sync fallback"
        );
        assert_eq!(wake_adapter_for("h264_amf", &nvenc), None, "another card");
        assert_eq!(wake_adapter_for("libx264", &nvenc), None, "the CPU");
        assert_eq!(wake_adapter_for("libx264", &gpu("libx264", 0)), None);
    }

    #[test]
    fn a_wake_is_dropped_when_a_session_started_first() {
        let touched = std::cell::Cell::new(false);
        assert!(!run_wake(|| false, || touched.set(true)));
        assert!(
            !touched.get(),
            "the card was touched during the session's startup"
        );
        assert!(run_wake(|| true, || touched.set(true)));
        assert!(touched.get());
    }

    #[test]
    fn a_session_after_a_long_backoff_restores_the_short_interval_at_once() {
        let start = Instant::now();
        let mut gate = WakeGate::default();
        // Browses with nobody connecting, until the wait is at its longest.
        let mut last = 0;
        for s in (0..7200).step_by(20) {
            if gate.browse(at(start, s), false, 7) {
                last = s;
            }
        }
        assert_eq!(gate.every, WAKE_EVERY_AT_MOST);
        // An iPad connects, leaves, and browses again 20 s after that wake.
        assert!(
            gate.browse(at(start, last + 20), false, 8),
            "the session's browse waited out the old ten-minute wait"
        );
        assert!(
            !gate.browse(at(start, last + 30), false, 8),
            "the 15 s minimum"
        );
    }

    #[test]
    fn wakes_no_session_followed_come_ever_less_often() {
        let start = Instant::now();
        let mut gate = WakeGate::default();
        // A device that browses every 20 s and never connects.
        let mut wakes = Vec::new();
        for s in (0..3600).step_by(20) {
            if gate.browse(at(start, s), false, 7) {
                wakes.push(s);
            }
        }
        let gaps: Vec<u64> = wakes.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(gaps.windows(2).all(|pair| pair[0] <= pair[1]), "{gaps:?}");
        // Then one every ten minutes: six an hour.
        let settled = &gaps[gaps.len() - 3..];
        assert!(
            settled
                .iter()
                .all(|&gap| gap >= WAKE_EVERY_AT_MOST.as_secs()),
            "{gaps:?}"
        );
        assert!(
            wakes.len() <= 12,
            "{} wakes in the first hour: {wakes:?}",
            wakes.len()
        );
        // An iPad connects: back to the short interval.
        assert!(gate.browse(at(start, 4000), false, 8));
        assert!(gate.browse(at(start, 4015), false, 9));
    }
}
