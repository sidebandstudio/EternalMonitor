//! Input relay: turns the client's INPUT_EVENT messages (normalized
//! coordinates over the displayed video) into Windows input injection.
//!
//! Coordinate mapping, stroke state, deduplication and wheel scaling are
//! portable and unit-tested. Native pen and `SendInput`
//! calls live in `windows_inject.rs`; other platforms record events for the
//! end-to-end tests instead.

use eternal_wire::v2::control::InputEvent;
mod hid;
pub use hid::hid_to_scancode;

#[cfg(windows)]
pub mod windows_inject;

#[cfg(any(windows, test))]
mod pen_contact;
#[cfg(any(windows, test))]
mod pen_device;
pub mod privilege;
#[cfg(any(windows, test))]
mod probe_guard;

/// Desktop-space rectangle of the captured output (from DXGI's
/// `DesktopCoordinates`) — the target space for absolute pointer mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureGeometry {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// The whole Windows virtual screen (all monitors' bounding box); absolute
/// `SendInput` coordinates are normalized 0..65535 over THIS rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualScreen {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// Input kinds on the wire (`InputEvent.kind`).
pub const KIND_TOUCH: u8 = 0;
pub const KIND_PENCIL: u8 = 1;
pub const KIND_MOUSE_ABS: u8 = 2;
pub const KIND_SCROLL: u8 = 3;
pub const KIND_KEY: u8 = 4;
pub const KIND_TEXT: u8 = 5;
pub const KIND_HOVER: u8 = 6;

/// Phases (`InputEvent.phase`).
pub const PHASE_BEGAN: u8 = 0;
pub const PHASE_MOVED: u8 = 1;
pub const PHASE_ENDED: u8 = 2;
pub const PHASE_CANCELLED: u8 = 3;

/// A fully-resolved injection command, ready for the Windows backend (or a test
/// recorder off Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injection {
    /// Native pen coordinates are desktop pixels, not SendInput's 0..65535 space.
    Pen(PenSample),
    KeyDown {
        scan: u16,
        extended: bool,
    },
    KeyUp {
        scan: u16,
        extended: bool,
    },
    Unicode(u16),
    /// Absolute pointer move in virtual-screen normalized space (0..65535).
    MoveAbs {
        x: u16,
        y: u16,
    },
    LeftDown {
        x: u16,
        y: u16,
    },
    LeftUp {
        x: u16,
        y: u16,
    },
    RightDown {
        x: u16,
        y: u16,
    },
    RightUp {
        x: u16,
        y: u16,
    },
    MiddleDown {
        x: u16,
        y: u16,
    },
    MiddleUp {
        x: u16,
        y: u16,
    },
    /// Vertical wheel in WHEEL_DELTA units (positive = away from user).
    Wheel {
        delta: i32,
    },
    /// Horizontal wheel.
    HWheel {
        delta: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPhase {
    Down,
    Move,
    Up,
    Cancel,
    Hover,
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PenSample {
    pub phase: PenPhase,
    pub x: i32,
    pub y: i32,
    /// The wire position `x` and `y` were mapped from, so a sample injected
    /// later can be mapped again onto the capture rect current by then.
    pub x_norm: u16,
    pub y_norm: u16,
    pub pressure: u32,
    pub tilt_x: i32,
    pub tilt_y: i32,
}

/// Map a wire-normalized point (0..65535 over the displayed video, which the
/// client letterbox-corrects before sending) to a desktop pixel on the
/// captured output.
pub fn norm_to_output_pixel(x_norm: u16, y_norm: u16, output: CaptureGeometry) -> (i32, i32) {
    let x = output.left
        + ((u64::from(x_norm) * u64::from(output.width.saturating_sub(1))) / 65_535) as i32;
    let y = output.top
        + ((u64::from(y_norm) * u64::from(output.height.saturating_sub(1))) / 65_535) as i32;
    (x, y)
}

/// Map a desktop pixel to `SendInput`'s absolute virtual-screen space
/// (0..65535 across the whole virtual desktop, MOUSEEVENTF_VIRTUALDESK).
///
/// Assumed landing model: Windows resolves `abs` to
/// `origin + floor(abs * span / 65536)`. That is the documented 0..65535
/// normalisation over the virtual desktop with truncation, the model the
/// probe guard uses, and it matched every pixel `Test-PenMapping.ps1`
/// measured on 5,360 to 8,092 px wide desktops. Pixel `rel` therefore owns
/// the abs interval `[ceil(rel * 65536 / span), ceil((rel + 1) * 65536 / span))`.
/// Aim at the pixel centre, which survives a rounding (rather than
/// truncating) driver too, but clamp it into that interval: on desktops wider
/// than 32,768 px the centre alone can fall below it. Desktops wider than
/// 65,536 px leave some pixels unreachable; those take the nearest one.
pub fn desktop_pixel_to_abs(x: i32, y: i32, screen: VirtualScreen) -> (u16, u16) {
    let clamp_span = |value: i32, origin: i32, span: u32| -> u16 {
        let span = i64::from(span.max(1));
        let rel = (i64::from(value) - i64::from(origin)).clamp(0, span - 1);
        let ceil_div = |n: i64| (n + span - 1) / span;
        let first = ceil_div(rel * 65_536);
        let last = (ceil_div((rel + 1) * 65_536) - 1).max(first);
        let centre = (2 * rel + 1) * 65_536 / (2 * span);
        centre.clamp(first, last).min(65_535) as u16
    };
    (
        clamp_span(x, screen.left, screen.width),
        clamp_span(y, screen.top, screen.height),
    )
}

/// Map a desktop pixel to `InjectSyntheticPointerInput`'s location space.
/// That API measures `ptPixelLocation` from the virtual screen's top-left,
/// not from the primary monitor's origin, so a monitor left of or above the
/// primary would otherwise shift every pen sample by the virtual screen's
/// negative origin, or push it off the desktop entirely.
pub fn desktop_pixel_to_pointer_location(x: i32, y: i32, screen: VirtualScreen) -> (i32, i32) {
    (x.saturating_sub(screen.left), y.saturating_sub(screen.top))
}

/// Where Pencil samples go on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPath {
    /// Windows Ink synthetic pen device.
    Native,
    /// `SendInput` mouse events, when no pen device could be created.
    Mouse,
    /// The off-Windows test recorder.
    Recorder,
}

/// Everything that decides where an injected pointer lands. Logged once per
/// session and whenever it changes, so field reports show the actual layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputMapping {
    pub output: CaptureGeometry,
    pub screen: VirtualScreen,
    pub dpi_awareness: &'static str,
    pub pen_path: PenPath,
}

/// One pixel of client scroll → wheel delta. Apple reports pixel-ish deltas;
/// Windows wheels tick in 120s. 120/40 ≈ three-pixels-per-tick feels natural.
pub fn scroll_to_wheel(delta_px: i16) -> i32 {
    i32::from(delta_px) * 3
}

/// Number of recent edge ids remembered. The client sends each press/release
/// twice for loss tolerance, and Wi-Fi aggregation reorders freely, so the two
/// copies of one edge routinely arrive with other edges between them. A single
/// remembered id could not survive that: began(N), ended(N+1), began(N) walked
/// the slot N -> N+1 -> N and let the replayed press through, injecting a
/// double click where the user tapped once.
const RECENT_EDGE_IDS: usize = 8;

