//! What Windows currently believes about our synthetic pen, and the frames
//! that bring it to the state each sample asks for.
//!
//! Pure so that tests can drive every transition against a model of Windows;
//! `windows_inject.rs` only performs the frames this type chooses. The model
//! follows the injection state table Microsoft documents for
//! `InjectTouchInput`, which synthetic pen injection shares: a contact starts
//! with DOWN from out of range or hover, updates while in contact, and ends
//! with UP or a cancel at the last updated position; hover starts or updates
//! with an in-range UPDATE and ends with a bare UPDATE. A rejected frame
//! cancels every injected contact. ERROR_NOT_READY applies nothing; the same
//! frame is retried, unless its sample goes stale first: then the next one is
//! planned from the state Windows last accepted. A frame that would come too
//! soon after the last waits the same way, for its spacing, and so does one
//! past the budget of a single flush.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::{CaptureGeometry, PenPhase, PenSample, VirtualScreen};

/// How long a Pencil sample may wait for Windows, natively or on the mouse
/// fallback, before it is dropped rather than injected. Waiting that long
/// means Windows is stalled, or input is blocked by UIPI or the secure
/// desktop; by the time it works again the ink is stale and could land in
/// another window. Every sample is aged from when the host received it.
pub(super) const INPUT_STALE: Duration = Duration::from_secs(1);

/// Samples that may wait for Windows. Positions coalesce, so this bounds
/// strokes that start and end while Windows stays not ready; past it native
/// input counts as stuck and the waiting samples go to the mouse.
pub(super) const QUEUE_LIMIT: usize = 8;

/// How long a device recreated because native input seemed stuck has to
/// accept a frame before Pencil input goes to the mouse instead. While
/// Windows holds a contact, native input also waits this much longer before
/// counting as stuck.
pub(super) const STUCK_TRIAL: Duration = Duration::from_millis(200);

/// `POINTER_FLAG_*` values from winuser.h, so frames can be built and checked
/// off Windows. `windows_inject.rs` asserts that they match the SDK's.
pub mod pointer_flag {
    pub const INRANGE: u32 = 0x0000_0002;
    pub const INCONTACT: u32 = 0x0000_0004;
    pub const FIRSTBUTTON: u32 = 0x0000_0010;
    pub const PRIMARY: u32 = 0x0000_2000;
    pub const CANCELED: u32 = 0x0000_8000;
    pub const DOWN: u32 = 0x0001_0000;
    pub const UPDATE: u32 = 0x0002_0000;
    pub const UP: u32 = 0x0004_0000;
}

/// The `POINTER_PEN_INFO` fields one injected frame sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerFrame {
    pub flags: u32,
    /// `ptPixelLocation`, measured from the virtual screen's top-left.
    pub location: (i32, i32),
    pub pressure: u32,
    pub tilt_x: i32,
    pub tilt_y: i32,
}

impl PointerFrame {
    pub fn new(frame: PenSample, screen: VirtualScreen) -> Self {
        use pointer_flag::*;
        let flags = match frame.phase {
            PenPhase::Down => DOWN | INRANGE | INCONTACT | FIRSTBUTTON,
            PenPhase::Move => UPDATE | INRANGE | INCONTACT | FIRSTBUTTON,
            // Finish contact completely. Supported UIKit hover starts a new
            // in-range update; unsupported devices must not leave ghost hover.
            PenPhase::Up => UP,
            PenPhase::Cancel => UP | CANCELED,
            PenPhase::Hover => UPDATE | INRANGE,
            PenPhase::Leave => UPDATE,
        };
        Self {
            flags: flags | PRIMARY,
            location: super::desktop_pixel_to_pointer_location(frame.x, frame.y, screen),
            pressure: frame.pressure,
            tilt_x: frame.tilt_x,
            tilt_y: frame.tilt_y,
        }
    }
}

/// Windows' view of our pointer after the last frame it accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerState {
    #[default]
    Out,
    Hover(PenSample),
    Contact(PenSample),
}

/// The pointer state without its sample, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    Out,
    Hover,
    Contact,
}

impl PointerState {
    pub fn kind(self) -> PointerKind {
        match self {
            PointerState::Out => PointerKind::Out,
            PointerState::Hover(_) => PointerKind::Hover,
            PointerState::Contact(_) => PointerKind::Contact,
        }
    }
}

/// The result of one injected frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Injected,
    /// ERROR_NOT_READY after the injector's own retry window: nothing applied.
    NotReady,
    /// Any other refusal. Windows has cancelled our contact.
    Rejected,
}

/// How long to retry a frame Windows was not ready for. Windows timestamps
/// injection at 0.1 ms resolution, so coalesced samples sent back to back
/// can be refused until its clock advances.
const NOT_READY_WINDOW: Duration = Duration::from_millis(2);

/// Retries ERROR_NOT_READY briefly while Windows is accepting frames. A frame
/// still refused after that means Windows' input queue is stuck; later
/// frames get one attempt each until one is accepted, so a stuck queue costs
/// one call per sample or refresh tick rather than stalling the transport.
#[derive(Debug, Default)]
pub struct NotReadyRetry {
    stalled: bool,
}

impl NotReadyRetry {
    pub fn call<T, E>(
        &mut self,
        mut attempt: impl FnMut() -> Result<T, E>,
        not_ready: impl Fn(&E) -> bool,
    ) -> Result<T, E> {
        let started = Instant::now();
        loop {
            let result = attempt();
            let refused = result.as_ref().is_err_and(&not_ready);
            if refused && !self.stalled && started.elapsed() < NOT_READY_WINDOW {
                std::thread::yield_now();
                continue;
            }
            self.stalled = refused;
            return result;
        }
    }
}

/// The least time between frames on the pen device beyond `FRAME_BURST`.
/// Windows refuses a frame with E_INVALIDARG once a dozen or so come faster
/// than about one every 100 µs. On the reference PC, 64 samples sent back to
/// back lost about one frame in twenty from the 13th on, frames 50 µs apart
/// about one in eighty, and frames 100 µs or more apart none. 250 µs is two
/// and a half times the closest clean spacing, and more than two ticks of
/// Windows' 0.1 ms input clock. Samples come that fast only as a backlog,
/// after Wi-Fi or the PC stalled; it then drains at 4000 frames a second,
/// about 16 times a Pencil's rate.
pub(super) const FRAME_SPACING: Duration = Duration::from_micros(250);

/// Frames that may go back to back. The iPad sends each batch of coalesced
/// samples together: at most four at 60 Hz, and the move and up that end a
/// stroke, so normal drawing is never held back. The soak never lost one of
/// the first twelve of a burst.
pub(super) const FRAME_BURST: u32 = 6;

/// The most frames one `flush` sends before it yields. Injection runs on the
/// transport loop, so a backlog goes out in chunks with NACK answers, input
/// and video pacing in between.
const PASS_FRAMES: u32 = FRAME_BURST;

/// The most time one `flush` spends sending frames before it yields, should
/// Windows take long enough per frame that the spacing never holds one back.
/// A pass stops before a frame that would take it past this at its average
/// cost so far; the first frame of a pass always goes.
const PASS_TIME: Duration = Duration::from_millis(1);

/// Keeps frames `FRAME_SPACING` apart on average, allowing `FRAME_BURST` at
/// once: a frame may go once the frames before it have had their spacing,
/// less the burst. Also ends each `flush` pass after `PASS_FRAMES` or
/// `PASS_TIME`. Waiting never blocks; `PenContact` holds the frame back.
#[derive(Debug, Default)]
struct FrameSpacing {
    /// When the next frame would go if every frame were spaced.
    next: Option<Instant>,
    /// When this pass started and the frames it has sent.
    pass: Option<(Instant, u32)>,
}

impl FrameSpacing {
    /// A `flush` pass starts at `now`.
    fn start_pass(&mut self, now: Instant) {
        self.pass = Some((now, 0));
    }

    /// When the next frame may go, if not yet at `now`: `now` itself once
    /// the pass has used its budget, so the next pass follows at once.
    fn wait(&self, now: Instant) -> Option<Instant> {
        if let Some((started, frames @ 1..)) = self.pass {
            let used = now.saturating_duration_since(started);
            if frames >= PASS_FRAMES || used + used / frames > PASS_TIME {
                return Some(now);
            }
        }
        let at = self.next?.checked_sub(FRAME_SPACING * (FRAME_BURST - 1))?;
        (at > now).then_some(at)
    }

    /// A frame reached Windows at `now`.
    fn sent(&mut self, now: Instant) {
        self.next = Some(self.next.map_or(now, |next| next.max(now)) + FRAME_SPACING);
        if let Some((_, frames)) = &mut self.pass {
            *frames += 1;
        }
    }
}

