//! The failure reporter: a second copy of the host, started with
//! `--failure-reporter`, that outlives the main process. A native crash leaves
//! only a fixed record behind; the reporter resolves the faulting module while
//! the crashed process waits, writes the log line and the upload event, turns
//! the virtual display off and shows the message. A host that ends past the
//! exception filter, as fail-fast exceptions and some heap corruption do, is
//! reported by its exit code. Panics and startup errors hand it just the
//! message. A message never holds up the cleanup or the host's exit; the
//! reporter itself stays until its messages are closed.
//!
//! Each launch's files in the crash folder are named by the host's process:
//! `.rec` the crash record, `.box` a message to show, `.ready` the reporter's
//! own id once it watches the host, `.shown` once the window works,
//! `.done` once the host quit normally or reported its failure itself, and
//! `.display` when it quit without turning the virtual display off.

use super::process::ProcessId;
#[cfg(windows)]
use super::record;
#[cfg(any(windows, test))]
use super::record::Record;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const REPORTER_ARG: &str = "--failure-reporter";
pub(super) const RECORD: &str = "rec";
pub(super) const REQUEST: &str = "box";
const READY: &str = "ready";
pub(super) const SHOWN: &str = "shown";
pub(super) const DONE: &str = "done";
pub(super) const DISPLAY: &str = "display";
/// How long a host that fails at once waits for its reporter to start.
const READY_WAIT: Duration = Duration::from_secs(3);
/// How long the crash message waits for the virtual display to turn off.
#[cfg(windows)]
const CLEANUP_WAIT: Duration = Duration::from_secs(5);
/// How long the reporter waits for the turn-off at most. This ends the wait,
/// not the turn-off: with no message open the reporter then exits, which ends
/// the turn-off too; while a message keeps the reporter open the turn-off may
/// run on. The driver task inside it gives up after 20 s (`vdd::run_task`).
#[cfg(windows)]
const CLEANUP_CAP: Duration = Duration::from_secs(30);

static REPORTER: OnceLock<ProcessId> = OnceLock::new();
/// The crash folder and this host, once a reporter was started for it.
static FILES: OnceLock<(PathBuf, ProcessId)> = OnceLock::new();

pub(super) fn owner_file(directory: &Path, owner: ProcessId, kind: &str) -> PathBuf {
    directory.join(format!("{owner}.{kind}"))
}

/// Start the reporter for this process. Messages fall back to an in-process
/// box when it cannot start.
#[cfg(windows)]
pub(super) fn start(directory: &Path, owner: ProcessId, launch_id: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // The reporter needs no handle of the host's, and must not keep one open
    // (a socket, a driver object) after the host ends.
    let spawned = directory
        .to_str()
        .ok_or_else(|| std::io::Error::other("the crash folder path is not Unicode"))
        .and_then(|directory| {
            super::process::spawn_detached(
                &exe,
                &[
                    REPORTER_ARG,
                    &owner.pid.to_string(),
                    &owner.start.to_string(),
                    directory,
                    launch_id,
                ],
            )
        });
    match spawned {
        Ok(id) => {
            let _ = REPORTER.set(id);
            let _ = FILES.set((directory.to_path_buf(), owner));
            tracing::info!(pid = id.pid, "Failure reporter started");
        }
        Err(error) => tracing::warn!(%error, "Could not start the failure reporter"),
    }
}

/// Tell the reporter how this launch is going (see the module notes).
pub(super) fn mark(kind: &str) {
    if let Some((directory, owner)) = FILES.get() {
        let _ = fs::write(owner_file(directory, *owner, kind), b"");
    }
}

/// Ask the reporter to show a message. False when no reporter is watching
/// this process, so the caller shows it itself.
pub(super) fn request_message(title: &str, body: &str) -> bool {
    let (Some(reporter), Some((directory, owner))) = (REPORTER.get(), FILES.get()) else {
        return false;
    };
    wait_until_ready(&owner_file(directory, *owner, READY), *reporter, READY_WAIT)
        && write_request(&owner_file(directory, *owner, REQUEST), title, body).is_ok()
}

