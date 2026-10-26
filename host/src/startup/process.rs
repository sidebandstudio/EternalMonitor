//! A process named by its id and start time, so a reused process id never
//! makes a file written by a dead process look like a running one's.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessId {
    pub pid: u32,
    /// Creation time in 100 ns units on Windows; 0 where it is not tracked.
    pub start: u64,
}

impl ProcessId {
    pub(crate) fn current() -> Self {
        let pid = std::process::id();
        Self {
            pid,
            start: start_time(pid).unwrap_or(0),
        }
    }

    /// Parse the `<pid>-<start>` prefix of a file name.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        let mut parts = name.splitn(3, ['-', '.']);
        let pid = parts.next()?.parse().ok()?;
        let start = parts.next()?.parse().ok()?;
        Some(Self { pid, start })
    }

    pub(crate) fn is_running(self) -> bool {
        start_time(self.pid) == Some(self.start)
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.pid, self.start)
    }
}

/// The start time of a running process, or `None` once it has exited.
#[cfg(windows)]
pub(crate) fn start_time(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let start = start_time_of(process);
        let _ = CloseHandle(process);
        start
    }
}

/// [`start_time`] of a process handle opened with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`.
#[cfg(windows)]
pub(crate) fn start_time_of(process: windows::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows::Win32::Foundation::STILL_ACTIVE;
    use windows::Win32::System::Threading::GetExitCodeProcess;
    let mut code = 0u32;
    let alive =
        unsafe { GetExitCodeProcess(process, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32;
    if !alive {
        return None;
    }
    created_at(process)
}

/// The start time of a process handle like [`start_time_of`]'s, also after
/// the process has exited.
#[cfg(windows)]
pub(crate) fn created_at(process: windows::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    unsafe { GetProcessTimes(process, created, exited, kernel, user) }.ok()?;
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// One argument quoted for a Windows command line, the way the C runtime
/// and `CommandLineToArgvW` split it again.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled so it stays a quote.
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

/// Starts `exe` with `args` without a console window and without passing on
/// any handle. `std::process::Command` lets the child inherit every
/// inheritable handle the host holds (sockets, driver objects), which keeps
/// them open as long as the child runs. Returns the child's id.
#[cfg(windows)]
pub(crate) fn spawn_detached(exe: &std::path::Path, args: &[&str]) -> std::io::Result<ProcessId> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let program = exe
        .to_str()
        .ok_or_else(|| std::io::Error::other("the executable path is not Unicode"))?;
    let mut line = quote_arg(program);
    for arg in args {
        line.push(' ');
        line.push_str(&quote_arg(arg));
    }
    let mut command: Vec<u16> = std::ffi::OsStr::new(&line)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let application: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            PWSTR(command.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &startup,
            &mut information,
        )
    }
    .map_err(std::io::Error::other)?;
    let id = ProcessId {
        pid: information.dwProcessId,
        start: start_time_of(information.hProcess).unwrap_or(0),
    };
    unsafe {
        let _ = CloseHandle(information.hThread);
        let _ = CloseHandle(information.hProcess);
    }
    Ok(id)
}

#[cfg(unix)]
pub(crate) fn start_time(pid: u32) -> Option<u64> {
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    alive.then_some(0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Held by tests that run copies of the test executable. A copy alive
    /// while another test scans for running copies would be taken for
    /// another host.
    #[cfg(windows)]
    pub(crate) static RUNS_THIS_EXE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn arguments_survive_the_windows_command_line() {
        assert_eq!(quote_arg("--failure-reporter"), "--failure-reporter");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(
            quote_arg(r"C:\Users\Example User\AppData\Roaming\EternalMonitor\crash"),
            r#""C:\Users\Example User\AppData\Roaming\EternalMonitor\crash""#
        );
        // A trailing backslash must not escape the closing quote.
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
        // Quotes inside, with and without backslashes before them.
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        // Backslashes elsewhere are literal.
        assert_eq!(quote_arg(r"C:\no\spaces"), r"C:\no\spaces");
    }

    /// The test binary starts itself as the child; the child checks whether
    /// an inheritable event of the parent is open in it, as a leaked socket
    /// would be, and writes the answer to a file.
    #[cfg(windows)]
    #[test]
    fn a_detached_child_inherits_no_handles() {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::{
            CloseHandle, CompareObjectHandles, GetHandleInformation, HANDLE,
        };
        use windows::Win32::Security::SECURITY_ATTRIBUTES;
        use windows::Win32::System::Threading::{
            CreateEventW, OpenEventW, SYNCHRONIZATION_SYNCHRONIZE,
        };
        const PROBE: &str = "EM_DETACHED_INHERIT_PROBE";
        const NAME: &str = "startup::process::tests::a_detached_child_inherits_no_handles";
        if let Ok(probe) = std::env::var(PROBE) {
            let mut parts = probe.splitn(3, '|');
            let value: usize = parts.next().unwrap().parse().unwrap();
            let name = parts.next().unwrap();
            let result = parts.next().unwrap();
            // Look for the value before opening a handle of our own, which
            // could otherwise get the same value and compare equal to itself.
            let mut flags = 0;
            let open = unsafe { GetHandleInformation(HANDLE(value as *mut _), &mut flags) }.is_ok();
            let inherited = open && {
                let ours =
                    unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) }
                        .unwrap();
                unsafe { CompareObjectHandles(HANDLE(value as *mut _), ours) }.as_bool()
            };
            let partial = format!("{result}.part");
            std::fs::write(
                &partial,
                if inherited {
                    "inherited"
                } else {
                    "not inherited"
                },
            )
            .unwrap();
            std::fs::rename(partial, result).unwrap();
            return;
        }
        let _copies = RUNS_THIS_EXE.lock();
        let name = format!("Local\\em-detached-probe-{}", std::process::id());
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let event = unsafe {
            CreateEventW(
                Some(&attributes),
                true,
                false,
                &HSTRING::from(name.as_str()),
            )
        }
        .unwrap();
        let result = std::env::temp_dir().join(format!("em-detached-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&result);
        std::env::set_var(
            PROBE,
            format!("{}|{name}|{}", event.0 as usize, result.display()),
        );
        let exe = std::env::current_exe().unwrap();
        let child = spawn_detached(&exe, &["--exact", NAME, "--test-threads=1"]);
        std::env::remove_var(PROBE);
        let child = child.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let answer = loop {
            if let Ok(text) = std::fs::read_to_string(&result) {
                break text;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child never answered"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let _ = std::fs::remove_file(&result);
        unsafe { CloseHandle(event) }.unwrap();
        // Release the lock only once the copy has exited.
        while child.is_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(answer, "not inherited");
    }

    #[test]
    fn names_round_trip_and_reject_other_files() {
        let id = ProcessId {
            pid: 4321,
            start: 133_000_000_000,
        };
        assert_eq!(ProcessId::from_name(&format!("{id}.rec")), Some(id));
        assert_eq!(ProcessId::from_name(&format!("{id}-vulkan.json")), Some(id));
        assert_eq!(ProcessId::from_name("renderer-history.json"), None);
    }

    #[test]
    fn only_the_live_process_with_the_same_start_counts_as_running() {
        let me = ProcessId::current();
        assert!(me.is_running());
        assert!(!ProcessId {
            start: me.start + 1,
            ..me
        }
        .is_running());
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/c", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let exited = ProcessId {
            pid: child.id(),
            start: start_time(child.id()).unwrap_or(0),
        };
        child.wait().unwrap();
        assert!(!exited.is_running());
    }
}
