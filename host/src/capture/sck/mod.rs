//! ScreenCaptureKit capture of one display, by `CGDirectDisplayID`: the
//! virtual display in second-screen mode, the main display when mirroring.
//!
//! This file is the policy, plain Rust that needs no permission to test:
//! the stream settings, what each frame status means, how ScreenCaptureKit
//! errors map to what the user is told, the wrong-display guard, and the
//! pacing. [`stream`] is the ScreenCaptureKit side, which needs the Screen
//! Recording grant and so only runs in the signed app.
//!
//! Pacing is sparse, as on Windows: ScreenCaptureKit sends a frame only when
//! the display changes (and marks the rest `Idle`), every new picture goes
//! to the encoder, and a static display costs one resend every
//! `IDLE_KEEPALIVE` (750 ms) while a client watches, never constant-rate
//! repeats. Frames arrive as IOSurface-backed `420v` `CVPixelBuffer`s that
//! go to the VideoToolbox encoder without a copy ([`Surface`]).

pub mod stream;

use std::time::{Duration, Instant};

use super::recovery::{CaptureBlock, CaptureUnavailable};
use super::{FrameUse, IdleGate, IDLE_KEEPALIVE};
use crate::input::mac::display::DisplayBounds;

/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`: what the direct
/// encoder's session takes as is.
pub const PIXEL_FORMAT_420V: u32 = u32::from_be_bytes(*b"420v");

/// The stream's configuration, before it is an `SCStreamConfiguration`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSettings {
    pub display_id: u32,
    /// Output size in pixels: the display's backing size, so a HiDPI
    /// display is captured at full resolution, not its size in points.
    pub width: u32,
    pub height: u32,
    /// `minimumFrameInterval` is 1/fps. macOS 15 defaults it to 1/60, so a
    /// 120 Hz stream must ask.
    pub fps: u32,
    pub pixel_format: u32,
    pub shows_cursor: bool,
    /// Buffers ScreenCaptureKit may have outstanding: the encoder's, the
    /// capture slot's, the cached keepalive frame, and two in flight.
    pub queue_depth: u32,
}

impl StreamSettings {
    pub fn new(display_id: u32, pixels: (u32, u32), fps: u32) -> Self {
        Self {
            display_id,
            // 4:2:0 needs even sizes.
            width: pixels.0.max(2) & !1,
            height: pixels.1.max(2) & !1,
            fps: fps.clamp(1, 240),
            pixel_format: PIXEL_FORMAT_420V,
            shows_cursor: true,
            queue_depth: 5,
        }
    }
}

/// `SCFrameStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStatus {
    /// A new picture.
    Complete,
    /// Nothing changed since the last frame.
    Idle,
    Blank,
    Suspended,
    Started,
    Stopped,
    Unknown(i64),
}

impl FrameStatus {
    pub fn from_raw(raw: i64) -> Self {
        match raw {
            0 => Self::Complete,
            1 => Self::Idle,
            2 => Self::Blank,
            3 => Self::Suspended,
            4 => Self::Started,
            5 => Self::Stopped,
            other => Self::Unknown(other),
        }
    }
}

/// Why a stream could not start or stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureFailure {
    /// Screen Recording is off for this app. Nothing changes until the
    /// user turns it on (and often reopens the app).
    PermissionDenied,
    /// The display is not among the ones ScreenCaptureKit offers: a
    /// virtual display that went away, or one it has not listed yet.
    DisplayNotFound(u32),
    /// The system stopped the stream (sleep, a display reconfiguration, the
    /// capture service restarting). Starting again usually works.
    Interrupted(i64),
    Failed(String),
}

/// `SCStreamErrorDomain`.
pub const SC_STREAM_ERROR_DOMAIN: &str = "com.apple.ScreenCaptureKit.SCStreamErrorDomain";

/// Maps an `NSError` from ScreenCaptureKit. `preflight` is
/// `CGPreflightScreenCaptureAccess()`: without the grant, every failure is
/// the grant's, whatever code the call reported.
pub fn classify_error(
    domain: &str,
    code: i64,
    description: &str,
    preflight: bool,
) -> CaptureFailure {
    if !preflight {
        return CaptureFailure::PermissionDenied;
    }
    if domain != SC_STREAM_ERROR_DOMAIN {
        return CaptureFailure::Failed(format!("{domain} {code}: {description}"));
    }
    match code {
        // UserDeclined.
        -3801 => CaptureFailure::PermissionDenied,
        // NoDisplayList, NoCaptureSource.
        -3814 | -3815 => CaptureFailure::DisplayNotFound(0),
        // ConnectionInvalid, ConnectionInterrupted, SystemStoppedStream.
        -3804 | -3805 | -3821 => CaptureFailure::Interrupted(code),
        _ => CaptureFailure::Failed(format!("ScreenCaptureKit {code}: {description}")),
    }
}

