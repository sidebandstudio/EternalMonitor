//! The synthetic pen device behind `PenContact`: created at startup,
//! recreated to end a pointer Windows may still hold, and asked for again
//! after Windows refused to create one. Portable, so tests can make Windows
//! refuse; `windows_inject.rs` supplies the Win32 calls.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::pen_contact::{Outcome, PenContact, PenTarget, PointerKind, INPUT_STALE, STUCK_TRIAL};
use super::{
    desktop_pixel_to_abs, norm_to_output_pixel, CaptureGeometry, Injection, PenPhase, PenSample,
    VirtualScreen,
};

/// After Windows refuses a device, ask again at most this often.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// The calls `PenDevice` makes into Windows.
pub trait PenApi {
    type Handle: Copy + std::fmt::Debug;
    type Error: std::fmt::Display;
    fn create(&mut self) -> Result<Self::Handle, Self::Error>;
    fn destroy(&mut self, handle: Self::Handle);
    /// The virtual screen that frame locations are measured on now.
    fn screen(&self) -> VirtualScreen;
    /// Inject one frame. `output` is the capture rect, for diagnostics.
    fn inject(
        &mut self,
        handle: Self::Handle,
        frame: PenSample,
        from: PointerKind,
        screen: VirtualScreen,
        output: Option<CaptureGeometry>,
    ) -> Outcome;
    /// Send mouse input for the fallback. Returns how many of the events,
    /// from the first, Windows inserted.
    fn send_mouse(&mut self, inputs: &[Injection]) -> usize;
    /// Keep a release the fallback still owes as its device goes, at the
    /// desktop pixel the pointer was last sent to: Windows holds the left
    /// button across a pipeline restart, so the next device must send it.
    fn owe_release(&mut self, at: (i32, i32));
    /// A release an earlier device left owed, for a new device to send.
    fn take_owed_release(&mut self) -> Option<(i32, i32)>;
}

/// One device per relay, retained across samples and destroyed at shutdown.
/// A mouse release it still owes then passes to the next device.
#[derive(Debug)]
pub struct PenDevice<A: PenApi> {
    slot: Slot<A>,
    /// What Windows believes about the pen; chooses every frame.
    contact: PenContact,
    mouse: Mouse,
    /// The last sample was a hover, so a hover now does not start one.
    hovering: bool,
}

/// A fallback mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseEvent {
    Move,
    Press,
    Release,
}

/// A mouse event waiting for `SendInput`: where it goes on the desktop, so
/// it is placed on the virtual screen current when it is sent, when the
/// host received the sample it came from, and whether that sample was a
/// down or move of a stroke still being drawn.
#[derive(Debug, Clone, Copy)]
struct Waiting {
    event: MouseEvent,
    /// The desktop pixel, used when no capture rect is known and for a
    /// release kept owed, which goes where the pointer last went.
    at: (i32, i32),
    /// The sample's wire position, mapped onto the capture rect current
    /// when the event is sent, so it follows a display that moved.
    norm: Option<(u16, u16)>,
    received: Instant,
    ink: bool,
}

/// The mouse fallback: an ordered queue of input for `SendInput`, like the
/// native queue for ERROR_NOT_READY. `INPUT_STALE` bounds it to about a
/// second of samples, so moves need not coalesce, and every fallback event
/// otherwise reaches Windows once.
#[derive(Debug, Default)]
struct Mouse {
    /// Events Windows has not inserted, oldest first. They go before any
    /// newer input, so a refused release always precedes the next press.
    waiting: VecDeque<Waiting>,
    /// Windows holds the left button: from inserted events only.
    pressed: bool,
    /// Whether Windows will hold the button once `waiting` is inserted; new
    /// input is planned from here.
    planned: bool,
    /// A stroke on the mouse: its samples stay on the mouse until it ends.
    wanted: bool,
    /// The desktop pixel Windows last moved the pointer to, for a release
    /// kept owed when stale input is dropped.
    at: (i32, i32),
    /// Waiting input dropped by design, with why, for tests.
    #[cfg(test)]
    dropped: Vec<(MouseEvent, (i32, i32), &'static str)>,
}

#[derive(Debug)]
struct Slot<A: PenApi> {
    api: A,
    handle: Option<A::Handle>,
    /// When Windows last refused to create the device, or native input was
    /// given up as stuck, while the device is missing.
    failed_at: Option<Instant>,
    /// Native input was given up because it accepted nothing, and has not
    /// accepted a frame since.
    stuck: bool,
    /// The device messages logged, for tests: capturing tracing output is
    /// unreliable while other test threads hit the same log sites.
    #[cfg(test)]
    reported: Vec<&'static str>,
}

impl<A: PenApi> Slot<A> {
    /// Log the first refusal of a streak and the recovery, not every retry.
    fn create(&mut self, now: Instant) {
        match self.api.create() {
            Ok(handle) => {
                // After native input was stuck, it is available again only
                // once Windows accepts a frame.
                if self.failed_at.take().is_some() && !self.stuck {
                    info!("Native Windows pen input is available again");
                    #[cfg(test)]
                    self.reported.push("available again");
                }
                self.handle = Some(handle);
            }
            Err(error) => {
                if self.failed_at.is_none() {
                    warn!(%error, "Native Windows pen input unavailable; Pencil uses the mouse");
                    #[cfg(test)]
                    self.reported.push("unavailable");
                }
                self.failed_at = Some(now);
            }
        }
    }

    fn destroy(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.api.destroy(handle);
        }
    }
}

/// The device as `PenContact` sees it for one batch, at `now`.
struct Target<'a, A: PenApi> {
    slot: &'a mut Slot<A>,
    screen: VirtualScreen,
    output: Option<CaptureGeometry>,
    now: Instant,
}

impl<A: PenApi> PenTarget for Target<'_, A> {
    fn screen(&self) -> VirtualScreen {
        self.screen
    }

    fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome {
        let Some(handle) = self.slot.handle else {
            return Outcome::Rejected;
        };
        let outcome = self
            .slot
            .api
            .inject(handle, frame, from, self.screen, self.output);
        if outcome == Outcome::Injected && std::mem::take(&mut self.slot.stuck) {
            info!("Native Windows pen input is available again");
            #[cfg(test)]
            self.slot.reported.push("available again");
        }
        outcome
    }

    fn available(&self) -> bool {
        self.slot.handle.is_some()
    }

    fn recreate(&mut self) {
        self.slot.destroy();
        self.slot.create(self.now);
        info!(
            available = self.slot.handle.is_some(),
            "Recreated the native pen device to end a pointer Windows may still hold"
        );
    }

    /// Log the first time in a streak, like a device Windows refused.
    fn give_up(&mut self) {
        self.slot.destroy();
        self.slot.failed_at = Some(self.now);
        if !std::mem::replace(&mut self.slot.stuck, true) {
            warn!("Native Windows pen input accepts nothing; Pencil uses the mouse");
            #[cfg(test)]
            self.slot.reported.push("stuck");
        }
    }
}

impl<A: PenApi> PenDevice<A> {
    pub fn new(api: A) -> Self {
        let now = Instant::now();
        let mut slot = Slot {
            api,
            handle: None,
            failed_at: None,
            stuck: false,
            #[cfg(test)]
            reported: Vec::new(),
        };
        slot.create(now);
        let mut device = Self {
            slot,
            contact: PenContact::default(),
            mouse: Mouse::default(),
            hovering: false,
        };
        // Windows still holds the button if the last device went with its
        // release refused: it is released before any new input.
        if let Some(at) = device.slot.api.take_owed_release() {
            (device.mouse.pressed, device.mouse.at) = (true, at);
            device.drop_mouse_input("reset", now);
        }
        device
    }

    pub fn available(&self) -> bool {
        self.slot.handle.is_some()
    }

    /// Where a stroke or hover starts, ask again for a device Windows refused
    /// or native input that was stuck, at most once per `RETRY_INTERVAL`.
    /// After a stuck streak the new device has `STUCK_TRIAL` to accept a
    /// frame.
    pub fn revive(&mut self, now: Instant) {
        let waiting = self
            .slot
            .failed_at
            .is_some_and(|at| now.saturating_duration_since(at) < RETRY_INTERVAL);
        if self.available() || waiting {
            return;
        }
        self.slot.create(now);
        // A new device holds no pointer.
        self.contact = if self.slot.stuck {
            PenContact::on_trial(now + STUCK_TRIAL)
        } else {
            PenContact::default()
        };
    }