/// Counts refused frames by error code, so a repeated refusal is logged at
/// its 1st, 10th, 100th, 1000th... occurrence rather than every time.
#[derive(Debug)]
pub struct RefusalLog<C> {
    counts: Vec<(C, u64)>,
}

impl<C> Default for RefusalLog<C> {
    fn default() -> Self {
        Self { counts: Vec::new() }
    }
}

impl<C: PartialEq> RefusalLog<C> {
    /// Count a refusal with `code`; its count when it should be logged.
    pub fn note(&mut self, code: C) -> Option<u64> {
        let count = match self.counts.iter_mut().find(|(c, _)| *c == code) {
            Some((_, count)) => {
                *count += 1;
                *count
            }
            None => {
                self.counts.push((code, 1));
                1
            }
        };
        (count == 10u64.pow(count.ilog10())).then_some(count)
    }
}

/// `sample` with its desktop pixels mapped from its wire position onto
/// `output`, the capture rect current now.
fn mapped(mut sample: PenSample, output: Option<CaptureGeometry>) -> PenSample {
    if let Some(output) = output {
        (sample.x, sample.y) = super::norm_to_output_pixel(sample.x_norm, sample.y_norm, output);
    }
    sample
}

/// Where `PenContact` sends frames: the synthetic pen device, or a model of
/// Windows in tests.
pub trait PenTarget {
    /// The virtual screen that frame locations are measured on now.
    fn screen(&self) -> VirtualScreen;
    /// Inject one frame and report its outcome, given the state the frame
    /// starts from.
    fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome;
    /// The time now, for spacing frames.
    fn now(&self) -> Instant;
    /// Destroy and recreate the synthetic pen device, which ends any pointer
    /// Windows holds for it.
    fn recreate(&mut self);
    /// Whether there is a device to inject into. False once Windows refused
    /// to recreate it: nothing more can be delivered natively.
    fn available(&self) -> bool {
        true
    }
    /// Native input accepts nothing, even on a new device: stop using the
    /// device, which ends any pointer it holds, so Pencil input goes to the
    /// mouse until a device is asked for again.
    fn give_up(&mut self);
}

