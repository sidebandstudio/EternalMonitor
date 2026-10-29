//! One host per installation. Two hosts from the same install would share the
//! virtual display and the port, and an update from either one closes both
//! (Setup's /CLOSEAPPLICATIONS), cutting the other one's stream. A second
//! launch brings the running window forward and exits.
//!
//! The key is the executable plus the settings folder, so test harnesses that
//! give each host its own APPDATA keep running side by side. Both are resolved
//! from open handles to their final paths, so a short (8.3) name, a junction
//! or a `\\?\` spelling of the same install reaches the same key. The mutex
//! is per Windows session.
//!
//! The first host owns the mutex for its whole life, so Windows marks it
//! abandoned when that process ends however it ends. A copy started by
//! "Restart EternalMonitor" (`relaunch`) waits on it for the old host to exit
//! instead of showing the old window; an ordinary launch never waits.
use std::path::Path;
use std::time::Duration;

/// `Local\EternalMonitor-<hash>` for this executable and settings folder.
pub fn mutex_name(exe: &Path, app_data: &Path) -> String {
    let key = format!(
        "{}\n{}",
        exe.display().to_string().to_lowercase(),
        app_data.display().to_string().to_lowercase()
    );
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    let hex: String = digest.as_ref()[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(r"Local\EternalMonitor-{hex}")
}

/// Held for the life of the process.
pub struct Guard {
    #[cfg(windows)]
    _handle: windows::Win32::Foundation::HANDLE,
    /// Why the check could not be made, if it could not. Taken before
    /// logging starts, so the host logs it once its log is open.
    pub problem: Option<String>,
}

/// `None` when another host from this installation is already running.
pub fn acquire() -> Option<Guard> {
    claim_install(None)
}

/// For a copy started by "Restart EternalMonitor": waits up to `timeout`
/// for the running host from this installation to exit, then holds the
/// mutex. `None` when that host still runs at the deadline.
pub fn take_over(timeout: Duration) -> Option<Guard> {
    claim_install(Some(timeout))
}

fn claim_install(wait: Option<Duration>) -> Option<Guard> {
    let exe = std::env::current_exe().ok()?;
    let Some(app_data) = crate::settings::app_data_dir() else {
        return Some(Guard::none());
    };
    // The settings folder may not exist yet on a first run.
    let _ = std::fs::create_dir_all(&app_data);
    claim_named(&mutex_name(&canonical(&exe), &canonical(&app_data)), wait)
}

/// Windows' named mutex, as the check uses it. Tests fake it.
pub trait NamedMutex {
    type Handle;
    /// Opens the mutex, creating it owned by the calling thread when it
    /// doesn't exist yet. Returns the handle and whether it existed (and so
    /// is not ours).
    fn create(&mut self) -> Result<(Self::Handle, bool), String>;
    /// Waits up to `timeout` to own the mutex: true once its owner released
    /// it or exited (abandoned), false at the deadline.
    fn wait(&mut self, handle: &Self::Handle, timeout: Duration) -> Result<bool, String>;
    fn close(&mut self, handle: Self::Handle);
}

/// The outcome of the check.
#[derive(Debug, PartialEq, Eq)]
pub enum Claim<H> {
    /// This launch is the host and owns the mutex.
    Owned(H),
    /// Another host runs (still, after a restart copy's wait).
    Running,
    /// The check itself failed; the launch goes ahead and logs why.
    Unchecked(Option<H>, String),
}

/// Whether this launch may be the host. An ordinary launch (`wait` is
/// `None`) loses to a running host at once; a restart copy waits up to
/// `wait` for the host that started it to exit. Either way a manual launch
/// meanwhile finds the mutex and shows the running window.
pub fn claim<M: NamedMutex>(mutex: &mut M, wait: Option<Duration>) -> Claim<M::Handle> {
    let (handle, existed) = match mutex.create() {
        Ok(created) => created,
        // Never refuse to start because the check itself failed.
        Err(error) => return Claim::Unchecked(None, error),
    };
    if !existed {
        return Claim::Owned(handle);
    }
    let Some(timeout) = wait else {
        mutex.close(handle);
        return Claim::Running;
    };
    match mutex.wait(&handle, timeout) {
        Ok(true) => Claim::Owned(handle),
        Ok(false) => {
            mutex.close(handle);
            Claim::Running
        }
        Err(error) => Claim::Unchecked(Some(handle), error),
    }
}

/// The final path of an existing file or folder, as Windows resolves it from
/// an open handle (links followed, short names expanded, `\\?\` removed).
/// Falls back to the path as given when it can't be opened.
pub fn canonical(path: &Path) -> std::path::PathBuf {
    resolve(path).unwrap_or_else(|| path.to_path_buf())
}

#[cfg(windows)]
fn resolve(path: &Path) -> Option<std::path::PathBuf> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    // FILE_FLAG_BACKUP_SEMANTICS opens folders too; no access needed.
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(0x7)
        .custom_flags(0x0200_0000)
        .open(path)
        .ok()?;
    let handle = windows::Win32::Foundation::HANDLE(file.as_raw_handle());
    eternal_updater::win::final_path(handle)
        .ok()
        .map(std::path::PathBuf::from)
}