    /// Deliver one pen sample to this device or to the mouse.
    ///
    /// Each sample goes to exactly one of the two, once and in order. A
    /// stroke or hover belongs to one path at a time. It moves to the mouse
    /// when native input cannot carry it (the device is gone, Windows
    /// rejects a frame, Windows stays not ready for a whole queue, or native
    /// input is stuck as `PenContact::flush` describes): `PenContact` then
    /// hands over the sample and every one waiting behind it. A stroke on
    /// the mouse stays there until it ends, and native input resumes only
    /// where a stroke or hover starts with the mouse idle: nothing waiting
    /// and no button held. That is also where a missing device is asked for
    /// again.
    pub fn deliver(
        &mut self,
        sample: PenSample,
        screen: VirtualScreen,
        output: Option<CaptureGeometry>,
        now: Instant,
    ) {
        self.pump(output, now);
        let starts = match sample.phase {
            PenPhase::Down => true,
            PenPhase::Hover => !self.hovering,
            _ => false,
        };
        self.hovering = sample.phase == PenPhase::Hover;
        if self.mouse_busy() {
            return self.mouse_for(sample, now, output, now);
        }
        if starts {
            self.revive(now);
        }
        if !self.available() {
            return self.mouse_for(sample, now, output, now);
        }
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
            now,
        };
        self.contact.deliver(sample, output, now, &mut target);
        self.hand_over(output, now);
    }

    /// The session ended or was taken over. Native input ends as
    /// `PenContact::reset` describes. Waiting mouse input is dropped like
    /// native samples Windows never received, keeping a release owed while
    /// Windows holds the button.
    pub fn reset(&mut self, now: Instant) {
        self.hovering = false;
        if self.available() {
            let mut target = Target {
                screen: self.slot.api.screen(),
                slot: &mut self.slot,
                output: None,
                now,
            };
            self.contact.reset(&mut target);
        }
        self.drop_mouse_input("reset", now);
        self.pump(None, now);
    }

    /// Called on every refresh tick: retry mouse input Windows refused, and
    /// deliver samples Windows was not ready for, both mapped onto `output`,
    /// the current capture rect.
    pub fn flush(&mut self, output: Option<CaptureGeometry>, now: Instant) {
        self.pump(output, now);
        if !self.available() || self.contact.pending().is_none() {
            return;
        }
        let screen = self.slot.api.screen();
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
            now,
        };
        self.contact.flush(output, now, &mut target);
        self.hand_over(output, now);
    }

    /// Whether the refresh tick has work even without a device: mouse input
    /// Windows refused.
    pub fn mouse_waiting(&self) -> bool {
        !self.mouse.waiting.is_empty()
    }

    fn mouse_busy(&self) -> bool {
        self.mouse.wanted || self.mouse.pressed || self.mouse_waiting()
    }

    /// Send the samples `PenContact` gave up on to the mouse, oldest first,
    /// each still aged from when it was received.
    fn hand_over(&mut self, output: Option<CaptureGeometry>, now: Instant) {
        for (sample, received) in self.contact.take_abandoned() {
            self.mouse_for(sample, received, output, now);
        }
    }

    /// Queue mouse input for `sample`, received at `received`, planned from
    /// the button state the waiting input leaves: a down or move presses
    /// unless the button will be held, so a stroke that left native input
    /// is pressed at its first sample on the mouse; anything else releases
    /// a button that will be held. The mouse is therefore never pressed
    /// twice or released without a press, and a new stroke's press follows
    /// the last stroke's release. `output` is the current capture rect.
    fn mouse_for(
        &mut self,
        sample: PenSample,
        received: Instant,
        output: Option<CaptureGeometry>,
        now: Instant,
    ) {
        let want = matches!(sample.phase, PenPhase::Down | PenPhase::Move);
        self.mouse.wanted = want;
        let events: &[MouseEvent] = match (want, self.mouse.planned) {
            (true, false) => &[MouseEvent::Move, MouseEvent::Press],
            (true, true) => &[MouseEvent::Move],
            (false, true) => &[MouseEvent::Move, MouseEvent::Release],
            (false, false) if sample.phase == PenPhase::Hover => &[MouseEvent::Move],
            (false, false) => &[],
        };
        self.mouse.planned = want;
        let (at, norm) = ((sample.x, sample.y), Some((sample.x_norm, sample.y_norm)));
        self.mouse
            .waiting
            .extend(events.iter().map(|&event| Waiting {
                event,
                at,
                norm,
                received,
                ink: want,
            }));
        self.pump(output, now);
    }

    /// Offer Windows the waiting input, oldest first, placed on `output`,
    /// the capture rect current now, and on the virtual screen as it is now.
    /// What it inserts leaves the queue and sets the button state; the rest
    /// waits for the next sample, tick or reset. Stale ink is dropped first,
    /// so a late call never inserts it.
    fn pump(&mut self, output: Option<CaptureGeometry>, now: Instant) {
        self.expire_mouse(now);
        if self.mouse.waiting.is_empty() {
            return;
        }
        let at = |waiting: &Waiting| match (waiting.norm, output) {
            (Some((x, y)), Some(output)) => norm_to_output_pixel(x, y, output),
            _ => waiting.at,
        };
        let screen = self.slot.api.screen();
        let inputs: Vec<_> = self
            .mouse
            .waiting
            .iter()
            .map(|waiting| {
                let (x, y) = at(waiting);
                let (x, y) = desktop_pixel_to_abs(x, y, screen);
                match waiting.event {
                    MouseEvent::Move => Injection::MoveAbs { x, y },
                    MouseEvent::Press => Injection::LeftDown { x, y },
                    MouseEvent::Release => Injection::LeftUp { x, y },
                }
            })
            .collect();
        let inserted = self.slot.api.send_mouse(&inputs).min(inputs.len());
        for waiting in self.mouse.waiting.drain(..inserted) {
            match waiting.event {
                MouseEvent::Move => self.mouse.at = at(&waiting),
                // A press carries its position, so it moves the pointer too.
                MouseEvent::Press => (self.mouse.at, self.mouse.pressed) = (at(&waiting), true),
                MouseEvent::Release => self.mouse.pressed = false,
            }
        }
    }

    /// Drop each waiting move or press whose sample arrived `INPUT_STALE`
    /// ago, as the native queue does, and plan the rest again: a stroke
    /// whose press went is pressed at its next fresh down or move, never at
    /// the move placing an up, cancel or leave, so ending a stroke Windows
    /// never saw pressed cannot click; a release stays only while it ends a
    /// press Windows holds or will hold. A kept
    /// release goes where the pointer will be, though SendInput does not
    /// move it for one.
    fn expire_mouse(&mut self, now: Instant) {
        let stale = |waiting: &Waiting| {
            waiting.event != MouseEvent::Release
                && now.saturating_duration_since(waiting.received) >= INPUT_STALE
        };
        if !self.mouse.waiting.iter().any(stale) {
            return;
        }
        let (mut held, mut press_owed) = (self.mouse.pressed, false);
        let (mut at, mut norm) = (self.mouse.at, None);
        let mut kept = VecDeque::with_capacity(self.mouse.waiting.len());
        for waiting in std::mem::take(&mut self.mouse.waiting) {
            match waiting.event {
                _ if stale(&waiting) => {
                    #[cfg(test)]
                    self.mouse
                        .dropped
                        .push((waiting.event, waiting.at, "stale"));
                    press_owed |= waiting.event == MouseEvent::Press;
                }
                MouseEvent::Move => {
                    (at, norm) = (waiting.at, waiting.norm);
                    kept.push_back(waiting);
                    if std::mem::take(&mut press_owed) && waiting.ink {
                        kept.push_back(Waiting {
                            event: MouseEvent::Press,
                            ..waiting
                        });
                        held = true;
                    }
                }
                MouseEvent::Press => {
                    held = true;
                    kept.push_back(waiting);
                }
                MouseEvent::Release if held => {
                    held = false;
                    kept.push_back(Waiting {
                        at,
                        norm,
                        ..waiting
                    });
                }
                MouseEvent::Release => {
                    #[cfg(test)]
                    self.mouse
                        .dropped
                        .push((waiting.event, waiting.at, "stale"));
                    press_owed = false;
                }
            }
        }
        self.mouse.waiting = kept;
        self.mouse.planned = held;
    }

    /// Drop waiting mouse input and end any stroke on the mouse, keeping a
    /// release owed while Windows holds the button, so input that works
    /// again neither replays old ink nor leaves the button held.
    #[cfg_attr(not(test), allow(unused_variables))]
    fn drop_mouse_input(&mut self, why: &'static str, now: Instant) {
        #[cfg(test)]
        self.mouse.dropped.extend(
            self.mouse
                .waiting
                .iter()
                .map(|waiting| (waiting.event, waiting.at, why)),
        );
        self.mouse.waiting.clear();
        self.mouse.wanted = false;
        self.mouse.planned = false;
        if self.mouse.pressed {
            self.mouse.waiting.push_back(Waiting {
                event: MouseEvent::Release,
                at: self.mouse.at,
                norm: None,
                received: now,
                ink: false,
            });
        }
    }

    #[cfg(test)]
    fn take_dropped_mouse_input(&mut self) -> Vec<(MouseEvent, (i32, i32), &'static str)> {
        std::mem::take(&mut self.mouse.dropped)
    }
}