impl CaptureFailure {
    /// The error the capture stage returns. A missing grant becomes a
    /// blocked state, which the stage retries with backoff and the GUI
    /// explains; restarting the pipeline cannot grant it.
    pub fn into_error(self) -> Box<dyn std::error::Error + Send + Sync> {
        match self {
            Self::PermissionDenied => Box::new(CaptureUnavailable {
                block: CaptureBlock::ScreenRecordingDenied,
                detail: "Screen Recording is not allowed for EternalMonitor".into(),
            }),
            Self::DisplayNotFound(id) => format!("display {id} is not available to capture").into(),
            Self::Interrupted(code) => format!("the system stopped screen capture ({code})").into(),
            Self::Failed(why) => why.into(),
        }
    }
}

/// Whether a frame's `screenRect` (global points) is the display it was
/// meant to show. After a display reconfiguration ScreenCaptureKit has
/// been seen to deliver another virtual display's frames (FB17797423);
/// those must never reach the client, whose input maps onto `bounds`.
pub fn is_from_display(screen_rect: DisplayBounds, bounds: DisplayBounds) -> bool {
    const SLOP: f64 = 1.0;
    (screen_rect.x - bounds.x).abs() <= SLOP
        && (screen_rect.y - bounds.y).abs() <= SLOP
        && (screen_rect.width - bounds.width).abs() <= SLOP
        && (screen_rect.height - bounds.height).abs() <= SLOP
}

/// What the capture loop does with each delivery and each quiet moment.
pub(crate) struct SparsePacing {
    gate: IdleGate,
    last_sent: Option<Instant>,
}

impl SparsePacing {
    pub fn new() -> Self {
        Self {
            gate: IdleGate::new(),
            last_sent: None,
        }
    }

    /// A delivered frame: publish it, cache it, or (with no new picture)
    /// ignore it. Idle frames are never repeated here; the keepalive covers
    /// a static display.
    pub fn on_frame(
        &mut self,
        status: FrameStatus,
        watching: bool,
        now: Instant,
    ) -> Option<FrameUse> {
        if status != FrameStatus::Complete {
            return None;
        }
        let action = self.gate.on_frame(watching, now);
        if action == FrameUse::Publish {
            self.last_sent = Some(now);
        }
        Some(action)
    }

    /// How long until the cached picture is due again; `None` when no
    /// resend should happen (nobody watching, or nothing cached yet).
    pub fn keepalive_in(&self, watching: bool, have_frame: bool, now: Instant) -> Option<Duration> {
        if !watching || !have_frame {
            return None;
        }
        Some(match self.last_sent {
            Some(sent) => (sent + IDLE_KEEPALIVE).saturating_duration_since(now),
            // A viewer arrived while the display sat still: send at once.
            None => Duration::ZERO,
        })
    }

    pub fn resent(&mut self, now: Instant) {
        self.last_sent = Some(now);
    }

    /// A viewer left: the next one gets the cached picture at once.
    pub fn viewer_left(&mut self) {
        self.last_sent = None;
    }
}

/// An IOSurface-backed `CVPixelBuffer` from ScreenCaptureKit, retained for
/// as long as the encoder or the keepalive cache holds it.
pub struct Surface(std::ptr::NonNull<std::ffi::c_void>);

mod surface_ffi {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRetain(cf: *const c_void) -> *const c_void;
        pub fn CFRelease(cf: *const c_void);
    }
    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        pub fn CVPixelBufferGetWidth(buffer: *mut c_void) -> usize;
        pub fn CVPixelBufferGetHeight(buffer: *mut c_void) -> usize;
    }
}

// SAFETY: a CVPixelBuffer is a reference-counted CF object. Retain and
// release are thread-safe, and the host only reads its size and hands it
// to VideoToolbox, which reads the pixels.
unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