/// Drops duplicate began/ended events while letting distinct events through.
#[derive(Debug, Default)]
pub struct EventDeduper {
    recent: [Option<u32>; RECENT_EDGE_IDS],
    next: usize,
}

impl EventDeduper {
    /// True if the event should be processed.
    pub fn accept(&mut self, event: &InputEvent) -> bool {
        match event.phase {
            PHASE_BEGAN | PHASE_ENDED | PHASE_CANCELLED => {
                if self.recent.contains(&Some(event.event_id)) {
                    return false;
                }
                self.recent[self.next] = Some(event.event_id);
                self.next = (self.next + 1) % RECENT_EDGE_IDS;
                true
            }
            // Moves are idempotent-ish; duplicates are harmless.
            _ => true,
        }
    }
}

/// Resolve one wire event into zero or more injections.
pub fn resolve(
    event: &InputEvent,
    output: CaptureGeometry,
    screen: VirtualScreen,
) -> Vec<Injection> {
    if event.input_ver != 1 && !(event.input_ver == 2 && event.kind == KIND_PENCIL) {
        return Vec::new();
    }
    let (px, py) = norm_to_output_pixel(event.x_norm, event.y_norm, output);
    let (ax, ay) = desktop_pixel_to_abs(px, py, screen);

    match event.kind {
        KIND_PENCIL => {
            let contact = event.input_ver == 1 || event.buttons & 1 != 0;
            let phase = match (event.phase, contact) {
                (PHASE_BEGAN, true) => PenPhase::Down,
                (PHASE_MOVED, true) => PenPhase::Move,
                (PHASE_ENDED, true) => PenPhase::Up,
                (PHASE_CANCELLED, true) => PenPhase::Cancel,
                (PHASE_MOVED, false) => PenPhase::Hover,
                (PHASE_ENDED | PHASE_CANCELLED, false) => PenPhase::Leave,
                _ => return Vec::new(),
            };
            vec![Injection::Pen(PenSample {
                phase,
                x: px,
                y: py,
                x_norm: event.x_norm,
                y_norm: event.y_norm,
                pressure: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                    (u32::from(event.pressure_x1000.min(1000)) * 1024 + 500) / 1000
                } else {
                    0
                },
                tilt_x: i32::from(event.tilt_x.clamp(-90, 90)),
                tilt_y: i32::from(event.tilt_y.clamp(-90, 90)),
            })]
        }
        KIND_TOUCH | KIND_MOUSE_ABS => {
            let right_button = event.buttons & 0b10 != 0;
            let middle_button = event.buttons & 0b100 != 0;
            match event.phase {
                PHASE_BEGAN => {
                    if middle_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::MiddleDown { x: ax, y: ay },
                        ]
                    } else if right_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::RightDown { x: ax, y: ay },
                        ]
                    } else {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::LeftDown { x: ax, y: ay },
                        ]
                    }
                }
                PHASE_MOVED => vec![Injection::MoveAbs { x: ax, y: ay }],
                PHASE_ENDED | PHASE_CANCELLED => {
                    if middle_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::MiddleUp { x: ax, y: ay },
                        ]
                    } else if right_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::RightUp { x: ax, y: ay },
                        ]
                    } else {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::LeftUp { x: ax, y: ay },
                        ]
                    }
                }
                _ => Vec::new(),
            }
        }
        KIND_HOVER if event.phase == PHASE_MOVED => vec![Injection::MoveAbs { x: ax, y: ay }],
        KIND_KEY => {
            let Some((scan, extended)) = hid_to_scancode(event.keycode) else {
                return Vec::new();
            };
            match event.phase {
                PHASE_BEGAN => vec![Injection::KeyDown { scan, extended }],
                PHASE_ENDED | PHASE_CANCELLED => vec![Injection::KeyUp { scan, extended }],
                _ => Vec::new(),
            }
        }
        KIND_TEXT if event.phase == PHASE_BEGAN => vec![Injection::Unicode(event.keycode)],
        KIND_SCROLL => {
            let mut injections = vec![Injection::MoveAbs { x: ax, y: ay }];
            if event.scroll_dy != 0 {
                injections.push(Injection::Wheel {
                    delta: scroll_to_wheel(event.scroll_dy),
                });
            }
            if event.scroll_dx != 0 {
                injections.push(Injection::HWheel {
                    delta: scroll_to_wheel(event.scroll_dx),
                });
            }
            injections
        }
        _ => Vec::new(),
    }
}

/// Off-Windows sink: records what WOULD be injected so the end-to-end tests
/// can assert the full wire→mapping path. Windows injects for real.
#[cfg(not(windows))]
pub mod recorder {
    use super::Injection;

    /// The end-to-end tests record on the transport's thread and read on
    /// their own. Unit tests run in parallel, so each keeps its own record.
    fn recorded<R>(f: impl FnOnce(&mut Vec<Injection>) -> R) -> R {
        #[cfg(not(test))]
        {
            static RECORDED: std::sync::Mutex<Vec<Injection>> = std::sync::Mutex::new(Vec::new());
            f(&mut RECORDED.lock().unwrap())
        }
        #[cfg(test)]
        {
            thread_local! {
                static RECORDED: std::cell::RefCell<Vec<Injection>> =
                    const { std::cell::RefCell::new(Vec::new()) };
            }
            RECORDED.with_borrow_mut(f)
        }
    }

    pub fn record(injections: &[Injection]) {
        recorded(|recorded| recorded.extend_from_slice(injections));
    }

    pub fn take() -> Vec<Injection> {
        recorded(std::mem::take)
    }

    pub fn peek() -> Vec<Injection> {
        recorded(|recorded| recorded.clone())
    }
}

fn log_injections(injections: &[Injection]) {
    if std::env::var("ETERNAL_INPUT_RECORDER_LOG").is_ok_and(|value| value == "1") {
        for injection in injections {
            tracing::info!(command = ?injection, "Input injection");
        }
    }
}

/// The transport's per-connection relay state: an edge deduper scoped to the
/// current session (a superseded session restarts its `event_id` counter, so
/// dedupe state must not leak across sessions).
#[derive(Debug, Default)]
pub struct InputRelay {
    session: Option<(u32, EventDeduper)>,
    held: HeldInputs,
    pen_sequence: Option<u32>,
    last_pen_input: Option<std::time::Instant>,
    /// The capture rect the last batch was mapped onto.
    output: Option<CaptureGeometry>,
    logged_mapping: Option<InputMapping>,
    #[cfg(windows)]
    pen_device: windows_inject::PenDevice,
}

#[derive(Debug, Default)]
struct HeldInputs {
    keys: std::collections::BTreeSet<(u16, bool)>,
    buttons: u8,
    pointer: (u16, u16),
    pen: Option<PenSample>,
}

