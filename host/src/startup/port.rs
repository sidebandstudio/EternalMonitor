//! The stream listens on one UDP port. While another program holds it, often
//! another copy of EternalMonitor, every start of the stream fails until the
//! supervisor gives up and waits for a manual Start (problem 11: os error
//! 10048). The port is checked before the stream starts, so the window can
//! name the program, and watched until it is free; then the stream starts by
//! itself.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Mutex;
use std::time::Duration;

use tracing::{info, warn};

const RECHECK: Duration = Duration::from_secs(2);

/// The program holding the stream port.
#[derive(Clone, Debug, PartialEq)]
pub struct Holder {
    pub port: u16,
    /// What Windows calls the program, when it says.
    pub program: Option<String>,
    /// Another copy of EternalMonitor, from another folder or user.
    pub eternal_monitor: bool,
}

static HOLDER: Mutex<Option<Holder>> = Mutex::new(None);

/// Who holds the stream port, while someone else does.
pub fn holder() -> Option<Holder> {
    HOLDER.lock().ok()?.clone()
}

/// Check the port before the stream starts. When another program holds it,
/// keep checking, and call `free` once it is free.
pub fn check_at_startup(port: u16, free: impl FnOnce() + Send + 'static) {
    watch(port, std::process::id(), free);
}

/// As [`check_at_startup`], for the host whose process is `own`.
fn watch(port: u16, own: u32, free: impl FnOnce() + Send + 'static) {
    let Some(held) = probe(port, own) else {
        return;
    };
    warn!(
        port,
        program = held.program.as_deref().unwrap_or("unknown"),
        "Another program is using the stream port; iPads cannot connect until it closes"
    );
    set_holder(Some(held));
    let watch = std::thread::Builder::new()
        .name("port-watch".into())
        .spawn(move || loop {
            std::thread::sleep(RECHECK);
            match probe(port, own) {
                Some(held) => set_holder(Some(held)),
                None => {
                    set_holder(None);
                    info!(port, "The stream port is free; starting the stream");
                    free();
                    return;
                }
            }
        });
    if let Err(error) = watch {
        warn!(%error, "Could not watch the stream port");
    }
}

fn set_holder(held: Option<Holder>) {
    if let Ok(mut holder) = HOLDER.lock() {
        *holder = held;
    }
}

/// Who holds `port`, if anyone but process `own` (whose stream may hold it
/// by now). Binds as the stream does, so it fails exactly when the stream
/// would.
fn probe(port: u16, own: u32) -> Option<Holder> {
    match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)) {
        Ok(_) => None,
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            let owner = owner(port);
            if owner == Some(own) {
                return None;
            }
            let image = owner.and_then(image_path);
            Some(Holder {
                port,
                eternal_monitor: image.as_deref().is_some_and(is_eternal_monitor),
                program: owner.and_then(|pid| program_name(pid, image.as_deref())),
            })
        }
        Err(_) => None,
    }
}

fn is_eternal_monitor(image: &str) -> bool {
    let file = image.rsplit(['\\', '/']).next().unwrap_or(image);
    let file = file.to_lowercase();
    file.starts_with("eternalmonitor") || file.starts_with("eternal-host")
}

/// The process listening on UDP `port`, IPv4 or dual-stack IPv6.
#[cfg(windows)]
fn owner(port: u16) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedUdpTable, MIB_UDP6TABLE_OWNER_PID, MIB_UDPTABLE_OWNER_PID, UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    fn table(family: u16) -> Option<Vec<u8>> {
        let mut size = 0u32;
        // Sized first; the table can grow between the calls.
        for _ in 0..4 {
            let mut buffer = vec![0u8; size as usize];
            let pointer = (!buffer.is_empty()).then(|| buffer.as_mut_ptr().cast());
            let result = unsafe {
                GetExtendedUdpTable(
                    pointer,
                    &mut size,
                    false,
                    u32::from(family),
                    UDP_TABLE_OWNER_PID,
                    0,
                )
            };
            if result == 0 && pointer.is_some() {
                return Some(buffer);
            }
        }
        None
    }

    let wanted = u32::from(port.to_be());
    if let Some(buffer) = table(AF_INET.0) {
        let table = buffer.as_ptr().cast::<MIB_UDPTABLE_OWNER_PID>();
        let rows = unsafe {
            std::slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize)
        };
        if let Some(row) = rows.iter().find(|row| row.dwLocalPort & 0xffff == wanted) {
            return Some(row.dwOwningPid);
        }
    }
    let buffer = table(AF_INET6.0)?;
    let table = buffer.as_ptr().cast::<MIB_UDP6TABLE_OWNER_PID>();
    let rows = unsafe {
        std::slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize)
    };
    rows.iter()
        .find(|row| row.dwLocalPort & 0xffff == wanted)
        .map(|row| row.dwOwningPid)
}

