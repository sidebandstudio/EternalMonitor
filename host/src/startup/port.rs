//! The stream listens on one UDP port. While another program holds it, often
//! another copy of EternalMonitor, every start of the stream failed until the
//! supervisor gave up and waited for a manual Start (os error 10048). Now the
//! supervisor checks the port before each start with a bind like the
//! stream's, released at once ([`check`]). While the port is held, the
//! Stream page names the program holding it and the supervisor checks again
//! every [`RECHECK`]; the stream starts as soon as the port is free.
//!
//! Windows also refuses a bind with 10013 when nothing holds the port: a
//! firewall, security tool or reserved port range. After [`REFUSALS`] checks
//! in a row that find no holder, the page says Windows is blocking the port
//! instead, and the error code is logged once. The checks go on.
//!
//! Logs and uploads say only what kind of program holds the port (see
//! [`category`]); the program's own name is shown on the Stream page alone.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;

use tracing::{info, warn};

/// How often the supervisor checks a held port again.
pub const RECHECK: Duration = Duration::from_secs(2);

/// How long a check waits to learn which program holds the port. Reading an
/// executable on a slow network share can take much longer; its holder is
/// then [`HeldBy::Unknown`].
const LOOKUP: Duration = Duration::from_millis(500);

/// Refused binds in a row, with no program on the port, before the Stream
/// page says Windows is blocking it (about 6 s).
pub const REFUSALS: u32 = 3;

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
    /// Windows itself (the System process).
    Windows,
    /// A service in a Windows service host.
    WindowsService,
    /// What Task Manager calls the program. Shown on the Stream page only,
    /// never logged.
    Program(String),
    /// Windows doesn't say which program.
    Unknown,
    /// This host's previous stream, which hasn't let go yet.
    ThisCopy,
    /// Windows refused the bind and no program has the port, fewer than
    /// [`REFUSALS`] times in a row so far.
    Refused,
    /// Windows refused the bind [`REFUSALS`] times in a row and no program
    /// has the port: a firewall, security tool or reserved port range.
    /// `code` is the bind's error code; `ipv6` is a program on the port over
    /// IPv6 only, which doesn't stop the stream's IPv4 bind by itself.
    Blocked {
        code: Option<i32>,
        ipv6: Option<Box<HeldBy>>,
    },
}

/// What the stream waits for, and how many refused binds in a row found no
/// holder.
struct Waiting {
    held: Option<Held>,
    refusals: u32,
}

static HELD: Mutex<Waiting> = Mutex::new(Waiting {
    held: None,
    refusals: 0,
});

/// The stream port and its holder, while the stream waits for it.
pub fn held() -> Option<Held> {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).held.clone()
}

/// Whether binding the stream port failed because something else has it:
/// another socket on it (10048), or on Windows one that holds it
/// exclusively (`SO_EXCLUSIVEADDRUSE`) or a range Windows reserved (10013).
pub fn is_held(error: &io::Error) -> bool {
    error.kind() == ErrorKind::AddrInUse
        || (cfg!(windows) && error.kind() == ErrorKind::PermissionDenied)
}

/// Whether the stream may start: binds `port` as the stream does and lets go
/// at once. A held port is recorded with its holder for the Stream page; a
/// free port clears that. Any other failure is left for the stream's own bind
/// to report.
pub fn check(port: u16) -> bool {
    check_as(port, std::process::id())
}

/// As [`check`], for the host whose process is `own`.
fn check_as(port: u16, own: u32) -> bool {
    let bound = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port));
    check_with(
        port,
        Bind::of(bound.map(drop)),
        || holder(port, own, Family::V4),
        || holder(port, own, Family::V6),
    )
}

/// How the check's bind went.
#[derive(Clone, Copy, Debug)]
enum Bind {
    /// Free, or failed for a reason the stream's own bind reports.
    Free,
    /// Another socket is on the port (10048).
    InUse,
    /// Windows refused it (10013): a program holds it exclusively, or
    /// something blocks it.
    Refused(Option<i32>),
}

impl Bind {
    fn of(bound: io::Result<()>) -> Self {
        match bound {
            Err(error) if error.kind() == ErrorKind::AddrInUse => Self::InUse,
            Err(error) if is_held(&error) => Self::Refused(error.raw_os_error()),
            _ => Self::Free,
        }
    }
}