/// A startup error can end the host before its reporter watches it; a
/// request written then would never be read.
fn wait_until_ready(ready: &Path, reporter: ProcessId, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if !reporter.is_running() {
            return false;
        }
        if ready.exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether a reporter is still handling the files of `owner`.
pub(super) fn still_reporting(directory: &Path, owner: ProcessId) -> bool {
    fs::read_to_string(owner_file(directory, owner, READY))
        .ok()
        .and_then(|text| ProcessId::from_name(text.trim()))
        .is_some_and(ProcessId::is_running)
}

/// The failure reporters watching hosts of this folder, by their ready files.
#[cfg(windows)]
fn reporters(directory: &Path) -> Vec<ProcessId> {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == READY))
        .filter_map(|entry| ProcessId::from_name(fs::read_to_string(entry.path()).ok()?.trim()))
        .collect()
}

fn write_request(path: &Path, title: &str, body: &str) -> std::io::Result<()> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, format!("{}\n{body}", title.replace('\n', " ")))?;
    fs::rename(temp, path)
}

pub(super) fn take_request(path: &Path) -> Option<(String, String)> {
    let text = fs::read_to_string(path).ok()?;
    let _ = fs::remove_file(path);
    let (title, body) = text.split_once('\n')?;
    Some((title.to_string(), body.to_string()))
}

/// What to report once the host has ended, when the reporter has not already
/// reported its crash: a crash record it left, or else its exit code. Nothing
/// after a normal quit, a failure the host reported itself, or while Windows
/// ends the session and stops every program.
#[cfg(any(windows, test))]
fn after_exit(
    record: Option<Record>,
    done: bool,
    exit_code: Option<u32>,
    window_shown: bool,
    session_ending: bool,
) -> Option<Record> {
    if record.is_some() {
        return record;
    }
    if done || exit_code == Some(0) || session_ending {
        return None;
    }
    // Thread 0 marks an exit code; code 0 one that could not be read.
    Some(Record {
        code: exit_code.unwrap_or(0),
        address: 0,
        thread: 0,
        window_shown,
    })
}

/// Write the log line and the upload event. Both must succeed before the
/// record is removed; otherwise the next launch reports it again.
#[cfg(windows)]
fn save(outcome: &super::Outcome, launch_id: &str) -> std::io::Result<()> {
    let logged = crate::logging::append_now(&outcome.log_line());
    let queued = if launch_id == "-" {
        Ok(())
    } else {
        crate::telemetry::queue_for_launch(launch_id, 1, &outcome.event(), outcome.level())
    };
    logged.and(queued)
}

/// The launch is handled. Its crash record stays when the report could not be
/// saved, so the next launch saves and shows it. The ready file stays while
/// this reporter waits for its messages, so it is not taken for a host.
#[cfg(any(windows, test))]
fn remove_files(directory: &Path, owner: ProcessId, saved: bool) {
    for kind in [REQUEST, "tmp", DONE, SHOWN, DISPLAY] {
        let _ = fs::remove_file(owner_file(directory, owner, kind));
    }
    if saved {
        let _ = fs::remove_file(owner_file(directory, owner, RECORD));
    }
}

#[cfg(windows)]
fn read_record(path: &Path) -> Option<(Record, bool)> {
    record::parse(&fs::read(path).ok()?)
}

/// Run as the reporter when the command line asks for it; never returns then.
pub fn run_if_requested() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(REPORTER_ARG) {
        std::process::exit(run(&args[2..]));
    }
}

#[cfg(not(windows))]
fn run(_args: &[String]) -> i32 {
    0
}