impl HeldInputs {
    fn record(&mut self, injections: &[Injection]) {
        for injection in injections {
            match *injection {
                Injection::Pen(sample) => {
                    self.pen = if matches!(
                        sample.phase,
                        PenPhase::Up | PenPhase::Leave | PenPhase::Cancel
                    ) {
                        None
                    } else {
                        Some(sample)
                    };
                }
                Injection::KeyDown { scan, extended } => {
                    self.keys.insert((scan, extended));
                }
                Injection::KeyUp { scan, extended } => {
                    self.keys.remove(&(scan, extended));
                }
                Injection::MoveAbs { x, y } => self.pointer = (x, y),
                Injection::LeftDown { .. } => self.buttons |= 1,
                Injection::LeftUp { .. } => self.buttons &= !1,
                Injection::RightDown { .. } => self.buttons |= 2,
                Injection::RightUp { .. } => self.buttons &= !2,
                Injection::MiddleDown { .. } => self.buttons |= 4,
                Injection::MiddleUp { .. } => self.buttons &= !4,
                _ => {}
            }
        }
    }

    fn release(&mut self) -> Vec<Injection> {
        let mut releases: Vec<_> = std::mem::take(&mut self.keys)
            .into_iter()
            .map(|(scan, extended)| Injection::KeyUp { scan, extended })
            .collect();
        let (x, y) = self.pointer;
        if self.buttons & 1 != 0 {
            releases.push(Injection::LeftUp { x, y });
        }
        if self.buttons & 2 != 0 {
            releases.push(Injection::RightUp { x, y });
        }
        if self.buttons & 4 != 0 {
            releases.push(Injection::MiddleUp { x, y });
        }
        self.buttons = 0;
        if let Some(mut sample) = self.pen.take() {
            sample.phase = if matches!(sample.phase, PenPhase::Down | PenPhase::Move) {
                PenPhase::Cancel
            } else {
                PenPhase::Leave
            };
            sample.pressure = 0;
            releases.push(Injection::Pen(sample));
        }
        releases
    }
}