impl Surface {
    /// Retains `raw` (a `CVPixelBufferRef` on loan from a sample buffer).
    ///
    /// # Safety
    /// `raw` must be null or a valid `CVPixelBuffer`.
    pub unsafe fn retain(raw: *mut std::ffi::c_void) -> Option<Self> {
        let raw = std::ptr::NonNull::new(raw)?;
        surface_ffi::CFRetain(raw.as_ptr());
        Some(Self(raw))
    }

    /// The buffer, still owned by this `Surface`.
    pub fn as_raw(&self) -> *mut std::ffi::c_void {
        self.0.as_ptr()
    }

    /// A new +1 reference for a consumer that releases it itself.
    pub fn retained_raw(&self) -> *mut std::ffi::c_void {
        // SAFETY: valid for our lifetime; the caller owns the new reference.
        unsafe { surface_ffi::CFRetain(self.0.as_ptr()) };
        self.0.as_ptr()
    }

    pub fn size(&self) -> (u32, u32) {
        // SAFETY: valid for our lifetime.
        unsafe {
            (
                surface_ffi::CVPixelBufferGetWidth(self.0.as_ptr()) as u32,
                surface_ffi::CVPixelBufferGetHeight(self.0.as_ptr()) as u32,
            )
        }
    }
}

impl Clone for Surface {
    fn clone(&self) -> Self {
        // SAFETY: valid for our lifetime; the clone owns the new reference.
        unsafe { surface_ffi::CFRetain(self.0.as_ptr()) };
        Self(self.0)
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference.
        unsafe { surface_ffi::CFRelease(self.0.as_ptr()) };
    }
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (width, height) = self.size();
        write!(f, "Surface({width}x{height})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn settings_capture_backing_pixels_with_the_cursor_in_420v() {
        let duo = StreamSettings::new(7, (2670, 1878), 120);
        assert_eq!(
            duo,
            StreamSettings {
                display_id: 7,
                width: 2670,
                height: 1878,
                fps: 120,
                pixel_format: PIXEL_FORMAT_420V,
                shows_cursor: true,
                queue_depth: 5,
            }
        );
        assert_eq!(PIXEL_FORMAT_420V, 0x3432_3076);
        let odd = StreamSettings::new(1, (3441, 1441), 0);
        assert_eq!((odd.width, odd.height, odd.fps), (3440, 1440, 1));
    }

    #[test]
    fn frame_statuses_map_from_sc_frame_status() {
        let all: Vec<_> = (0..=6).map(FrameStatus::from_raw).collect();
        assert_eq!(
            all,
            [
                FrameStatus::Complete,
                FrameStatus::Idle,
                FrameStatus::Blank,
                FrameStatus::Suspended,
                FrameStatus::Started,
                FrameStatus::Stopped,
                FrameStatus::Unknown(6)
            ]
        );
    }

    #[test]
    fn a_missing_grant_is_a_clear_blocked_state_whatever_the_code() {
        for (domain, code) in [
            (SC_STREAM_ERROR_DOMAIN, -3801),
            (SC_STREAM_ERROR_DOMAIN, -3802),
            ("NSOSStatusErrorDomain", -1),
        ] {
            assert_eq!(
                classify_error(domain, code, "", false),
                CaptureFailure::PermissionDenied
            );
        }
        // The user declined even though preflight said yes.
        assert_eq!(
            classify_error(SC_STREAM_ERROR_DOMAIN, -3801, "", true),
            CaptureFailure::PermissionDenied
        );
        let error = CaptureFailure::PermissionDenied.into_error();
        let blocked = error
            .downcast_ref::<CaptureUnavailable>()
            .expect("a blocked state, retried by the stage");
        assert_eq!(blocked.block, CaptureBlock::ScreenRecordingDenied);
    }

