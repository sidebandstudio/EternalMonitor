//! The stream listens on one UDP port. While another program holds it, often
//! another copy of EternalMonitor, every start of the stream failed until the
//! supervisor gave up and waited for a manual Start (os error 10048). Now the
//! supervisor checks the port before each start with a bind like the
//! stream's, released at once ([`check`]). While the port is held, the
//! Stream page names the program holding it and the supervisor checks again
//! every [`RECHECK`]; the stream starts as soon as the port is free.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Mutex;
use std::time::Duration;

use tracing::{info, warn};

/// How often the supervisor checks a held port again.
pub const RECHECK: Duration = Duration::from_secs(2);

/// The stream port, while another program holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub port: u16,
    pub by: HeldBy,
}

/// Who holds the stream port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeldBy {
    /// Another copy of EternalMonitor, from another folder or user.
    OtherCopy,
    /// What Task Manager calls the program.
    Program(String),
    /// Windows doesn't say which program.
    Unknown,
    /// This host's previous stream, which hasn't let go yet.
    ThisCopy,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

/// The stream port and its holder, while the stream waits for it.
pub fn held() -> Option<Held> {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether the stream may start: binds `port` as the stream does and lets go
/// at once. A port in use is recorded with its holder for the Stream page; a
/// free port clears that. Any other failure is left for the stream's own bind
/// to report.
pub fn check(port: u16) -> bool {
    check_as(port, std::process::id())
}

/// As [`check`], for the host whose process is `own`.
fn check_as(port: u16, own: u32) -> bool {
    match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)) {
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            note(Some(Held {
                port,
                by: holder(port, own),
            }));
            false
        }
        _ => {
            if HELD.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                info!(port, "The stream port is free; starting the stream");
            }
            note(None);
            true
        }
    }
}

/// The stream no longer waits for the port (the user stopped it).
pub(crate) fn forget() {
    note(None);
}

fn note(held: Option<Held>) {
    let mut current = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if *current == held {
        return;
    }
    if let Some(held) = &held {
        let holder = match &held.by {
            HeldBy::OtherCopy => "another copy of EternalMonitor",
            HeldBy::Program(program) => program,
            HeldBy::Unknown => "unknown",
            HeldBy::ThisCopy => "this copy's previous stream",
        };
        warn!(
            port = held.port,
            holder, "Another program is using the stream port; the stream starts once it is free"
        );
    }
    *current = held;
}

/// Who holds `port`, as process `own` sees it.
fn holder(port: u16, own: u32) -> HeldBy {
    let Some(pid) = owner(port) else {
        return HeldBy::Unknown;
    };
    if pid == own {
        return HeldBy::ThisCopy;
    }
    let image = image_path(pid);
    if image.as_deref().is_some_and(is_eternal_monitor) {
        return HeldBy::OtherCopy;
    }
    program_name(pid, image.as_deref()).map_or(HeldBy::Unknown, HeldBy::Program)
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

    /// Serializes the tests that share the recorded holder.
    static HELD_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn a_held_port_is_recorded_with_its_holder_until_it_is_free() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let held = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = held.local_addr().unwrap().port();

        // As another process sees it.
        assert!(!check_as(port, 0), "held");
        let recorded = super::held().expect("named on the Stream page");
        assert_eq!(recorded.port, port);
        if cfg!(windows) {
            // This test binary holds it; it has no description.
            let HeldBy::Program(program) = &recorded.by else {
                panic!("Windows names the program: {recorded:?}");
            };
            assert!(program.to_lowercase().contains("eternal"), "{program}");
            // As this process sees it: its own earlier stream.
            assert!(!check(port));
            assert_eq!(super::held().unwrap().by, HeldBy::ThisCopy);
        } else {
            assert_eq!(recorded.by, HeldBy::Unknown, "only Windows names it");
        }

        drop(held);
        assert!(check_as(port, 0), "free once released");
        assert_eq!(super::held(), None);
    }

    #[test]
    fn a_stop_forgets_the_holder() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let held = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(!check_as(port, 0));
        forget();
        assert_eq!(super::held(), None);
    }

    /// The check binds IPv4 like the stream; a dual-stack IPv6 socket on the
    /// port blocks the stream too, and the check sees it.
    #[cfg(windows)]
    #[test]
    fn a_dual_stack_holder_is_seen() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let socket =
            socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None).unwrap();
        socket.set_only_v6(false).unwrap();
        socket
            .bind(&"[::]:0".parse::<std::net::SocketAddr>().unwrap().into())
            .unwrap();
        let port = socket.local_addr().unwrap().as_socket().unwrap().port();
        assert!(!check_as(port, 0));
        assert!(matches!(super::held().unwrap().by, HeldBy::Program(_)));
        drop(socket);
        assert!(check_as(port, 0));
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
        assert_eq!(program_name(1234, None), None, "unreadable: Unknown");
    }
}