#[cfg(not(windows))]
fn owner(_port: u16) -> Option<u32> {
    None
}

#[cfg(windows)]
fn image_path(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut path = vec![0u16; 32_768];
        let mut size = path.len() as u32;
        let named = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        named.ok()?;
        Some(String::from_utf16_lossy(&path[..size as usize]))
    }
}

#[cfg(not(windows))]
fn image_path(_pid: u32) -> Option<String> {
    None
}

/// The name Task Manager shows: the file's description, else its file name.
/// Windows' own processes get a plain name rather than "System" or a
/// service host.
fn program_name(pid: u32, image: Option<&str>) -> Option<String> {
    if pid == 4 {
        return Some("Windows".into());
    }
    let image = image?;
    let file = image.rsplit(['\\', '/']).next().unwrap_or(image);
    if file.eq_ignore_ascii_case("svchost.exe") {
        return Some("A Windows service".into());
    }
    description(image).or_else(|| Some(file.to_string()))
}

#[cfg(windows)]
fn description(image: &str) -> Option<String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    let path = HSTRING::from(image);
    unsafe {
        let size = GetFileVersionInfoSizeW(&path, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&path, 0, size, data.as_mut_ptr().cast()).ok()?;
        let query = |key: &str| -> Option<(*const u8, u32)> {
            let mut value = std::ptr::null_mut();
            let mut length = 0u32;
            VerQueryValueW(
                data.as_ptr().cast(),
                PCWSTR(HSTRING::from(key).as_ptr()),
                &mut value,
                &mut length,
            )
            .as_bool()
            .then_some((value.cast::<u8>().cast_const(), length))
        };
        // The first language and code page the file lists.
        let (translation, length) = query(r"\VarFileInfo\Translation")?;
        if length < 4 {
            return None;
        }
        let language = translation.cast::<u16>().read_unaligned();
        let code_page = translation.cast::<u16>().add(1).read_unaligned();
        let (text, length) = query(&format!(
            r"\StringFileInfo\{language:04x}{code_page:04x}\FileDescription"
        ))?;
        let text = std::slice::from_raw_parts(text.cast::<u16>(), length as usize);
        let text = String::from_utf16_lossy(text);
        let text = text.trim_end_matches('\0').trim();
        (!text.is_empty()).then(|| text.to_string())
    }
}

#[cfg(not(windows))]
fn description(_image: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_another_process_holds_is_reported_and_this_one_s_is_not() {
        let held = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        // As seen by another process.
        let holder = probe(port, 0).expect("the port is held");
        assert_eq!(holder.port, port);
        if cfg!(windows) {
            // This test binary holds it.
            let program = holder.program.expect("Windows names the program");
            assert!(program.to_lowercase().contains("eternal"), "{program}");
        }
        // As seen by this process, once its own stream holds the port.
        if cfg!(windows) {
            assert_eq!(probe(port, std::process::id()), None);
        }
        drop(held);
        assert_eq!(probe(port, 0), None, "free once released");
    }

    #[test]
    fn a_held_port_is_watched_until_it_is_free() {
        let held = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        // As another process sees this test's port.
        watch(port, 0, move || tx.send(()).unwrap());
        assert_eq!(holder().map(|h| h.port), Some(port));
        assert!(rx.recv_timeout(RECHECK * 2).is_err(), "still held");
        drop(held);
        rx.recv_timeout(RECHECK * 3).expect("starts once free");
        assert_eq!(holder(), None);
    }

    #[test]
    fn another_eternal_monitor_is_recognized_by_its_file() {
        assert!(is_eternal_monitor(
            r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe"
        ));
        assert!(is_eternal_monitor(
            r"D:\build\target\debug\eternal-host.exe"
        ));
        assert!(!is_eternal_monitor(r"C:\Program Files\VLC\vlc.exe"));
        assert_eq!(program_name(4, None).as_deref(), Some("Windows"));
        assert_eq!(
            program_name(1234, Some(r"C:\Windows\System32\svchost.exe")).as_deref(),
            Some("A Windows service")
        );
    }
}