impl<A: PenApi> Drop for PenDevice<A> {
    fn drop(&mut self) {
        self.slot.destroy();
        if self.mouse.pressed {
            self.slot.api.owe_release(self.mouse.at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const SCREEN: VirtualScreen = VirtualScreen {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    /// Windows creating devices as scripted (true once the script runs
    /// out), answering frames on a live device with `answers`, then
    /// `answer`, and inserting at most `mouse_limit` mouse events a call.
    #[derive(Debug)]
    struct FakeApi {
        creations: VecDeque<bool>,
        created: usize,
        destroyed: Vec<usize>,
        answer: Outcome,
        answers: VecDeque<Outcome>,
        frames: Vec<(usize, PenPhase)>,
        mouse_limit: usize,
        /// Mouse events Windows inserted.
        mouse: Vec<Injection>,
        /// Whether Windows holds the left button.
        button: bool,
        /// The virtual screen, which can change while input waits.
        screen: VirtualScreen,
        /// A release a dropped device left owed, where Windows keeps it for
        /// the next device's Windows.
        owed: std::rc::Rc<std::cell::Cell<Option<(i32, i32)>>>,
    }

    impl FakeApi {
        fn new(creations: &[bool]) -> Self {
            Self {
                creations: creations.iter().copied().collect(),
                created: 0,
                destroyed: Vec::new(),
                answer: Outcome::Injected,
                answers: VecDeque::new(),
                frames: Vec::new(),
                mouse_limit: usize::MAX,
                mouse: Vec::new(),
                button: false,
                screen: SCREEN,
                owed: Default::default(),
            }
        }
    }

    /// SendInput as the fakes model it: returns how many events, from the
    /// first, Windows inserted.
    trait FakeSendInput {
        fn record_mouse(&mut self, inputs: &[Injection]) -> usize;
    }

    impl FakeSendInput for FakeApi {
        /// Insert events from the first, up to the limit.
        fn record_mouse(&mut self, inputs: &[Injection]) -> usize {
            let inserted = inputs.len().min(self.mouse_limit);
            for &input in &inputs[..inserted] {
                match input {
                    Injection::LeftDown { .. } => {
                        assert!(!std::mem::replace(&mut self.button, true), "pressed twice")
                    }
                    Injection::LeftUp { .. } => {
                        assert!(
                            std::mem::replace(&mut self.button, false),
                            "released unpressed"
                        )
                    }
                    _ => {}
                }
                self.mouse.push(input);
            }
            inserted
        }
    }

    impl PenApi for FakeApi {
        type Handle = usize;
        type Error = &'static str;

        fn create(&mut self) -> Result<usize, &'static str> {
            self.created += 1;
            if self.creations.pop_front().unwrap_or(true) {
                Ok(self.created)
            } else {
                Err("refused")
            }
        }

        fn destroy(&mut self, handle: usize) {
            self.destroyed.push(handle);
        }

        fn screen(&self) -> VirtualScreen {
            self.screen
        }

        fn inject(
            &mut self,
            handle: usize,
            frame: PenSample,
            _: PointerKind,
            _: VirtualScreen,
            _: Option<CaptureGeometry>,
        ) -> Outcome {
            assert!(!self.destroyed.contains(&handle), "used a destroyed device");
            self.frames.push((handle, frame.phase));
            self.answers.pop_front().unwrap_or(self.answer)
        }

        fn send_mouse(&mut self, inputs: &[Injection]) -> usize {
            self.record_mouse(inputs)
        }

        fn owe_release(&mut self, at: (i32, i32)) {
            self.owed.set(Some(at));
        }

        fn take_owed_release(&mut self) -> Option<(i32, i32)> {
            self.owed.take()
        }
    }

    fn pen(phase: PenPhase) -> Injection {
        pen_at(phase, 100)
    }

    fn pen_at(phase: PenPhase, y: i32) -> Injection {
        Injection::Pen(PenSample {
            phase,
            x: 100,
            y,
            x_norm: 6_000,
            y_norm: 6_000,
            pressure: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                512
            } else {
                0
            },
            tilt_x: 0,
            tilt_y: 0,
        })
    }

    /// Deliver one sample as `windows_inject::inject` does.
    fn deliver_one<A: PenApi + FakeSendInput>(
        device: &mut PenDevice<A>,
        sample: Injection,
        now: Instant,
    ) {
        let Injection::Pen(sample) = sample else {
            unreachable!()
        };
        device.deliver(sample, SCREEN, None, now);
    }

    /// A refresh tick, as `keep_pen_alive` does.
    fn flush_one<A: PenApi + FakeSendInput>(device: &mut PenDevice<A>, now: Instant) {
        device.flush(None, now);
    }

    /// A session reset, as `InputRelay::reset` does.
    fn reset_one<A: PenApi + FakeSendInput>(device: &mut PenDevice<A>, now: Instant) {
        device.reset(now);
    }

    /// Deliver a batch; returns the mouse input Windows inserted for it.
    fn send(device: &mut PenDevice<FakeApi>, phases: &[PenPhase], now: Instant) -> Vec<Injection> {
        for &phase in phases {
            deliver_one(device, pen(phase), now);
        }
        std::mem::take(&mut device.slot.api.mouse)
    }

    /// A refresh tick; returns the mouse input Windows inserted for it.
    fn tick(device: &mut PenDevice<FakeApi>) -> Vec<Injection> {
        flush_one(device, Instant::now());
        std::mem::take(&mut device.slot.api.mouse)
    }

    /// A refresh tick at `now`; returns the mouse input Windows inserted.
    fn tick_at(device: &mut PenDevice<FakeApi>, now: Instant) -> Vec<Injection> {
        flush_one(device, now);
        std::mem::take(&mut device.slot.api.mouse)
    }

    /// Lose the device: a stuck frame makes the session reset recreate it,
    /// and Windows refuses the new one.
    fn lose_device(device: &mut PenDevice<FakeApi>) {
        send(device, &[PenPhase::Down], Instant::now());
        device.slot.api.answer = Outcome::NotReady;
        send(device, &[PenPhase::Move], Instant::now());
        reset_one(device, Instant::now());
        device.slot.api.answer = Outcome::Injected;
        assert!(!device.available());
    }

    #[test]
    fn a_failed_recreation_is_retried_at_the_next_stroke() {
        // The reviewer's case: creation fails once, then would succeed.
        let mut device = PenDevice::new(FakeApi::new(&[true, false]));
        lose_device(&mut device);
        let lost = Instant::now();
        assert_eq!(device.slot.api.created, 2);
        // Refresh ticks and resets do not ask Windows; samples go to the
        // mouse until a stroke starts, and a move presses for the stroke the
        // lost device ended.
        for _ in 0..100 {
            assert!(tick(&mut device).is_empty());
            reset_one(&mut device, Instant::now());
        }
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        assert_eq!(
            send(&mut device, &[PenPhase::Move, PenPhase::Up], lost),
            [
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
        assert_eq!(device.slot.api.created, 2);
        // A stroke within a second of the refusal still uses the mouse.
        let soon = lost + Duration::from_millis(500);
        assert_eq!(
            send(&mut device, &[PenPhase::Down], soon),
            [Injection::MoveAbs { x, y }, Injection::LeftDown { x, y }]
        );
        send(&mut device, &[PenPhase::Up], soon);
        assert_eq!(device.slot.api.created, 2);
        // The next stroke after that gets a new device and draws natively.
        device.slot.api.frames.clear();
        let later = lost + RETRY_INTERVAL + Duration::from_millis(100);
        let strokes = [PenPhase::Down, PenPhase::Move, PenPhase::Up];
        assert!(send(&mut device, &strokes, later).is_empty());
        assert_eq!(device.slot.api.created, 3);
        assert_eq!(
            device.slot.api.frames,
            [(3, PenPhase::Down), (3, PenPhase::Move), (3, PenPhase::Up)]
        );
        assert_eq!(device.slot.api.destroyed, [1]);
    }

    #[test]
    fn a_persistent_refusal_uses_the_mouse_with_bounded_retries_and_one_log_line() {
        let start = Instant::now();
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        let mut device = PenDevice::new(FakeApi::new(&[false; 200]));
        // Ten strokes and hovers a second for ten seconds.
        for tenth in 1..=100 {
            let now = start + Duration::from_millis(100 * tenth);
            let phases = [
                PenPhase::Hover,
                PenPhase::Down,
                PenPhase::Move,
                PenPhase::Up,
            ];
            let mouse = send(&mut device, &phases, now);
            assert!(mouse.contains(&Injection::LeftDown { x, y }));
        }
        // The startup attempt, then at most one a second.
        assert!(device.slot.api.created <= 11, "{}", device.slot.api.created);
        assert!(device.slot.api.frames.is_empty());
        assert_eq!(std::mem::take(&mut device.slot.reported), ["unavailable"]);
        // Recovery is logged once, and a new streak logs again once.
        device.slot.api.creations = [true].into();
        let later = start + Duration::from_secs(20);
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], later).is_empty());
        assert_eq!(
            std::mem::take(&mut device.slot.reported),
            ["available again"]
        );
        device.slot.api.creations = [false; 50].into();
        lose_device(&mut device);
        for second in 1..=5 {
            send(
                &mut device,
                &[PenPhase::Down],
                later + Duration::from_secs(second),
            );
        }
        assert_eq!(device.slot.reported, ["unavailable"]);
    }

    #[test]
    fn a_device_is_never_asked_for_in_the_middle_of_a_stroke() {
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let soon = Instant::now();
        let later = soon + RETRY_INTERVAL * 2;
        // A mouse stroke starts within a second of the refusal.
        send(&mut device, &[PenPhase::Down], soon);
        // Moves, ups, cancels and leaves never create a device.
        let ends = [
            PenPhase::Move,
            PenPhase::Up,
            PenPhase::Cancel,
            PenPhase::Leave,
            PenPhase::Move,
        ];
        send(&mut device, &ends, later);
        assert_eq!(device.slot.api.created, 1);
        // A down does; a cancel before it in the same batch still goes to
        // the mouse, releasing the button the mouse stroke held.
        let mouse = send(&mut device, &[PenPhase::Cancel, PenPhase::Down], later);
        assert!(matches!(
            mouse.as_slice(),
            [Injection::MoveAbs { .. }, Injection::LeftUp { .. }]
        ));
        assert_eq!(device.slot.api.created, 2);
        assert_eq!(device.slot.api.frames, [(2, PenPhase::Down)]);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_session_started_without_the_device_draws_with_the_mouse_then_recovers() {
        use super::super::{recorder, InputRelay, KIND_PENCIL, PHASE_BEGAN, PHASE_ENDED};
        use eternal_wire::v2::control::InputEvent;
        let output = CaptureGeometry {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        };
        // A v2 Drawing mode stroke from `session`, as the relay resolves it.
        let mut relay = InputRelay::default();
        let mut stroke = |session, first_id| {
            for (id, phase) in [(first_id, PHASE_BEGAN), (first_id + 1, PHASE_ENDED)] {
                let event = InputEvent {
                    input_ver: 2,
                    kind: KIND_PENCIL,
                    phase,
                    buttons: 1,
                    event_id: id,
                    x_norm: 6_000,
                    y_norm: 6_000,
                    pressure_x1000: 500,
                    scroll_dx: 0,
                    scroll_dy: 0,
                    keycode: 0,
                    modifiers: 0,
                    client_time_us: 0,
                    tilt_x: 10,
                    tilt_y: 0,
                };
                relay.relay(session, &event, output);
            }
            recorder::take()
        };
        // Deliver as `windows_inject::inject` does.
        let deliver = |device: &mut PenDevice<FakeApi>, batch: &[Injection], now| {
            for &injection in batch {
                deliver_one(device, injection, now);
            }
            std::mem::take(&mut device.slot.api.mouse)
        };
        let mut device = PenDevice::new(FakeApi::new(&[true, false]));
        lose_device(&mut device);
        let lost = Instant::now();
        // A new session starts while the device is missing; its reset does
        // not ask Windows. Its first stroke, within a second of the refusal,
        // is injected as mouse input.
        reset_one(&mut device, Instant::now());
        let first = stroke(2, 1);
        assert!(matches!(
            first.as_slice(),
            [
                Injection::Pen(PenSample {
                    phase: PenPhase::Down,
                    ..
                }),
                Injection::Pen(PenSample {
                    phase: PenPhase::Up,
                    ..
                })
            ]
        ));
        let Injection::Pen(down) = first[0] else {
            unreachable!()
        };
        let (x, y) = super::super::desktop_pixel_to_abs(down.x, down.y, SCREEN);
        assert_eq!(
            deliver(&mut device, &first, lost + Duration::from_millis(200)),
            [
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
        assert_eq!(device.slot.api.created, 2);
        // Its next stroke asks again, gets a device and draws natively.
        device.slot.api.frames.clear();
        let second = stroke(2, 3);
        assert!(deliver(&mut device, &second, lost + RETRY_INTERVAL * 2).is_empty());
        assert_eq!(device.slot.api.created, 3);
        assert_eq!(
            device.slot.api.frames,
            [(3, PenPhase::Down), (3, PenPhase::Up)]
        );
    }

    #[test]
    fn a_stroke_that_loses_the_device_at_its_cancel_still_presses_the_mouse() {
        // The reviewer's case: the previous up was lost, so the relay sends
        // Cancel, Down. Windows refuses the cancel, then refuses to recreate
        // the device that would end the old contact. Without the relay's
        // cancel, PenContact ends the old contact itself, inside the down.
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        for ends_first in [true, false] {
            let mut device = PenDevice::new(FakeApi::new(&[true, false]));
            let now = Instant::now();
            send(&mut device, &[PenPhase::Down], now);
            device.slot.api.answer = Outcome::Rejected;
            let mut strokes = vec![PenPhase::Down, PenPhase::Move, PenPhase::Up];
            if ends_first {
                strokes.insert(0, PenPhase::Cancel);
            }
            let mouse = send(&mut device, &strokes, now);
            assert!(!device.available());
            assert_eq!(device.slot.api.created, 2);
            assert_eq!(
                mouse,
                [
                    Injection::MoveAbs { x, y },
                    Injection::LeftDown { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::LeftUp { x, y },
                ],
                "cancel first: {ends_first}"
            );
        }
    }

    #[test]
    fn a_queued_down_survives_a_refused_recreation() {
        // Sol's case: an old contact is held; Windows is not ready for the
        // relay's cancel and the next down, then rejects the cancel and
        // refuses to recreate the device. The queued down, found waiting
        // when the device is lost by a later sample or by a refresh tick,
        // must still press the mouse for its stroke.
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        for flush_on_tick in [false, true] {
            let mut device = PenDevice::new(FakeApi::new(&[true, false]));
            let now = Instant::now();
            send(&mut device, &[PenPhase::Down], now);
            device.slot.api.answer = Outcome::NotReady;
            assert!(send(&mut device, &[PenPhase::Cancel, PenPhase::Down], now).is_empty());
            assert_eq!(device.contact.pending().unwrap().phase, PenPhase::Cancel);
            device.slot.api.answer = Outcome::Rejected;
            let mut mouse = Vec::new();
            if flush_on_tick {
                mouse.extend(tick(&mut device));
            }
            mouse.extend(send(&mut device, &[PenPhase::Move, PenPhase::Up], now));
            assert!(!device.available());
            assert_eq!(device.slot.api.created, 2);
            assert_eq!(
                mouse,
                [
                    Injection::MoveAbs { x, y },
                    Injection::LeftDown { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::LeftUp { x, y },
                ],
                "flush on tick: {flush_on_tick}"
            );
        }
    }

    /// The left-button edges Windows inserted, with the stroke (the
    /// sample's y) each was for.
    fn presses(mouse: &[Injection]) -> Vec<(bool, i32)> {
        let stroke = |abs| {
            (0..=200)
                .find(|&y| super::super::desktop_pixel_to_abs(0, y, SCREEN).1 == abs)
                .unwrap()
        };
        mouse
            .iter()
            .filter_map(|input| match *input {
                Injection::LeftDown { y, .. } => Some((true, stroke(y))),
                Injection::LeftUp { y, .. } => Some((false, stroke(y))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_full_queue_hands_every_waiting_stroke_to_the_mouse() {
        // Sol's case: Windows holds stroke 0 and is not ready for its
        // cancel and four taps behind it, then rejects the cancel and
        // refuses a new device. No tap may be lost.
        let mut device = PenDevice::new(FakeApi::new(&[true, false]));
        let now = Instant::now();
        deliver_one(&mut device, pen_at(PenPhase::Down, 10), now);
        device.slot.api.answer = Outcome::NotReady;
        deliver_one(&mut device, pen_at(PenPhase::Cancel, 10), now);
        for stroke in 11..=14 {
            deliver_one(&mut device, pen_at(PenPhase::Down, stroke), now);
            deliver_one(&mut device, pen_at(PenPhase::Up, stroke), now);
        }
        device.slot.api.answer = Outcome::Rejected;
        flush_one(&mut device, now);
        let mouse = std::mem::take(&mut device.slot.api.mouse);
        assert_eq!(
            presses(&mouse),
            [11, 12, 13, 14]
                .into_iter()
                .flat_map(|s| [(true, s), (false, s)])
                .collect::<Vec<_>>(),
            "{mouse:?}"
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_rejected_down_draws_its_tap_on_the_mouse() {
        // Windows rejects the down (E_INVALIDARG, as field hosts logged).
        let mut device = PenDevice::new(FakeApi::new(&[]));
        let now = Instant::now();
        device.slot.api.answers = [Outcome::Rejected].into();
        let mouse = send(&mut device, &[PenPhase::Down, PenPhase::Up], now);
        assert_eq!(presses(&mouse), [(true, 100), (false, 100)]);
        // A move whose fresh start is rejected too continues on the mouse.
        device.slot.api.frames.clear();
        send(&mut device, &[PenPhase::Down], now);
        device.slot.api.answers = [Outcome::Rejected, Outcome::Rejected].into();
        let mouse = send(
            &mut device,
            &[PenPhase::Move, PenPhase::Move, PenPhase::Up],
            now,
        );
        assert_eq!(presses(&mouse), [(true, 100), (false, 100)]);
        assert_eq!(
            device.slot.api.frames,
            [
                (1, PenPhase::Down),
                (1, PenPhase::Move),
                (1, PenPhase::Down)
            ]
        );
        // The next stroke is native again.
        device.slot.api.frames.clear();
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], now).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(1, PenPhase::Down), (1, PenPhase::Up)]
        );
        assert!(!device.slot.api.button);
    }

    fn pen_xy(phase: PenPhase, x: i32, y: i32) -> Injection {
        let Injection::Pen(sample) = pen_at(phase, y) else {
            unreachable!()
        };
        Injection::Pen(PenSample { x, ..sample })
    }

    fn abs(x: i32, y: i32) -> (u16, u16) {
        super::super::desktop_pixel_to_abs(x, y, SCREEN)
    }

    #[test]
    fn a_refused_release_comes_before_the_next_strokes_press() {
        // Sol's case: SendInput refuses stroke 1's release, its retry and
        // stroke 2's press, then takes stroke 2's move. Stroke 1 must be
        // released before stroke 2 is pressed, not joined to it.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let now = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 11, 1), now);
        flush_one(&mut device, now);
        deliver_one(&mut device, pen_xy(PenPhase::Down, 20, 2), now);
        device.slot.api.mouse_limit = usize::MAX;
        deliver_one(&mut device, pen_xy(PenPhase::Move, 21, 2), now);
        deliver_one(&mut device, pen_xy(PenPhase::Up, 22, 2), now);
        let at = |x, y| {
            let (x, y) = abs(x, y);
            (Injection::MoveAbs { x, y }, x, y)
        };
        let (m10, x10, y10) = at(10, 1);
        let (m11, x11, y11) = at(11, 1);
        let (m20, x20, y20) = at(20, 2);
        let (m21, _, _) = at(21, 2);
        let (m22, x22, y22) = at(22, 2);
        assert_eq!(
            device.slot.api.mouse,
            [
                m10,
                Injection::LeftDown { x: x10, y: y10 },
                m11,
                Injection::LeftUp { x: x11, y: y11 },
                m20,
                Injection::LeftDown { x: x20, y: y20 },
                m21,
                m22,
                Injection::LeftUp { x: x22, y: y22 },
            ]
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_tap_whose_press_was_refused_is_pressed_before_its_release() {
        // Sol's case: SendInput inserts only the move of a tap's down, then
        // recovers before the up.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let now = Instant::now();
        device.slot.api.mouse_limit = 1;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        device.slot.api.mouse_limit = usize::MAX;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 10, 1), now);
        let (x, y) = abs(10, 1);
        assert_eq!(
            device.slot.api.mouse,
            [
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
        assert!(!device.slot.api.button);
    }

    #[cfg(windows)]
    #[test]
    fn a_press_retried_apart_from_its_move_lands_where_the_pencil_touched() {
        // Sol's case: SendInput inserts only the move of a tap's down, and
        // the physical mouse moves the cursor before the press is retried.
        use windows::Win32::UI::Input::KeyboardAndMouse::{MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_MOVE};
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let now = Instant::now();
        device.slot.api.mouse_limit = 1;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        let first = std::mem::take(&mut device.slot.api.mouse);
        device.slot.api.mouse_limit = usize::MAX;
        let retried = tick_at(&mut device, now);
        let (x, y) = abs(10, 1);
        assert_eq!(first, [Injection::MoveAbs { x, y }]);
        // SendInput as Windows applies it: a mouse event's coordinates move
        // the cursor only with MOUSEEVENTF_MOVE, and a press lands where the
        // cursor is.
        let apply = |batch: &[Injection], cursor: &mut (i32, i32)| {
            let mut inputs = Vec::new();
            for &injection in batch {
                super::super::windows_inject::push_input(&mut inputs, injection);
            }
            let mut pressed = None;
            for input in &inputs {
                let mouse = unsafe { input.Anonymous.mi };
                if mouse.dwFlags.0 & MOUSEEVENTF_MOVE.0 != 0 {
                    *cursor = (mouse.dx, mouse.dy);
                }
                if mouse.dwFlags.0 & MOUSEEVENTF_LEFTDOWN.0 != 0 {
                    pressed = Some(*cursor);
                }
            }
            pressed
        };
        let mut cursor = (0, 0);
        assert_eq!(apply(&first, &mut cursor), None);
        // The physical mouse moves the cursor before the retry.
        cursor = (40_000, 40_000);
        assert_eq!(
            apply(&retried, &mut cursor),
            Some((i32::from(x), i32::from(y)))
        );
    }

    #[test]
    fn a_refused_move_in_a_stroke_is_retried_in_order() {
        // Sol's case: SendInput refuses a move between a down and an up it
        // takes.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let now = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Move, 15, 1), now);
        device.slot.api.mouse_limit = usize::MAX;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 20, 1), now);
        let moves: Vec<_> = device
            .slot
            .api
            .mouse
            .iter()
            .filter_map(|input| match *input {
                Injection::MoveAbs { x, y } => Some((x, y)),
                _ => None,
            })
            .collect();
        assert_eq!(moves, [abs(10, 1), abs(15, 1), abs(20, 1)]);
        assert_eq!(presses(&device.slot.api.mouse), [(true, 1), (false, 1)]);
    }

    #[test]
    fn native_input_waits_for_a_refused_release() {
        // Windows refused the device, so a stroke uses the mouse, and then
        // SendInput refuses its release. A stroke that could start natively
        // must wait on the mouse until the release is inserted.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        send(&mut device, &[PenPhase::Down], start);
        device.slot.api.mouse_limit = 0;
        send(&mut device, &[PenPhase::Up], start);
        assert!(device.slot.api.button);
        let later = start + RETRY_INTERVAL;
        send(&mut device, &[PenPhase::Down, PenPhase::Up], later);
        assert!(device.slot.api.frames.is_empty());
        assert_eq!(device.slot.api.created, 1);
        device.slot.api.mouse_limit = usize::MAX;
        let mouse = tick(&mut device);
        assert_eq!(presses(&mouse), [(false, 100), (true, 100), (false, 100)]);
        // With the mouse idle, the next stroke gets a device again.
        let later = start + RETRY_INTERVAL * 2;
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], later).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(2, PenPhase::Down), (2, PenPhase::Up)]
        );
    }

    #[test]
    fn stale_mouse_input_is_dropped_but_the_release_stays_owed() {
        // SendInput refuses everything for over a second, as under UIPI or
        // on the secure desktop, while a stroke on the mouse ends.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Move, 11, 1), start);
        let half = start + INPUT_STALE / 2;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 12, 1), half);
        // Past the limit each move is dropped once it has itself waited
        // that long; the release, owed to the press Windows holds, is not.
        let stale = start + INPUT_STALE;
        flush_one(&mut device, stale);
        assert_eq!(
            device.take_dropped_mouse_input(),
            [(MouseEvent::Move, (11, 1), "stale")]
        );
        flush_one(&mut device, half + INPUT_STALE);
        assert_eq!(
            device.take_dropped_mouse_input(),
            [(MouseEvent::Move, (12, 1), "stale")]
        );
        // A release left waiting alone is never dropped.
        for second in 2..5 {
            flush_one(&mut device, start + INPUT_STALE * second);
        }
        assert!(device.take_dropped_mouse_input().is_empty());
        // A new stroke waits behind it, and input that works again releases
        // first, without replaying the old stroke.
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Down, 20, 2),
            start + INPUT_STALE * 5,
        );
        device.slot.api.mouse_limit = usize::MAX;
        flush_one(&mut device, start + INPUT_STALE * 5);
        let (x10, y10) = abs(10, 1);
        let (x20, y20) = abs(20, 2);
        assert_eq!(
            device.slot.api.mouse,
            [
                Injection::MoveAbs { x: x10, y: y10 },
                Injection::LeftDown { x: x10, y: y10 },
                Injection::LeftUp { x: x10, y: y10 },
                Injection::MoveAbs { x: x20, y: y20 },
                Injection::LeftDown { x: x20, y: y20 },
            ]
        );
        // A reset drops waiting input the same way.
        device.slot.api.mouse_limit = 0;
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Up, 21, 2),
            start + INPUT_STALE * 5,
        );
        reset_one(&mut device, start + INPUT_STALE * 5);
        assert!(device
            .take_dropped_mouse_input()
            .iter()
            .all(|&(_, _, why)| why == "reset"));
        device.slot.api.mouse_limit = usize::MAX;
        flush_one(&mut device, start + INPUT_STALE * 5);
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_release_refused_as_the_pipeline_stops_is_sent_by_the_next_device() {
        // Sol's case: a stroke on the mouse holds the button, SendInput
        // refuses everything, and the pipeline restarts before a retry
        // succeeds: the relay resets and drops its device. Windows still
        // holds the button, so the next pipeline's device must release it
        // before any new input, on a refresh tick or ahead of a sample.
        let (x10, y10) = abs(10, 1);
        let (x20, y20) = abs(20, 2);
        for ticked in [true, false] {
            let mut device = PenDevice::new(FakeApi::new(&[false]));
            let start = Instant::now();
            deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
            device.slot.api.mouse_limit = 0;
            reset_one(&mut device, start);
            // Windows outlives the device.
            let (held, owed) = (
                device.slot.api.button,
                std::rc::Rc::clone(&device.slot.api.owed),
            );
            assert!(held);
            drop(device);
            let mut windows = FakeApi::new(&[false]);
            (windows.button, windows.owed) = (held, owed);
            let mut device = PenDevice::new(windows);
            // With no device, the transport ticks for the release alone.
            assert!(!device.available() && device.mouse_waiting());
            let later = start + Duration::from_millis(50);
            let mut mouse = Vec::new();
            if ticked {
                mouse.extend(tick_at(&mut device, later));
            }
            deliver_one(&mut device, pen_xy(PenPhase::Down, 20, 2), later);
            mouse.extend(std::mem::take(&mut device.slot.api.mouse));
            assert_eq!(
                mouse,
                [
                    Injection::LeftUp { x: x10, y: y10 },
                    Injection::MoveAbs { x: x20, y: y20 },
                    Injection::LeftDown { x: x20, y: y20 },
                ],
                "ticked: {ticked}"
            );
        }
    }

    /// What reached Windows, in order, for the random streams.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Seen {
        /// A native frame Windows accepted: phase, then the sample's x and
        /// y, its number and its stroke's.
        Native(PenPhase, i32, i32, Instant),
        /// A native frame Windows rejected, which ends its contact.
        Refused(PenPhase),
        /// The device was destroyed, ending its contact.
        Destroyed,
        /// A fallback mouse event, and whether SendInput inserted it.
        Mouse(Injection, bool, Instant),
        /// The test reset the session here.
        Reset,
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

    /// How often each kind of failure happened across the random streams.
    #[derive(Debug, Default)]
    struct Coverage {
        rejected: std::collections::BTreeMap<String, usize>,
        not_ready: usize,
        devices_refused: usize,
        devices_granted: usize,
        mouse_refused: usize,
        /// Batches SendInput cut short, by how many events it inserted.
        mouse_partial_after: std::collections::BTreeMap<usize, usize>,
        overflows: usize,
        streams_overflowed: usize,
        native_stale: usize,
        /// Streams where native input was given up as stuck.
        streams_given_up: usize,
        /// Ends Windows was owed, delivered `INPUT_STALE` or more after
        /// their sample arrived.
        late_owed_ends: usize,
        streams_losing_device: usize,
        streams_mouse_blocked: usize,
        resets: usize,
        dropped: std::collections::BTreeMap<&'static str, usize>,
        samples_checked: usize,
        ups_checked: usize,
    }

    /// Windows for the random streams, until `ready`: not ready for a
    /// quarter of the frames, or all of them while `stuck`; rejecting a
    /// tenth of the frames of every kind; refusing half the devices; and
    /// inserting all of a mouse batch, none (a tenth), or only its first
    /// events (a fifth), or none at all while `blocked`.
    struct RandomApi {
        rng: Rng,
        ready: bool,
        stuck: bool,
        blocked: bool,
        /// The time of the call in progress.
        now: Instant,
        seen: Vec<Seen>,
        coverage: Coverage,
    }

    impl PenApi for RandomApi {
        type Handle = ();
        type Error = &'static str;

        fn create(&mut self) -> Result<(), &'static str> {
            if self.ready || self.rng.below(2) == 0 {
                self.coverage.devices_granted += 1;
                Ok(())
            } else {
                self.coverage.devices_refused += 1;
                Err("refused")
            }
        }

        fn destroy(&mut self, _: ()) {
            self.seen.push(Seen::Destroyed);
        }

        fn screen(&self) -> VirtualScreen {
            SCREEN
        }

        fn inject(
            &mut self,
            _: (),
            frame: PenSample,
            _: PointerKind,
            _: VirtualScreen,
            _: Option<CaptureGeometry>,
        ) -> Outcome {
            let roll = if self.ready { 19 } else { self.rng.below(20) };
            match roll {
                _ if self.stuck && !self.ready => {
                    self.coverage.not_ready += 1;
                    Outcome::NotReady
                }
                0..=4 => {
                    self.coverage.not_ready += 1;
                    Outcome::NotReady
                }
                5..=6 => {
                    *self
                        .coverage
                        .rejected
                        .entry(format!("{:?}", frame.phase))
                        .or_default() += 1;
                    self.seen.push(Seen::Refused(frame.phase));
                    Outcome::Rejected
                }
                _ => {
                    self.seen
                        .push(Seen::Native(frame.phase, frame.x, frame.y, self.now));
                    Outcome::Injected
                }
            }
        }

        fn send_mouse(&mut self, inputs: &[Injection]) -> usize {
            self.record_mouse(inputs)
        }

        // Each stream's Windows goes with its device.
        fn owe_release(&mut self, _: (i32, i32)) {}

        fn take_owed_release(&mut self) -> Option<(i32, i32)> {
            None
        }
    }

    impl FakeSendInput for RandomApi {
        fn record_mouse(&mut self, inputs: &[Injection]) -> usize {
            if inputs.is_empty() {
                return 0;
            }
            let inserted = match (self.ready, self.blocked, self.rng.below(10)) {
                (true, ..) => inputs.len(),
                (false, true, _) | (false, false, 0) => {
                    self.coverage.mouse_refused += 1;
                    0
                }
                (false, false, 1..=2) if inputs.len() > 1 => {
                    let inserted = 1 + self.rng.below(inputs.len() as u64 - 1) as usize;
                    *self
                        .coverage
                        .mouse_partial_after
                        .entry(inserted)
                        .or_default() += 1;
                    inserted
                }
                _ => inputs.len(),
            };
            for (index, &input) in inputs.iter().enumerate() {
                self.seen
                    .push(Seen::Mouse(input, index < inserted, self.now));
            }
            inserted
        }
    }

    #[test]
    fn random_strokes_reach_windows_or_the_mouse_exactly_once() {
        // Relay-shaped streams (a lost up shows as the relay's cancel before
        // the next down) against Windows rejecting frames of every kind,
        // staying not ready long enough to fill the queue, refusing or
        // granting devices, and SendInput inserting none or part of a batch
        // or refusing everything for a while; with refresh ticks, and
        // session resets after which a stroke carries on.
        //
        // Every sample has its own x (its number) and its stroke's y, so
        // each native frame and mouse event names the sample it came from.
        // Replaying what Windows took must give each sample's input once,
        // in sample order, on one path, except for samples dropped by
        // design: positions coalesced behind a native retry, and input
        // waiting when a reset or the staleness limit dropped it.
        let to_pixel = |count: i32, axis: fn((u16, u16)) -> u16| {
            (0..count)
                .map(|p| (axis(abs(p, p.min(1079))), p))
                .collect::<std::collections::HashMap<u16, i32>>()
        };
        let pixel_x = to_pixel(1920, |a| a.0);
        let pixel_y = to_pixel(1080, |a| a.1);
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut coverage = Coverage::default();
        for stream in 0..3000 {
            let start = Instant::now();
            let mut device = PenDevice::new(RandomApi {
                rng: Rng(rng.below(u64::MAX) | 1),
                ready: false,
                stuck: false,
                blocked: false,
                now: start,
                seen: Vec::new(),
                coverage: Coverage::default(),
            });
            let mut now = start;
            let (mut stroke, mut in_contact, mut hovering) = (0, false, false);
            let mut samples: Vec<PenSample> = Vec::new();
            // When each sample was delivered, by its number less one.
            let mut times: Vec<Instant> = Vec::new();
            let mut exempt = std::collections::BTreeSet::new();
            // Strokes a reset or the staleness limit cut short.
            let mut cut_strokes = std::collections::BTreeSet::new();
            let (mut overflowed, mut lost) = (false, false);
            let length = rng.below(100);
            // Some streams have Windows stuck, or SendInput refusing
            // everything, for a stretch.
            let stretch = |rng: &mut Rng, one_in| {
                if rng.below(one_in) == 0 {
                    let from = rng.below(60);
                    from..from + 5 + rng.below(25)
                } else {
                    u64::MAX..u64::MAX
                }
            };
            let stuck = stretch(&mut rng, 2);
            let blocked = stretch(&mut rng, 3);
            coverage.streams_mouse_blocked += usize::from(blocked.start < length);
            // Record what one call dropped by design, checking why.
            let note_drops = |device: &mut PenDevice<RandomApi>,
                              resetting: bool,
                              times: &[Instant],
                              exempt: &mut std::collections::BTreeSet<i32>,
                              cut_strokes: &mut std::collections::BTreeSet<i32>,
                              coverage: &mut Coverage| {
                let api_now = device.slot.api.now;
                // Each sample dropped as stale must itself have waited the
                // limit; fresher ones behind it are kept.
                let old_enough = |id: i32, what: &dyn std::fmt::Debug| {
                    let waited = api_now.saturating_duration_since(times[id as usize - 1]);
                    assert!(
                        waited >= INPUT_STALE,
                        "stream {stream}: {what:?} of sample {id} dropped as stale after {waited:?}"
                    );
                };
                for (sample, why) in device.contact.take_dropped() {
                    match why {
                        "coalesced" => assert!(
                            matches!(sample.phase, PenPhase::Move | PenPhase::Hover),
                            "stream {stream}: coalesced {sample:?}"
                        ),
                        "reset" => {
                            assert!(resetting, "stream {stream}: reset drop outside a reset")
                        }
                        // Only ink goes stale: an end is owed to whatever
                        // Windows holds. A stroke keeps its up, so it is
                        // still checked to land where the Pencil lifted.
                        "stale" => {
                            assert!(!resetting, "stream {stream}: stale drop in a reset");
                            assert!(
                                matches!(
                                    sample.phase,
                                    PenPhase::Down | PenPhase::Move | PenPhase::Hover
                                ),
                                "stream {stream}: dropped the end {sample:?} as stale"
                            );
                            old_enough(sample.x, &sample.phase);
                            coverage.native_stale += 1;
                        }
                        _ => panic!("stream {stream}: dropped {sample:?} for {why}"),
                    }
                    *coverage.dropped.entry(why).or_default() += 1;
                    exempt.insert(sample.x);
                    if why == "reset" {
                        cut_strokes.insert(sample.y);
                    }
                }
                for (event, (id, stroke), why) in device.take_dropped_mouse_input() {
                    match why {
                        "reset" => {
                            assert!(resetting, "stream {stream}: reset drop outside a reset")
                        }
                        "stale" => assert!(!resetting, "stream {stream}: stale drop in a reset"),
                        _ => panic!("stream {stream}: dropped {event:?} for {why}"),
                    }
                    *coverage.dropped.entry(why).or_default() += 1;
                    // A release goes with the press it ended.
                    if why == "stale" && event != MouseEvent::Release {
                        old_enough(id, &event);
                    }
                    exempt.insert(id);
                    cut_strokes.insert(stroke);
                }
            };
            for step in 0..length {
                device.slot.api.stuck = stuck.contains(&step);
                device.slot.api.blocked = blocked.contains(&step);
                let mut phases = Vec::new();
                match (in_contact, rng.below(10)) {
                    (false, 0..=5) if stroke < 1000 => {
                        stroke += 1;
                        phases.push((PenPhase::Down, stroke));
                        (in_contact, hovering) = (true, false);
                    }
                    (false, 0..=7) => {
                        phases.push((PenPhase::Hover, 0));
                        hovering = true;
                    }
                    (false, _) if hovering => {
                        phases.push((PenPhase::Leave, 0));
                        hovering = false;
                    }
                    (false, _) => {}
                    (true, 0..=3) => phases.push((PenPhase::Move, stroke)),
                    (true, 4..=7) => {
                        phases.push((PenPhase::Up, stroke));
                        in_contact = false;
                    }
                    (true, 8) if stroke < 1000 => {
                        // The up was lost: the relay cancels before the next down.
                        phases.push((PenPhase::Cancel, stroke));
                        stroke += 1;
                        phases.push((PenPhase::Down, stroke));
                    }
                    (true, _) => {
                        phases.push((PenPhase::Cancel, stroke));
                        in_contact = false;
                    }
                }
                // Mostly bursts that can fill the queue, sometimes a stall
                // long enough to make what waits stale.
                now += Duration::from_millis(if rng.below(16) == 0 {
                    300 + rng.below(1200)
                } else {
                    rng.below(40)
                });
                device.slot.api.now = now;
                for (phase, stroke) in phases {
                    let id = samples.len() as i32 + 1;
                    let Injection::Pen(sample) = pen_xy(phase, id, stroke) else {
                        unreachable!()
                    };
                    samples.push(sample);
                    times.push(now);
                    let edge = !matches!(phase, PenPhase::Move | PenPhase::Hover);
                    if edge && device.contact.waiting() >= super::super::pen_contact::QUEUE_LIMIT {
                        coverage.overflows += 1;
                        overflowed = true;
                    }
                    deliver_one(&mut device, Injection::Pen(sample), now);
                    note_drops(
                        &mut device,
                        false,
                        &times,
                        &mut exempt,
                        &mut cut_strokes,
                        &mut coverage,
                    );
                    lost |= !device.available();
                }
                if rng.below(2) == 0 {
                    flush_one(&mut device, now);
                    note_drops(
                        &mut device,
                        false,
                        &times,
                        &mut exempt,
                        &mut cut_strokes,
                        &mut coverage,
                    );
                    lost |= !device.available();
                }
                if rng.below(25) == 0 {
                    // The session resets; the stroke in progress carries on.
                    device.slot.api.seen.push(Seen::Reset);
                    reset_one(&mut device, now);
                    note_drops(
                        &mut device,
                        true,
                        &times,
                        &mut exempt,
                        &mut cut_strokes,
                        &mut coverage,
                    );
                    coverage.resets += 1;
                    lost |= !device.available();
                }
            }
            // Everything works again; the next tick delivers what waits.
            device.slot.api.ready = true;
            device.slot.api.now = now;
            flush_one(&mut device, now);
            note_drops(
                &mut device,
                false,
                &times,
                &mut exempt,
                &mut cut_strokes,
                &mut coverage,
            );
            coverage.streams_overflowed += usize::from(overflowed);
            coverage.streams_given_up += usize::from(device.slot.reported.contains(&"stuck"));
            coverage.streams_losing_device += usize::from(lost);

            // Replay what Windows took, in order.
            let output = std::mem::take(&mut device.slot.api.seen);
            let (mut native, mut mouse) = (None, None);
            // Strokes drawn on either path, and those Windows or a reset
            // ended early (a rejected frame, a destroyed device, a session
            // reset) with no press since: their up has nothing left to end.
            let mut drawn = std::collections::BTreeSet::new();
            let mut cut = std::collections::BTreeSet::new();
            let mut last_sample = 0;
            let mut taken = std::collections::BTreeMap::<(i32, &'static str), usize>::new();
            let mut take = |id: i32, kind: &'static str| {
                assert!(
                    id >= last_sample,
                    "stream {stream}: sample {id} {kind} after sample {last_sample}: {output:?}"
                );
                last_sample = id;
                *taken.entry((id, kind)).or_default() += 1;
            };
            // Nothing but an end Windows is owed may reach it `INPUT_STALE`
            // after its sample arrived: native cancels and leaves end the
            // pointer where Windows had it, an up (and the move to where the
            // Pencil lifted before it) ends a contact Windows holds, and a
            // mouse release may be owed for longer.
            let fresh = |id: i32, at: Instant, what: &str| {
                let age = at.saturating_duration_since(times[id as usize - 1]);
                assert!(
                    age < INPUT_STALE,
                    "stream {stream}: sample {id} {what} injected {age:?} after it arrived: {output:?}"
                );
            };
            for seen in &output {
                match *seen {
                    Seen::Native(phase, x, y, at) => {
                        let owed = match phase {
                            PenPhase::Cancel | PenPhase::Leave => true,
                            PenPhase::Up | PenPhase::Move => {
                                samples[x as usize - 1].phase == PenPhase::Up && native == Some(y)
                            }
                            _ => false,
                        };
                        if !owed {
                            fresh(x, at, "native frame");
                        } else if at.saturating_duration_since(times[x as usize - 1]) >= INPUT_STALE
                        {
                            coverage.late_owed_ends += 1;
                        }
                        if matches!(phase, PenPhase::Down | PenPhase::Move) {
                            assert_eq!(
                                mouse, None,
                                "stream {stream}: native and mouse together: {output:?}"
                            );
                        }
                        match phase {
                            PenPhase::Down => {
                                native = Some(y);
                                drawn.insert(y);
                                cut.remove(&y);
                            }
                            PenPhase::Up | PenPhase::Cancel => {
                                assert_eq!(
                                    native,
                                    Some(y),
                                    "stream {stream}: native up without its down: {output:?}"
                                );
                                native = None;
                            }
                            _ => {}
                        }
                        // A cancel or leave ends the pointer where Windows
                        // last saw it, an earlier sample's place.
                        match phase {
                            PenPhase::Down => take(x, "down"),
                            PenPhase::Move => take(x, "move"),
                            PenPhase::Up => take(x, "up"),
                            PenPhase::Hover => take(x, "hover"),
                            PenPhase::Cancel | PenPhase::Leave => {}
                        }
                    }
                    Seen::Refused(_) | Seen::Destroyed => {
                        if let Some(stroke) = native.take() {
                            cut.insert(stroke);
                        }
                    }
                    // A reset ends whatever is held; the frames or release
                    // that follow end it.
                    Seen::Reset => cut.extend(native.iter().chain(mouse.iter()).copied()),
                    Seen::Mouse(_, false, _) => {}
                    Seen::Mouse(Injection::MoveAbs { x, .. }, true, at) => {
                        fresh(pixel_x[&x], at, "mouse move");
                        take(pixel_x[&x], "mouse move")
                    }
                    Seen::Mouse(Injection::LeftDown { x, y }, true, at) => {
                        fresh(pixel_x[&x], at, "mouse press");
                        // Only a down or move draws: ending a stroke whose
                        // press never reached Windows must not click.
                        let phase = samples[pixel_x[&x] as usize - 1].phase;
                        assert!(
                            matches!(phase, PenPhase::Down | PenPhase::Move),
                            "stream {stream}: mouse press for a {phase:?}: {output:?}"
                        );
                        assert_eq!(
                            mouse, None,
                            "stream {stream}: mouse pressed twice: {output:?}"
                        );
                        assert_eq!(
                            native, None,
                            "stream {stream}: native and mouse together: {output:?}"
                        );
                        mouse = Some(pixel_y[&y]);
                        drawn.insert(pixel_y[&y]);
                        cut.remove(&pixel_y[&y]);
                        take(pixel_x[&x], "mouse press");
                    }
                    Seen::Mouse(Injection::LeftUp { .. }, true, _) => {
                        // A release kept owed after a drop sits at the last
                        // place the mouse went, an earlier sample's.
                        assert!(
                            mouse.is_some(),
                            "stream {stream}: mouse up without a down: {output:?}"
                        );
                        mouse = None;
                    }
                    Seen::Mouse(..) => unreachable!(),
                }
            }
            // Exactly once, on one path, for every sample not dropped by
            // design. An up, cancel or leave has nothing to deliver when
            // what it ends is already gone; the replay above checks that
            // nothing is ended twice and nothing stays held. An up whose
            // stroke is still live must land at its own position, where the
            // Pencil lifted: natively an up frame there, on the mouse a move
            // there before the release.
            let once = |id, kind| taken.get(&(id, kind)).copied().unwrap_or(0);
            for sample in &samples {
                let id = sample.x;
                let native_frames =
                    once(id, "down") + once(id, "move") + once(id, "up") + once(id, "hover");
                let mouse_input = once(id, "mouse move") + once(id, "mouse press");
                for kind in ["down", "move", "up", "hover", "mouse move", "mouse press"] {
                    assert!(
                        once(id, kind) <= 1,
                        "stream {stream}: sample {id} {kind} twice: {output:?}"
                    );
                }
                assert!(
                    native_frames == 0 || mouse_input == 0,
                    "stream {stream}: sample {id} on both paths: {output:?}"
                );
                let stroke = sample.y;
                if sample.phase == PenPhase::Up
                    && drawn.contains(&stroke)
                    && !cut.contains(&stroke)
                    && !cut_strokes.contains(&stroke)
                {
                    coverage.ups_checked += 1;
                    assert!(
                        once(id, "up") + once(id, "mouse move") == 1,
                        "stream {stream}: up {id} of stroke {stroke} missed its position: {output:?}"
                    );
                }
                if exempt.contains(&id)
                    || !matches!(
                        sample.phase,
                        PenPhase::Down | PenPhase::Move | PenPhase::Hover
                    )
                {
                    continue;
                }
                coverage.samples_checked += 1;
                assert!(
                    native_frames + mouse_input > 0,
                    "stream {stream}: sample {id} {:?} was lost: {output:?}",
                    sample.phase
                );
                if sample.phase == PenPhase::Down && mouse_input > 0 {
                    assert_eq!(
                        once(id, "mouse press"),
                        1,
                        "stream {stream}: down {id} not pressed: {output:?}"
                    );
                }
            }
            if !in_contact {
                assert_eq!(
                    (native, mouse),
                    (None, None),
                    "stream {stream}: left pressed: {output:?}"
                );
            }
            let api = &device.slot.api.coverage;
            for (phase, count) in &api.rejected {
                *coverage.rejected.entry(phase.clone()).or_default() += count;
            }
            for (inserted, count) in &api.mouse_partial_after {
                *coverage.mouse_partial_after.entry(*inserted).or_default() += count;
            }
            coverage.not_ready += api.not_ready;
            coverage.devices_refused += api.devices_refused;
            coverage.devices_granted += api.devices_granted;
            coverage.mouse_refused += api.mouse_refused;
        }
        eprintln!("COVERAGE {coverage:#?}");
        for phase in ["Down", "Move", "Up", "Cancel", "Hover", "Leave"] {
            assert!(
                coverage.rejected.get(phase).is_some_and(|&n| n > 100),
                "{coverage:#?}"
            );
        }
        for why in ["coalesced", "reset", "stale"] {
            assert!(
                coverage.dropped.get(why).is_some_and(|&n| n > 50),
                "{coverage:#?}"
            );
        }
        assert!(coverage.ups_checked > 10_000, "{coverage:#?}");
        assert!(coverage.streams_overflowed > 200, "{coverage:#?}");
        assert!(coverage.native_stale > 300, "{coverage:#?}");
        assert!(coverage.streams_given_up > 40, "{coverage:#?}");
        assert!(coverage.late_owed_ends > 50, "{coverage:#?}");
        assert!(coverage.streams_losing_device > 300, "{coverage:#?}");
        assert!(coverage.mouse_refused > 300, "{coverage:#?}");
        assert!(
            (1..=3).all(|k| coverage
                .mouse_partial_after
                .get(&k)
                .is_some_and(|&n| n > 20)),
            "{coverage:#?}"
        );
    }

    #[test]
    fn a_tap_windows_was_not_ready_for_is_not_injected_seconds_later() {
        // Claude's case: Windows is not ready for a tap, and the next tick
        // comes five seconds later with Windows ready again.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        send(&mut device, &[PenPhase::Down, PenPhase::Up], start);
        device.slot.api.answer = Outcome::Injected;
        device.slot.api.frames.clear();
        let late = start + Duration::from_secs(5);
        flush_one(&mut device, late);
        assert!(device.slot.api.frames.is_empty());
        assert!(device.slot.api.mouse.is_empty());
        // The down went stale. The up stays, but Windows holds nothing for
        // it to end, so it sends nothing and the device is kept.
        let dropped = device.contact.take_dropped();
        assert_eq!(
            dropped
                .iter()
                .map(|&(s, why)| (s.phase, why))
                .collect::<Vec<_>>(),
            [(PenPhase::Down, "stale")]
        );
        assert!(device.slot.api.destroyed.is_empty());
        // The next tap starts cleanly.
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], late).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(1, PenPhase::Down), (1, PenPhase::Up)]
        );
    }

    #[test]
    fn taps_windows_is_never_ready_for_are_clicked_when_they_come() {
        // Claude's case: taps ten seconds apart, on a 50 ms refresh tick,
        // while Windows stays not ready and SendInput works. No tap may come
        // out late, as an overflow once replayed old taps as clicks; once
        // native input is found stuck, each tap is a click when it comes.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        let (mut clicks, mut dropped) = (Vec::new(), Vec::new());
        for tap in 0..4 {
            let at = start + Duration::from_secs(10 * tap);
            let x = 100 + tap as i32;
            deliver_one(&mut device, pen_xy(PenPhase::Down, x, 1), at);
            deliver_one(&mut device, pen_xy(PenPhase::Up, x, 1), at);
            let mut mouse = std::mem::take(&mut device.slot.api.mouse);
            for tick in 1..=10 {
                mouse.extend(tick_at(&mut device, at + Duration::from_millis(50 * tick)));
            }
            clicks.push(mouse);
            dropped.extend(device.contact.take_dropped());
        }
        // The first tap only showed Windows to be slow, and went stale.
        assert!(clicks[0].is_empty());
        assert_eq!(
            dropped
                .iter()
                .map(|&(s, why)| (s.phase, s.x, why))
                .collect::<Vec<_>>(),
            [(PenPhase::Down, 100, "stale")]
        );
        for (tap, mouse) in clicks.iter().enumerate().skip(1) {
            let (x, y) = abs(100 + tap as i32, 1);
            assert_eq!(
                mouse,
                &[
                    Injection::MoveAbs { x, y },
                    Injection::LeftDown { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::LeftUp { x, y },
                ],
                "tap {tap}"
            );
        }
        assert_eq!(device.slot.reported, ["stuck"]);
    }

    #[test]
    fn strokes_reach_the_mouse_when_native_input_stays_not_ready() {
        // Claude's case: Windows answers every native frame ERROR_NOT_READY
        // while SendInput works, and ten half-second strokes come 0.7 s
        // apart on a 16 ms refresh tick. Each stroke used to wait, expire
        // and recreate the device, so nothing was drawn and nothing logged.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        let frame = Duration::from_millis(16);
        let mut began = Vec::new();
        for stroke in 1..=10 {
            let begin = start + Duration::from_millis(1200) * (stroke - 1);
            began.push((stroke as i32, begin));
            let mut now = begin;
            let mut x = 100;
            deliver_one(&mut device, pen_xy(PenPhase::Down, x, stroke as i32), now);
            while now < begin + Duration::from_millis(500) {
                now += frame;
                x += 1;
                deliver_one(&mut device, pen_xy(PenPhase::Move, x, stroke as i32), now);
            }
            deliver_one(&mut device, pen_xy(PenPhase::Up, x, stroke as i32), now);
            while now < begin + Duration::from_millis(1200) - frame {
                now += frame;
                flush_one(&mut device, now);
            }
        }
        let pressed: Vec<u16> = device
            .slot
            .api
            .mouse
            .iter()
            .filter_map(|input| match *input {
                Injection::LeftDown { y, .. } => Some(y),
                _ => None,
            })
            .collect();
        // Every stroke starting two seconds after Windows first refused a
        // frame is drawn on the mouse.
        for (stroke, begin) in began {
            if begin >= start + Duration::from_secs(2) {
                assert!(
                    pressed.contains(&abs(0, stroke).1),
                    "stroke {stroke} was not drawn: {pressed:?}"
                );
            }
        }
        assert!(!device.slot.api.button);
        assert_eq!(device.slot.reported, ["stuck"]);
    }

    #[test]
    fn a_lift_windows_is_slow_to_take_still_lands_where_the_pencil_lifted() {
        // Claude's case: a three-second native stroke, then Windows is not
        // ready from the lift for 1.2 s. The up is owed to the contact
        // Windows holds: it must not be dropped as stale or turned into a
        // destroyed device, which ends the contact as cancelled, and an ink
        // app may discard a cancelled stroke the user saw drawn.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        let start = Instant::now();
        let frame = Duration::from_millis(16);
        let mut now = start;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        for x in 11..=197 {
            now += frame;
            deliver_one(&mut device, pen_xy(PenPhase::Move, x, 1), now);
        }
        device.slot.api.answer = Outcome::NotReady;
        now += frame;
        let lift = now;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 300, 1), lift);
        while now < lift + Duration::from_millis(1200) {
            now += frame;
            flush_one(&mut device, now);
        }
        device.slot.api.answer = Outcome::Injected;
        now += frame;
        flush_one(&mut device, now);
        // A move to where the Pencil lifted, then the up there.
        assert_eq!(
            device.slot.api.frames[device.slot.api.frames.len() - 2..],
            [(1, PenPhase::Move), (1, PenPhase::Up)]
        );
        assert!(device.slot.api.destroyed.is_empty());
        assert!(!device
            .slot
            .api
            .frames
            .iter()
            .any(|&(_, phase)| phase == PenPhase::Cancel));
        assert!(device.contact.take_dropped().is_empty());
        assert!(device.slot.api.mouse.is_empty());
    }

    #[test]
    fn a_stroke_behind_stale_samples_keeps_its_fresh_down() {
        // Claude's case: Windows is not ready for a tap, nor for the next
        // stroke's down, which arrives 68 ms before the tap goes stale.
        // When Windows is ready again only the tap is dropped; the stroke
        // starts then, not 70 ms later.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Up, 10, 1),
            start + Duration::from_millis(100),
        );
        let expiry = start + INPUT_STALE;
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Down, 20, 2),
            expiry - Duration::from_millis(68),
        );
        device.slot.api.answer = Outcome::Injected;
        device.slot.api.frames.clear();
        flush_one(&mut device, expiry);
        let dropped = device.contact.take_dropped();
        assert_eq!(
            dropped
                .iter()
                .map(|&(s, why)| (s.phase, s.x, why))
                .collect::<Vec<_>>(),
            [(PenPhase::Down, 10, "stale")]
        );
        assert_eq!(device.slot.api.frames, [(1, PenPhase::Down)]);
        assert!(device.slot.api.destroyed.is_empty());
    }

    /// A native stroke drawn every 16 ms from `start` for `length`, during
    /// which Windows answers ERROR_NOT_READY from `hitch` for `stall`.
    fn stroke_with_hitch(
        device: &mut PenDevice<FakeApi>,
        start: Instant,
        length: Duration,
        hitch: Duration,
        stall: Duration,
    ) {
        let frame = Duration::from_millis(16);
        let (mut now, mut x) = (start, 10);
        let answer = |now: Instant| {
            if (start + hitch..start + hitch + stall).contains(&now) {
                Outcome::NotReady
            } else {
                Outcome::Injected
            }
        };
        device.slot.api.answer = answer(now);
        deliver_one(device, pen_xy(PenPhase::Down, x, 1), now);
        while now < start + length {
            now += frame;
            x += 1;
            device.slot.api.answer = answer(now);
            deliver_one(device, pen_xy(PenPhase::Move, x, 1), now);
        }
        deliver_one(device, pen_xy(PenPhase::Up, x, 1), now);
    }

    #[test]
    fn a_mid_stroke_hitch_that_clears_keeps_the_stroke_native() {
        // Claude's case: Windows is not ready for 1.1 s in the middle of a
        // stroke. Recreating the device at one second would cancel the
        // native part, which an ink app may discard, and gain nothing: the
        // stall clears on its own. The stroke stays one native stroke.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        stroke_with_hitch(
            &mut device,
            Instant::now(),
            Duration::from_millis(2100),
            Duration::from_millis(500),
            Duration::from_millis(1100),
        );
        let frames = &device.slot.api.frames;
        let count = |phase| frames.iter().filter(|&&(_, p)| p == phase).count();
        assert_eq!((count(PenPhase::Down), count(PenPhase::Cancel)), (1, 0));
        assert_eq!(frames.last(), Some(&(1, PenPhase::Up)));
        assert!(device.slot.api.destroyed.is_empty());
        assert!(device.slot.api.mouse.is_empty());
    }

    #[test]
    fn a_long_mid_stroke_hitch_still_reaches_the_mouse() {
        // Guard: a two-second stall mid-stroke is stuck native input; the
        // rest of the stroke is drawn on the mouse and released.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        stroke_with_hitch(
            &mut device,
            Instant::now(),
            Duration::from_millis(3000),
            Duration::from_millis(500),
            Duration::from_millis(2000),
        );
        assert_eq!(device.slot.reported, ["stuck"]);
        assert!(!device.slot.api.destroyed.is_empty());
        let mouse = &device.slot.api.mouse;
        assert!(matches!(mouse.get(1), Some(Injection::LeftDown { .. })));
        assert!(matches!(mouse.last(), Some(Injection::LeftUp { .. })));
        assert!(!device.slot.api.button);
    }

    #[test]
    fn an_owed_up_survives_a_stall_into_the_next_stroke() {
        // Claude's case: Windows is not ready from the lift for 1.15 s, and
        // the next stroke starts 0.3 s after the lift, as in handwriting.
        // The up is owed to the contact Windows holds and goes natively
        // once the stall clears, before the next stroke, which stays native.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        let start = Instant::now();
        let frame = Duration::from_millis(16);
        let mut now = start;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        for x in 11..40 {
            now += frame;
            deliver_one(&mut device, pen_xy(PenPhase::Move, x, 1), now);
        }
        device.slot.api.answer = Outcome::NotReady;
        now += frame;
        let lift = now;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 60, 1), lift);
        let clears = lift + Duration::from_millis(1150);
        let mut x = 100;
        while now < lift + Duration::from_millis(1500) {
            now += frame;
            if now >= clears {
                device.slot.api.answer = Outcome::Injected;
            }
            if now < lift + Duration::from_millis(300) {
                flush_one(&mut device, now);
                continue;
            }
            let phase = if x == 100 {
                PenPhase::Down
            } else {
                PenPhase::Move
            };
            deliver_one(&mut device, pen_xy(phase, x, 2), now);
            x += 1;
        }
        deliver_one(&mut device, pen_xy(PenPhase::Up, x, 2), now);
        let frames = &device.slot.api.frames;
        let first = |phase| frames.iter().position(|&(_, p)| p == phase);
        let downs: Vec<_> = (0..frames.len())
            .filter(|&i| frames[i].1 == PenPhase::Down)
            .collect();
        assert_eq!(downs.len(), 2, "{frames:?}");
        assert!(
            first(PenPhase::Up).is_some_and(|up| up < downs[1]),
            "{frames:?}"
        );
        assert_eq!(first(PenPhase::Cancel), None);
        assert!(frames.iter().all(|&(handle, _)| handle == 1));
        assert!(device.slot.api.destroyed.is_empty());
        assert!(device.slot.api.mouse.is_empty());
    }

    #[test]
    fn a_session_reset_ends_a_hover_so_the_next_one_asks_for_a_device() {
        // Windows refuses the device. A hover asks again once, where it
        // starts; the session resets while it carries on, so the next
        // hover starts afresh and asks again.
        let mut device = PenDevice::new(FakeApi::new(&[false, false]));
        let start = Instant::now();
        let hover = |device: &mut PenDevice<FakeApi>, at: Duration| {
            deliver_one(device, pen_xy(PenPhase::Hover, 100, 1), start + at);
        };
        hover(&mut device, Duration::from_secs(2));
        hover(&mut device, Duration::from_secs(4));
        assert_eq!(device.slot.api.created, 2);
        reset_one(&mut device, start + Duration::from_secs(4));
        hover(&mut device, Duration::from_millis(4100));
        assert_eq!(device.slot.api.created, 3);
        assert!(device.available());
    }

    #[test]
    fn without_a_held_contact_native_input_is_stuck_after_a_second() {
        // Windows is not ready for a stroke's down, so it holds no contact:
        // the device is recreated a second after the first refusal, with no
        // grace for a contact to keep.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        for x in 11..=72 {
            let at = start + Duration::from_millis(16) * (x - 10);
            deliver_one(&mut device, pen_xy(PenPhase::Move, x as i32, 1), at);
        }
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Move, 74, 1),
            start + INPUT_STALE - Duration::from_millis(1),
        );
        assert_eq!(device.slot.api.created, 1);
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Move, 75, 1),
            start + INPUT_STALE,
        );
        assert_eq!(device.slot.api.created, 2);
    }

    #[test]
    fn hovering_over_stuck_native_input_tries_one_device() {
        // Claude's case: eight seconds of hover while native input stays not
        // ready. Once it is given up, the hover carries on on the mouse; it
        // does not start again, so no device is asked for, and the cursor
        // never freezes for a device's trial.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        let frame = Duration::from_millis(16);
        let (mut now, mut x) = (start, 10);
        while now < start + Duration::from_secs(8) {
            deliver_one(&mut device, pen_xy(PenPhase::Hover, x, 1), now);
            now += frame;
            x = 10 + (x + 1) % 1000;
        }
        // The first device and the one recreated when native input seemed
        // stuck.
        assert_eq!(device.slot.api.created, 2);
        assert_eq!(device.slot.reported, ["stuck"]);
        let moves = device
            .slot
            .api
            .mouse
            .iter()
            .filter(|input| matches!(input, Injection::MoveAbs { .. }))
            .count();
        assert!(moves > 400, "{moves}");
        // A new hover may try again.
        deliver_one(&mut device, pen_xy(PenPhase::Leave, x, 1), now);
        deliver_one(&mut device, pen_xy(PenPhase::Hover, x, 1), now);
        assert_eq!(device.slot.api.created, 3);
    }

    #[test]
    fn a_stroke_cancelled_before_its_press_reached_windows_never_clicks() {
        // Sol's case: SendInput refuses a stroke's press, the stroke is
        // cancelled 990 ms later, and input works again at 1010 ms, when
        // the press has gone stale. Re-creating it at the cancel's place
        // would click whatever window is there now.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        let cancel = start + Duration::from_millis(990);
        deliver_one(&mut device, pen_xy(PenPhase::Cancel, 10, 1), cancel);
        device.slot.api.mouse_limit = usize::MAX;
        let mouse = tick_at(&mut device, start + Duration::from_millis(1010));
        assert!(
            !mouse.iter().any(|input| matches!(
                input,
                Injection::LeftDown { .. } | Injection::LeftUp { .. }
            )),
            "{mouse:?}"
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_lift_after_a_stale_press_moves_without_clicking() {
        // As above with the Pencil lifting at 990 ms, somewhere else: the up
        // is fresh, but its stroke was never pressed, so the pointer goes
        // to the lift with no button event.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        let lift = start + Duration::from_millis(990);
        deliver_one(&mut device, pen_xy(PenPhase::Up, 20, 1), lift);
        device.slot.api.mouse_limit = usize::MAX;
        let (x, y) = abs(20, 1);
        assert_eq!(
            tick_at(&mut device, start + Duration::from_millis(1010)),
            [Injection::MoveAbs { x, y }]
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_stroke_that_carries_on_after_a_stale_press_is_pressed_again() {
        // The press goes stale but the stroke goes on: its fresh move is
        // pressed, so the rest of the stroke draws and is released.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        let moved = start + Duration::from_millis(990);
        deliver_one(&mut device, pen_xy(PenPhase::Move, 15, 1), moved);
        device.slot.api.mouse_limit = usize::MAX;
        let recovered = start + Duration::from_millis(1010);
        let mut mouse = tick_at(&mut device, recovered);
        deliver_one(&mut device, pen_xy(PenPhase::Up, 20, 1), recovered);
        mouse.extend(std::mem::take(&mut device.slot.api.mouse));
        let (x15, y15) = abs(15, 1);
        let (x20, y20) = abs(20, 1);
        assert_eq!(
            mouse,
            [
                Injection::MoveAbs { x: x15, y: y15 },
                Injection::LeftDown { x: x15, y: y15 },
                Injection::MoveAbs { x: x20, y: y20 },
                Injection::LeftUp { x: x20, y: y20 },
            ]
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn mouse_fallback_draws_pen_strokes_where_the_pen_would() {
        // Restored guard: the fallback's events, converted when sent, land
        // on the pen's desktop pixel at the far corner of a monitor left of
        // the primary, where the virtual screen starts at a negative x.
        // SendInput resolves `origin + abs * span / 65536`.
        let screen = VirtualScreen {
            left: -2360,
            top: 0,
            width: 4920,
            height: 1640,
        };
        let landing = |x: u16, y: u16| {
            (
                screen.left + (u64::from(x) * u64::from(screen.width) / 65_536) as i32,
                screen.top + (u64::from(y) * u64::from(screen.height) / 65_536) as i32,
            )
        };
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        device.slot.api.screen = screen;
        let now = Instant::now();
        for phase in [
            PenPhase::Hover,
            PenPhase::Down,
            PenPhase::Move,
            PenPhase::Up,
        ] {
            deliver_one(&mut device, pen_xy(phase, -2360, 1639), now);
        }
        let mouse = &device.slot.api.mouse;
        let (x, y) = desktop_pixel_to_abs(-2360, 1639, screen);
        assert_eq!(landing(x, y), (-2360, 1639));
        assert_eq!(
            mouse,
            &[
                Injection::MoveAbs { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
    }

    #[test]
    fn random_strokes_reach_the_mouse_once_native_input_stays_not_ready() {
        // Liveness: Windows takes native frames until a random moment, then
        // answers every one ERROR_NOT_READY for good, while SendInput works.
        // Strokes of random length and spacing, with hovers or refresh
        // ticks between them every 16 ms: every stroke starting two seconds
        // or more after Windows first refused a frame is drawn on the mouse,
        // and a long hover does not keep asking for devices.
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let frame = Duration::from_millis(16);
        let mut checked = 0;
        for stream in 0..300 {
            let mut device = PenDevice::new(FakeApi::new(&[]));
            let start = Instant::now();
            let stuck_at = start + Duration::from_millis(rng.below(5000));
            let mut now = start;
            let mut refused_at = None;
            let mut began = Vec::new();
            // Devices created while a hover carries on.
            let (mut hovering, mut created_mid_hover) = (false, 0);
            let mut call = |device: &mut PenDevice<FakeApi>, sample: Option<Injection>, now| {
                if now >= stuck_at {
                    device.slot.api.answer = Outcome::NotReady;
                }
                let (attempts, created) = (device.slot.api.frames.len(), device.slot.api.created);
                let phase = sample.map(|sample| {
                    let Injection::Pen(sample) = sample else {
                        unreachable!()
                    };
                    sample.phase
                });
                let carries_on = phase == Some(PenPhase::Hover) && hovering;
                hovering = phase == Some(PenPhase::Hover);
                match sample {
                    Some(sample) => deliver_one(device, sample, now),
                    None => flush_one(device, now),
                }
                if carries_on {
                    created_mid_hover += device.slot.api.created - created;
                }
                if now >= stuck_at && device.slot.api.frames.len() > attempts {
                    refused_at.get_or_insert(now);
                }
            };
            for stroke in 1..=40 {
                let hovering = rng.below(2) == 0;
                for _ in 0..rng.below(200) {
                    now += frame;
                    let hover = hovering.then(|| pen_xy(PenPhase::Hover, 1000, 0));
                    call(&mut device, hover, now);
                }
                if hovering {
                    call(&mut device, Some(pen_xy(PenPhase::Leave, 1000, 0)), now);
                }
                began.push((stroke, now));
                let mut x = 100;
                call(&mut device, Some(pen_xy(PenPhase::Down, x, stroke)), now);
                for _ in 0..rng.below(60) {
                    now += frame;
                    x += 1;
                    call(&mut device, Some(pen_xy(PenPhase::Move, x, stroke)), now);
                }
                call(&mut device, Some(pen_xy(PenPhase::Up, x, stroke)), now);
            }
            // The refresh tick carries on after the last stroke.
            for _ in 0..30 {
                now += frame;
                call(&mut device, None, now);
            }
            let pressed: std::collections::BTreeSet<u16> = device
                .slot
                .api
                .mouse
                .iter()
                .filter_map(|input| match *input {
                    Injection::LeftDown { y, .. } => Some(y),
                    _ => None,
                })
                .collect();
            // However long a hover carries on, it asks for no device; the
            // one recreated when native input first seemed stuck may land
            // in a hover.
            assert!(
                created_mid_hover <= 1,
                "stream {stream}: {created_mid_hover} devices created mid-hover"
            );
            let Some(refused_at) = refused_at else {
                continue;
            };
            for &(stroke, begin) in &began {
                if begin >= refused_at + Duration::from_secs(2) {
                    checked += 1;
                    assert!(
                        pressed.contains(&abs(0, stroke).1),
                        "stream {stream}: stroke {stroke} at {:?} not drawn, native stuck from {:?}",
                        begin - start,
                        refused_at - start
                    );
                }
            }
            assert_eq!(device.slot.reported, ["stuck"], "stream {stream}");
        }
        assert!(checked > 5000, "{checked}");
    }

    #[test]
    fn refused_mouse_input_lands_where_the_virtual_screen_is_on_retry() {
        // SendInput refuses a press; a monitor is added to the left before
        // the retry. The pen's desktop position must map onto the new
        // virtual screen, not the one it was queued under.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        let wider = VirtualScreen {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        };
        device.slot.api.screen = wider;
        device.slot.api.mouse_limit = usize::MAX;
        flush_one(&mut device, start);
        let (x, y) = super::super::desktop_pixel_to_abs(10, 1, wider);
        assert_ne!((x, y), abs(10, 1));
        assert_eq!(
            device.slot.api.mouse,
            [Injection::MoveAbs { x, y }, Injection::LeftDown { x, y }]
        );
    }

    #[test]
    fn refused_mouse_input_follows_the_captured_display_when_it_moves() {
        // Sol's case: SendInput refuses a tap at the centre of a 2360x1640
        // display right of the primary, and the display moves to the
        // primary's left before the retry. The tap must land on the display
        // where it is now, not on the primary's right edge.
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
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        device.slot.api.screen = VirtualScreen {
            left: 0,
            top: 0,
            width: 4920,
            height: 1640,
        };
        device.slot.api.mouse_limit = 0;
        let start = Instant::now();
        let (x, y) = super::super::norm_to_output_pixel(32_768, 32_768, before);
        assert_eq!((x, y), (3739, 819));
        for phase in [PenPhase::Down, PenPhase::Up] {
            let Injection::Pen(sample) = pen_xy(phase, x, y) else {
                unreachable!()
            };
            let sample = PenSample {
                x_norm: 32_768,
                y_norm: 32_768,
                ..sample
            };
            device.deliver(sample, device.slot.api.screen, Some(before), start);
        }
        let moved = VirtualScreen {
            left: -2360,
            top: 0,
            width: 4920,
            height: 1640,
        };
        device.slot.api.screen = moved;
        device.slot.api.mouse_limit = usize::MAX;
        device.flush(Some(after), start + Duration::from_millis(20));
        let (x, y) = desktop_pixel_to_abs(-1181, 819, moved);
        assert_eq!(
            device.slot.api.mouse,
            [
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn expired_input_is_never_inserted_when_insertion_recovers() {
        // Sol's case: SendInput refuses a stroke, then the next tick comes
        // two seconds later (sleep or load) with insertion working again.
        // Only a release owed for a button Windows holds may be inserted.
        let (x, y) = abs(10, 1);
        for press_inserted in [false, true] {
            let mut device = PenDevice::new(FakeApi::new(&[false]));
            let start = Instant::now();
            device.slot.api.mouse_limit = if press_inserted { 2 } else { 0 };
            deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
            device.slot.api.mouse_limit = 0;
            deliver_one(&mut device, pen_xy(PenPhase::Up, 12, 1), start);
            let inserted_before = std::mem::take(&mut device.slot.api.mouse);
            device.slot.api.mouse_limit = usize::MAX;
            flush_one(&mut device, start + INPUT_STALE * 2);
            let owed: &[Injection] = if press_inserted {
                &[Injection::LeftUp { x, y }]
            } else {
                &[]
            };
            assert_eq!(
                device.slot.api.mouse, owed,
                "press inserted: {press_inserted}, before: {inserted_before:?}"
            );
            assert!(!device.slot.api.button);
        }
    }
}