impl InputRelay {
    pub fn pen_available(&self) -> bool {
        #[cfg(windows)]
        {
            self.pen_device.available()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Whether the transport's refresh tick has pen work: a native device to
    /// keep alive, or mouse fallback input Windows refused.
    pub fn pen_tick_needed(&self) -> bool {
        #[cfg(windows)]
        {
            self.pen_device.available() || self.pen_device.mouse_waiting()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub fn pen_path(&self) -> PenPath {
        if cfg!(windows) {
            if self.pen_available() {
                PenPath::Native
            } else {
                PenPath::Mouse
            }
        } else {
            PenPath::Recorder
        }
    }

    fn inject(&mut self, injections: &[Injection]) {
        #[cfg(windows)]
        windows_inject::inject(injections, &mut self.pen_device, self.output);
        #[cfg(not(windows))]
        {
            recorder::record(injections);
            log_injections(injections);
        }
    }

    /// Release keys/buttons when a session ends, expires, or is superseded.
    pub fn reset(&mut self) {
        let releases = self.held.release();
        // The pen device ends only the pointer Windows holds, natively or on
        // the mouse fallback, and drops samples Windows never received,
        // which a release would queue behind.
        #[cfg(windows)]
        let releases: Vec<_> = {
            self.pen_device.reset(std::time::Instant::now());
            releases
                .into_iter()
                .filter(|injection| !matches!(injection, Injection::Pen(_)))
                .collect()
        };
        self.inject(&releases);
        self.session = None;
        self.pen_sequence = None;
        self.last_pen_input = None;
        self.logged_mapping = None;
    }

    fn log_mapping(&mut self, session_id: u32, output: CaptureGeometry, screen: VirtualScreen) {
        #[cfg(windows)]
        let dpi_awareness = windows_inject::dpi_awareness();
        #[cfg(not(windows))]
        let dpi_awareness = "not_windows";
        let mapping = InputMapping {
            output,
            screen,
            dpi_awareness,
            pen_path: self.pen_path(),
        };
        if self.logged_mapping == Some(mapping) {
            return;
        }
        self.logged_mapping = Some(mapping);
        let (offset_x, offset_y) =
            desktop_pixel_to_pointer_location(output.left, output.top, screen);
        tracing::info!(
            session_id,
            output_left = output.left,
            output_top = output.top,
            output_width = output.width,
            output_height = output.height,
            virtual_left = screen.left,
            virtual_top = screen.top,
            virtual_width = screen.width,
            virtual_height = screen.height,
            pen_origin_x = offset_x,
            pen_origin_y = offset_y,
            dpi_awareness,
            pen_path = ?mapping.pen_path,
            "Input mapping geometry"
        );
    }

    /// Windows expires stationary synthetic contacts unless they are refreshed.
    /// Real samples always win; never keep a released or disconnected tip down.
    /// `output` is the current capture rect: a held contact follows it if the
    /// display moved or changed mode since the Pencil last moved.
    pub fn keep_pen_alive(&mut self, output: Option<CaptureGeometry>) {
        #[cfg(windows)]
        self.pen_device
            .flush(output.or(self.output), std::time::Instant::now());
        let Some(output) = output else {
            return;
        };
        if let Some(refresh) = self.pen_refresh(std::time::Instant::now(), output) {
            let screen = current_virtual_screen(output);
            if let Some((session_id, _)) = self.session {
                self.log_mapping(session_id, output, screen);
            }
            self.output = Some(output);
            self.inject(&[refresh]);
        }
    }

    fn pen_refresh(
        &mut self,
        now: std::time::Instant,
        output: CaptureGeometry,
    ) -> Option<Injection> {
        let last = self.last_pen_input?;
        if now.saturating_duration_since(last) < std::time::Duration::from_millis(50) {
            return None;
        }
        let mut sample = self.held.pen?;
        sample.phase = match sample.phase {
            PenPhase::Down | PenPhase::Move => PenPhase::Move,
            PenPhase::Hover => PenPhase::Hover,
            _ => return None,
        };
        // Follow the display if it moved or changed mode under a still Pencil.
        (sample.x, sample.y) = norm_to_output_pixel(sample.x_norm, sample.y_norm, output);
        self.held.record(&[Injection::Pen(sample)]);
        self.last_pen_input = Some(now);
        Some(Injection::Pen(sample))
    }

    /// Dedupe, map, and inject one session-validated event.
    pub fn relay(&mut self, session_id: u32, event: &InputEvent, output: CaptureGeometry) {
        match &self.session {
            Some((id, _)) if *id == session_id => {}
            _ => {
                self.reset();
                self.session = Some((session_id, EventDeduper::default()));
            }
        }
        // Pen updates are ordered samples. Replayed or reordered UDP packets
        // must never move a stroke backwards or revive a released contact.
        if event.kind == KIND_PENCIL {
            if !matches!(event.input_ver, 1 | 2) || event.phase > PHASE_CANCELLED {
                return;
            }
            if self
                .pen_sequence
                .is_some_and(|last| event.event_id.wrapping_sub(last) as i32 <= 0)
            {
                return;
            }
            self.pen_sequence = Some(event.event_id);
        }
        let deduper = &mut self.session.as_mut().expect("just ensured").1;
        if !deduper.accept(event) {
            return;
        }
        let screen = current_virtual_screen(output);
        self.log_mapping(session_id, output, screen);
        self.output = Some(output);
        let mut injections = resolve(event, output, screen);
        if let Some(Injection::Pen(sample)) = injections.first().copied() {
            let held = self
                .held
                .pen
                .filter(|p| matches!(p.phase, PenPhase::Down | PenPhase::Move));
            match (sample.phase, held) {
                (PenPhase::Down, Some(mut previous)) => {
                    previous.phase = PenPhase::Cancel;
                    previous.pressure = 0;
                    injections.insert(0, Injection::Pen(previous));
                }
                // A lost UDP down can recover at the first actual sample.
                (PenPhase::Move, None) => {
                    injections[0] = Injection::Pen(PenSample {
                        phase: PenPhase::Down,
                        ..sample
                    })
                }
                (PenPhase::Up | PenPhase::Cancel, None) => return,
                (PenPhase::Up, Some(previous))
                    if (sample.x, sample.y) != (previous.x, previous.y) =>
                {
                    // Windows requires up at the last contact position. Carry
                    // a final position update first when the tip lifted
                    // between UIKit's last moved and ended callbacks.
                    injections.insert(
                        0,
                        Injection::Pen(PenSample {
                            phase: PenPhase::Move,
                            pressure: previous.pressure,
                            ..sample
                        }),
                    );
                }
                (PenPhase::Cancel, Some(previous)) => {
                    injections[0] = Injection::Pen(PenSample {
                        phase: PenPhase::Cancel,
                        pressure: 0,
                        ..previous
                    });
                }
                (PenPhase::Hover | PenPhase::Leave, Some(mut previous)) => {
                    previous.phase = PenPhase::Cancel;
                    previous.pressure = 0;
                    injections.insert(0, Injection::Pen(previous));
                }
                _ => {}
            }
        }
        self.held.record(&injections);
        if event.kind == KIND_PENCIL {
            self.last_pen_input = Some(std::time::Instant::now());
        }
        self.inject(&injections);
    }
}

impl Drop for InputRelay {
    fn drop(&mut self) {
        self.reset();
    }
}

/// The absolute space injections land in. Windows asks the OS for the live
/// multi-monitor bounding box; other platforms (the test recorder) treat the
/// captured output as the whole screen.
fn current_virtual_screen(output: CaptureGeometry) -> VirtualScreen {
    #[cfg(windows)]
    {
        let _ = output;
        windows_inject::virtual_screen()
    }
    #[cfg(not(windows))]
    VirtualScreen {
        left: output.left,
        top: output.top,
        width: output.width,
        height: output.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u8, phase: u8, x: u16, y: u16) -> InputEvent {
        InputEvent {
            input_ver: 1,
            kind,
            phase,
            buttons: 1,
            event_id: 1,
            x_norm: x,
            y_norm: y,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: 0,
            modifiers: 0,
            client_time_us: 0,
            tilt_x: 0,
            tilt_y: 0,
        }
    }

    const OUTPUT: CaptureGeometry = CaptureGeometry {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };
    const SINGLE_SCREEN: VirtualScreen = VirtualScreen {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn corners_and_center_map_correctly() {
        assert_eq!(norm_to_output_pixel(0, 0, OUTPUT), (0, 0));
        assert_eq!(norm_to_output_pixel(65_535, 65_535, OUTPUT), (1919, 1079));
        let (cx, cy) = norm_to_output_pixel(32_768, 32_768, OUTPUT);
        assert!((cx - 960).abs() <= 1, "center x {cx}");
        assert!((cy - 540).abs() <= 1, "center y {cy}");
    }

    #[test]
    fn extended_display_offset_is_applied() {
        // Virtual display sits LEFT of the primary at -1920.
        let extended = CaptureGeometry {
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        let screen = VirtualScreen {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        };
        // Touch at the extended display's center lands in the LEFT half of the
        // virtual screen.
        let (px, py) = norm_to_output_pixel(32_768, 32_768, extended);
        assert!((px - -960).abs() <= 1, "pixel x {px}");
        let (ax, _ay) = desktop_pixel_to_abs(px, py, screen);
        // The left display's center = 25% of the way across the dual-monitor
        // virtual screen (one pixel ≈ 17 abs units; allow ±2 px of rounding).
        assert!(
            (i32::from(ax) - 16_384).abs() <= 40,
            "abs x {ax} should be ~16384"
        );
    }

    #[test]
    fn tap_produces_move_then_click_edges() {
        let began = resolve(
            &event(KIND_TOUCH, PHASE_BEGAN, 100, 100),
            OUTPUT,
            SINGLE_SCREEN,
        );
        assert!(matches!(began[0], Injection::MoveAbs { .. }));
        assert!(matches!(began[1], Injection::LeftDown { .. }));

        let ended = resolve(
            &event(KIND_TOUCH, PHASE_ENDED, 100, 100),
            OUTPUT,
            SINGLE_SCREEN,
        );
        assert!(matches!(ended[1], Injection::LeftUp { .. }));
    }

    #[test]
    fn right_button_flag_makes_right_clicks() {
        let mut e = event(KIND_TOUCH, PHASE_BEGAN, 5, 5);
        e.buttons = 0b10;
        let injections = resolve(&e, OUTPUT, SINGLE_SCREEN);
        assert!(matches!(injections[1], Injection::RightDown { .. }));
    }

    #[test]
    fn scroll_maps_to_wheel_deltas() {
        let mut e = event(KIND_SCROLL, PHASE_MOVED, 0, 0);
        e.scroll_dy = -40;
        e.scroll_dx = 10;
        let injections = resolve(&e, OUTPUT, SINGLE_SCREEN);
        assert!(injections.contains(&Injection::Wheel { delta: -120 }));
        assert!(injections.contains(&Injection::HWheel { delta: 30 }));
    }

    #[test]
    fn pen_preserves_pressure_tilt_and_desktop_pixels_without_mouse_events() {
        let mut e = event(KIND_PENCIL, PHASE_BEGAN, 65_535, 0);
        e.input_ver = 2;
        e.pressure_x1000 = 750;
        e.tilt_x = -37;
        e.tilt_y = 62;
        let output = CaptureGeometry {
            left: -2560,
            top: -200,
            width: 2560,
            height: 1440,
        };
        assert_eq!(
            resolve(&e, output, SINGLE_SCREEN),
            vec![Injection::Pen(PenSample {
                phase: PenPhase::Down,
                x: -1,
                y: -200,
                x_norm: 65_535,
                y_norm: 0,
                pressure: 768,
                tilt_x: -37,
                tilt_y: 62,
            })]
        );
        e.phase = PHASE_MOVED;
        e.pressure_x1000 = u16::MAX;
        e.tilt_x = i16::MIN;
        e.tilt_y = i16::MAX;
        let [Injection::Pen(sample)] = resolve(&e, output, SINGLE_SCREEN)[..] else {
            panic!()
        };
        assert_eq!(
            (sample.pressure, sample.tilt_x, sample.tilt_y),
            (1024, -90, 90)
        );
        for (phase, buttons, expected) in [
            (PHASE_ENDED, 1, PenPhase::Up),
            (PHASE_CANCELLED, 1, PenPhase::Cancel),
            (PHASE_MOVED, 0, PenPhase::Hover),
            (PHASE_ENDED, 0, PenPhase::Leave),
        ] {
            e.phase = phase;
            e.buttons = buttons;
            let [Injection::Pen(sample)] = resolve(&e, output, SINGLE_SCREEN)[..] else {
                panic!()
            };
            assert_eq!((sample.phase, sample.pressure), (expected, 0));
        }
        e.input_ver = 99;
        assert!(resolve(&e, output, SINGLE_SCREEN).is_empty());
        assert_eq!(
            norm_to_output_pixel(
                65535,
                65535,
                CaptureGeometry {
                    width: 1,
                    height: 1,
                    ..output
                }
            ),
            (-2560, -200)
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn relay_resets_dedupe_on_session_change() {
        let mut relay = InputRelay::default();
        let mut e = event(KIND_TOUCH, PHASE_BEGAN, 10, 10);
        e.event_id = 1;
        relay.relay(7, &e, OUTPUT); // injected (MoveAbs + LeftDown)
        relay.relay(7, &e, OUTPUT); // duplicate edge: dropped
        relay.relay(8, &e, OUTPUT); // new session, same event_id: injected
        let recorded = recorder::take();
        assert_eq!(recorded.len(), 5);
        assert!(matches!(recorded[2], Injection::LeftUp { .. }));
        // Empty the release that Drop correctly injects for session 8 too.
        drop(relay);
        assert!(matches!(
            recorder::take().as_slice(),
            [Injection::LeftUp { .. }]
        ));

        let mut relay = InputRelay::default();
        let mut pen = event(KIND_PENCIL, PHASE_BEGAN, 100, 100);
        pen.input_ver = 2;
        pen.event_id = u32::MAX - 1;
        pen.pressure_x1000 = 500;
        relay.relay(9, &pen, OUTPUT);
        relay.relay(9, &pen, OUTPUT);
        pen.phase = PHASE_MOVED;
        pen.event_id = u32::MAX;
        relay.relay(9, &pen, OUTPUT);
        pen.event_id = 0; // wrapping IDs still advance
        pen.phase = PHASE_ENDED;
        relay.relay(9, &pen, OUTPUT);
        pen.event_id = u32::MAX;
        pen.phase = PHASE_MOVED;
        relay.relay(9, &pen, OUTPUT); // delayed sample cannot revive the stroke
        let phases: Vec<_> = recorder::take()
            .iter()
            .map(|i| match i {
                Injection::Pen(p) => p.phase,
                _ => panic!("Pencil became mouse input"),
            })
            .collect();
        assert_eq!(phases, [PenPhase::Down, PenPhase::Move, PenPhase::Up]);
        pen.event_id = 1;
        relay.relay(9, &pen, OUTPUT); // lost down recovers
        assert!(matches!(
            recorder::take().as_slice(),
            [Injection::Pen(PenSample {
                phase: PenPhase::Down,
                ..
            })]
        ));
        pen.event_id = 2;
        pen.phase = PHASE_BEGAN;
        relay.relay(9, &pen, OUTPUT); // new stroke after lost up cancels old one
        assert!(matches!(
            recorder::take().as_slice(),
            [
                Injection::Pen(PenSample {
                    phase: PenPhase::Cancel,
                    ..
                }),
                Injection::Pen(PenSample {
                    phase: PenPhase::Down,
                    ..
                })
            ]
        ));
        relay.reset();
        assert!(matches!(
            recorder::take().as_slice(),
            [Injection::Pen(PenSample {
                phase: PenPhase::Cancel,
                pressure: 0,
                ..
            })]
        ));
        relay.reset();
        assert!(recorder::take().is_empty());

        pen.event_id = 3;
        relay.relay(10, &pen, OUTPUT);
        let now = relay.last_pen_input.unwrap();
        assert!(relay
            .pen_refresh(now + std::time::Duration::from_millis(49), OUTPUT)
            .is_none());
        assert!(matches!(
            relay.pen_refresh(now + std::time::Duration::from_millis(50), OUTPUT),
            Some(Injection::Pen(PenSample {
                phase: PenPhase::Move,
                ..
            }))
        ));
        recorder::take();
        pen.event_id = 4;
        pen.phase = PHASE_ENDED;
        pen.x_norm = 200;
        relay.relay(10, &pen, OUTPUT);
        assert!(matches!(
            recorder::take().as_slice(),
            [
                Injection::Pen(PenSample {
                    phase: PenPhase::Move,
                    ..
                }),
                Injection::Pen(PenSample {
                    phase: PenPhase::Up,
                    ..
                })
            ]
        ));
        assert!(relay
            .pen_refresh(now + std::time::Duration::from_secs(1), OUTPUT)
            .is_none());
        relay.reset();
        assert!(recorder::take().is_empty());
    }

    #[test]
    fn reordered_duplicate_edges_still_dedupe() {
        // The wire sends each edge twice; the network may interleave them.
        // began(7), ended(8), began(7), ended(8) must inject one press and
        // one release, not two of each.
        let mut dedupe = EventDeduper::default();
        let mut began = event(KIND_TOUCH, PHASE_BEGAN, 0, 0);
        began.event_id = 7;
        let mut ended = event(KIND_TOUCH, PHASE_ENDED, 0, 0);
        ended.event_id = 8;

        assert!(dedupe.accept(&began));
        assert!(dedupe.accept(&ended));
        assert!(
            !dedupe.accept(&began),
            "the replayed press must not click again"
        );
        assert!(!dedupe.accept(&ended));
    }

    #[test]
    fn edge_duplicates_are_dropped_moves_pass() {
        let mut dedupe = EventDeduper::default();
        let mut began = event(KIND_TOUCH, PHASE_BEGAN, 0, 0);
        began.event_id = 7;
        assert!(dedupe.accept(&began));
        assert!(!dedupe.accept(&began), "second edge with the same id drops");

        let mut moved = event(KIND_TOUCH, PHASE_MOVED, 1, 1);
        moved.event_id = 7;
        assert!(dedupe.accept(&moved));
        assert!(dedupe.accept(&moved), "moves are not deduped");

        let mut ended = event(KIND_TOUCH, PHASE_ENDED, 2, 2);
        ended.event_id = 8;
        assert!(dedupe.accept(&ended));
    }

    #[test]
    fn keyboard_text_hover_and_middle_resolve_without_extra_clicks() {
        let mut e = event(KIND_KEY, PHASE_BEGAN, 0, 0);
        e.keycode = 0x4f;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::KeyDown {
                scan: 0x4d,
                extended: true
            }]
        );
        e.phase = PHASE_CANCELLED;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::KeyUp {
                scan: 0x4d,
                extended: true
            }]
        );
        e.keycode = 0xffff;
        assert!(resolve(&e, OUTPUT, SINGLE_SCREEN).is_empty());
        e.kind = KIND_TEXT;
        e.phase = PHASE_BEGAN;
        e.keycode = 0xd83d;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::Unicode(0xd83d)]
        );
        e.phase = PHASE_ENDED;
        assert!(resolve(&e, OUTPUT, SINGLE_SCREEN).is_empty());
        e.kind = KIND_HOVER;
        e.phase = PHASE_MOVED;
        e.buttons = 7;
        let (x, y) = desktop_pixel_to_abs(0, 0, SINGLE_SCREEN);
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::MoveAbs { x, y }]
        );
        e.kind = KIND_MOUSE_ABS;
        e.buttons = 4;
        e.phase = PHASE_BEGAN;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN)[1],
            Injection::MiddleDown { x, y }
        );
        e.phase = PHASE_ENDED;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN)[1],
            Injection::MiddleUp { x, y }
        );
        e.kind = KIND_KEY;
        e.event_id = 99;
        let mut deduper = EventDeduper::default();
        assert!(deduper.accept(&e));
        assert!(!deduper.accept(&e));
    }

    #[test]
    fn held_inputs_release_once_after_disconnect() {
        let mut held = HeldInputs::default();
        held.record(&[
            Injection::KeyDown {
                scan: 0x1d,
                extended: false,
            },
            Injection::KeyDown {
                scan: 0x1d,
                extended: false,
            },
            Injection::KeyDown {
                scan: 0x2e,
                extended: false,
            },
            Injection::KeyUp {
                scan: 0x2e,
                extended: false,
            },
            Injection::MoveAbs { x: 123, y: 456 },
            Injection::MiddleDown { x: 123, y: 456 },
        ]);
        assert_eq!(
            held.release(),
            vec![
                Injection::KeyUp {
                    scan: 0x1d,
                    extended: false
                },
                Injection::MiddleUp { x: 123, y: 456 }
            ]
        );
        assert!(held.release().is_empty());
    }

    /// Windows' documented conversions, for asserting where input lands.
    /// `InjectSyntheticPointerInput` measures from the virtual screen's
    /// top-left; `SendInput` resolves `origin + abs * span / 65536`.
    fn windows_pen_landing(location: (i32, i32), screen: VirtualScreen) -> (i32, i32) {
        (screen.left + location.0, screen.top + location.1)
    }

    fn windows_mouse_landing(abs: (u16, u16), screen: VirtualScreen) -> (i32, i32) {
        (
            screen.left + (u64::from(abs.0) * u64::from(screen.width) / 65_536) as i32,
            screen.top + (u64::from(abs.1) * u64::from(screen.height) / 65_536) as i32,
        )
    }

    fn bounding(monitors: &[CaptureGeometry]) -> VirtualScreen {
        let left = monitors.iter().map(|m| m.left).min().unwrap();
        let top = monitors.iter().map(|m| m.top).min().unwrap();
        let right = monitors
            .iter()
            .map(|m| m.left + m.width as i32)
            .max()
            .unwrap();
        let bottom = monitors
            .iter()
            .map(|m| m.top + m.height as i32)
            .max()
            .unwrap();
        VirtualScreen {
            left,
            top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        }
    }

    const fn rect(left: i32, top: i32, width: u32, height: u32) -> CaptureGeometry {
        CaptureGeometry {
            left,
            top,
            width,
            height,
        }
    }

    /// (captured output, every monitor) in physical pixels.
    fn layouts() -> Vec<(&'static str, CaptureGeometry, Vec<CaptureGeometry>)> {
        let primary = rect(0, 0, 2560, 1440);
        vec![
            (
                "single primary",
                rect(0, 0, 1920, 1080),
                vec![rect(0, 0, 1920, 1080)],
            ),
            // The 2026-10-21 report: 2560x1440 primary at 125%, mirrored,
            // with a portrait 1080x1920 monitor to its left.
            (
                "mirror primary, portrait left",
                primary,
                vec![primary, rect(-1080, -240, 1080, 1920)],
            ),
            (
                "mirror primary, monitor above",
                primary,
                vec![primary, rect(0, -1080, 1920, 1080)],
            ),
            (
                "extend VDD left of primary",
                rect(-2360, 0, 2360, 1640),
                vec![primary, rect(-2360, 0, 2360, 1640)],
            ),
            (
                "extend VDD above, primary not at the virtual origin",
                rect(-400, -1640, 2360, 1640),
                vec![
                    primary,
                    rect(-400, -1640, 2360, 1640),
                    rect(2560, 200, 1920, 1080),
                ],
            ),
            (
                "extend VDD right, mixed DPI monitors",
                rect(5120, 0, 2360, 1640),
                vec![
                    rect(0, 0, 3840, 2160),
                    rect(3840, 300, 1280, 1024),
                    rect(5120, 0, 2360, 1640),
                ],
            ),
            (
                "mirror secondary at negative origin",
                rect(-1920, -300, 1920, 1080),
                vec![rect(-1920, -300, 1920, 1080), primary],
            ),
        ]
    }

    const CORNERS_AND_CENTRE: [(u16, u16); 5] = [
        (0, 0),
        (65_535, 0),
        (0, 65_535),
        (65_535, 65_535),
        (32_768, 32_768),
    ];

    #[test]
    fn pen_lands_under_the_tip_for_every_monitor_layout() {
        for (name, output, monitors) in layouts() {
            let screen = bounding(&monitors);
            for (nx, ny) in CORNERS_AND_CENTRE {
                let mut e = event(KIND_PENCIL, PHASE_BEGAN, nx, ny);
                e.input_ver = 2;
                let [Injection::Pen(sample)] = resolve(&e, output, screen)[..] else {
                    panic!("{name}: Pencil did not resolve to one pen sample")
                };
                let target = norm_to_output_pixel(nx, ny, output);
                assert_eq!((sample.x, sample.y), target, "{name}");
                let location = desktop_pixel_to_pointer_location(sample.x, sample.y, screen);
                assert!(
                    location.0 >= 0
                        && location.1 >= 0
                        && (location.0 as u32) < screen.width
                        && (location.1 as u32) < screen.height,
                    "{name}: {location:?} is outside the virtual screen"
                );
                assert_eq!(
                    windows_pen_landing(location, screen),
                    target,
                    "{name} at {nx},{ny}"
                );
            }
        }
    }

    #[test]
    fn desktop_pen_coordinates_miss_when_the_virtual_screen_is_not_at_the_origin() {
        // 0.3.1 passed desktop pixels straight to InjectSyntheticPointerInput.
        // With the customer's portrait monitor on the left, the top-left
        // corner of the mirrored primary landed 1080 px left, on the other
        // monitor, and the bottom-right fell off the desktop (E_INVALIDARG).
        let primary = rect(0, 0, 2560, 1440);
        let screen = bounding(&[primary, rect(-1080, -240, 1080, 1920)]);
        let old = |p: (i32, i32)| windows_pen_landing(p, screen);
        assert_eq!(old((0, 0)), (-1080, -240));
        assert_eq!(old((2559, 1439)), (1479, 1199));
        let fixed = |p: (i32, i32)| {
            windows_pen_landing(desktop_pixel_to_pointer_location(p.0, p.1, screen), screen)
        };
        assert_eq!(fixed((0, 0)), (0, 0));
        assert_eq!(fixed((2559, 1439)), (2559, 1439));
    }

    #[test]
    fn mouse_input_lands_on_the_exact_pixel_for_every_layout() {
        for (name, output, monitors) in layouts() {
            let screen = bounding(&monitors);
            for (nx, ny) in CORNERS_AND_CENTRE {
                let e = event(KIND_TOUCH, PHASE_BEGAN, nx, ny);
                let injections = resolve(&e, output, screen);
                let Injection::MoveAbs { x, y } = injections[0] else {
                    panic!("{name}: touch did not start with a move")
                };
                assert_eq!(
                    windows_mouse_landing((x, y), screen),
                    norm_to_output_pixel(nx, ny, output),
                    "{name} at {nx},{ny}"
                );
            }
            // Every pixel along both axes of the output survives the round
            // trip; the 0.3.1 edge-aimed formula landed one pixel short at
            // many of them (for example the second column of a 3640 px
            // virtual screen).
            for px in output.left..output.left + output.width as i32 {
                let abs = desktop_pixel_to_abs(px, output.top, screen);
                assert_eq!(windows_mouse_landing(abs, screen).0, px, "{name} x={px}");
            }
            for py in output.top..output.top + output.height as i32 {
                let abs = desktop_pixel_to_abs(output.left, py, screen);
                assert_eq!(windows_mouse_landing(abs, screen).1, py, "{name} y={py}");
            }
        }
        // The customer's virtual screen width: 0.3.1's edge-aimed formula
        // put many columns one pixel short.
        let span = 3640;
        let screen = VirtualScreen {
            left: -1080,
            top: 0,
            width: span as u32,
            height: 1,
        };
        let edge_aimed = |rel: i64| (rel * 65_535 / (span - 1)) as u16;
        let missed = (0..span)
            .filter(|&rel| {
                windows_mouse_landing((edge_aimed(rel), 0), screen).0 != rel as i32 - 1080
            })
            .count();
        assert!(missed > 100, "edge-aimed formula missed {missed} columns");
    }

    #[test]
    fn abs_round_trip_is_exact_on_every_representable_desktop_width() {
        // Exhaustive over each width, against the truncating landing model
        // documented on desktop_pixel_to_abs, with negative origins.
        let centre_only = |rel: i64, span: i64| ((2 * rel + 1) * 65_536 / (2 * span)).min(65_535);
        for (span, origin) in [
            (1u32, 0),
            (2, -1),
            (1920, 0),
            (3640, -1080),
            (8092, -2732),
            (32_767, -100),
            (32_768, -16_384),
            (32_769, 0),
            (38_400, -15_360),
            (60_000, -30_000),
            (65_535, -1),
            (65_536, -32_768),
        ] {
            let screen = VirtualScreen {
                left: origin,
                top: origin,
                width: span,
                height: span,
            };
            let mut centre_misses = 0;
            for rel in 0..i64::from(span) {
                let pixel = origin + rel as i32;
                let abs = desktop_pixel_to_abs(pixel, pixel, screen);
                assert_eq!(
                    windows_mouse_landing(abs, screen),
                    (pixel, pixel),
                    "span {span} origin {origin} pixel {pixel}"
                );
                let old = centre_only(rel, i64::from(span)) as u16;
                if windows_mouse_landing((old, old), screen).0 != pixel {
                    centre_misses += 1;
                }
            }
            if span == 38_400 {
                // The reviewer's case: pixel -15357 used to land on -15358.
                assert_eq!(centre_misses, 5_120);
                let abs = desktop_pixel_to_abs(-15_357, 0, screen);
                assert_eq!(windows_mouse_landing(abs, screen).0, -15_357);
            }
        }
        // Wider than 65,536 px: some pixels cannot be addressed; never more
        // than one pixel off, and still monotonic.
        let screen = VirtualScreen {
            left: -40_000,
            top: 0,
            width: 80_000,
            height: 1,
        };
        let mut previous = i32::MIN;
        for pixel in -40_000..40_000 {
            let landed = windows_mouse_landing(desktop_pixel_to_abs(pixel, 0, screen), screen).0;
            assert!((landed - pixel).abs() <= 1, "pixel {pixel} landed {landed}");
            assert!(landed >= previous);
            previous = landed;
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn keepalive_remaps_a_still_pencil_when_the_capture_rect_moves() {
        // The reviewer's case: a 2360x1640 output moves from left=2560 to
        // left=-2360 while the Pencil is held still.
        let before = rect(2560, 0, 2360, 1640);
        let after = rect(-2360, 0, 2360, 1640);
        let mut relay = InputRelay::default();
        let mut e = event(KIND_PENCIL, PHASE_BEGAN, 32_768, 32_768);
        e.input_ver = 2;
        e.pressure_x1000 = 500;
        relay.relay(1, &e, before);
        let [Injection::Pen(down)] = recorder::take()[..] else {
            panic!("expected one pen down")
        };
        assert_eq!((down.x, down.y), (3739, 819));
        // Stationary: only the refresh tick runs. It must follow the display
        // and log the new geometry without any input event.
        relay.last_pen_input =
            std::time::Instant::now().checked_sub(std::time::Duration::from_millis(60));
        relay.keep_pen_alive(Some(after));
        assert_eq!(
            recorder::take(),
            [Injection::Pen(PenSample {
                phase: PenPhase::Move,
                x: -1181,
                y: 819,
                ..down
            })]
        );
        assert_eq!(relay.logged_mapping.unwrap().output, after);
        // A disconnect now releases where the contact actually is.
        relay.reset();
        assert!(matches!(
            recorder::take().as_slice(),
            [Injection::Pen(PenSample {
                phase: PenPhase::Cancel,
                x: -1181,
                y: 819,
                ..
            })]
        ));
        // Without geometry the refresh does nothing rather than guess.
        let mut relay = InputRelay::default();
        relay.relay(2, &e, before);
        recorder::take();
        relay.last_pen_input =
            std::time::Instant::now().checked_sub(std::time::Duration::from_millis(60));
        relay.keep_pen_alive(None);
        assert!(recorder::take().is_empty());
        relay.reset();
        recorder::take();
    }

    #[cfg(not(windows))]
    #[test]
    fn a_moved_pencil_after_a_remap_lifts_where_it_is() {
        let before = rect(0, 0, 1920, 1080);
        let after = rect(-1920, 0, 1920, 1080);
        let mut relay = InputRelay::default();
        let mut e = event(KIND_PENCIL, PHASE_BEGAN, 0, 0);
        e.input_ver = 2;
        e.event_id = 1;
        relay.relay(1, &e, before);
        relay.last_pen_input =
            std::time::Instant::now().checked_sub(std::time::Duration::from_millis(60));
        relay.keep_pen_alive(Some(after));
        recorder::take();
        e.event_id = 2;
        e.phase = PHASE_ENDED;
        relay.relay(1, &e, after);
        let phases: Vec<_> = recorder::take()
            .into_iter()
            .map(|i| match i {
                Injection::Pen(p) => (p.phase, p.x, p.y),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(phases, [(PenPhase::Up, -1920, 0)]);
    }

    #[cfg(not(windows))]
    #[test]
    fn mapping_is_logged_per_session_and_on_geometry_change() {
        let mut relay = InputRelay::default();
        let mut e = event(KIND_HOVER, PHASE_MOVED, 100, 100);
        relay.relay(1, &e, OUTPUT);
        let first = relay.logged_mapping.expect("mapping logged");
        assert_eq!(first.output, OUTPUT);
        assert_eq!(first.pen_path, PenPath::Recorder);
        // A resolution change republishes the capture rect; the very next
        // event maps onto the new rect and logs it.
        let resized = rect(0, 0, 2360, 1640);
        e.x_norm = 65_535;
        relay.relay(1, &e, resized);
        assert_eq!(relay.logged_mapping.unwrap().output, resized);
        assert!(matches!(
            recorder::take().last(),
            Some(Injection::MoveAbs { .. })
        ));
        relay.reset();
        assert!(relay.logged_mapping.is_none(), "a new session logs again");
        recorder::take();
    }

    /// Windows for `inject_like_windows`: every frame gets `outcome`, and
    /// is recorded as the injector would build it.
    #[cfg(not(windows))]
    struct RecordingTarget {
        screen: VirtualScreen,
        outcome: pen_contact::Outcome,
        frames: Vec<pen_contact::PointerFrame>,
        recreated: usize,
    }

    #[cfg(not(windows))]
    impl pen_contact::PenTarget for RecordingTarget {
        fn screen(&self) -> VirtualScreen {
            self.screen
        }

        fn inject(
            &mut self,
            frame: PenSample,
            _: pen_contact::PointerKind,
        ) -> pen_contact::Outcome {
            self.frames
                .push(pen_contact::PointerFrame::new(frame, self.screen));
            self.outcome
        }

        fn recreate(&mut self) {
            self.recreated += 1;
        }

        fn give_up(&mut self) {
            self.recreated += 1;
        }
    }

    /// Send the relay's recorded pen batch the way `windows_inject.rs` does,
    /// without Win32: the refresh tick flushes first, as `keep_pen_alive`
    /// does, then each sample goes through `PenContact` and every frame is
    /// built against the virtual screen at injection.
    #[cfg(not(windows))]
    fn inject_like_windows(
        contact: &mut pen_contact::PenContact,
        output: CaptureGeometry,
        screen: VirtualScreen,
        outcome: pen_contact::Outcome,
        tick: bool,
    ) -> Vec<pen_contact::PointerFrame> {
        let mut target = RecordingTarget {
            screen,
            outcome,
            frames: Vec::new(),
            recreated: 0,
        };
        if tick {
            contact.flush(Some(output), std::time::Instant::now(), &mut target);
        }
        for injection in recorder::take() {
            let Injection::Pen(sample) = injection else {
                panic!("expected only pen input, got {injection:?}");
            };
            contact.deliver(sample, Some(output), std::time::Instant::now(), &mut target);
        }
        assert_eq!(target.recreated, 0);
        target.frames
    }

    #[cfg(not(windows))]
    #[test]
    fn a_down_windows_was_not_ready_for_starts_where_the_display_moved() {
        use pen_contact::{pointer_flag as flag, Outcome, PenContact, PointerFrame};
        // A 2360x1640 output right of a 2560x1440 primary moves to its left
        // while Windows is not ready for the down.
        let primary = rect(0, 0, 2560, 1440);
        let (before, after) = (rect(2560, 0, 2360, 1640), rect(-2360, 0, 2360, 1640));
        let (screen_before, screen_after) =
            (bounding(&[primary, before]), bounding(&[after, primary]));
        let mut relay = InputRelay::default();
        let mut contact = PenContact::default();
        let mut e = event(KIND_PENCIL, PHASE_BEGAN, 32_768, 32_768);
        e.input_ver = 2;
        e.pressure_x1000 = 500;
        relay.relay(1, &e, before);
        let mut frames = inject_like_windows(
            &mut contact,
            before,
            screen_before,
            Outcome::NotReady,
            false,
        );
        relay.last_pen_input =
            std::time::Instant::now().checked_sub(std::time::Duration::from_millis(60));
        relay.keep_pen_alive(Some(after));
        frames.extend(inject_like_windows(
            &mut contact,
            after,
            screen_after,
            Outcome::Injected,
            true,
        ));
        e.event_id = 2;
        e.phase = PHASE_ENDED;
        relay.relay(1, &e, after);
        frames.extend(inject_like_windows(
            &mut contact,
            after,
            screen_after,
            Outcome::Injected,
            false,
        ));
        let contact_flags = flag::INRANGE | flag::INCONTACT | flag::FIRSTBUTTON | flag::PRIMARY;
        let frame = |flags, location, pressure| PointerFrame {
            flags,
            location,
            pressure,
            tilt_x: 0,
            tilt_y: 0,
        };
        // Desktop (3739, 819) before the move, (-1181, 819) after it, which
        // is (1179, 819) on the virtual screen that now starts at -2360.
        assert_eq!(
            frames,
            [
                frame(flag::DOWN | contact_flags, (3739, 819), 512),
                frame(flag::DOWN | contact_flags, (1179, 819), 512),
                frame(flag::UPDATE | contact_flags, (1179, 819), 512),
                frame(flag::UP | flag::PRIMARY, (1179, 819), 0),
            ]
        );
        assert!(contact.pending().is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn a_stroke_windows_never_received_is_not_sent_after_the_session_ends() {
        // The reviewer's case, through the relay. On Windows the relay's
        // reset resets the pen device, as `reset_like_windows` does here.
        use pen_contact::{Outcome, PenContact};
        let reset_like_windows = |contact: &mut PenContact| {
            let mut target = RecordingTarget {
                screen: SINGLE_SCREEN,
                outcome: Outcome::Injected,
                frames: Vec::new(),
                recreated: 0,
            };
            contact.reset(&mut target);
            (target.frames, target.recreated)
        };
        for takeover in [false, true] {
            let mut relay = InputRelay::default();
            let mut contact = PenContact::default();
            let mut e = event(KIND_PENCIL, PHASE_BEGAN, 32_768, 32_768);
            e.input_ver = 2;
            relay.relay(7, &e, OUTPUT);
            inject_like_windows(
                &mut contact,
                OUTPUT,
                SINGLE_SCREEN,
                Outcome::NotReady,
                false,
            );
            e.event_id = 2;
            e.phase = PHASE_ENDED;
            relay.relay(7, &e, OUTPUT);
            inject_like_windows(
                &mut contact,
                OUTPUT,
                SINGLE_SCREEN,
                Outcome::NotReady,
                false,
            );
            if takeover {
                // A new session's first event resets the old one.
                let hover = event(KIND_HOVER, PHASE_MOVED, 0, 0);
                relay.relay(8, &hover, OUTPUT);
                assert!(matches!(
                    recorder::take().as_slice(),
                    [Injection::MoveAbs { .. }]
                ));
            } else {
                relay.reset();
                assert!(recorder::take().is_empty(), "the relay holds no pen");
            }
            // Windows never accepted the down, so nothing is ended; a fresh
            // device replaces the one still refusing it.
            assert_eq!(reset_like_windows(&mut contact), (Vec::new(), 1));
            let replayed =
                inject_like_windows(&mut contact, OUTPUT, SINGLE_SCREEN, Outcome::Injected, true);
            assert!(
                replayed.is_empty(),
                "replayed after session reset: {replayed:?}"
            );
        }
    }
}