    #[test]
    fn other_errors_map_to_restartable_failures() {
        assert_eq!(
            classify_error(SC_STREAM_ERROR_DOMAIN, -3815, "", true),
            CaptureFailure::DisplayNotFound(0)
        );
        assert_eq!(
            classify_error(SC_STREAM_ERROR_DOMAIN, -3821, "", true),
            CaptureFailure::Interrupted(-3821)
        );
        let CaptureFailure::Failed(why) =
            classify_error(SC_STREAM_ERROR_DOMAIN, -3812, "bad parameter", true)
        else {
            panic!()
        };
        assert!(
            why.contains("-3812") && why.contains("bad parameter"),
            "{why}"
        );
        assert!(matches!(
            classify_error("NSCocoaErrorDomain", 4, "x", true),
            CaptureFailure::Failed(_)
        ));
        for failure in [
            CaptureFailure::DisplayNotFound(9),
            CaptureFailure::Interrupted(-3821),
            CaptureFailure::Failed("x".into()),
        ] {
            // Not a blocked state: the supervisor restarts the pipeline,
            // which falls back to mirroring if the display is gone.
            assert!(failure
                .into_error()
                .downcast_ref::<CaptureUnavailable>()
                .is_none());
        }
    }

    #[test]
    fn frames_from_another_display_are_recognised() {
        let virtual_display = DisplayBounds {
            x: 3440.0,
            y: 0.0,
            width: 1335.0,
            height: 939.0,
        };
        assert!(is_from_display(virtual_display, virtual_display));
        assert!(is_from_display(
            DisplayBounds {
                x: 3440.5,
                ..virtual_display
            },
            virtual_display
        ));
        // Another virtual display of the same size, placed elsewhere.
        assert!(!is_from_display(
            DisplayBounds {
                x: 4775.0,
                ..virtual_display
            },
            virtual_display
        ));
        // The outer-size display at the same origin.
        assert!(!is_from_display(
            DisplayBounds {
                width: 1017.0,
                height: 699.0,
                ..virtual_display
            },
            virtual_display
        ));
    }

    #[test]
    fn a_static_display_costs_one_resend_per_keepalive_not_constant_repeats() {
        let start = Instant::now();
        let mut pacing = SparsePacing::new();
        // The first picture opens the encoder.
        assert_eq!(
            pacing.on_frame(FrameStatus::Complete, true, start),
            Some(FrameUse::Publish)
        );
        // Ten seconds of a still display at 120 Hz: ScreenCaptureKit sends
        // Idle frames, which publish nothing; only the keepalive resends.
        let mut sent = 0;
        let mut t = start;
        while t < start + Duration::from_secs(10) {
            t += Duration::from_micros(8_333);
            assert_eq!(pacing.on_frame(FrameStatus::Idle, true, t), None);
            if pacing.keepalive_in(true, true, t) == Some(Duration::ZERO) {
                pacing.resent(t);
                sent += 1;
            }
        }
        assert_eq!(sent, 13, "one every 750 ms");
        assert!(pacing.keepalive_in(true, true, t).unwrap() <= IDLE_KEEPALIVE);
    }

    #[test]
    fn every_new_picture_is_sent_while_watching_and_cached_while_not() {
        let start = Instant::now();
        let mut pacing = SparsePacing::new();
        for i in 0..120u32 {
            assert_eq!(
                pacing.on_frame(FrameStatus::Complete, true, start + i * 8 * MS),
                Some(FrameUse::Publish)
            );
        }
        // Moving content keeps resetting the keepalive.
        let last = start + 119 * 8 * MS;
        assert_eq!(
            pacing.keepalive_in(true, true, last + 100 * MS),
            Some(IDLE_KEEPALIVE - 100 * MS)
        );
        // Nobody watching: pictures are cached, not encoded, and nothing is
        // resent.
        pacing.viewer_left();
        assert_eq!(
            pacing.on_frame(FrameStatus::Complete, false, last + 200 * MS),
            Some(FrameUse::Cache)
        );
        assert_eq!(pacing.keepalive_in(false, true, last + 5_000 * MS), None);
        // A viewer arriving at a still display gets the cached picture now.
        assert_eq!(
            pacing.keepalive_in(true, true, last + 5_000 * MS),
            Some(Duration::ZERO)
        );
        // Without a cached picture there is nothing to resend.
        assert_eq!(SparsePacing::new().keepalive_in(true, false, start), None);
    }

    #[test]
    fn statuses_without_a_new_picture_publish_nothing() {
        let mut pacing = SparsePacing::new();
        let now = Instant::now();
        for status in [
            FrameStatus::Idle,
            FrameStatus::Blank,
            FrameStatus::Suspended,
            FrameStatus::Started,
            FrameStatus::Stopped,
            FrameStatus::Unknown(9),
        ] {
            assert_eq!(pacing.on_frame(status, true, now), None, "{status:?}");
        }
    }
}