/// [`check`] once the bind is done. `v4` and `v6` look up the program on
/// the port in Windows' IPv4 and IPv6 tables, None when it lists none. Only
/// `v4` decides the holder, since the stream binds IPv4 and an IPv6 socket is
/// IPv6-only unless it says otherwise; `v6` is asked only for the blocked
/// notice. They run without [`HELD`], which the Stream page reads every frame.
fn check_with(
    port: u16,
    bind: Bind,
    v4: impl FnOnce() -> Option<HeldBy>,
    v6: impl FnOnce() -> Option<HeldBy>,
) -> bool {
    let refusals = || HELD.lock().unwrap_or_else(|e| e.into_inner()).refusals;
    let (by, refusals) = match bind {
        Bind::Free => {
            let mut waiting = HELD.lock().unwrap_or_else(|e| e.into_inner());
            if waiting.held.is_some() {
                info!(port, "The stream port is free; starting the stream");
            }
            note(&mut waiting, None);
            return true;
        }
        Bind::InUse => (v4().unwrap_or(HeldBy::Unknown), 0),
        Bind::Refused(code) => match v4() {
            Some(by) => (by, 0),
            None => match refusals() + 1 {
                refusals if refusals >= REFUSALS => {
                    let ipv6 = v6().map(Box::new);
                    (HeldBy::Blocked { code, ipv6 }, refusals)
                }
                refusals => (HeldBy::Refused, refusals),
            },
        },
    };
    let mut waiting = HELD.lock().unwrap_or_else(|e| e.into_inner());
    waiting.refusals = refusals;
    note(&mut waiting, Some(Held { port, by }));
    false
}

/// The stream no longer waits for the port (the user stopped it).
pub(crate) fn forget() {
    note(&mut HELD.lock().unwrap_or_else(|e| e.into_inner()), None);
}

fn note(waiting: &mut Waiting, held: Option<Held>) {
    if held.is_none() {
        waiting.refusals = 0;
    }
    let said = |held: &Option<Held>| held.as_ref().map(|held| (held.port, category(&held.by)));
    if let Some(held) = held.as_ref().filter(|_| said(&waiting.held) != said(&held)) {
        let port = held.port;
        match &held.by {
            // Checked again before anything is said.
            HeldBy::Refused => {}
            HeldBy::Blocked { code, .. } => warn!(
                port,
                code, "Windows is blocking the stream port; checking again every 2 s"
            ),
            by => warn!(
                port,
                holder = category(by),
                "Another program is using the stream port; the stream starts once it is free"
            ),
        }
    }
    waiting.held = held;
}

/// What logs and uploads say holds the port. Never a program's own name: a
/// file description or name can carry this user's or another account's.
fn category(by: &HeldBy) -> &'static str {
    match by {
        HeldBy::OtherCopy => "another copy of EternalMonitor",
        HeldBy::Windows => "Windows",
        HeldBy::WindowsService => "a Windows service",
        HeldBy::Program(_) => "another program",
        HeldBy::Unknown => "unknown",
        HeldBy::ThisCopy => "this copy's previous stream",
        HeldBy::Refused => "refused",
        HeldBy::Blocked { .. } => "blocked",
    }
}

/// Which of Windows' UDP tables to look in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    V4,
    V6,
}

/// Who holds `port` in `family`'s table, as process `own` sees it; None
/// when Windows lists no program on it.
fn holder(port: u16, own: u32, family: Family) -> Option<HeldBy> {
    let pid = owner(port, family)?;
    Some(identify(pid, own, |pid| {
        named(pid, image_path(pid).as_deref())
    }))
}

/// Who process `pid` is, as `name` finds out within [`LOOKUP`].
fn identify(pid: u32, own: u32, name: impl FnOnce(u32) -> HeldBy + Send + 'static) -> HeldBy {
    static LOOKING: AtomicBool = AtomicBool::new(false);
    if pid == own {
        return HeldBy::ThisCopy;
    }
    within(&LOOKING, LOOKUP, move || name(pid)).unwrap_or(HeldBy::Unknown)
}

/// Runs `lookup` on its own thread and waits up to `limit` for it; None if
/// it takes longer. While one is still running (`busy`), none is started.
fn within<T: Send + 'static>(
    busy: &'static AtomicBool,
    limit: Duration,
    lookup: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    if busy.swap(true, Ordering::AcqRel) {
        return None;
    }
    let (found_tx, found_rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("port-holder".into())
        .spawn(move || {
            let found = lookup();
            busy.store(false, Ordering::Release);
            let _ = found_tx.send(found);
        });
    if spawned.is_err() {
        busy.store(false, Ordering::Release);
        return None;
    }
    found_rx.recv_timeout(limit).ok()
}