#[cfg(windows)]
fn run(args: &[String]) -> i32 {
    use std::io::Write;
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};
    let [pid, start, directory, launch_id] = args else {
        return 2;
    };
    let (Ok(pid), Ok(start)) = (pid.parse(), start.parse()) else {
        return 2;
    };
    let owner = ProcessId { pid, start };
    let directory = PathBuf::from(directory);
    let file = |kind| owner_file(&directory, owner, kind);
    let process = open_host(owner);
    let _ = fs::write(file(READY), ProcessId::current().to_string());

    // Each message on its own thread: an open message never holds the
    // watching, the report or the cleanup. Without a thread, a message waits
    // until everything else is done.
    let mut messages = Vec::new();
    let mut waiting = Vec::new();
    let mut show_soon = |title: String, body: String| {
        let (thread_title, thread_body) = (title.clone(), body.clone());
        match std::thread::Builder::new()
            .name("failure-message".into())
            .spawn(move || show(&thread_title, &thread_body))
        {
            Ok(thread) => messages.push(thread),
            Err(error) => {
                note(owner, &format!("could not start a message thread: {error}"));
                waiting.push((title, body));
            }
        }
    };
    let mut report = None;
    if let Some(process) = process {
        loop {
            let exited = unsafe { WaitForSingleObject(process, 100) } == WAIT_OBJECT_0;
            if let Some((title, body)) = take_request(&file(REQUEST)) {
                show_soon(title, body);
            }
            if let Some((record, false)) = read_record(&file(RECORD)) {
                let module = module_at(process, record.address);
                let outcome = super::Outcome::new(
                    record,
                    module.as_ref().map(|(n, o)| (n.as_str(), *o)),
                    false,
                );
                let saved = save(&outcome, launch_id);
                // Release the crashed process whether or not the saves
                // worked; the record stays until they do.
                if let Ok(mut crash) = fs::OpenOptions::new().append(true).open(file(RECORD)) {
                    let _ = crash.write_all(record::ACK);
                }
                // It ends once its filter sees the acknowledgement; a process
                // hung by its own fault does not hold the cleanup for long.
                unsafe {
                    WaitForSingleObject(process, 10_000);
                }
                report = Some((outcome, saved));
                break;
            }
            if exited {
                break;
            }
        }
    }
    // Also when the host ended before this process could watch it.
    if let Some((title, body)) = take_request(&file(REQUEST)) {
        show_soon(title, body);
    }
    let exit_code = process.and_then(|process| {
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(process, &mut code) }
            .is_ok()
            .then_some(code)
    });
    if let Some(process) = process {
        unsafe {
            let _ = CloseHandle(process);
        }
    }

    // After any abnormal end, whatever the report says, turn the virtual
    // display off while the report is saved. The crash message waits for it
    // at most CLEANUP_WAIT, and this process at most CLEANUP_CAP.
    let cleanup_started = Instant::now();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    if cleans_up(exit_code, file(DISPLAY).exists()) {
        let reporters = reporters(&directory);
        let thread_reporters = reporters.clone();
        let spawned = std::thread::Builder::new()
            .name("display-cleanup".into())
            .spawn(move || {
                clean_up(owner, &thread_reporters);
                let _ = done_tx.send(());
            });
        // The sender went with the closure, so the waits below end at once.
        if let Err(error) = spawned {
            note(
                owner,
                &format!("could not start the cleanup thread: {error}"),
            );
            clean_up(owner, &reporters);
        }
    } else {
        // A normal quit turned the display off: the waits below end at once.
        drop(done_tx);
    }

    if report.is_none() {
        let left = read_record(&file(RECORD)).map(|(record, _)| record);
        let session_ending = unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) } != 0;
        if let Some(record) = after_exit(
            left,
            file(DONE).exists(),
            exit_code,
            file(SHOWN).exists(),
            session_ending,
        ) {
            let mut bytes = [0u8; record::RECORD_LEN];
            record::format(record, &mut bytes);
            // Kept until the saves succeed, for the next launch.
            let kept = if left.is_some() {
                Ok(())
            } else {
                fs::write(file(RECORD), bytes)
            };
            let outcome = super::Outcome::new(record, None, false);
            let saved = save(&outcome, launch_id).and(kept);
            report = Some((outcome, saved));
        }
    }

    let _ = done_rx.recv_timeout(CLEANUP_WAIT.saturating_sub(cleanup_started.elapsed()));
    if let Some((outcome, _)) = &report {
        if outcome.crash && super::dialogs_wanted() {
            let log_path = crate::logging::session_log_path();
            show_soon(
                outcome.summary.clone(),
                super::message_body(&outcome.detail, &log_path.display().to_string(), false),
            );
        }
    }
    let _ = done_rx.recv_timeout(CLEANUP_CAP.saturating_sub(cleanup_started.elapsed()));
    let saved = report.as_ref().is_none_or(|(_, saved)| saved.is_ok());
    remove_files(&directory, owner, saved);
    // Everything is saved and cleaned up; stay only for messages.
    for (title, body) in waiting {
        show(&title, &body);
    }
    for message in messages {
        let _ = message.join();
    }
    let _ = fs::remove_file(file(READY));
    0
}

