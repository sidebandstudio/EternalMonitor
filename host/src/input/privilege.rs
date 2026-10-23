//! Input that never reaches an app because it runs at a higher privilege
//! level than EternalMonitor, usually because it was opened as
//! administrator. Windows keeps a lower level from controlling a higher one
//! (User Interface Privilege Isolation), so neither the pen nor `SendInput`
//! reaches it, and only running both at the same level helps. Windows does
//! not say so: on the reference PC both report success and the app receives
//! nothing. So `windows_inject.rs` checks the app in front whenever the
//! foreground window changes, and the GUI explains while one is in front.

use parking_lot::Mutex;

/// An app running at a higher privilege level than EternalMonitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevatedApp {
    /// Its program file name without ".exe", when Windows tells us.
    pub name: Option<String>,
}

/// The foreground window input last went to, and the higher-level app it
/// belongs to, if any.
static FOREGROUND: Mutex<(isize, Option<ElevatedApp>)> = parking_lot::const_mutex((0, None));

/// The app Windows is keeping our input from, if any.
pub fn blocked_by() -> Option<ElevatedApp> {
    FOREGROUND.lock().1.clone()
}

/// Input is going to the foreground window `window`, 0 when there is none,
/// as on the lock screen or a permission prompt. When it is not the window
/// input last went to, `check` says whether its app runs at a higher level,
/// so a window is looked up once however much input goes to it.
#[cfg(any(windows, test))]
pub(crate) fn input_for(window: isize, check: impl FnOnce() -> Option<ElevatedApp>) {
    let mut foreground = FOREGROUND.lock();
    if foreground.0 == window {
        return;
    }
    let app = if window == 0 { None } else { check() };
    match (&foreground.1, &app) {
        (_, Some(app)) if foreground.1.as_ref() != Some(app) => tracing::warn!(
            app = app.name.as_deref().unwrap_or("unknown"),
            "Windows blocks Pencil, touch and keyboard input to the app in front, which runs as administrator"
        ),
        (Some(_), None) => tracing::info!("Input goes to an app Windows lets it reach again"),
        _ => {}
    }
    *foreground = (window, app);
}

/// `path`'s program name for the notice: its file name without ".exe".
#[cfg(any(windows, test))]
pub(crate) fn program_name(path: &str) -> Option<String> {
    let file = path.rsplit(['\\', '/']).next()?;
    let name = file
        .strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file);
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_program_without_its_folder() {
        assert_eq!(
            program_name(
                r"C:\Program Files\CELSYS\CLIP STUDIO 1.5\CLIP STUDIO PAINT\CLIPStudioPaint.exe"
            )
            .as_deref(),
            Some("CLIPStudioPaint")
        );
        assert_eq!(program_name("Taskmgr.EXE").as_deref(), Some("Taskmgr"));
        assert_eq!(program_name(r"C:\Tools\"), None);
    }

    /// The only test that touches the process-wide state; off Windows
    /// nothing else injects.
    #[test]
    fn the_app_in_front_is_checked_once_per_window_and_cleared_when_it_goes() {
        let app = || ElevatedApp {
            name: Some("Taskmgr".into()),
        };
        let mut checks = 0;
        let mut input_for_window = |window, found: Option<ElevatedApp>| {
            input_for(window, || {
                checks += 1;
                found
            })
        };
        input_for_window(1, None);
        assert_eq!(blocked_by(), None);
        // An app running as administrator comes to the front: checked once,
        // however much input goes to it.
        for _ in 0..3 {
            input_for_window(2, Some(app()));
        }
        assert_eq!(blocked_by(), Some(app()));
        // Another window in front clears it; so does none at all (the lock
        // screen), without a check.
        input_for_window(3, None);
        assert_eq!(blocked_by(), None);
        input_for_window(4, Some(ElevatedApp { name: None }));
        assert_eq!(blocked_by(), Some(ElevatedApp { name: None }));
        input_for_window(0, Some(app()));
        assert_eq!(blocked_by(), None);
        assert_eq!(checks, 4);
    }
}