/// Who process `pid`, running `image`, is. Windows' own processes get a
/// plain name rather than "System" or a service host.
fn named(pid: u32, image: Option<&str>) -> HeldBy {
    if pid == 4 {
        return HeldBy::Windows;
    }
    let Some(image) = image else {
        return HeldBy::Unknown;
    };
    if is_eternal_monitor(image) {
        return HeldBy::OtherCopy;
    }
    let file = image.rsplit(['\\', '/']).next().unwrap_or(image);
    if file.eq_ignore_ascii_case("svchost.exe") {
        return HeldBy::WindowsService;
    }
    // What Task Manager shows: the file's description, else its file name.
    HeldBy::Program(description(image).unwrap_or_else(|| file.to_string()))
}

fn is_eternal_monitor(image: &str) -> bool {
    let file = image.rsplit(['\\', '/']).next().unwrap_or(image);
    let file = file.to_lowercase();
    file.starts_with("eternalmonitor") || file.starts_with("eternal-host")
}

/// The process listening on UDP `port` in `family`'s table.
#[cfg(windows)]
fn owner(port: u16, family: Family) -> Option<u32> {
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
    if family == Family::V4 {
        let buffer = table(AF_INET.0)?;
        let table = buffer.as_ptr().cast::<MIB_UDPTABLE_OWNER_PID>();
        let rows = unsafe {
            std::slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize)
        };
        return rows
            .iter()
            .find(|row| row.dwLocalPort & 0xffff == wanted)
            .map(|row| row.dwOwningPid);
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
fn owner(_port: u16, _family: Family) -> Option<u32> {
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

    /// The stream binds IPv4 only. Whether a dual-stack IPv6 socket on the
    /// port blocks it is Windows' choice (a plain one doesn't); the check
    /// agrees with the stream's own bind either way, and names the holder
    /// from the IPv4 table when blocked.
    #[cfg(windows)]
    #[test]
    fn a_dual_stack_holder_blocks_the_check_exactly_when_it_blocks_the_stream() {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Networking::WinSock::{setsockopt, SOCKET, SOL_SOCKET};
        const SO_EXCLUSIVEADDRUSE: i32 = !4;
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        for exclusive in [false, true] {
            let socket =
                socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None).unwrap();
            socket.set_only_v6(false).unwrap();
            if exclusive {
                let on = 1i32.to_ne_bytes();
                let set = unsafe {
                    setsockopt(
                        SOCKET(socket.as_raw_socket() as usize),
                        SOL_SOCKET,
                        SO_EXCLUSIVEADDRUSE,
                        Some(&on),
                    )
                };
                assert_eq!(set, 0);
            }
            socket
                .bind(&"[::]:0".parse::<std::net::SocketAddr>().unwrap().into())
                .unwrap();
            let port = socket.local_addr().unwrap().as_socket().unwrap().port();
            let free = check_as(port, 0);
            // As `transport::start_sender` binds.
            let stream = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)
                .unwrap()
                .bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into());
            assert_eq!(free, stream.is_ok(), "exclusive: {exclusive}");
            if !free {
                // Windows lists the exclusive one in its IPv4 table too.
                assert!(
                    matches!(super::held().unwrap().by, HeldBy::Program(_)),
                    "{:?}",
                    super::held()
                );
            }
            eprintln!(
                "dual-stack, exclusive {exclusive}: blocks the stream: {}",
                !free
            );
            drop(socket);
            assert!(check_as(port, 0));
        }
    }

    /// Log lines `run` writes on this thread.
    fn logged(run: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Lines(std::sync::Arc<Mutex<Vec<u8>>>);
        impl io::Write for Lines {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let lines = Lines::default();
        let writer = lines.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        let bytes = lines.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    /// Windows refused the bind (10013) and a program is on the port: the
    /// Stream page names it, as for 10048, however long it stays. The log
    /// says only "another program".
    #[test]
    fn a_refused_bind_with_a_holder_names_it() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let vlc = || Some(HeldBy::Program("VLC media player".into()));
        let log = logged(|| {
            for _ in 0..REFUSALS * 3 {
                assert!(!check_with(9876, Bind::Refused(Some(10013)), vlc, || None));
                assert_eq!(super::held().unwrap().by, vlc().unwrap());
            }
        });
        assert_eq!(log.matches("another program").count(), 1, "{log}");
        assert!(!log.contains("VLC"), "{log}");
        assert!(!log.contains("blocking"), "{log}");
        assert!(check_with(9876, Bind::Free, || None, || None));
        assert_eq!(super::held(), None);
    }

    /// Windows refused the bind and no program is on the port: nothing is
    /// said for the first checks, then that Windows blocks it, logged once
    /// with the error code. The checks go on until the bind succeeds.
    #[test]
    fn a_refused_bind_without_a_holder_says_windows_blocks_it() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let refused = Bind::Refused(Some(10013));
        let blocked = HeldBy::Blocked {
            code: Some(10013),
            ipv6: None,
        };
        let log = logged(|| {
            for check in 1..=REFUSALS * 3 {
                assert!(
                    !check_with(9876, refused, || None, || None),
                    "still waiting"
                );
                let by = super::held().unwrap().by;
                if check < REFUSALS {
                    assert_eq!(by, HeldBy::Refused, "check {check}");
                } else {
                    assert_eq!(by, blocked, "check {check}");
                }
            }
        });
        assert_eq!(log.matches("Windows is blocking").count(), 1, "{log}");
        assert!(log.contains("10013"), "{log}");
        assert!(!log.contains("Another program"), "{log}");

        // A holder in between starts the count again.
        assert!(!check_with(
            9876,
            refused,
            || Some(HeldBy::Unknown),
            || None
        ));
        assert!(!check_with(9876, refused, || None, || None));
        assert_eq!(super::held().unwrap().by, HeldBy::Refused);
        // So does a free port.
        for _ in 0..REFUSALS {
            check_with(9876, refused, || None, || None);
        }
        assert_eq!(super::held().unwrap().by, blocked);
        assert!(check_with(9876, Bind::Free, || None, || None));
        assert_eq!(super::held(), None);
        assert!(!check_with(9876, refused, || None, || None));
        assert_eq!(super::held().unwrap().by, HeldBy::Refused);
        forget();
        assert_eq!(super::held(), None);
    }

    /// 10048 is held on every system; 10013 only on Windows, keeping its code.
    #[test]
    fn bind_errors_are_sorted_as_the_stream_s_bind_would_be() {
        assert!(matches!(
            Bind::of(Err(io::Error::from(ErrorKind::AddrInUse))),
            Bind::InUse
        ));
        assert!(matches!(
            Bind::of(Err(io::Error::from(ErrorKind::AddrNotAvailable))),
            Bind::Free
        ));
        let refused = Bind::of(Err(io::Error::from(ErrorKind::PermissionDenied)));
        assert_eq!(matches!(refused, Bind::Refused(_)), cfg!(windows));
        #[cfg(windows)]
        assert!(matches!(
            Bind::of(Err(io::Error::from_raw_os_error(10013))),
            Bind::Refused(Some(10013))
        ));
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
        assert_eq!(named(4, None), HeldBy::Windows);
        assert_eq!(
            named(1234, Some(r"C:\Windows\System32\svchost.exe")),
            HeldBy::WindowsService
        );
        assert_eq!(
            named(
                1234,
                Some(r"C:\Program Files\EternalMonitor\EternalMonitor.exe")
            ),
            HeldBy::OtherCopy
        );
        assert_eq!(named(1234, None), HeldBy::Unknown, "unreadable");
    }

    /// Windows' IPv6 sockets are IPv6-only unless they say otherwise, so a
    /// program on the port over IPv6 doesn't explain a refused IPv4 bind: the
    /// port is still blocked from the third check, with that program named
    /// beside it. It is asked for only then, and never for 10048.
    #[test]
    fn an_ipv6_listener_does_not_hide_a_blocked_port() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let refused = Bind::Refused(Some(10013));
        let asked = std::cell::Cell::new(0);
        let steam = || {
            asked.set(asked.get() + 1);
            Some(HeldBy::Program("Steam".into()))
        };
        let log = logged(|| {
            for check in 1..=REFUSALS * 3 {
                assert!(!check_with(9876, refused, || None, steam));
                let by = super::held().unwrap().by;
                if check < REFUSALS {
                    assert_eq!(by, HeldBy::Refused, "check {check}");
                    assert_eq!(asked.get(), 0, "check {check}");
                } else {
                    assert_eq!(
                        by,
                        HeldBy::Blocked {
                            code: Some(10013),
                            ipv6: Some(Box::new(HeldBy::Program("Steam".into()))),
                        },
                        "check {check}"
                    );
                }
            }
        });
        assert_eq!(log.matches("Windows is blocking").count(), 1, "{log}");
        assert!(!log.contains("Steam"), "{log}");

        let asked_before = asked.get();
        assert!(!check_with(9876, Bind::InUse, || None, steam));
        assert_eq!(super::held().unwrap().by, HeldBy::Unknown);
        assert_eq!(asked.get(), asked_before, "IPv6 isn't asked for 10048");
        forget();
    }

    /// Logs say what kind of program holds the port, never its name, on
    /// every path: 10048, a refused bind with a holder, and the IPv6 program
    /// beside a blocked port.
    #[test]
    fn no_program_name_reaches_a_log_line() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let alice = || Some(HeldBy::Program("AliceTool.exe".into()));
        let bob = || Some(HeldBy::Program("Bob's Helper".into()));
        let refused = Bind::Refused(Some(10013));
        let log = logged(|| {
            check_with(9876, Bind::InUse, alice, || None);
            check_with(9876, Bind::InUse, bob, || None);
            check_with(9876, Bind::Free, || None, || None);
            check_with(9876, refused, bob, || None);
            check_with(9876, Bind::Free, || None, || None);
            for _ in 0..REFUSALS {
                check_with(9876, refused, || None, alice);
            }
            for by in [
                HeldBy::OtherCopy,
                HeldBy::Windows,
                HeldBy::WindowsService,
                HeldBy::Unknown,
            ] {
                check_with(9876, Bind::InUse, || Some(by), || None);
            }
            forget();
        });
        assert!(!log.contains("Alice"), "{log}");
        assert!(!log.contains("Bob"), "{log}");
        for category in [
            "another program",
            "another copy of EternalMonitor",
            "Windows",
            "a Windows service",
            "unknown",
        ] {
            let field = format!("holder=\"{category}\"");
            assert!(log.contains(&field), "{field}: {log}");
        }
        assert!(log.contains("Windows is blocking"), "{log}");
    }

    /// A program whose file is slow to read (a network share) is Unknown
    /// after [`LOOKUP`], and no second lookup starts while it runs.
    #[test]
    fn a_slow_lookup_is_unknown_after_half_a_second() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let by = identify(1234, 0, move |_| {
            let _ = release_rx.recv();
            HeldBy::Program("Slow".into())
        });
        let took = started.elapsed();
        assert_eq!(by, HeldBy::Unknown);
        assert!(took >= LOOKUP && took < LOOKUP * 4, "{took:?}");

        let ran = std::sync::Arc::new(AtomicBool::new(false));
        let started = std::time::Instant::now();
        let by = identify(1234, 0, {
            let ran = ran.clone();
            move |_| {
                ran.store(true, Ordering::SeqCst);
                HeldBy::Program("Quick".into())
            }
        });
        assert_eq!(by, HeldBy::Unknown, "the slow one still runs");
        assert!(started.elapsed() < LOOKUP, "didn't wait");
        assert!(!ran.load(Ordering::SeqCst), "no second lookup");

        drop(release_tx);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while identify(1234, 0, |_| HeldBy::Program("Quick".into())) == HeldBy::Unknown {
            assert!(std::time::Instant::now() < deadline, "the slow one ends");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(identify(7, 7, |_| HeldBy::Unknown), HeldBy::ThisCopy);
    }

    /// The Stream page reads the holder every frame; a check whose lookup is
    /// slow doesn't make it wait.
    #[test]
    fn the_stream_page_reads_the_holder_while_a_lookup_is_slow() {
        let _serial = HELD_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        check_with(9876, Bind::InUse, || Some(HeldBy::Unknown), || None);
        let (looking_tx, looking_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let checker = thread::spawn(move || {
            check_with(
                9876,
                Bind::InUse,
                || {
                    looking_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                    Some(HeldBy::OtherCopy)
                },
                || None,
            )
        });
        looking_rx.recv().unwrap();
        let (read_tx, read_rx) = mpsc::channel();
        thread::spawn(move || read_tx.send(super::held()).unwrap());
        let read = read_rx.recv_timeout(Duration::from_secs(1));
        drop(release_tx);
        assert!(!checker.join().unwrap());
        assert_eq!(
            read.expect("read during the lookup").unwrap().by,
            HeldBy::Unknown
        );
        assert_eq!(super::held().unwrap().by, HeldBy::OtherCopy);
        forget();
    }
}