#[derive(Debug, Default)]
pub struct PenContact {
    state: PointerState,
    /// Samples Windows has not been brought to yet, oldest first: one that
    /// hit ERROR_NOT_READY and those that arrived behind it. Each is planned
    /// from the accepted state when its turn comes, so a frame deferred part
    /// way through a sample (the final move before an up) is retried before
    /// the rest of it, and mapped from its wire position onto the capture
    /// rect current at that moment. Each carries when the host received it.
    queue: VecDeque<(PenSample, Instant)>,
    /// Samples that cannot be delivered natively, oldest first, with when
    /// they were received, for the mouse; see `take_abandoned`.
    abandoned: Vec<(PenSample, Instant)>,
    /// When Windows first answered ERROR_NOT_READY since it last accepted a
    /// frame.
    refused_since: Option<Instant>,
    /// The device is new because native input seemed stuck: unless it
    /// accepts a frame by then, Pencil input goes to the mouse.
    trial_until: Option<Instant>,
    spacing: FrameSpacing,
    /// When the frame held back to keep frames `FRAME_SPACING` apart may go.
    spaced_until: Option<Instant>,
    /// The first waiting sample was rejected and started again before its
    /// frame was held back or Windows was not ready for it, so a second
    /// rejection gives it up.
    resumed: bool,
    /// Samples dropped by design, with why, so tests can tell them from
    /// samples lost by mistake.
    #[cfg(test)]
    dropped: Vec<(PenSample, &'static str)>,
}

/// What became of one sample.
enum Brought {
    Delivered,
    /// Windows was not ready: it waits, to be planned again from where it
    /// stopped.
    Waiting,
    /// Its next frame would come too soon after the last, or past this
    /// pass's budget: it waits for `PenContact::spaced_until` and is planned
    /// again from where it stopped.
    Spaced,
    /// It cannot be delivered natively: the device is gone, or Windows
    /// rejected a frame for it that a fresh start did not fix.
    Abandoned,
}

impl PenContact {
    /// A contact on a device asked for again after native input was found
    /// stuck: it has until `until` to accept a frame.
    pub fn on_trial(until: Instant) -> Self {
        Self {
            trial_until: Some(until),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> PointerState {
        self.state
    }

    /// The oldest sample still waiting for Windows.
    pub fn pending(&self) -> Option<PenSample> {
        self.queue.front().map(|&(sample, _)| sample)
    }

    /// When to `flush` again for a frame held back to keep frames
    /// `FRAME_SPACING` apart, or at once after a pass that used its budget.
    pub fn spaced_until(&self) -> Option<Instant> {
        self.spaced_until.filter(|_| !self.queue.is_empty())
    }

    /// Bring Windows to the state `sample`, received `now`, asks for, after
    /// anything still waiting. `output` is the current capture rect.
    pub fn deliver(
        &mut self,
        sample: PenSample,
        output: Option<CaptureGeometry>,
        now: Instant,
        target: &mut impl PenTarget,
    ) {
        self.expire(now);
        // Only samples waiting on a refusal count: a backlog waiting for
        // its spacing is moving.
        if self.refused_since.is_some()
            && self.queue.len() >= QUEUE_LIMIT
            && !self.coalesces(&sample)
        {
            // Windows has refused the first waiting frame for a whole queue
            // of samples: native input is stuck. Recreating the device ends
            // any pointer it holds and the retry it is owed; every waiting
            // sample, and this one, goes to the mouse instead of being lost.
            target.recreate();
            self.state = PointerState::Out;
            self.abandon(output);
            self.abandoned.push((mapped(sample, output), now));
            return;
        }
        // Behind frames held back, the sample waits for `spaced_until`'s
        // flush, so one transport pass never sends more than one budget.
        let held = self.spaced_until().is_some();
        self.enqueue(sample, now);
        if !held {
            self.flush(output, now, target);
        }
    }

    /// Deliver waiting samples in order until Windows is not ready, the next
    /// frame must wait for its spacing, or this pass has sent `PASS_FRAMES`
    /// or taken `PASS_TIME`. Call on every refresh tick so an end of contact
    /// is not left waiting for the next Pencil sample, and at
    /// `spaced_until`.
    ///
    /// Native input is stuck once Windows has accepted nothing for
    /// `INPUT_STALE` while ink waits, or `STUCK_TRIAL` longer while it holds
    /// a contact: recreating the device cancels that contact, so a stall
    /// that clears on its own just after a second keeps the stroke whole.
    /// The device is then recreated once; if the new one accepts nothing
    /// within `STUCK_TRIAL` either, the waiting samples go to the mouse and
    /// the device is given up. An end waiting alone never counts:
    /// recreating could only lose it.
    pub fn flush(
        &mut self,
        output: Option<CaptureGeometry>,
        now: Instant,
        target: &mut impl PenTarget,
    ) {
        self.expire(now);
        self.spaced_until = None;
        self.spacing.start_pass(target.now());
        loop {
            while let Some(sample) = self.pending() {
                match self.bring(mapped(sample, output), target) {
                    Brought::Delivered => {
                        self.queue.pop_front();
                    }
                    Brought::Waiting => break,
                    Brought::Spaced => return,
                    Brought::Abandoned => return self.abandon(output),
                }
            }
            if self.queue.is_empty() {
                return;
            }
            let refused_since = *self.refused_since.get_or_insert(now);
            if !self.queue.iter().any(|(sample, _)| is_ink(sample)) {
                return;
            }
            let stuck_after = match self.state {
                PointerState::Contact(_) => INPUT_STALE + STUCK_TRIAL,
                _ => INPUT_STALE,
            };
            match self.trial_until {
                Some(until) if now >= until => {
                    target.give_up();
                    self.state = PointerState::Out;
                    return self.abandon(output);
                }
                None if now.saturating_duration_since(refused_since) >= stuck_after => {
                    target.recreate();
                    self.state = PointerState::Out;
                    self.trial_until = Some(now + STUCK_TRIAL);
                }
                _ => return,
            }
        }
    }

    /// Samples that cannot be delivered natively, oldest first, with when
    /// they were received: each one left the queue without reaching
    /// Windows, and so did every sample behind it. The caller sends them to
    /// the mouse.
    pub fn take_abandoned(&mut self) -> Vec<(PenSample, Instant)> {
        std::mem::take(&mut self.abandoned)
    }

    /// Drop each waiting down, move or hover that has itself waited
    /// `INPUT_STALE`. Ends stay: an up, cancel or leave is owed to whatever
    /// Windows holds, and plans to nothing if Windows holds nothing. Fresher
    /// samples are planned from the state Windows last accepted, so a move
    /// whose down was dropped starts the contact itself.
    fn expire(&mut self, now: Instant) {
        let waiting = self.queue.len();
        self.queue.retain(|&(sample, received)| {
            let stale = is_ink(&sample) && now.saturating_duration_since(received) >= INPUT_STALE;
            #[cfg(test)]
            if stale {
                self.dropped.push((sample, "stale"));
            }
            !stale
        });
        // The oldest sample is the first to go stale.
        self.resumed &= self.queue.len() == waiting;
    }

    #[cfg(test)]
    pub fn take_dropped(&mut self) -> Vec<(PenSample, &'static str)> {
        std::mem::take(&mut self.dropped)
    }

    fn abandon(&mut self, output: Option<CaptureGeometry>) {
        self.resumed = false;
        let waiting = self
            .queue
            .drain(..)
            .map(|(sample, received)| (mapped(sample, output), received));
        self.abandoned.extend(waiting);
    }

    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    /// The session ended or was taken over: drop samples Windows never
    /// received, and end only the pointer it holds, where it holds it. If
    /// Windows is still refusing a frame, recreate the device rather than
    /// send it a different one. The end is not held back for its spacing:
    /// if Windows refuses it, recreating the device ends the pointer anyway.
    pub fn reset(&mut self, target: &mut impl PenTarget) {
        if !self.queue.is_empty() {
            #[cfg(test)]
            self.dropped
                .extend(self.queue.iter().map(|&(s, _)| (s, "reset")));
            self.queue.clear();
            self.resumed = false;
            if self.refused_since.is_some() {
                target.recreate();
                self.state = PointerState::Out;
                return;
            }
        }
        let end = match self.state {
            PointerState::Out => return,
            PointerState::Contact(last) => PenSample {
                phase: PenPhase::Cancel,
                pressure: 0,
                ..last
            },
            PointerState::Hover(last) => PenSample {
                phase: PenPhase::Leave,
                pressure: 0,
                ..last
            },
        };
        if self.end(end, target) == Outcome::NotReady {
            target.recreate();
            self.state = PointerState::Out;
        }
    }

    /// Whether `sample` replaces the last waiting one while Windows refuses
    /// the first: only the latest position matters, but the first sample is
    /// being retried, and Windows must get that same frame again. Samples
    /// waiting only for their spacing all go.
    fn coalesces(&self, sample: &PenSample) -> bool {
        let update = |sample: &PenSample| matches!(sample.phase, PenPhase::Move | PenPhase::Hover);
        self.refused_since.is_some()
            && self.queue.len() > 1
            && self.queue.back().is_some_and(|(last, _)| update(last))
            && update(sample)
    }

    fn enqueue(&mut self, sample: PenSample, received: Instant) {
        if self.coalesces(&sample) {
            let last = self
                .queue
                .back_mut()
                .expect("coalesces only behind the first");
            #[cfg(test)]
            self.dropped.push((last.0, "coalesced"));
            *last = (sample, received);
        } else {
            self.queue.push_back((sample, received));
        }
    }

    /// Plan and send the frames for one sample. Ending the old pointer comes
    /// first in every plan, so a sample abandoned on the way has delivered
    /// none of its own frames; a cancel or leave is delivered once the
    /// pointer has ended, even by recreating the device.
    fn bring(&mut self, sample: PenSample, target: &mut impl PenTarget) -> Brought {
        // An up goes where the Pencil lifted. If a display change put that
        // off the virtual screen, end the contact where Windows has it.
        let sample = match sample.phase {
            PenPhase::Up if !on_screen(sample, target.screen()) => PenSample {
                phase: PenPhase::Cancel,
                ..sample
            },
            _ => sample,
        };
        // A rejection cancels the contact; resume a move or hover from this
        // same, freshly mapped sample once, even after waiting for its
        // spacing or for Windows to be ready. A frame still rejected after
        // that, or a rejected down or up, gives the sample up.
        let mut restarted = std::mem::take(&mut self.resumed);
        loop {
            let mut rejected = false;
            for frame in self.plan(sample).into_iter().flatten() {
                if !target.available() {
                    return Brought::Abandoned;
                }
                if let Some(until) = self.spacing.wait(target.now()) {
                    self.spaced_until = Some(until);
                    self.resumed = restarted;
                    return Brought::Spaced;
                }
                let outcome = match frame.phase {
                    PenPhase::Cancel | PenPhase::Leave => self.end(frame, target),
                    _ => self.send(frame, target),
                };
                match outcome {
                    Outcome::Injected => {}
                    Outcome::NotReady => {
                        self.resumed = restarted;
                        return Brought::Waiting;
                    }
                    Outcome::Rejected => {
                        rejected = true;
                        break;
                    }
                }
            }
            if !rejected {
                return Brought::Delivered;
            }
            if restarted || !matches!(sample.phase, PenPhase::Move | PenPhase::Hover) {
                return Brought::Abandoned;
            }
            restarted = true;
        }
    }

    /// End the pointer Windows holds with `frame`, a cancel or leave where
    /// Windows last saw it. That place may be off the virtual screen after a
    /// display change, and a refused end may leave the pointer held; either
    /// way, recreating the device ends it instead.
    fn end(&mut self, frame: PenSample, target: &mut impl PenTarget) -> Outcome {
        if on_screen(frame, target.screen()) {
            let outcome = self.send(frame, target);
            if outcome != Outcome::Rejected {
                return outcome;
            }
        }
        target.recreate();
        self.state = PointerState::Out;
        Outcome::Injected
    }

    fn send(&mut self, frame: PenSample, target: &mut impl PenTarget) -> Outcome {
        let outcome = target.inject(frame, self.state.kind());
        // Every call counts, refused or not: Windows received the frame.
        self.spacing.sent(target.now());
        match outcome {
            Outcome::Injected => {
                self.refused_since = None;
                self.trial_until = None;
                self.state = match frame.phase {
                    PenPhase::Down | PenPhase::Move => PointerState::Contact(frame),
                    PenPhase::Hover => PointerState::Hover(frame),
                    PenPhase::Up | PenPhase::Cancel | PenPhase::Leave => PointerState::Out,
                };
            }
            Outcome::NotReady => {}
            Outcome::Rejected => self.state = PointerState::Out,
        }
        outcome
    }

    /// The frames, at most two, that take Windows from its current state to
    /// `sample`'s. A fixed array keeps the per-sample path free of
    /// allocation.
    fn plan(&self, sample: PenSample) -> [Option<PenSample>; 2] {
        let at = |phase, pressure, from: PenSample| PenSample {
            phase,
            pressure,
            ..from
        };
        match (sample.phase, self.state) {
            // A new contact or hover while one is held: end the old one where
            // Windows last saw it.
            (PenPhase::Down | PenPhase::Hover, PointerState::Contact(last)) => {
                [Some(at(PenPhase::Cancel, 0, last)), Some(sample)]
            }
            (PenPhase::Down | PenPhase::Hover, _) => [Some(sample), None],
            (PenPhase::Move, PointerState::Contact(_)) => [Some(sample), None],
            // No contact to update (it was cancelled or never started):
            // start one here.
            (PenPhase::Move, _) => [Some(at(PenPhase::Down, sample.pressure, sample)), None],
            (PenPhase::Up, PointerState::Contact(last)) => {
                if (last.x, last.y) == (sample.x, sample.y) {
                    [Some(sample), None]
                } else {
                    // Windows requires UP at the last updated position.
                    [
                        Some(at(PenPhase::Move, last.pressure, sample)),
                        Some(sample),
                    ]
                }
            }
            (PenPhase::Cancel, PointerState::Contact(last)) => {
                [Some(at(PenPhase::Cancel, 0, last)), None]
            }
            (PenPhase::Leave, PointerState::Hover(last)) => {
                [Some(at(PenPhase::Leave, 0, last)), None]
            }
            (PenPhase::Leave, PointerState::Contact(last)) => {
                [Some(at(PenPhase::Cancel, 0, last)), None]
            }
            // Nothing is in contact or in range: there is nothing to end.
            (PenPhase::Up | PenPhase::Cancel | PenPhase::Leave, _) => [None, None],
        }
    }
}

/// A down, move or hover: input that draws or moves the pointer, as opposed
/// to an end Windows may be owed.
fn is_ink(sample: &PenSample) -> bool {
    matches!(
        sample.phase,
        PenPhase::Down | PenPhase::Move | PenPhase::Hover
    )
}

/// Whether `frame`'s location lies on `screen`.
fn on_screen(frame: PenSample, screen: VirtualScreen) -> bool {
    let (x, y) = super::desktop_pixel_to_pointer_location(frame.x, frame.y, screen);
    (0..i64::from(screen.width)).contains(&i64::from(x))
        && (0..i64::from(screen.height)).contains(&i64::from(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(phase: PenPhase, x: i32, y: i32) -> PenSample {
        PenSample {
            phase,
            x,
            y,
            x_norm: (x - OUTPUT.left) as u16,
            y_norm: (y - OUTPUT.top) as u16,
            pressure: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                512
            } else {
                0
            },
            tilt_x: 0,
            tilt_y: 0,
        }
    }

    /// A capture rect on which wire positions are desktop pixels offset by
    /// its origin, with room for negative desktop coordinates.
    const OUTPUT: CaptureGeometry = CaptureGeometry {
        left: -8192,
        top: -8192,
        width: 65_536,
        height: 65_536,
    };
    const SCREEN: VirtualScreen = VirtualScreen {
        left: -8192,
        top: -8192,
        width: 65_536,
        height: 65_536,
    };

    /// Microsoft's documented transitions for one injected pointer
    /// (`InjectTouchInput`'s table, which synthetic pen injection shares),
    /// keyed on the flags of the frame Windows receives. Written from the
    /// documentation rather than from `PenContact`, so it checks the planner.
    /// Button and primary flags do not affect the transition.
    const TRANSITIONS: &[(PointerKind, u32, PointerKind)] = {
        use pointer_flag::*;
        use PointerKind::{Contact, Hover, Out};
        &[
            (Out, DOWN | INRANGE | INCONTACT, Contact),
            (Out, UPDATE | INRANGE, Hover),
            (Hover, UPDATE | INRANGE, Hover),
            (Hover, DOWN | INRANGE | INCONTACT, Contact),
            (Hover, UPDATE, Out),
            (Contact, UPDATE | INRANGE | INCONTACT, Contact),
            // An up must be at the last updated location; see `inject`.
            (Contact, UP, Out),
            (Contact, UP | INRANGE, Hover),
            (Contact, UP | CANCELED, Out),
        ]
    };

    /// What Windows saw: a frame at a desktop pixel and its outcome, or a
    /// recreated or given up device.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Seen {
        F(PenPhase, i32, i32, Outcome),
        Recreated,
        GaveUp,
    }

    /// Windows as documented: a frame in the table is applied, anything else
    /// is rejected and cancels the contact. A location off the virtual
    /// screen is rejected without ending the pointer, which Windows may then
    /// still hold. After ERROR_NOT_READY the next frame must be the same
    /// one, unless the display changed (these tests never age a sample
    /// until it is dropped as stale). Recreating the device ends the
    /// pointer. Tests can script NOT_READY and rejections, and cancel the
    /// contact behind the injector's back (a display change). Positions are
    /// where Windows resolves each frame's location on the virtual screen
    /// current at injection.
    struct FakeWindows {
        state: PointerKind,
        /// Desktop pixel of the last applied frame.
        at: (i32, i32),
        screen: VirtualScreen,
        not_ready: usize,
        refuse: usize,
        /// The frame that last hit NOT_READY, which must be retried.
        retry: Option<PointerFrame>,
        frames: Vec<Seen>,
        illegal: Vec<(PenPhase, PointerKind)>,
        /// The time now. Each frame takes `call`; by default that keeps
        /// frames far enough apart that none is held back.
        clock: Instant,
        call: Duration,
        /// When each frame reached Windows.
        sent_at: Vec<Instant>,
        /// Refuse frames as the reference PC did: the 13th in a row to come
        /// less than 100 µs after the one before, and each after it.
        rate_limited: bool,
        fast: usize,
    }

    impl Default for FakeWindows {
        fn default() -> Self {
            Self {
                state: PointerKind::Out,
                at: (0, 0),
                screen: SCREEN,
                not_ready: 0,
                refuse: 0,
                retry: None,
                frames: Vec::new(),
                illegal: Vec::new(),
                clock: Instant::now(),
                call: FRAME_SPACING,
                sent_at: Vec::new(),
                rate_limited: false,
                fast: 0,
            }
        }
    }

    impl PenTarget for FakeWindows {
        fn screen(&self) -> VirtualScreen {
            self.screen
        }

        fn now(&self) -> Instant {
            self.clock
        }

        fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome {
            assert_eq!(from, self.belief_of(from), "injector state diverged");
            let fast = self
                .sent_at
                .last()
                .is_some_and(|&last| self.clock - last < Duration::from_micros(100));
            self.fast = if fast { self.fast + 1 } else { 0 };
            self.sent_at.push(self.clock);
            self.clock += self.call;
            let pointer = PointerFrame::new(frame, self.screen);
            if let Some(retry) = self.retry.take() {
                assert_eq!(pointer, retry, "{frame:?} replaced a frame being retried");
            }
            let at = (
                pointer.location.0 + self.screen.left,
                pointer.location.1 + self.screen.top,
            );
            let flags = pointer.flags & !(pointer_flag::PRIMARY | pointer_flag::FIRSTBUTTON);
            let next = TRANSITIONS
                .iter()
                .find(|&&(state, f, _)| state == self.state && f == flags)
                .map(|&(_, _, next)| next)
                .filter(|_| flags & pointer_flag::UP == 0 || at == self.at);
            let (x, y) = pointer.location;
            let on_screen = (0..self.screen.width as i32).contains(&x)
                && (0..self.screen.height as i32).contains(&y);
            let outcome = if self.refuse > 0 || (self.rate_limited && self.fast >= 12) {
                self.refuse = self.refuse.saturating_sub(1);
                self.state = PointerKind::Out;
                Outcome::Rejected
            } else if !on_screen {
                self.illegal.push((frame.phase, self.state));
                Outcome::Rejected
            } else if let Some(next) = next.filter(|_| self.not_ready == 0) {
                self.state = next;
                self.at = at;
                Outcome::Injected
            } else if next.is_some() {
                self.not_ready -= 1;
                self.retry = Some(pointer);
                Outcome::NotReady
            } else {
                self.illegal.push((frame.phase, self.state));
                self.state = PointerKind::Out;
                Outcome::Rejected
            };
            self.frames.push(Seen::F(frame.phase, at.0, at.1, outcome));
            outcome
        }

        fn recreate(&mut self) {
            self.state = PointerKind::Out;
            self.retry = None;
            self.frames.push(Seen::Recreated);
        }

        fn give_up(&mut self) {
            self.state = PointerKind::Out;
            self.retry = None;
            self.frames.push(Seen::GaveUp);
        }
    }

    impl FakeWindows {
        /// The display moved or changed mode: a retried frame may be mapped
        /// somewhere else now.
        fn display_changed(&mut self) {
            self.retry = None;
        }

        /// Windows ends the contact without telling the injector.
        fn cancel(&mut self) {
            self.state = PointerKind::Out;
        }

        /// The injector may only lag Windows after a silent cancellation.
        fn belief_of(&self, injector: PointerKind) -> PointerKind {
            if self.state == PointerKind::Out {
                injector
            } else {
                self.state
            }
        }

        fn take(&mut self) -> Vec<Seen> {
            std::mem::take(&mut self.frames)
        }
    }

    fn deliver(contact: &mut PenContact, windows: &mut FakeWindows, sample: PenSample) {
        contact.deliver(sample, Some(OUTPUT), Instant::now(), windows);
    }

    use Outcome::{Injected, NotReady, Rejected};
    use PenPhase::{Cancel, Down, Hover, Leave, Move, Up};
    use Seen::{Recreated, F};

    #[test]
    fn a_normal_stroke_injects_exactly_its_samples() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        for sample in [
            pen(Hover, 10, 10),
            pen(Down, 10, 10),
            pen(Move, 12, 10),
            pen(Up, 12, 10),
            pen(Hover, 12, 11),
            pen(Leave, 12, 11),
        ] {
            deliver(&mut contact, &mut windows, sample);
        }
        assert_eq!(
            windows.take(),
            [
                F(Hover, 10, 10, Injected),
                F(Down, 10, 10, Injected),
                F(Move, 12, 10, Injected),
                F(Up, 12, 10, Injected),
                F(Hover, 12, 11, Injected),
                F(Leave, 12, 11, Injected),
            ]
        );
        assert_eq!(contact.state(), PointerState::Out);
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn a_cancelled_contact_resumes_at_the_same_sample() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        // A display change cancels the contact without telling us.
        windows.cancel();
        windows.take();
        deliver(&mut contact, &mut windows, pen(Move, 20, 10));
        assert_eq!(
            windows.take(),
            [F(Move, 20, 10, Rejected), F(Down, 20, 10, Injected)]
        );
        deliver(&mut contact, &mut windows, pen(Move, 21, 10));
        deliver(&mut contact, &mut windows, pen(Up, 21, 10));
        assert_eq!(
            windows.take(),
            [F(Move, 21, 10, Injected), F(Up, 21, 10, Injected)]
        );
    }

    #[test]
    fn a_rejected_resume_waits_for_the_next_sample_without_looping() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.cancel();
        windows.take();
        windows.refuse = 2;
        deliver(&mut contact, &mut windows, pen(Move, 20, 10));
        assert_eq!(
            windows.take(),
            [F(Move, 20, 10, Rejected), F(Down, 20, 10, Rejected)]
        );
        assert_eq!(contact.state(), PointerState::Out);
        deliver(&mut contact, &mut windows, pen(Move, 22, 10));
        assert_eq!(windows.take(), [F(Down, 22, 10, Injected)]);
    }

