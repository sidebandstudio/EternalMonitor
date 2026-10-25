//! The ScreenCaptureKit side of macOS capture: an `SCStream` of one display
//! delivering `420v` IOSurface frames, and the capture loop that paces them
//! into the frame slot.
//!
//! Starting a stream needs the Screen Recording grant, so nothing here runs
//! in the test suite; the decisions it makes are in the parent module and
//! tested there. The real stream runs in the signed app.

use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_foundation::CGRect;
use objc2_core_graphics::{
    kCGDisplayStreamYCbCrMatrix_ITU_R_709_2, CGDisplayBounds, CGDisplayCopyDisplayMode,
    CGDisplayMode, CGPreflightScreenCaptureAccess,
};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSError, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamFrameInfoScreenRect, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};
use tracing::{info, warn};

use super::{
    classify_error, is_from_display, mailbox, next_step, CaptureFailure, Delivery, FrameStatus,
    Inbox, Mailbox, SparsePacing, Step, StreamSettings, Surface,
};
use crate::capture::{heartbeat, idle_heartbeat, FrameSlot, FrameUse, RawFrame};
use crate::control::SharedControl;
use crate::input::mac::display::DisplayBounds;
use crate::stats::PIPELINE_STATS;

/// How long ScreenCaptureKit may take to list displays or start.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// The loop wakes at least this often to keep heartbeats and see a stop.
const POLL: Duration = Duration::from_millis(50);

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetSampleAttachmentsArray(sample: *mut c_void, create: u8) -> *const c_void;
    fn CMSampleBufferGetImageBuffer(sample: *mut c_void) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, kind: isize, value: *mut c_void) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGRectMakeWithDictionaryRepresentation(dictionary: *const c_void, rect: *mut CGRect)
        -> bool;
}

/// `kCFNumberSInt64Type`.
const CF_NUMBER_SINT64: isize = 4;

/// One delivery from ScreenCaptureKit.
pub struct Delivered {
    pub status: FrameStatus,
    /// Set for complete frames.
    pub surface: Option<Surface>,
    /// Where the captured content is, in global points.
    pub screen_rect: Option<DisplayBounds>,
    pub at: Instant,
}

/// Reads a sample buffer's frame status and screen rect.
///
/// # Safety
/// `sample` must be a valid `CMSampleBuffer`.
unsafe fn frame_info(sample: *mut c_void) -> Option<(FrameStatus, Option<DisplayBounds>)> {
    let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, 0);
    if attachments.is_null() || CFArrayGetCount(attachments) < 1 {
        return None;
    }
    let info = CFArrayGetValueAtIndex(attachments, 0);
    let key = |name: &NSString| name as *const NSString as *const c_void;
    let number = CFDictionaryGetValue(info, key(SCStreamFrameInfoStatus));
    let mut raw = 0i64;
    if number.is_null()
        || !CFNumberGetValue(number, CF_NUMBER_SINT64, (&mut raw as *mut i64).cast())
    {
        return None;
    }
    let rect_dictionary = CFDictionaryGetValue(info, key(SCStreamFrameInfoScreenRect));
    let mut rect = CGRect::default();
    let screen_rect = (!rect_dictionary.is_null()
        && CGRectMakeWithDictionaryRepresentation(rect_dictionary, &mut rect))
    .then(|| rect.into());
    Some((FrameStatus::from_raw(raw), screen_rect))
}

fn failure_from(error: Option<&NSError>) -> CaptureFailure {
    let preflight = CGPreflightScreenCaptureAccess();
    match error {
        Some(error) => classify_error(
            &error.domain().to_string(),
            error.code() as i64,
            &error.localizedDescription().to_string(),
            preflight,
        ),
        None if !preflight => CaptureFailure::PermissionDenied,
        None => CaptureFailure::Failed("ScreenCaptureKit gave no reason".into()),
    }
}

struct OutputIvars {
    deliveries: Mailbox<Delivered>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class
    // implements no Drop.
    #[unsafe(super(NSObject))]
    #[name = "EternalMonitorCaptureOutput"]
    #[ivars = OutputIvars]
    struct CaptureOutput;

    unsafe impl NSObjectProtocol for CaptureOutput {}

    unsafe impl SCStreamOutput for CaptureOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            let raw = sample as *const CMSampleBuffer as *mut c_void;
            // SAFETY: ScreenCaptureKit lends a valid sample buffer for the
            // call; the surface is retained before it returns.
            let Some((status, screen_rect)) = (unsafe { frame_info(raw) }) else {
                return;
            };
            let surface = if status == FrameStatus::Complete {
                unsafe { Surface::retain(CMSampleBufferGetImageBuffer(raw)) }
            } else {
                None
            };
            // A full queue means the loop is behind: drop this delivery
            // rather than hold more of ScreenCaptureKit's buffers.
            self.ivars().deliveries.frame(Delivered {
                status,
                surface,
                screen_rect,
                at: Instant::now(),
            });
        }
    }

    unsafe impl SCStreamDelegate for CaptureOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            // Never queued behind frames: the loop always sees it.
            self.ivars().deliveries.stop(failure_from(Some(error)));
        }
    }
);