#[cfg(not(windows))]
fn resolve(path: &Path) -> Option<std::path::PathBuf> {
    std::fs::canonicalize(path).ok()
}

#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        if !self._handle.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self._handle);
            }
        }
    }
}

impl Guard {
    fn none() -> Self {
        Self {
            #[cfg(windows)]
            _handle: windows::Win32::Foundation::HANDLE::default(),
            problem: None,
        }
    }
}

#[cfg(windows)]
struct Win32Mutex(windows::core::HSTRING);

#[cfg(windows)]
impl NamedMutex for Win32Mutex {
    type Handle = windows::Win32::Foundation::HANDLE;

    fn create(&mut self) -> Result<(Self::Handle, bool), String> {
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        let handle =
            unsafe { CreateMutexW(None, true, &self.0) }.map_err(|error| error.to_string())?;
        Ok((handle, unsafe { GetLastError() } == ERROR_ALREADY_EXISTS))
    }

    fn wait(&mut self, handle: &Self::Handle, timeout: Duration) -> Result<bool, String> {
        use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::WaitForSingleObject;
        let millis = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        match unsafe { WaitForSingleObject(*handle, millis) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(windows::core::Error::from_win32().to_string()),
        }
    }

    fn close(&mut self, handle: Self::Handle) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
    }
}

/// `None` when another host holds `name` (still, after waiting `wait`).
#[cfg(windows)]
pub fn claim_named(name: &str, wait: Option<Duration>) -> Option<Guard> {
    match claim(&mut Win32Mutex(name.into()), wait) {
        Claim::Owned(handle) => Some(Guard {
            _handle: handle,
            problem: None,
        }),
        Claim::Running => None,
        Claim::Unchecked(handle, problem) => Some(Guard {
            _handle: handle.unwrap_or_default(),
            problem: Some(problem),
        }),
    }
}

#[cfg(not(windows))]
pub fn claim_named(_name: &str, _wait: Option<Duration>) -> Option<Guard> {
    Some(Guard::none())
}

/// Whether `image` (a running process's path) is the executable whose final
/// path, lowercased, is `exe`.
#[cfg(windows)]
fn same_file(image: &str, exe: &str) -> bool {
    canonical(Path::new(image))
        .display()
        .to_string()
        .to_lowercase()
        == exe
}

/// Restores and focuses the running host's window: one titled
/// "EternalMonitor" whose process runs this same executable.
#[cfg(windows)]
pub fn show_existing() {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
        SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };

    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let exe = &*(data.0 as *const String);
        let mut title = [0u16; 64];
        let length = GetWindowTextW(hwnd, &mut title) as usize;
        if String::from_utf16_lossy(&title[..length]) != "EternalMonitor" {
            return true.into();
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return true.into();
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true.into();
        };
        let mut path = vec![0u16; 32_768];
        let mut size = path.len() as u32;
        let same = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut size,
        )
        .is_ok()
            && same_file(&String::from_utf16_lossy(&path[..size as usize]), exe);
        if !same {
            let _ = CloseHandle(process);
            return true.into();
        }
        // The window's own thread restores it and asks to be activated, so
        // the running host needs the right to come to the front. Windows
        // gives it to the launch the user just started; pass it on. Without
        // that right (a launch from a background task) the window is only
        // restored and its taskbar button flashes. The handle stays open
        // through the grant: an open handle keeps the verified process's id
        // from being reused by another process meanwhile.
        let _ = AllowSetForegroundWindow(pid);
        let _ = CloseHandle(process);
        let _ = ShowWindow(
            hwnd,
            if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        // This launch has no log of its own: it must not touch the running
        // host's session log.
        if SetForegroundWindow(hwnd).as_bool() {
            eprintln!("Brought the running EternalMonitor window (process {pid}) to the front");
        } else {
            eprintln!("Windows kept another window in front of EternalMonitor (process {pid})");
        }
        false.into()
    }

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Compared by final path: a second launch through a junction or short
    // name must still find the first window.
    let exe = canonical(&exe).display().to_string().to_lowercase();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&exe as *const String as isize));
    }
}