/// Whether to turn the virtual display off after the host ended: after any
/// abnormal end, and after a normal quit that could not turn it off itself.
#[cfg(any(windows, test))]
pub(super) fn cleans_up(exit_code: Option<u32>, display_left: bool) -> bool {
    exit_code != Some(0) || display_left
}

/// The host's process, to watch it and read its exit code. A host that ended
/// before this process opened it still counts while someone holds it open:
/// its exit code may be a crash's. Matching the start time keeps a later
/// process with the same id out.
#[cfg(windows)]
fn open_host(owner: ProcessId) -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_VM_READ,
    };
    let process = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            owner.pid,
        )
    }
    .ok()?;
    if super::process::created_at(process) == Some(owner.start) {
        return Some(process);
    }
    unsafe {
        let _ = CloseHandle(process);
    }
    None
}

/// Turn the virtual display off after `owner` ended, if it still should be,
/// and log what happened.
#[cfg(windows)]
fn clean_up(owner: ProcessId, reporters: &[ProcessId]) {
    match crate::vdd::disable_for_ended(owner, reporters) {
        Ok(true) => note(owner, "turned the virtual display off"),
        Ok(false) => note(owner, "could not turn the virtual display off"),
        Err(reason) if reason == super::display::NO_OWNER => {}
        Err(reason) => note(owner, &reason),
    }
}

/// One line about the reporter's own work in the session log.
#[cfg(windows)]
fn note(owner: ProcessId, text: &str) {
    let _ = crate::logging::append_now(&super::info_line(&format!(
        "Failure reporter for {owner} {text}"
    )));
}

/// The module of another process that contains `address`, and the offset.
#[cfg(windows)]
fn module_at(process: windows::Win32::Foundation::HANDLE, address: u64) -> Option<(String, u64)> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::ProcessStatus::{
        EnumProcessModulesEx, GetModuleBaseNameW, GetModuleInformation, LIST_MODULES_ALL,
        MODULEINFO,
    };
    let mut modules = vec![HMODULE::default(); 1024];
    let mut needed = 0u32;
    unsafe {
        EnumProcessModulesEx(
            process,
            modules.as_mut_ptr(),
            (modules.len() * std::mem::size_of::<HMODULE>()) as u32,
            &mut needed,
            LIST_MODULES_ALL,
        )
        .ok()?;
    }
    modules.truncate(needed as usize / std::mem::size_of::<HMODULE>());
    for module in modules {
        let mut info = MODULEINFO::default();
        if unsafe {
            GetModuleInformation(
                process,
                module,
                &mut info,
                std::mem::size_of::<MODULEINFO>() as u32,
            )
        }
        .is_err()
        {
            continue;
        }
        let base = info.lpBaseOfDll as u64;
        if (base..base + u64::from(info.SizeOfImage)).contains(&address) {
            let mut name = [0u16; 260];
            let length = unsafe { GetModuleBaseNameW(process, module, &mut name) } as usize;
            return Some((String::from_utf16_lossy(&name[..length]), address - base));
        }
    }
    None
}

