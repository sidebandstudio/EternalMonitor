//! The Stream page's "Fix it" for a Windows Firewall block on this exe.
//!
//! The block Windows adds when its "allow access" prompt is dismissed beats
//! the installer's Allow rule, and the only fix the PC offered was to run
//! Setup again. "Fix it" runs the same netsh lines Setup runs for the
//! firewall, in the same order, with one Windows prompt: this exe starts a
//! copy of itself as administrator with [`FIX_ARG`], and that copy runs
//! them and exits. Each line is a fixed string with only this exe's own
//! path in it, run with the system's netsh.exe and no shell, so nothing
//! anyone types reaches an elevated command line. Only rules for this exe
//! and the installer's own rule names are touched.
//!
//! A user who answers No to the Windows prompt changes nothing: the notice
//! stays as it was. After the copy ran, the firewall is read again at once.

use std::path::Path;

/// The argument the elevated copy runs with.
pub const FIX_ARG: &str = "--fix-firewall";

/// The netsh lines of the installer's `[Run]` section, as Setup runs them,
/// for this exe at `exe`. `{exe}` is the only thing filled in.
const SETUP_LINES: [&str; 4] = [
    r#"advfirewall firewall delete rule name="EternalMonitor Host UDP""#,
    r#"advfirewall firewall delete rule name="EternalMonitor Host TCP""#,
    r#"advfirewall firewall delete rule name=all dir=in program="{exe}""#,
    r#"advfirewall firewall add rule name="EternalMonitor Host UDP" dir=in action=allow program="{exe}" protocol=UDP profile=domain,private,public enable=yes"#,
];

/// The netsh parameter lines the fix runs, in order, for this exe at `exe`.
/// None for a path that can't go inside quotes as it is.
pub fn command_lines(exe: &Path) -> Option<Vec<String>> {
    let exe = exe.to_str()?;
    if exe.is_empty() || exe.contains('"') || exe.chars().any(char::is_control) {
        return None;
    }
    Some(
        SETUP_LINES
            .iter()
            .map(|line| line.replace("{exe}", exe))
            .collect(),
    )
}

/// How starting the elevated copy went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevated {
    /// It ran and exited with this code.
    Exited(u32),
    /// The user answered No to the Windows prompt.
    Declined,
    /// Windows couldn't start it, or it didn't finish in time.
    Failed(String),
}

/// Starts this exe again as administrator with `arg`, and waits for it.
/// The real one asks Windows; tests fake it.
pub trait Elevate {
    fn run_elevated(&self, exe: &Path, arg: &str) -> Elevated;
}

/// What the Stream page shows after "Fix it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The lines ran: the firewall was read again, and the notice follows it.
    Ran,
    /// No to the Windows prompt: nothing changed.
    Declined,
    /// The fix didn't run or didn't finish. English, as other errors are.
    Failed(&'static str),
}

/// Runs the fix: the elevated copy, then a fresh read of the firewall
/// (`recheck`), whatever it did, so the notice shows what is true now.
pub fn fix(elevate: &impl Elevate, exe: &Path, recheck: impl FnOnce()) -> Outcome {
    if command_lines(exe).is_none() {
        return Outcome::Failed(FAILED);
    }
    match elevate.run_elevated(exe, FIX_ARG) {
        Elevated::Declined => Outcome::Declined,
        Elevated::Exited(0) => {
            recheck();
            Outcome::Ran
        }
        Elevated::Exited(_) | Elevated::Failed(_) => {
            recheck();
            Outcome::Failed(FAILED)
        }
    }
}

/// Where the Stream page's "Fix it" is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Progress {
    #[default]
    Idle,
    /// The Windows prompt or the netsh lines are running.
    Running,
    Done(Outcome),
}

static PROGRESS: std::sync::Mutex<Progress> = std::sync::Mutex::new(Progress::Idle);

pub fn progress() -> Progress {
    PROGRESS.lock().map_or(Progress::Idle, |progress| *progress)
}

/// Runs `fix` off the window's thread, once at a time: the Windows prompt
/// waits for the user, and netsh for Windows.
pub fn start(fix: impl FnOnce() -> Outcome + Send + 'static) {
    {
        let Ok(mut progress) = PROGRESS.lock() else {
            return;
        };
        if *progress == Progress::Running {
            return;
        }
        *progress = Progress::Running;
    }
    let spawned = std::thread::Builder::new()
        .name("firewall-fix".into())
        .spawn(move || {
            let outcome = fix();
            tracing::info!(?outcome, "Firewall fix finished");
            if let Ok(mut progress) = PROGRESS.lock() {
                *progress = Progress::Done(outcome);
            }
        });
    if spawned.is_err() {
        if let Ok(mut progress) = PROGRESS.lock() {
            *progress = Progress::Done(Outcome::Failed(FAILED));
        }
    }
}

/// The error under the notice when the fix didn't work.
pub const FAILED: &str =
    "Windows didn't apply the fix. Run the EternalMonitor installer again instead.";

