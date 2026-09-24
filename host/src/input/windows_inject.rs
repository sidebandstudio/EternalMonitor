//! Windows mouse/keyboard and native Windows Ink pen injection.

use tracing::{debug, warn};
use windows::Win32::Foundation::{ERROR_NOT_READY, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
    POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
};
use windows::Win32::UI::Input::Pointer::{
    InjectSyntheticPointerInput, POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN,
    POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_FLAG_PRIMARY,
    POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_INFO, POINTER_PEN_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowTextW,
    GetWindowThreadProcessId, PEN_MASK_PRESSURE, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, PT_PEN,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::{Injection, PenPhase, PenSample, VirtualScreen};

/// One device per relay, retained across samples and destroyed at shutdown.
#[derive(Debug)]
pub struct PenDevice {
    handle: Option<HSYNTHETICPOINTERDEVICE>,
    warned: bool,
}

// This is a process-owned User32 handle, not a pointer we dereference. Tokio
// may move the relay between threads; &mut self serializes every API call.
unsafe impl Send for PenDevice {}

impl Default for PenDevice {
    fn default() -> Self {
        let handle = match unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) }
        {
            Ok(handle) => Some(handle),
            Err(error) => {
                warn!(%error, "Native Windows pen input unavailable");
                None
            }
        };
        Self {
            handle,
            warned: false,
        }
    }
}

impl PenDevice {
    pub fn available(&self) -> bool {
        self.handle.is_some()
    }

    fn inject(&mut self, sample: PenSample) {
        let Some(handle) = self.handle else {
            return;
        };
        let flags = match sample.phase {
            PenPhase::Down => {
                POINTER_FLAG_DOWN
                    | POINTER_FLAG_INRANGE
                    | POINTER_FLAG_INCONTACT
                    | POINTER_FLAG_FIRSTBUTTON
            }
            PenPhase::Move => {
                POINTER_FLAG_UPDATE
                    | POINTER_FLAG_INRANGE
                    | POINTER_FLAG_INCONTACT
                    | POINTER_FLAG_FIRSTBUTTON
            }
            // Finish contact completely. Supported UIKit hover starts a new
            // in-range update; unsupported devices must not leave ghost hover.
            PenPhase::Up => POINTER_FLAG_UP,
            PenPhase::Cancel => POINTER_FLAG_UP | POINTER_FLAG_CANCELED,
            PenPhase::Hover => POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE,
            PenPhase::Leave => POINTER_FLAG_UPDATE,
        };
        let info = POINTER_TYPE_INFO {
            r#type: PT_PEN,
            Anonymous: POINTER_TYPE_INFO_0 {
                penInfo: POINTER_PEN_INFO {
                    pointerInfo: POINTER_INFO {
                        pointerType: PT_PEN,
                        pointerId: 1,
                        pointerFlags: flags | POINTER_FLAG_PRIMARY,
                        ptPixelLocation: POINT {
                            x: sample.x,
                            y: sample.y,
                        },
                        ..Default::default()
                    },
                    penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
                    pressure: sample.pressure,
                    tiltX: sample.tilt_x,
                    tiltY: sample.tilt_y,
                    ..Default::default()
                },
            },
        };
        // Windows timestamps injection at 0.1 ms resolution. Coalesced UIKit
        // samples can arrive together; retry that same sample if the OS clock
        // has not advanced, instead of silently losing a stroke point or up.
        let started = std::time::Instant::now();
        let result = loop {
            let result = unsafe { InjectSyntheticPointerInput(handle, &[info]) };
            if result.as_ref().is_err_and(|error| {
                error.code() == windows::core::HRESULT::from_win32(ERROR_NOT_READY.0)
            }) && started.elapsed() < std::time::Duration::from_millis(2)
            {
                std::thread::yield_now();
                continue;
            }
            break result;
        };
        match result {
            Ok(()) => super::log_injections(&[Injection::Pen(sample)]),
            Err(error) if !self.warned => {
                self.warned = true;
                warn!(%error, "Windows rejected native pen input");
            }
            Err(_) => {}
        }
    }
}