impl CaptureOutput {
    fn new(deliveries: Mailbox<Delivered>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { deliveries });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

/// ScreenCaptureKit objects are immutable descriptions, usable from any
/// thread; this carries one out of a completion handler.
struct Sendable<T>(T);
// SAFETY: see above.
unsafe impl<T> Send for Sendable<T> {}

fn find_display(display_id: u32) -> Result<Retained<SCDisplay>, CaptureFailure> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: ScreenCaptureKit passes valid objects or null.
            let result = match unsafe { content.as_ref() } {
                Some(content) => unsafe { content.displays() }
                    .iter()
                    .find(|display| unsafe { display.displayID() } == display_id)
                    .map(Sendable)
                    .ok_or(CaptureFailure::DisplayNotFound(display_id)),
                None => Err(failure_from(unsafe { error.as_ref() })),
            };
            let _ = tx.send(result);
        },
    );
    // SAFETY: the handler is a valid block of the declared type.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    match rx.recv_timeout(CALL_TIMEOUT) {
        Ok(result) => result.map(|display| display.0),
        Err(_) => Err(CaptureFailure::Failed(
            "ScreenCaptureKit did not list the displays in time".into(),
        )),
    }
}

/// Whether ScreenCaptureKit offers `display_id` for capture now.
pub fn display_listed(display_id: u32) -> Result<bool, CaptureFailure> {
    match find_display(display_id) {
        Ok(_) => Ok(true),
        Err(CaptureFailure::DisplayNotFound(_)) => Ok(false),
        Err(failure) => Err(failure),
    }
}

/// A running capture of one display. Stops when dropped.
pub struct ScreenStream {
    stream: Retained<SCStream>,
    _output: Retained<CaptureOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    deliveries: Inbox<Delivered>,
}

impl ScreenStream {
    pub fn start(settings: StreamSettings) -> Result<Self, CaptureFailure> {
        if !CGPreflightScreenCaptureAccess() {
            return Err(CaptureFailure::PermissionDenied);
        }
        let display = find_display(settings.display_id)?;
        // SAFETY: plain ScreenCaptureKit object setup with valid arguments.
        let (filter, config) = unsafe {
            let filter = SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            );
            let config = SCStreamConfiguration::new();
            config.setWidth(settings.width as usize);
            config.setHeight(settings.height as usize);
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: settings.fps as i32,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            });
            config.setPixelFormat(settings.pixel_format);
            config.setShowsCursor(settings.shows_cursor);
            config.setQueueDepth(settings.queue_depth as isize);
            // The decoder assumes BT.709.
            config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
            (filter, config)
        };
        let (mailbox, deliveries) = mailbox(4);
        let output = CaptureOutput::new(mailbox);
        let queue = DispatchQueue::new("com.sidebandstudio.eternalmonitor.capture", None);
        // SAFETY: as above; the output and queue outlive the stream (both
        // are held in `Self`).
        let stream = unsafe {
            let stream = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                Some(ProtocolObject::from_ref(&*output)),
            );
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|error| failure_from(Some(&error)))?;
            stream
        };
        let (done_tx, done) = mpsc::channel();
        let handler = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: a valid error or null.
            let _ = done_tx.send(unsafe { error.as_ref() }.map(|error| failure_from(Some(error))));
        });
        // SAFETY: a configured stream and a valid block.
        unsafe { stream.startCaptureWithCompletionHandler(Some(&handler)) };
        match done.recv_timeout(CALL_TIMEOUT) {
            Ok(None) => Ok(Self {
                stream,
                _output: output,
                _queue: queue,
                deliveries,
            }),
            Ok(Some(failure)) => Err(failure),
            Err(_) => Err(CaptureFailure::Failed(
                "ScreenCaptureKit did not start in time".into(),
            )),
        }
    }

    pub fn deliveries(&self) -> &Inbox<Delivered> {
        &self.deliveries
    }
}