/// The elevated copy's work: runs each line with `netsh` and returns the
/// exit code. Removing a rule that isn't there fails, as it does for Setup,
/// and doesn't count; only the Allow rule must be added.
pub fn run_lines(lines: &[String], mut netsh: impl FnMut(&str) -> std::io::Result<i32>) -> i32 {
    let mut code = 0;
    for line in lines {
        let added = line.starts_with("advfirewall firewall add rule ");
        match netsh(line) {
            Ok(0) => {}
            Ok(_) | Err(_) if !added => {}
            Ok(failed) => code = failed.max(1),
            Err(_) => code = 2,
        }
    }
    code
}

/// If this process is the elevated copy ([`FIX_ARG`]), runs the fix and
/// exits with its code. Called first thing in `main`.
pub fn run_if_requested() {
    if std::env::args().nth(1).as_deref() != Some(FIX_ARG) {
        return;
    }
    let code = std::env::current_exe()
        .ok()
        .and_then(|exe| command_lines(&exe))
        .map_or(3, |lines| run_lines(&lines, windows_netsh::run));
    std::process::exit(code);
}

/// The fix as the Stream page runs it, on Windows.
#[cfg(windows)]
pub fn fix_now() -> Outcome {
    let Ok(exe) = std::env::current_exe() else {
        return Outcome::Failed(FAILED);
    };
    fix(&windows_elevate::Shell, &exe, crate::firewall::recheck_now)
}

/// Off Windows there is no firewall notice, so no fix.
#[cfg(not(windows))]
pub fn fix_now() -> Outcome {
    Outcome::Failed(FAILED)
}

#[cfg(windows)]
mod windows_netsh {
    use std::os::windows::process::CommandExt;

    /// Runs the system's netsh.exe with `line` exactly as written, no shell.
    pub fn run(line: &str) -> std::io::Result<i32> {
        let mut system = [0u16; 260];
        let len = unsafe {
            windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut system))
        };
        if len == 0 || len as usize >= system.len() {
            return Err(std::io::Error::other("no system folder"));
        }
        let netsh = std::path::PathBuf::from(String::from_utf16_lossy(&system[..len as usize]))
            .join("netsh.exe");
        let status = std::process::Command::new(netsh)
            .raw_arg(line)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()?;
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(not(windows))]
mod windows_netsh {
    pub fn run(_line: &str) -> std::io::Result<i32> {
        Err(std::io::Error::other("Windows only"))
    }
}

#[cfg(windows)]
mod windows_elevate {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    use super::{Elevate, Elevated};

    /// How long the copy may take: four netsh calls.
    const WAIT_MS: u32 = 60_000;

    pub struct Shell;

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    impl Elevate for Shell {
        fn run_elevated(&self, exe: &Path, arg: &str) -> Elevated {
            let verb = wide("runas".as_ref());
            let file = wide(exe.as_os_str());
            let params = wide(arg.as_ref());
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_NOCLOSEPROCESS,
                lpVerb: PCWSTR(verb.as_ptr()),
                lpFile: PCWSTR(file.as_ptr()),
                lpParameters: PCWSTR(params.as_ptr()),
                nShow: SW_HIDE.0,
                ..Default::default()
            };
            if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
                return if error.code() == ERROR_CANCELLED.to_hresult() {
                    Elevated::Declined
                } else {
                    Elevated::Failed(error.to_string())
                };
            }
            if info.hProcess.is_invalid() {
                return Elevated::Failed("no process".into());
            }
            let elevated = unsafe {
                if WaitForSingleObject(info.hProcess, WAIT_MS) != WAIT_OBJECT_0 {
                    Elevated::Failed("did not finish in time".into())
                } else {
                    let mut code = 0u32;
                    match GetExitCodeProcess(info.hProcess, &mut code) {
                        Ok(()) => Elevated::Exited(code),
                        Err(error) => Elevated::Failed(error.to_string()),
                    }
                }
            };
            unsafe {
                let _ = CloseHandle(info.hProcess);
            }
            elevated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const EXE: &str = r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe";

    /// The installer's [Run] netsh Parameters, as Setup passes them to
    /// netsh, for the installed exe at `exe`.
    fn setup_lines(exe: &str) -> Vec<String> {
        let iss = include_str!("../../installer/EternalMonitor.iss");
        let run = iss
            .split("\n[Run]")
            .nth(1)
            .and_then(|section| section.split("\n[").next())
            .expect("EternalMonitor.iss has a [Run] section");
        run.lines()
            .filter(|line| !line.trim_start().starts_with(';') && line.contains(r"\netsh.exe"))
            .map(|line| {
                line.split("Parameters: \"")
                    .nth(1)
                    .and_then(|rest| rest.split("\"; ").next())
                    .expect("a netsh line has Parameters")
                    .replace("\"\"", "\"")
                    .replace(r"{app}\EternalMonitor-host.exe", exe)
            })
            .collect()
    }