    #[test]
    fn not_ready_during_resume_keeps_the_replacement_down_pending() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.cancel();
        // The move is rejected, then the replacement DOWN is not ready.
        windows.take();
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Move, 20, 10));
        assert_eq!(contact.state(), PointerState::Out);
        // The move waits; planned again from Out, it retries the same down.
        assert_eq!(contact.pending().map(|p| p.phase), Some(Move));
        // A refresh tick retries the same frame; the stroke then continues.
        contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        deliver(&mut contact, &mut windows, pen(Move, 21, 10));
        deliver(&mut contact, &mut windows, pen(Up, 21, 10));
        assert_eq!(
            windows.take(),
            [
                F(Move, 20, 10, Rejected),
                F(Down, 20, 10, NotReady),
                F(Down, 20, 10, Injected),
                F(Move, 21, 10, Injected),
                F(Up, 21, 10, Injected),
            ]
        );
        assert_eq!(windows.illegal, [(Move, PointerKind::Out)]);
    }

    #[test]
    fn not_ready_on_an_edge_retries_that_frame_before_the_next_sample() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Up, 10, 10));
        assert_eq!(contact.state().kind(), PointerKind::Contact);
        // The next stroke must not start until the up has landed.
        deliver(&mut contact, &mut windows, pen(Down, 30, 30));
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Up, 30, 30));
        // Still unready when the next stroke arrives: the pending up is
        // retried first and stays pending; the new down waits behind it.
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Down, 40, 40));
        assert_eq!(contact.pending().map(|p| p.phase), Some(Up));
        contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        assert_eq!(
            windows.take(),
            [
                F(Down, 10, 10, Injected),
                F(Up, 10, 10, NotReady),
                F(Up, 10, 10, Injected),
                F(Down, 30, 30, Injected),
                F(Up, 30, 30, NotReady),
                F(Up, 30, 30, NotReady),
                F(Up, 30, 30, Injected),
                F(Down, 40, 40, Injected),
            ]
        );
        assert!(windows.illegal.is_empty());
        assert_eq!(contact.state().kind(), PointerKind::Contact);
        assert!(contact.pending().is_none());
    }

    #[test]
    fn release_while_lost_injects_nothing() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        for sample in [pen(Up, 5, 5), pen(Cancel, 5, 5), pen(Leave, 5, 5)] {
            deliver(&mut contact, &mut windows, sample);
        }
        deliver(&mut contact, &mut windows, pen(Down, 5, 5));
        windows.cancel();
        windows.refuse = 2;
        deliver(&mut contact, &mut windows, pen(Move, 6, 5));
        windows.take();
        deliver(&mut contact, &mut windows, pen(Up, 6, 5));
        deliver(&mut contact, &mut windows, pen(Cancel, 6, 5));
        assert!(windows.take().is_empty());
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn geometry_change_mid_stroke_and_during_keepalive_follows_the_new_position() {
        // The relay remaps a held contact against the current capture rect
        // before refreshing it; the injector must move there, and resume
        // there if the display change cancelled the contact.
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 3739, 819));
        deliver(&mut contact, &mut windows, pen(Move, 3739, 819));
        windows.cancel();
        windows.take();
        deliver(&mut contact, &mut windows, pen(Move, -1181, 819));
        assert_eq!(
            windows.take(),
            [F(Move, -1181, 819, Rejected), F(Down, -1181, 819, Injected)]
        );
        deliver(&mut contact, &mut windows, pen(Move, -1181, 819));
        // An up reported elsewhere is preceded by a move to it.
        deliver(&mut contact, &mut windows, pen(Up, -1100, 800));
        assert_eq!(
            windows.take(),
            [
                F(Move, -1181, 819, Injected),
                F(Move, -1100, 800, Injected),
                F(Up, -1100, 800, Injected),
            ]
        );
        assert_eq!(windows.illegal, [(Move, PointerKind::Out)]);
    }

    #[test]
    fn a_new_stroke_or_hover_over_a_held_contact_cancels_it_first() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 1, 1));
        deliver(&mut contact, &mut windows, pen(Down, 9, 9));
        deliver(&mut contact, &mut windows, pen(Hover, 9, 9));
        deliver(&mut contact, &mut windows, pen(Leave, 9, 9));
        // Leave without any hover (a lost hover datagram) is skipped.
        deliver(&mut contact, &mut windows, pen(Leave, 9, 9));
        assert_eq!(
            windows.take(),
            [
                F(Down, 1, 1, Injected),
                F(Cancel, 1, 1, Injected),
                F(Down, 9, 9, Injected),
                F(Cancel, 9, 9, Injected),
                F(Hover, 9, 9, Injected),
                F(Leave, 9, 9, Injected),
            ]
        );
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn frames_carry_the_documented_flags_relative_to_the_virtual_screen() {
        use pointer_flag::*;
        let screen = VirtualScreen {
            left: -2732,
            top: -2048,
            width: 6172,
            height: 3488,
        };
        let flags = |phase| {
            let frame = PointerFrame::new(pen(phase, -1, 0), screen);
            assert_eq!(frame.location, (2731, 2048));
            frame.flags & !PRIMARY
        };
        assert_eq!(flags(Down), DOWN | INRANGE | INCONTACT | FIRSTBUTTON);
        assert_eq!(flags(Move), UPDATE | INRANGE | INCONTACT | FIRSTBUTTON);
        assert_eq!(flags(Up), UP);
        assert_eq!(flags(Cancel), UP | CANCELED);
        assert_eq!(flags(Hover), UPDATE | INRANGE);
        assert_eq!(flags(Leave), UPDATE);
        let frame = PointerFrame::new(pen(Down, 0, 0), screen);
        assert_ne!(frame.flags & PRIMARY, 0);
        assert_eq!((frame.pressure, frame.tilt_x, frame.tilt_y), (512, 0, 0));
    }

    #[test]
    fn a_release_survives_a_deferred_final_position_update() {
        // The tip lifted away from the last move: the up needs a move first,
        // and Windows is not ready for that move.
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Up, 20, 10));
        assert_eq!(contact.pending().map(|p| p.phase), Some(Up));
        // The relay holds no pen now; only the refresh tick finishes it.
        contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        assert_eq!(
            windows.take(),
            [
                F(Down, 10, 10, Injected),
                F(Move, 20, 10, NotReady),
                F(Move, 20, 10, Injected),
                F(Up, 20, 10, Injected),
            ]
        );
        assert_eq!(contact.state(), PointerState::Out);
        assert!(contact.pending().is_none());
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn a_later_stroke_cannot_discard_the_release_windows_is_owed() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        for sample in [pen(Up, 10, 10), pen(Down, 20, 10), pen(Up, 20, 10)] {
            windows.not_ready = 1;
            deliver(&mut contact, &mut windows, sample);
        }
        contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        assert_eq!(
            windows.take(),
            [
                F(Down, 10, 10, Injected),
                F(Up, 10, 10, NotReady),
                F(Up, 10, 10, NotReady),
                F(Up, 10, 10, NotReady),
                F(Up, 10, 10, Injected),
                F(Down, 20, 10, Injected),
                F(Up, 20, 10, Injected),
            ]
        );
        assert_eq!(contact.state(), PointerState::Out);
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn a_full_queue_hands_every_waiting_sample_over_in_order() {
        // Windows stays not ready while strokes queue behind a held
        // contact. Rather than evict a sample, the edge that would overflow
        // the queue counts native input as stuck: the device is recreated,
        // ending the contact, and every waiting sample and that edge are
        // handed over for the mouse, in order.
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.not_ready = usize::MAX;
        let mut queued = vec![pen(Move, 12, 10), pen(Up, 12, 10)];
        for x in 20..22 {
            queued.extend([pen(Down, x, 10), pen(Move, x, 11), pen(Up, x, 11)]);
        }
        for &sample in &queued {
            deliver(&mut contact, &mut windows, sample);
            assert!(contact.take_abandoned().is_empty());
        }
        assert_eq!(contact.queue.len(), QUEUE_LIMIT);
        windows.take();
        deliver(&mut contact, &mut windows, pen(Down, 22, 10));
        assert_eq!(windows.take(), [Recreated]);
        queued.push(pen(Down, 22, 10));
        let handed: Vec<_> = contact
            .take_abandoned()
            .iter()
            .map(|(s, _)| (s.phase, s.x, s.y))
            .collect();
        let expected: Vec<_> = queued.iter().map(|s| (s.phase, s.x, s.y)).collect();
        assert_eq!(handed, expected);
        assert!(contact.pending().is_none());
        assert_eq!(contact.state(), PointerState::Out);
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn waiting_samples_land_where_the_display_is_when_they_are_sent() {
        // The reviewer's case: a 2360x1640 output moves from left=2560 to
        // left=-2360 while a down, and later an up, wait for Windows.
        let before = CaptureGeometry {
            left: 2560,
            top: 0,
            width: 2360,
            height: 1640,
        };
        let after = CaptureGeometry {
            left: -2360,
            ..before
        };
        let at = |phase, output| {
            let (x, y) = super::super::norm_to_output_pixel(32_768, 32_768, output);
            PenSample {
                x_norm: 32_768,
                y_norm: 32_768,
                ..pen(phase, x, y)
            }
        };
        assert_eq!((at(Down, before).x, at(Down, after).x), (3739, -1181));
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        windows.not_ready = 1;
        contact.deliver(at(Down, before), Some(before), Instant::now(), &mut windows);
        // The keepalive flushes onto the new rect, then refreshes the hold.
        windows.display_changed();
        contact.flush(Some(after), Instant::now(), &mut windows);
        contact.deliver(at(Move, after), Some(after), Instant::now(), &mut windows);
        // An up waiting across the move back is sent where the contact is
        // now, after a move to it.
        windows.not_ready = 1;
        contact.deliver(at(Up, after), Some(after), Instant::now(), &mut windows);
        windows.display_changed();
        contact.flush(Some(before), Instant::now(), &mut windows);
        assert_eq!(
            windows.take(),
            [
                F(Down, 3739, 819, NotReady),
                F(Down, -1181, 819, Injected),
                F(Move, -1181, 819, Injected),
                F(Up, -1181, 819, NotReady),
                F(Move, 3739, 819, Injected),
                F(Up, 3739, 819, Injected),
            ]
        );
        assert!(windows.illegal.is_empty());
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    /// Windows refusing a third of the frames as not ready until `ready`, checking that
    /// each frame placed at a sample is mapped onto the current capture
    /// rect, and recording what it accepted (`None` for a recreated device).
    struct Flaky<'a> {
        windows: &'a mut FakeWindows,
        rng: &'a mut Rng,
        output: CaptureGeometry,
        /// Windows is ready again.
        ready: bool,
        accepted: Vec<Option<PenSample>>,
    }

    impl PenTarget for Flaky<'_> {
        fn screen(&self) -> VirtualScreen {
            self.windows.screen
        }

        fn now(&self) -> Instant {
            self.windows.now()
        }

        fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome {
            if !matches!(frame.phase, Cancel | Leave) {
                let mapped =
                    super::super::norm_to_output_pixel(frame.x_norm, frame.y_norm, self.output);
                assert_eq!(
                    (frame.x, frame.y),
                    mapped,
                    "{frame:?} is not on {:?}",
                    self.output
                );
            }
            self.windows.not_ready = usize::from(!self.ready && self.rng.below(3) == 0);
            let outcome = self.windows.inject(frame, from);
            if outcome == Injected {
                self.accepted.push(Some(frame));
            }
            outcome
        }

        fn recreate(&mut self) {
            self.windows.recreate();
            self.accepted.push(None);
        }

        fn give_up(&mut self) {
            self.windows.give_up();
            self.accepted.push(None);
        }
    }

    #[test]
    fn random_strokes_end_every_contact_in_order_on_the_current_display() {
        // Relay-shaped streams with Windows not ready at random, refresh
        // ticks between samples, session resets, and the captured display moving or the
        // virtual screen changing, sometimes leaving where Windows last saw
        // the pen off the screen.
        let rect = |left, top, width, height| CaptureGeometry {
            left,
            top,
            width,
            height,
        };
        let screen = |left, top, width, height| VirtualScreen {
            left,
            top,
            width,
            height,
        };
        let displays = [
            // A primary with a tablet output on its right, then on its left.
            (rect(0, 0, 2560, 1440), screen(0, 0, 4920, 1640)),
            (rect(2560, 0, 2360, 1640), screen(0, 0, 4920, 1640)),
            (rect(-2360, 0, 2360, 1640), screen(-2360, 0, 4920, 1640)),
            (rect(0, 0, 2560, 1440), screen(-2360, 0, 4920, 1640)),
            // The primary alone.
            (rect(0, 0, 2560, 1440), screen(0, 0, 2560, 1440)),
        ];
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut recreated = 0;
        for _ in 0..4000 {
            let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
            let (mut output, first_screen) = displays[0];
            windows.screen = first_screen;
            let mut flaky = Flaky {
                windows: &mut windows,
                rng: &mut rng,
                output,
                ready: false,
                accepted: Vec::new(),
            };
            let mut intent = PointerKind::Out;
            let mut last = None;
            for index in 0..flaky.rng.below(40) as u16 {
                if flaky.rng.below(10) == 0 {
                    (output, flaky.windows.screen) = displays[flaky.rng.below(5) as usize];
                    flaky.output = output;
                    flaky.windows.display_changed();
                }
                use PointerKind as K;
                let phase = match (intent, flaky.rng.below(10)) {
                    (K::Out, 0..=5) | (K::Hover, 0..=3) => Down,
                    (K::Out | K::Hover, 0..=7) => Hover,
                    (K::Hover, _) => Leave,
                    (K::Out, _) => Hover,
                    (K::Contact, 0..=5) => Move,
                    (K::Contact, 6..=8) => Up,
                    (K::Contact, _) => Cancel,
                };
                intent = match phase {
                    Down | Move => K::Contact,
                    Hover => K::Hover,
                    Up | Cancel | Leave => K::Out,
                };
                // The index records each sample's order in y_norm.
                let (x_norm, y_norm) = (flaky.rng.below(65_536) as u16, index);
                let (x, y) = super::super::norm_to_output_pixel(x_norm, y_norm, output);
                let sample = PenSample {
                    x_norm,
                    y_norm,
                    ..pen(phase, x, y)
                };
                last = Some(sample);
                if flaky.rng.below(30) == 0 {
                    // The session ends; the next one starts out of range.
                    contact.reset(&mut flaky);
                    assert!(contact.pending().is_none());
                    assert_eq!(flaky.windows.state, PointerKind::Out);
                    intent = PointerKind::Out;
                    last = None;
                    continue;
                }
                contact.deliver(sample, Some(output), Instant::now(), &mut flaky);
                if flaky.rng.below(2) == 0 {
                    contact.flush(Some(output), Instant::now(), &mut flaky);
                }
            }
            flaky.ready = true;
            contact.flush(Some(output), Instant::now(), &mut flaky);
            while contact.spaced_until().is_some() {
                contact.flush(Some(output), Instant::now(), &mut flaky);
            }
            let accepted = std::mem::take(&mut flaky.accepted);
            assert!(windows.illegal.is_empty(), "{:?}", windows.illegal);
            // Every contact Windows accepted ended exactly once, by an up, a
            // cancel or a recreated device; frames went out in sample order,
            // and ends sit where Windows had the pen.
            let mut open = false;
            let mut previous: Option<PenSample> = None;
            for frame in &accepted {
                let Some(frame) = frame else {
                    recreated += 1;
                    open = false;
                    previous = None;
                    continue;
                };
                match frame.phase {
                    Down => assert!(!std::mem::replace(&mut open, true)),
                    Up | Cancel => assert!(std::mem::replace(&mut open, false)),
                    _ => {}
                }
                if let Some(previous) = previous {
                    if matches!(frame.phase, Cancel | Leave) {
                        assert_eq!(frame.y_norm, previous.y_norm);
                    } else {
                        assert!(frame.y_norm >= previous.y_norm, "{accepted:?}");
                    }
                }
                previous = Some(*frame);
            }
            assert_eq!(windows.state, intent);
            if let (Some(last), true) = (last, intent != PointerKind::Out) {
                assert_eq!(windows.at, (last.x, last.y));
            }
        }
        // The streams do reach the off-screen case.
        assert!(recreated > 0);
    }

    #[test]
    fn a_stuck_windows_queue_costs_one_attempt_per_frame() {
        let mut retry = NotReadyRetry::default();
        let attempts = std::cell::Cell::new(0);
        let refuse = || {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(NotReady)
        };
        // Back-to-back frames are retried until Windows' clock advances.
        let started = Instant::now();
        assert_eq!(retry.call(refuse, |e| *e == NotReady), Err(NotReady));
        assert!(started.elapsed() >= NOT_READY_WINDOW);
        // Still refused after that window: Windows is stuck, so later frames
        // are tried once each.
        for _ in 0..240 {
            attempts.set(0);
            assert_eq!(retry.call(refuse, |e| *e == NotReady), Err(NotReady));
            assert_eq!(attempts.get(), 1);
        }
        // Any other answer means Windows is responding again.
        assert_eq!(
            retry.call(|| Err::<(), _>(Rejected), |e| *e == NotReady),
            Err(Rejected)
        );
        let mut answers = [Err(NotReady), Ok(())].into_iter();
        assert_eq!(
            retry.call(|| answers.next().unwrap(), |e| *e == NotReady),
            Ok(())
        );
        assert!(!retry.stalled);
    }

    #[test]
    fn an_end_off_the_virtual_screen_recreates_the_device_instead() {
        // The reviewer's probe: Windows holds the pen at desktop 3739 when
        // the virtual screen moves to start at -2360, so a cancel there would
        // be at location 6099 on a screen 4920 wide.
        let right = VirtualScreen {
            left: 0,
            top: 0,
            width: 4920,
            height: 1640,
        };
        let left = VirtualScreen {
            left: -2360,
            ..right
        };
        for (start, next) in [(Down, Down), (Down, Hover), (Down, Cancel), (Hover, Leave)] {
            let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
            windows.screen = right;
            deliver(&mut contact, &mut windows, pen(start, 3739, 819));
            windows.screen = left;
            windows.display_changed();
            deliver(&mut contact, &mut windows, pen(next, -1181, 819));
            let mut expected = vec![F(start, 3739, 819, Injected), Recreated];
            if matches!(next, Down | Hover) {
                expected.push(F(next, -1181, 819, Injected));
            }
            assert_eq!(windows.take(), expected, "{start:?} then {next:?}");
            assert!(windows.illegal.is_empty());
        }
    }

    #[test]
    fn an_up_off_the_virtual_screen_ends_the_contact_where_windows_has_it() {
        // The display holding the lift leaves the virtual screen while the
        // up waits: the up cannot go where the Pencil lifted, so the contact
        // ends where Windows has it, never with a frame off the screen.
        let right = VirtualScreen {
            left: 0,
            top: 0,
            width: 4920,
            height: 1640,
        };
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        windows.screen = right;
        deliver(&mut contact, &mut windows, pen(Down, 2000, 819));
        windows.screen = VirtualScreen {
            width: 2560,
            ..right
        };
        windows.display_changed();
        deliver(&mut contact, &mut windows, pen(Up, 3739, 819));
        assert_eq!(
            windows.take(),
            [F(Down, 2000, 819, Injected), F(Cancel, 2000, 819, Injected)]
        );
        assert!(windows.illegal.is_empty());
        assert_eq!(contact.state(), PointerState::Out);
    }

    #[test]
    fn a_refused_end_recreates_the_device() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.refuse = 1;
        deliver(&mut contact, &mut windows, pen(Cancel, 10, 10));
        deliver(&mut contact, &mut windows, pen(Down, 20, 20));
        assert_eq!(
            windows.take(),
            [
                F(Down, 10, 10, Injected),
                F(Cancel, 10, 10, Rejected),
                Recreated,
                F(Down, 20, 20, Injected),
            ]
        );
    }

    #[test]
    fn a_session_reset_drops_unsent_samples_and_ends_only_what_windows_holds() {
        let off_screen = VirtualScreen {
            left: -2360,
            top: 0,
            width: 4920,
            height: 1640,
        };
        let reset = |setup: &dyn Fn(&mut PenContact, &mut FakeWindows)| {
            let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
            setup(&mut contact, &mut windows);
            windows.take();
            contact.reset(&mut windows);
            let seen = windows.take();
            // Nothing from the old session reaches Windows afterwards.
            contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
            assert!(windows.take().is_empty());
            assert!(contact.pending().is_none());
            assert_eq!(windows.state, PointerKind::Out);
            assert!(windows.illegal.is_empty());
            seen
        };
        // The reviewer's case: a whole stroke Windows was never ready for.
        // Windows holds nothing, but a new device keeps the next session
        // from sending something other than the refused down.
        let unsent = reset(&|contact, windows| {
            windows.not_ready = usize::MAX;
            deliver(contact, windows, pen(Down, 10, 10));
            deliver(contact, windows, pen(Up, 10, 10));
            windows.not_ready = 0;
        });
        assert_eq!(unsent, [Recreated]);
        let idle = reset(&|_, _| {});
        assert_eq!(idle, []);
        let held = reset(&|contact, windows| deliver(contact, windows, pen(Down, 10, 10)));
        assert_eq!(held, [F(Cancel, 10, 10, Injected)]);
        let hovering = reset(&|contact, windows| deliver(contact, windows, pen(Hover, 10, 10)));
        assert_eq!(hovering, [F(Leave, 10, 10, Injected)]);
        // Windows holds the contact but is refusing a later frame: no other
        // frame may replace that one.
        let stuck = reset(&|contact, windows| {
            deliver(contact, windows, pen(Down, 10, 10));
            windows.not_ready = 1;
            deliver(contact, windows, pen(Move, 20, 10));
        });
        assert_eq!(stuck, [Recreated]);
        let moved_away = reset(&|contact, windows| {
            windows.screen = VirtualScreen {
                left: 0,
                ..off_screen
            };
            deliver(contact, windows, pen(Down, 3739, 819));
            windows.screen = off_screen;
            windows.display_changed();
        });
        assert_eq!(moved_away, [Recreated]);
        let refused = reset(&|contact, windows| {
            deliver(contact, windows, pen(Down, 10, 10));
            windows.refuse = 1;
        });
        assert_eq!(refused, [F(Cancel, 10, 10, Rejected), Recreated]);
        let not_ready = reset(&|contact, windows| {
            deliver(contact, windows, pen(Down, 10, 10));
            windows.not_ready = 1;
        });
        assert_eq!(not_ready, [F(Cancel, 10, 10, NotReady), Recreated]);
        // Frames held back only for their spacing are not a refusal: the
        // contact ends where Windows has it, without a new device.
        let spaced = reset(&|contact, windows| {
            windows.call = Duration::from_micros(20);
            for x in 10..30 {
                deliver(
                    contact,
                    windows,
                    pen(if x == 10 { Down } else { Move }, x, 10),
                );
            }
            assert!(contact.spaced_until().is_some());
        });
        assert_eq!(spaced, [F(Cancel, 15, 10, Injected)]);
    }

    /// Windows that takes `call` per frame and refuses frames as the
    /// reference PC did.
    fn measured_windows() -> FakeWindows {
        FakeWindows {
            call: Duration::from_micros(20),
            rate_limited: true,
            ..FakeWindows::default()
        }
    }

    /// Flush whenever the contact asks to, as the transport's deadline does.
    fn wait_for_spacing(contact: &mut PenContact, windows: &mut FakeWindows) {
        while let Some(until) = contact.spaced_until() {
            windows.clock = windows.clock.max(until);
            contact.flush(Some(OUTPUT), Instant::now(), windows);
        }
    }

    /// A 64-sample stroke, as it arrives after Wi-Fi or the PC stalled.
    fn backlog() -> Vec<PenSample> {
        (0..64)
            .map(|x| pen(if x == 0 { Down } else { Move }, x, 10))
            .collect()
    }

    #[test]
    fn a_backlog_of_samples_is_spaced_so_windows_takes_every_one() {
        // Sent back to back, unspaced, the soak's burst loses frames.
        let mut windows = measured_windows();
        for sample in backlog() {
            windows.inject(sample, windows.state);
        }
        assert!(windows.take().contains(&F(Move, 12, 10, Rejected)));

        let (mut contact, mut windows) = (PenContact::default(), measured_windows());
        for sample in backlog() {
            deliver(&mut contact, &mut windows, sample);
        }
        wait_for_spacing(&mut contact, &mut windows);
        let every_sample: Vec<_> = backlog()
            .iter()
            .map(|sample| F(sample.phase, sample.x, sample.y, Injected))
            .collect();
        assert_eq!(windows.take(), every_sample);
        assert!(contact.take_dropped().is_empty());
        assert!(contact.take_abandoned().is_empty());
        // The first FRAME_BURST go at once; after that, frames go
        // FRAME_SPACING apart. The first gap is short by the time the
        // burst itself took.
        let burst = FRAME_BURST as usize;
        let gaps: Vec<_> = windows.sent_at.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps[..burst - 1].iter().all(|&gap| gap == windows.call));
        assert_eq!(
            gaps[burst - 1],
            FRAME_SPACING - windows.call * (FRAME_BURST - 2)
        );
        assert!(gaps[burst..].iter().all(|&gap| gap >= FRAME_SPACING));
        // Windows never gets more than FRAME_BURST ahead of the spacing,
        // and the backlog drains as fast as that allows.
        let start = windows.sent_at[0];
        for (i, &at) in windows.sent_at.iter().enumerate() {
            let spaced = (i as u32).saturating_sub(FRAME_BURST - 1);
            assert!(at - start >= FRAME_SPACING * spaced, "frame {i}");
        }
        assert_eq!(
            windows.sent_at[63] - start,
            FRAME_SPACING * (64 - FRAME_BURST) + windows.call
        );
    }

    #[test]
    fn samples_at_pencil_rates_are_never_held_back() {
        // 240 Hz one at a time, and the iPad's batches at 120 and 60 Hz,
        // each stroke ending with its last batch, a move and an up.
        for (interval_us, batch) in [(4_167, 1), (8_333, 2), (16_667, 4)] {
            let (mut contact, mut windows) = (PenContact::default(), measured_windows());
            let mut samples = vec![pen(Down, 0, 10)];
            samples.extend((1..=240).map(|x| pen(Move, x, 10)));
            samples.push(pen(Up, 241, 10));
            let mut arrival = windows.clock;
            for batch in samples.chunks(batch) {
                arrival += Duration::from_micros(interval_us);
                windows.clock = arrival;
                for &sample in batch {
                    let sent = windows.sent_at.len();
                    let at = windows.clock;
                    deliver(&mut contact, &mut windows, sample);
                    // Sent at once, at the time it arrived.
                    assert!(
                        contact.pending().is_none(),
                        "{sample:?} at {interval_us} µs"
                    );
                    assert_eq!(windows.sent_at[sent], at);
                }
            }
            assert_eq!(windows.frames.len(), samples.len() + 1);
            assert!(windows
                .take()
                .iter()
                .all(|seen| matches!(seen, F(_, _, _, Injected))));
            assert!(windows.illegal.is_empty());
        }
    }

    #[test]
    fn a_frame_refused_in_a_backlog_restarts_the_contact_at_that_sample() {
        let (mut contact, mut windows) = (PenContact::default(), measured_windows());
        for sample in backlog() {
            deliver(&mut contact, &mut windows, sample);
        }
        // Windows refuses the 10th frame for another reason.
        while windows.sent_at.len() < 9 {
            let until = contact.spaced_until().expect("held back");
            windows.clock = until;
            contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        }
        windows.refuse = 1;
        wait_for_spacing(&mut contact, &mut windows);
        let seen = windows.take();
        assert_eq!(
            seen[8..12],
            [
                F(Move, 8, 10, Injected),
                F(Move, 9, 10, Rejected),
                F(Down, 9, 10, Injected),
                F(Move, 10, 10, Injected),
            ]
        );
        assert_eq!(seen.len(), 65);
        // The new down waited for its spacing too.
        assert!(windows.sent_at[10] - windows.sent_at[9] >= FRAME_SPACING);
        assert_eq!(windows.illegal, []);
        assert!(contact.take_abandoned().is_empty());
    }

    #[test]
    fn a_restart_refused_after_waiting_for_its_spacing_gives_the_sample_up() {
        let (mut contact, mut windows) = (PenContact::default(), measured_windows());
        for sample in backlog() {
            deliver(&mut contact, &mut windows, sample);
        }
        while windows.sent_at.len() < 9 {
            windows.clock = contact.spaced_until().expect("held back");
            contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        }
        windows.refuse = 2;
        wait_for_spacing(&mut contact, &mut windows);
        // Started again once, not until the sample goes stale: the rest of
        // the stroke goes to the mouse.
        assert_eq!(
            windows.take()[8..],
            [
                F(Move, 8, 10, Injected),
                F(Move, 9, 10, Rejected),
                F(Down, 9, 10, Rejected),
            ]
        );
        let abandoned = contact.take_abandoned();
        assert_eq!(abandoned.len(), 64 - 9);
        assert_eq!((abandoned[0].0.phase, abandoned[0].0.x), (Move, 9));
    }

    #[test]
    fn a_restart_windows_was_not_ready_for_and_then_refused_gives_the_sample_up() {
        let (mut contact, mut windows) = (PenContact::default(), measured_windows());
        for sample in backlog() {
            deliver(&mut contact, &mut windows, sample);
        }
        while windows.sent_at.len() < 9 {
            windows.clock = contact.spaced_until().expect("held back");
            contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        }
        // The 10th frame is refused; the restart waits for its spacing.
        windows.refuse = 1;
        windows.clock = contact.spaced_until().expect("held back");
        contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        let until = contact.spaced_until().expect("restart held back");
        assert!(until > windows.clock);
        // Windows is not ready for the restart, then refuses it.
        windows.clock = until;
        windows.not_ready = 1;
        contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        windows.clock += FRAME_SPACING;
        windows.refuse = 1;
        contact.flush(Some(OUTPUT), Instant::now(), &mut windows);
        // Started again once, not twice: the rest of the stroke goes to the
        // mouse.
        assert_eq!(
            windows.take()[8..],
            [
                F(Move, 8, 10, Injected),
                F(Move, 9, 10, Rejected),
                F(Down, 9, 10, NotReady),
                F(Down, 9, 10, Rejected),
            ]
        );
        let abandoned = contact.take_abandoned();
        assert_eq!(abandoned.len(), 64 - 9);
        assert_eq!((abandoned[0].0.phase, abandoned[0].0.x), (Move, 9));
    }

    #[test]
    fn a_backlog_goes_out_in_passes_short_enough_for_the_transport() {
        // A backlog piles up behind frames held back for their spacing, and
        // then Windows takes longer per frame, up to 300 µs: from 250 µs on
        // the spacing never holds a frame back. Each pass, a sample's
        // delivery or a flush, is one turn of the transport loop.
        let every_sample: Vec<_> = backlog()
            .iter()
            .map(|sample| F(sample.phase, sample.x, sample.y, Injected))
            .collect();
        for call in [20, 100, 300].map(Duration::from_micros) {
            let (mut contact, mut windows) = (PenContact::default(), measured_windows());
            let mut passes = Vec::new();
            let mut yielded = 0;
            let mut pass = |contact: &mut PenContact, windows: &mut FakeWindows, sample| {
                let (sent, started) = (windows.sent_at.len(), windows.clock);
                match sample {
                    Some(sample) => deliver(contact, windows, sample),
                    None => contact.flush(Some(OUTPUT), Instant::now(), windows),
                }
                passes.push((windows.sent_at.len() - sent, windows.clock - started));
                // A pass that used its budget asks for the next at once.
                yielded += usize::from(contact.spaced_until() == Some(windows.clock));
            };
            for sample in backlog() {
                pass(&mut contact, &mut windows, Some(sample));
            }
            windows.call = call;
            while let Some(until) = contact.spaced_until() {
                windows.clock = windows.clock.max(until);
                pass(&mut contact, &mut windows, None);
            }
            for &(frames, took) in &passes {
                assert!(
                    frames <= PASS_FRAMES as usize,
                    "{frames} frames at {call:?}"
                );
                assert!(took <= PASS_TIME, "{took:?} at {call:?}");
            }
            if call >= FRAME_SPACING {
                assert!(yielded > 0, "at {call:?}");
            }
            assert_eq!(windows.take(), every_sample, "at {call:?}");
            assert!(contact.take_dropped().is_empty());
            assert!(contact.take_abandoned().is_empty());
        }
    }

    #[test]
    fn refusals_are_logged_at_each_power_of_ten_with_their_count() {
        const E_INVALIDARG: u32 = 0x8007_0057;
        const ERROR_NOT_READY: u32 = 0x8007_0015;
        let mut log = RefusalLog::default();
        let logged: Vec<_> = (0..1000).filter_map(|_| log.note(E_INVALIDARG)).collect();
        assert_eq!(logged, [1, 10, 100, 1000]);
        // Each code counts on its own.
        assert_eq!(log.note(ERROR_NOT_READY), Some(1));
        assert_eq!(log.note(E_INVALIDARG), None);
    }
}