impl Drop for PenDevice {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            unsafe { DestroySyntheticPointerDevice(handle) };
        }
    }
}

/// The Windows virtual screen (all monitors' bounding box) for absolute
/// pointer mapping. Queried per batch — display topology can change under us.
pub fn virtual_screen() -> VirtualScreen {
    unsafe {
        VirtualScreen {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as u32,
        }
    }
}

fn keyboard_input(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    dx: i32,
    dy: i32,
    data: i32,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Execute a resolved injection batch.
pub fn inject(injections: &[Injection], pen: &mut PenDevice) {
    if !probe_guard_allows(injections) {
        warn!("Input probe guard rejected an injection batch");
        return;
    }
    let mut inputs: Vec<INPUT> = Vec::with_capacity(injections.len() * 2);
    for injection in injections {
        let input = match *injection {
            Injection::Pen(sample) => {
                // Flush keyboard releases first when a session is cancelled.
                if !inputs.is_empty() {
                    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
                    inputs.clear();
                }
                pen.inject(sample);
                continue;
            }
            Injection::KeyDown { scan, extended } | Injection::KeyUp { scan, extended } => {
                let mut flags = KEYEVENTF_SCANCODE;
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if matches!(injection, Injection::KeyUp { .. }) {
                    flags |= KEYEVENTF_KEYUP;
                }
                keyboard_input(scan, flags)
            }
            Injection::Unicode(unit) => {
                inputs.push(keyboard_input(unit, KEYEVENTF_UNICODE));
                keyboard_input(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)
            }
            Injection::MoveAbs { x, y } => mouse_input(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::LeftDown { x, y } => mouse_input(
                MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::LeftUp { x, y } => mouse_input(
                MOUSEEVENTF_LEFTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::RightDown { x, y } => mouse_input(
                MOUSEEVENTF_RIGHTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::RightUp { x, y } => mouse_input(
                MOUSEEVENTF_RIGHTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::Wheel { delta } => mouse_input(MOUSEEVENTF_WHEEL, 0, 0, delta),
            Injection::MiddleDown { x, y } => mouse_input(
                MOUSEEVENTF_MIDDLEDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::MiddleUp { x, y } => mouse_input(
                MOUSEEVENTF_MIDDLEUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::HWheel { delta } => mouse_input(MOUSEEVENTF_HWHEEL, 0, 0, delta),
        };
        inputs.push(input);
    }
    if inputs.is_empty() {
        return;
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        super::log_injections(injections);
    } else {
        debug!(
            sent,
            requested = inputs.len(),
            "SendInput injected fewer events than requested"
        );
    }
}

fn probe_guard_allows(injections: &[Injection]) -> bool {
    // An absent variable leaves the normal product input path unchanged. A
    // malformed value fails closed, as does focus moving away from the probe.
    let Ok(value) = std::env::var("ETERNAL_INPUT_WINDOW_PID") else {
        return true;
    };
    let expected_pid = value.parse::<u32>().unwrap_or(0);
    unsafe {
        let window = GetForegroundWindow();
        let mut foreground_pid = 0;
        GetWindowThreadProcessId(window, Some(&mut foreground_pid));
        let mut title = [0u16; 128];
        let length = GetWindowTextW(window, &mut title).max(0) as usize;
        if String::from_utf16_lossy(&title[..length]) != "EternalMonitor input probe" {
            return false;
        }
        let mut rect = RECT::default();
        let mut origin = POINT::default();
        let mut cursor = POINT::default();
        if GetClientRect(window, &mut rect).is_err()
            || !ClientToScreen(window, &mut origin).as_bool()
            || GetCursorPos(&mut cursor).is_err()
        {
            return false;
        }
        super::probe_guard::permits(
            expected_pid,
            foreground_pid,
            super::CaptureGeometry {
                left: origin.x,
                top: origin.y,
                width: (rect.right - rect.left).max(0) as u32,
                height: (rect.bottom - rect.top).max(0) as u32,
            },
            virtual_screen(),
            (cursor.x, cursor.y),
            injections,
        )
    }
}
