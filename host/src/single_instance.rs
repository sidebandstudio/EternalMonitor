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
//! A host run as administrator creates a mutex a normal launch may not open.
//! Being refused it still means a host from this install is running.
use std::path::Path;

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
    let exe = std::env::current_exe().ok()?;
    let Some(app_data) = crate::settings::app_data_dir() else {
        return Some(Guard::none());
    };
    // The settings folder may not exist yet on a first run.
    let _ = std::fs::create_dir_all(&app_data);
    acquire_named(&mutex_name(&canonical(&exe), &canonical(&app_data)))
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
pub fn acquire_named(name: &str) -> Option<Guard> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    match unsafe { CreateMutexW(None, false, &HSTRING::from(name)) } {
        Ok(handle) if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS => {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            None
        }
        Ok(handle) => Some(Guard {
            _handle: handle,
            problem: None,
        }),
        // An elevated host holds it.
        Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => None,
        // Never refuse to start because the check itself failed.
        Err(error) => Some(Guard {
            problem: Some(error.to_string()),
            ..Guard::none()
        }),
    }
}

#[cfg(not(windows))]
pub fn acquire_named(_name: &str) -> Option<Guard> {
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
        let first = acquire_named(&name).expect("first");
        assert!(acquire_named(&name).is_none(), "second");
        drop(first);
        // Windows frees the name with its last handle; a later start succeeds.
        let mut guard = None;
        for _ in 0..20 {
            guard = acquire_named(&name);
            if guard.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(guard.is_some(), "after the first exits");
    }

    /// A host run as administrator holds a mutex whose default security a
    /// normal launch may not open. An empty DACL refuses this process the
    /// same way.
    #[cfg(windows)]
    #[test]
    fn a_mutex_this_launch_may_not_open_still_means_a_host_is_running() {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::Security::{
            InitializeSecurityDescriptor, SetSecurityDescriptorDacl, PSECURITY_DESCRIPTOR,
            SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        };
        use windows::Win32::System::Threading::CreateMutexW;

        let name = format!(r"Local\EternalMonitor-test-elevated-{}", std::process::id());
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let pointer = PSECURITY_DESCRIPTOR(&mut descriptor as *mut _ as *mut _);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: pointer.0,
            bInheritHandle: false.into(),
        };
        let first = unsafe {
            // Revision 1; a present, empty DACL grants nobody access.
            InitializeSecurityDescriptor(pointer, 1).unwrap();
            SetSecurityDescriptorDacl(pointer, true, None, false).unwrap();
            CreateMutexW(Some(&attributes), false, &HSTRING::from(name.as_str())).unwrap()
        };
        assert!(acquire_named(&name).is_none(), "refused means running");
        unsafe {
            let _ = CloseHandle(first);
        }
    }
}
