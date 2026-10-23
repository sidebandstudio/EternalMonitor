//! The stream listens on one UDP port. While another program holds it, often
//! another copy of EternalMonitor, every start of the stream fails until the
//! supervisor gives up and waits for a manual Start (problem 11: os error
//! 10048). Whatever binds the stream port passes the result through
//! [`watch_bind`]. A port in use is named on the Stream page and watched
//! until it is free; then the stream starts by itself, unless the user
//! stopped it meanwhile. A bind that succeeds ends the conflict.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
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

/// The conflict being watched. Its watcher stops when the id changes: a
/// successful bind ended it.
struct Conflict {
    holder: Holder,
    id: u64,
}

static CONFLICT: Mutex<Option<Conflict>> = Mutex::new(None);
static NEXT_CONFLICT: AtomicU64 = AtomicU64::new(1);
/// The user's last choice: streaming on (at launch, Start, Restart) or off
/// (Stop). A port that frees up starts the stream only while it is on.
static WANTED: AtomicBool = AtomicBool::new(true);
/// Starts the stream; `main` installs it.
static STARTER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Who holds the stream port, while someone else does.
pub fn holder() -> Option<Holder> {
    let conflict = CONFLICT.lock().ok()?;
    conflict.as_ref().map(|conflict| conflict.holder.clone())
}

/// Whether the stream starts by itself once the held port is free.
pub fn starts_when_free() -> bool {
    holder().is_some() && WANTED.load(Ordering::SeqCst)
}

/// How to start the stream once a held port is free.
pub fn on_free(start: impl Fn() + Send + Sync + 'static) {
    let _ = STARTER.set(Box::new(start));
}

/// The user clicked Start or Restart.
pub fn user_started() {
    WANTED.store(true, Ordering::SeqCst);
}

/// The user clicked Stop: a port that frees up later leaves the stream off.
pub fn user_stopped() {
    WANTED.store(false, Ordering::SeqCst);
}

/// The one call for whatever binds the stream port: pass the bind's result
/// through. A port in use is reported and watched; a successful bind means
/// this host holds the port, which ends any conflict.
pub fn watch_bind<T>(port: u16, bound: io::Result<T>) -> io::Result<T> {
    watch_bind_as(port, std::process::id(), bound, || {
        if let Some(start) = STARTER.get() {
            start();
        }
    })
}

/// As [`watch_bind`], for the host whose process is `own`.
fn watch_bind_as<T>(
    port: u16,
    own: u32,
    bound: io::Result<T>,
    start: impl Fn() + Send + 'static,
) -> io::Result<T> {
    match &bound {
        Ok(_) => end_conflict(),
        Err(error) if error.kind() == ErrorKind::AddrInUse => report(port, own, start),
        Err(_) => {}
    }
    bound
}

fn end_conflict() {
    if let Ok(mut conflict) = CONFLICT.lock() {
        if conflict.take().is_some() {
            info!("The stream port is free again and the stream holds it");
        }
    }
}

fn report(port: u16, own: u32, start: impl Fn() + Send + 'static) {
    // Released meanwhile: the next bind succeeds.
    let Some(held) = probe(port, own) else {
        return;
    };
    let Ok(mut conflict) = CONFLICT.lock() else {
        return;
    };
    if let Some(current) = conflict.as_mut() {
        current.holder = held;
        return;
    }
    warn!(
        port,
        program = held.program.as_deref().unwrap_or("unknown"),
        "Another program is using the stream port; iPads cannot connect until it closes"
    );
    let id = NEXT_CONFLICT.fetch_add(1, Ordering::SeqCst);
    *conflict = Some(Conflict { holder: held, id });
    drop(conflict);
    let watcher = std::thread::Builder::new()
        .name("port-watch".into())
        .spawn(move || watch(port, own, id, start));
    if let Err(error) = watcher {
        warn!(%error, "Could not watch the stream port");
    }
}

/// Re-checks the port until it is free or a bind ends the conflict.
fn watch(port: u16, own: u32, id: u64, start: impl Fn()) {
    loop {
        std::thread::sleep(RECHECK);
        let held = probe(port, own);
        let Ok(mut conflict) = CONFLICT.lock() else {
            return;
        };
        let Some(current) = conflict.as_mut().filter(|current| current.id == id) else {
            return;
        };
        if let Some(held) = held {
            current.holder = held;
            continue;
        }
        *conflict = None;
        drop(conflict);
        if WANTED.load(Ordering::SeqCst) {
            info!(port, "The stream port is free; starting the stream");
            start();
        } else {
            info!(
                port,
                "The stream port is free; the stream stays off, as chosen"
            );
        }
        return;
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

    /// Serializes the tests that share the conflict and the user's choice.
    static CONFLICT_TESTS: Mutex<()> = Mutex::new(());

    fn held_port() -> (UdpSocket, u16) {
        let held = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        (held, port)
    }

    /// The stream's bind, as another process sees it (`own` = 0).
    fn bind_stream(port: u16, starts: &std::sync::mpsc::Sender<()>) -> io::Result<UdpSocket> {
        let starts = starts.clone();
        watch_bind_as(
            port,
            0,
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)),
            move || starts.send(()).unwrap(),
        )
    }

    /// Sol's race: the port was free when the stream last checked, then a
    /// competitor took it before the bind. The bind reports it; once the
    /// competitor lets go, the stream starts by itself.
    #[test]
    fn a_port_taken_before_the_bind_is_named_and_the_stream_starts_once_free() {
        let _serial = CONFLICT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        user_started();
        let (competitor, port) = held_port();
        let (starts, started) = std::sync::mpsc::channel();
        assert!(bind_stream(port, &starts).is_err());
        assert_eq!(
            holder().map(|h| h.port),
            Some(port),
            "named on the Stream page"
        );
        assert!(starts_when_free());
        assert!(started.recv_timeout(RECHECK * 2).is_err(), "still held");
        drop(competitor);
        started.recv_timeout(RECHECK * 3).expect("starts once free");
        assert_eq!(holder(), None);
    }

    /// Sol's sequence: during a conflict a supervisor retry binds the port
    /// and streams, then the user clicks Stop. The freed port must not start
    /// the stream again.
    #[test]
    fn stop_after_a_retry_got_the_port_is_not_undone() {
        let _serial = CONFLICT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        user_started();
        let (competitor, port) = held_port();
        let (starts, started) = std::sync::mpsc::channel();
        assert!(bind_stream(port, &starts).is_err());
        drop(competitor);
        let stream = bind_stream(port, &starts).expect("the retry binds");
        assert_eq!(holder(), None, "the bind ended the conflict");
        user_stopped();
        drop(stream);
        assert!(started.recv_timeout(RECHECK * 3).is_err(), "Stop stays");
        user_started();
    }

    /// A Stop during the conflict cancels the automatic start; a later
    /// Start brings it back.
    #[test]
    fn stop_during_a_conflict_cancels_the_automatic_start() {
        let _serial = CONFLICT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        user_started();
        let (competitor, port) = held_port();
        let (starts, started) = std::sync::mpsc::channel();
        assert!(bind_stream(port, &starts).is_err());
        user_stopped();
        assert!(!starts_when_free(), "the Stream page says to click Start");
        drop(competitor);
        assert!(started.recv_timeout(RECHECK * 3).is_err(), "stays off");
        assert_eq!(holder(), None);

        let (competitor, port) = held_port();
        assert!(bind_stream(port, &starts).is_err());
        user_stopped();
        user_started();
        drop(competitor);
        started.recv_timeout(RECHECK * 3).expect("Start, then free");
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