#[cfg(not(windows))]
pub fn show_existing() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_the_install_and_its_settings() {
        let exe = Path::new(r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe");
        let data = Path::new(r"C:\Users\A\AppData\Roaming\EternalMonitor");
        let name = mutex_name(exe, data);
        assert!(name.starts_with(r"Local\EternalMonitor-"), "{name}");
        assert_eq!(
            name,
            mutex_name(
                Path::new(r"c:\program files\eternalmonitor\EternalMonitor-host.exe"),
                data
            ),
            "Windows paths ignore case"
        );
        assert_ne!(
            name,
            mutex_name(exe, Path::new(r"D:\test\appdata\EternalMonitor"))
        );
        assert_ne!(
            name,
            mutex_name(Path::new(r"D:\other\EternalMonitor-host.exe"), data)
        );
    }

    /// Aliases of one install and settings folder reach one key.
    #[test]
    fn aliases_of_the_same_install_share_a_key() {
        let dir = std::env::temp_dir().join(format!("eternal-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("install")).unwrap();
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let exe = dir.join("install").join("EternalMonitor-host.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let link = dir.join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("install"), &link).unwrap();
        #[cfg(windows)]
        assert!(std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(dir.join("install"))
            .output()
            .unwrap()
            .status
            .success());
        let key = |exe: &Path, data: &Path| mutex_name(&canonical(exe), &canonical(data));
        let real = key(&exe, &dir.join("data"));
        assert_eq!(
            real,
            key(&link.join("EternalMonitor-host.exe"), &dir.join("data")),
            "through a link"
        );
        assert_eq!(
            real,
            key(&exe, &dir.join("install").join("..").join("data")),
            "with .."
        );
        #[cfg(windows)]
        {
            let verbatim = format!(r"\\?\{}", exe.display());
            assert_eq!(
                real,
                key(Path::new(&verbatim), &dir.join("data")),
                "\\\\?\\ prefix"
            );
        }
        assert_ne!(
            real,
            key(&exe, &dir.join("install")),
            "another settings folder"
        );
        #[cfg(windows)]
        let _ = std::process::Command::new("cmd")
            .args(["/c", "rmdir"])
            .arg(&link)
            .status();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&link);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_second_host_from_the_same_install_is_refused() {
        let name = format!(r"Local\EternalMonitor-test-{}", std::process::id());
        let first = claim_named(&name, None).expect("first");
        assert!(claim_named(&name, None).is_none(), "second");
        drop(first);
        // Windows frees the name with its last handle; a later start succeeds.
        let mut guard = None;
        for _ in 0..20 {
            guard = claim_named(&name, None);
            if guard.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(guard.is_some(), "after the first exits");
    }

    /// The old host is a thread here: Windows ties mutex ownership to the
    /// thread, and marks it abandoned when that thread ends, as when the
    /// old host's process ends.
    #[cfg(windows)]
    #[test]
    fn a_restart_copy_waits_for_the_old_host_and_a_manual_launch_never_does() {
        use std::sync::mpsc;
        use std::time::Instant;
        let name = format!(r"Local\EternalMonitor-test-wait-{}", std::process::id());
        let (owned, is_owned) = mpsc::channel();
        let (quit, quitting) = mpsc::channel::<()>();
        let old_name = name.clone();
        let old = std::thread::spawn(move || {
            let _guard = claim_named(&old_name, None).expect("old");
            owned.send(()).unwrap();
            quitting.recv().unwrap();
        });
        is_owned.recv().unwrap();
        let started = Instant::now();
        assert!(claim_named(&name, None).is_none(), "manual launch");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(
            claim_named(&name, Some(Duration::from_millis(200))).is_none(),
            "the old host still runs"
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            quit.send(()).unwrap();
        });
        let started = Instant::now();
        let copy = claim_named(&name, Some(Duration::from_secs(20))).expect("copy");
        assert!(copy.problem.is_none(), "{:?}", copy.problem);
        assert!(started.elapsed() >= Duration::from_millis(250));
        old.join().unwrap();
        release.join().unwrap();
        // Now the copy runs: a manual launch from another thread sees it.
        let probe = name.clone();
        assert!(
            std::thread::spawn(move || claim_named(&probe, None).is_none())
                .join()
                .unwrap()
        );
    }
}