impl Drop for ScreenStream {
    fn drop(&mut self) {
        // SAFETY: a started stream; no completion handler is needed.
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}

/// A display's current bounds (points) and backing size (pixels).
pub fn display_geometry(display_id: u32) -> (DisplayBounds, (u32, u32)) {
    let bounds: DisplayBounds = CGDisplayBounds(display_id).into();
    let mode = CGDisplayCopyDisplayMode(display_id);
    let pixels = match mode.as_deref() {
        Some(mode) => (
            CGDisplayMode::pixel_width(Some(mode)) as u32,
            CGDisplayMode::pixel_height(Some(mode)) as u32,
        ),
        None => (bounds.width as u32, bounds.height as u32),
    };
    (bounds, pixels)
}

fn publish_geometry(shared: &SharedControl, bounds: DisplayBounds, pixels: (u32, u32)) {
    *shared.capture_geometry.lock() = Some(crate::input::CaptureGeometry {
        left: bounds.x as i32,
        top: bounds.y as i32,
        width: pixels.0,
        height: pixels.1,
    });
}

/// Which display to capture, asked on every pass of the capture loop.
pub trait DisplayChoice {
    fn display(&mut self, now: Instant) -> u32;
    /// ScreenCaptureKit does not offer `display_id`. Returns whether to
    /// ask again (another display may be chosen) instead of failing.
    fn not_listed(&mut self, display_id: u32, now: Instant) -> bool;
}

/// Captures the chosen display into `slot` until the generation stops, or
/// the stream fails (returned as the stage's error: a missing grant is a
/// blocked state, anything else restarts the pipeline). When the choice
/// changes, the stream moves to the new display within this generation.
pub fn run_capture_loop(
    slot: &FrameSlot,
    shared: SharedControl,
    generation: u64,
    frame_number: &mut u64,
    choice: &mut dyn DisplayChoice,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let empty = Arc::new(Vec::new());
    'stream: loop {
        let display_id = choice.display(Instant::now());
        let (mut bounds, pixels) = display_geometry(display_id);
        let settings = StreamSettings::new(display_id, pixels, shared.effective_fps());
        let stream = match ScreenStream::start(settings) {
            Ok(stream) => stream,
            Err(CaptureFailure::DisplayNotFound(_))
                if choice.not_listed(display_id, Instant::now()) =>
            {
                continue 'stream;
            }
            Err(failure) => return Err(failure.into_error()),
        };
        info!(?settings, ?bounds, "ScreenCaptureKit capture started");
        publish_geometry(&shared, bounds, pixels);

        let mut pacing = SparsePacing::new();
        let mut cached: Option<Surface> = None;
        let mut was_watching = false;
        let mut resumed = false;
        let publish = |surface: Surface, at: Instant, frame_number: &mut u64| {
            *frame_number += 1;
            let (width, height) = surface.size();
            PIPELINE_STATS.lock().record_capture(width, height);
            heartbeat(&shared.hb_capture_frame_ms);
            slot.publish(RawFrame {
                frame_number: *frame_number,
                timestamp: at,
                data: Arc::clone(&empty),
                width,
                height,
                surface: Some(surface),
            });
        };
        loop {
            if !shared.running.load(Ordering::SeqCst)
                || !crate::supervisor::generation_is_current(generation)
            {
                info!("Capture loop stopping (stop requested or generation superseded)");
                return Ok(());
            }
            if shared.effective_fps() != settings.fps {
                info!("Frame rate changed; restarting ScreenCaptureKit capture");
                continue 'stream;
            }
            let chosen = choice.display(Instant::now());
            if chosen != display_id {
                info!(
                    from = display_id,
                    to = chosen,
                    "Moving capture to another display"
                );
                continue 'stream;
            }
            heartbeat(&shared.hb_capture_loop_ms);
            let watching = shared.client_connected();
            if was_watching && !watching {
                pacing.viewer_left();
            }
            was_watching = watching;
            if !watching {
                idle_heartbeat(&shared, crate::clock::host_now_us() / 1000);
            }
            let now = Instant::now();
            let wait = pacing
                .keepalive_in(watching, cached.is_some(), now)
                .map_or(POLL, |due| due.min(POLL));
            match next_step(stream.deliveries(), wait.is_zero()) {
                Step::Fail(failure) => {
                    warn!(?failure, "ScreenCaptureKit stopped the stream");
                    return Err(failure.into_error());
                }
                Step::Resend => {
                    if let Some(surface) = cached.clone() {
                        publish(surface, now, frame_number);
                        pacing.resent(now);
                    }
                    continue;
                }
                Step::Wait => {}
            }
            match stream.deliveries().next(wait) {
                Some(Delivery::Frame(delivered)) => {
                    if let Some(rect) = delivered.screen_rect {
                        if !is_from_display(rect, bounds) {
                            // The user may have moved the display; only a
                            // frame matching its live bounds is shown.
                            let (live, _) = display_geometry(display_id);
                            if !is_from_display(rect, live) {
                                warn!(?rect, ?live, "Dropped a frame from another display");
                                continue;
                            }
                            bounds = live;
                            publish_geometry(&shared, bounds, pixels);
                        }
                    }
                    if !resumed && delivered.status == FrameStatus::Complete {
                        resumed = true;
                        crate::capture::recovery::note_capture_resumed(&shared);
                    }
                    match pacing.on_frame(delivered.status, watching, delivered.at) {
                        Some(FrameUse::Publish) => {
                            if let Some(surface) = delivered.surface {
                                cached = Some(surface.clone());
                                publish(surface, delivered.at, frame_number);
                            }
                        }
                        Some(FrameUse::Cache) => cached = delivered.surface,
                        None => {}
                    }
                }
                Some(Delivery::Stopped(failure)) => {
                    warn!(?failure, "ScreenCaptureKit stopped the stream");
                    return Err(failure.into_error());
                }
                None => {}
            }
        }
    }
}