#[cfg(windows)]
fn show(title: &str, body: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(body),
            &HSTRING::from(title),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "eternal-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn exited_process() -> ProcessId {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/c", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let id = ProcessId {
            pid: child.id(),
            start: 1,
        };
        child.wait().unwrap();
        id
    }

    #[test]
    fn a_message_request_is_read_once() {
        let directory = temp_dir("request");
        let path = owner_file(&directory, ProcessId { pid: 1, start: 2 }, REQUEST);
        write_request(&path, "EternalMonitor\nstopped", "line one\nline two").unwrap();
        assert_eq!(
            take_request(&path),
            Some(("EternalMonitor stopped".into(), "line one\nline two".into()))
        );
        assert_eq!(take_request(&path), None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_request_waits_for_a_starting_reporter_but_not_for_a_dead_one() {
        let directory = temp_dir("ready");
        let ready = directory.join("host.ready");
        // Still starting: this test process stands in for it.
        let reporter = ProcessId::current();
        assert!(!wait_until_ready(
            &ready,
            reporter,
            Duration::from_millis(50)
        ));
        let writer = {
            let ready = ready.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                fs::write(ready, reporter.to_string()).unwrap();
            })
        };
        assert!(wait_until_ready(&ready, reporter, Duration::from_secs(5)));
        writer.join().unwrap();
        // A reporter that exited reads nothing, ready or not.
        let started = Instant::now();
        assert!(!wait_until_ready(
            &ready,
            exited_process(),
            Duration::from_secs(5)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_host_that_ends_past_the_exception_filter_is_reported_by_its_exit_code() {
        // __fastfail: STATUS_STACK_BUFFER_OVERRUN, with no crash record.
        let fail_fast = after_exit(None, false, Some(0xC000_0409), true, false).unwrap();
        assert_eq!((fail_fast.code, fail_fast.thread), (0xC000_0409, 0));
        assert!(fail_fast.window_shown);
        assert_eq!(
            super::super::native_detail(fail_fast, None),
            "Exit code 0xC0000409 without a crash record"
        );
        // Ended before the reporter could read its exit code.
        let unknown = after_exit(None, false, None, false, false).unwrap();
        assert_eq!(
            super::super::native_detail(unknown, None),
            "The process ended without a crash record"
        );
        // A crash record left behind comes first.
        let crash = Record {
            code: 0xC000_0005,
            address: 0x1000,
            thread: 9,
            window_shown: false,
        };
        assert_eq!(
            after_exit(Some(crash), true, Some(1), false, false),
            Some(crash)
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_reporter_that_opens_its_host_after_a_fail_fast_exit_still_reads_the_code() {
        use std::os::windows::process::CommandExt;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::GetExitCodeProcess;
        // Waits for its input, then ends with __fastfail's code.
        let mut child = std::process::Command::new("cmd")
            .raw_arg("/c set /p line= & exit -1073740791")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let host = ProcessId {
            pid: child.id(),
            start: super::super::process::start_time(child.id()).unwrap(),
        };
        drop(child.stdin.take());
        assert_eq!(child.wait().unwrap().code(), Some(0xC000_0409_u32 as i32));
        // `child` still holds its handle, so the ended host can be opened.
        let process = open_host(host).expect("the ended host");
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(process, &mut code) }.unwrap();
        unsafe {
            let _ = CloseHandle(process);
        }
        let record = after_exit(None, false, Some(code), true, false).unwrap();
        let outcome = super::super::Outcome::new(record, None, false);
        assert_eq!((record.code, outcome.level()), (0xC000_0409, "error"));
        // A process created at another time is not the host.
        assert!(open_host(ProcessId {
            start: host.start + 1,
            ..host
        })
        .is_none());
        drop(child);
    }

    #[test]
    fn a_normal_quit_or_a_reported_failure_is_not_reported_again() {
        assert_eq!(after_exit(None, false, Some(0), false, false), None);
        assert_eq!(after_exit(None, true, Some(101), true, false), None);
        // Windows ending the session stops every program.
        assert_eq!(after_exit(None, false, Some(1), true, true), None);
    }

    #[test]
    fn a_crash_record_stays_until_its_report_is_saved() {
        let directory = temp_dir("handled");
        let host = exited_process();
        for kind in [RECORD, READY, DONE, SHOWN, DISPLAY] {
            fs::write(owner_file(&directory, host, kind), b"x").unwrap();
        }
        remove_files(&directory, host, false);
        let mut left: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        left.sort();
        let mut kept = [
            owner_file(&directory, host, RECORD),
            owner_file(&directory, host, READY),
        ];
        kept.sort();
        assert_eq!(left, kept);
        remove_files(&directory, host, true);
        assert_eq!(
            fs::read_dir(&directory)
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .collect::<Vec<_>>(),
            [owner_file(&directory, host, READY)]
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