    /// The fix runs exactly what Setup runs, in the same order.
    #[test]
    fn the_fix_runs_the_lines_setup_runs() {
        let lines = command_lines(Path::new(EXE)).unwrap();
        assert_eq!(lines, setup_lines(EXE));
        assert_eq!(lines.len(), 4);
        // Only this exe's rules and the installer's own names.
        for line in &lines {
            assert!(
                line.contains(r#"name="EternalMonitor Host "#)
                    || line.contains(&format!(r#"program="{EXE}""#)),
                "{line}"
            );
        }
        // A copy elsewhere fixes its own rules.
        let other = r"D:\Apps\EM\EternalMonitor-host.exe";
        assert!(
            command_lines(Path::new(other)).unwrap()[2].ends_with(&format!(r#"program="{other}""#))
        );
    }

    /// A path that would break out of its quotes never reaches netsh.
    #[test]
    fn a_path_that_cant_be_quoted_is_refused() {
        for bad in [r#"C:\a"b\x.exe"#, "C:\\a\nb\\x.exe", ""] {
            assert_eq!(command_lines(Path::new(bad)), None, "{bad:?}");
        }
    }

    struct Fake {
        answer: Elevated,
        asked: RefCell<Vec<(String, String)>>,
    }

    impl Elevate for Fake {
        fn run_elevated(&self, exe: &Path, arg: &str) -> Elevated {
            self.asked
                .borrow_mut()
                .push((exe.display().to_string(), arg.to_string()));
            self.answer.clone()
        }
    }

    fn fake(answer: Elevated) -> Fake {
        Fake {
            answer,
            asked: RefCell::new(Vec::new()),
        }
    }

    /// The elevated copy is this exe with the fixed argument, and the
    /// firewall is read again once it ran.
    #[test]
    fn the_fix_starts_this_exe_elevated_then_reads_the_firewall_again() {
        let elevate = fake(Elevated::Exited(0));
        let rechecked = Cell::new(false);
        assert_eq!(
            fix(&elevate, Path::new(EXE), || rechecked.set(true)),
            Outcome::Ran
        );
        assert!(rechecked.get());
        assert_eq!(
            *elevate.asked.borrow(),
            [(EXE.to_string(), FIX_ARG.to_string())]
        );
    }

    /// No to the Windows prompt: nothing ran, nothing is read again, and no
    /// error shows.
    #[test]
    fn declining_the_windows_prompt_changes_nothing() {
        let elevate = fake(Elevated::Declined);
        let rechecked = Cell::new(false);
        assert_eq!(
            fix(&elevate, Path::new(EXE), || rechecked.set(true)),
            Outcome::Declined
        );
        assert!(!rechecked.get());
    }

    /// A copy that failed, or that Windows couldn't start: the error, and
    /// the firewall read again in case some lines ran.
    #[test]
    fn a_fix_that_fails_says_so() {
        for answer in [Elevated::Exited(1), Elevated::Failed("x".into())] {
            let rechecked = Cell::new(false);
            assert_eq!(
                fix(&fake(answer.clone()), Path::new(EXE), || rechecked
                    .set(true)),
                Outcome::Failed(FAILED),
                "{answer:?}"
            );
            assert!(rechecked.get(), "{answer:?}");
        }
        let elevate = fake(Elevated::Exited(0));
        assert_eq!(fix(&elevate, Path::new(""), || {}), Outcome::Failed(FAILED));
        assert!(
            elevate.asked.borrow().is_empty(),
            "nothing elevated for a bad path"
        );
    }

    /// The copy runs every line in order. A rule to remove that isn't there
    /// doesn't count; the Allow rule that isn't added does.
    #[test]
    fn the_elevated_copy_runs_every_line_and_fails_only_on_the_allow_rule() {
        let lines = command_lines(Path::new(EXE)).unwrap();
        let ran = RefCell::new(Vec::new());
        let code = run_lines(&lines, |line| {
            ran.borrow_mut().push(line.to_string());
            Ok(if line.contains("delete") { 1 } else { 0 })
        });
        assert_eq!(code, 0);
        assert_eq!(*ran.borrow(), lines);
        assert_ne!(
            run_lines(&lines, |line| Ok(i32::from(line.contains(" add ")))),
            0
        );
        assert_ne!(
            run_lines(&lines, |line| if line.contains(" add ") {
                Err(std::io::Error::other("x"))
            } else {
                Ok(0)
            }),
            0
        );
    }

    /// One fix at a time, and the window hears how it went.
    #[test]
    fn the_page_sees_the_fix_run_and_finish() {
        let (go, wait) = std::sync::mpsc::channel::<()>();
        start(move || {
            let _ = wait.recv();
            Outcome::Declined
        });
        assert_eq!(progress(), Progress::Running);
        // A second click while it runs starts nothing.
        start(|| panic!("a second fix started"));
        go.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while progress() == Progress::Running {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(progress(), Progress::Done(Outcome::Declined));
    }
}
