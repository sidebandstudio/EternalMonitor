//! Which host turned the virtual display on. The driver is one device for the
//! whole machine, so the failure reporter of a host that ended abnormally
//! turns it off only when that host owned it and no other EternalMonitor host
//! of this install is running, which may be using it. Hosts only write the
//! record; a driver change never waits for it. Every driver change holds the
//! driver lock, shared by every process of the user, so a reporter's turn-off
//! and a new host's turn-on never overlap.

#![cfg_attr(not(windows), allow(dead_code))]

use super::ProcessId;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const OWNER_FILE: &str = "virtual-display-owner";
const LOCK_FILE: &str = "virtual-display.lock";
/// Why the reporter leaves the display alone after most ends: no host had it on.
pub(crate) const NO_OWNER: &str = "no host turned it on";

/// The owner record and the driver lock.
fn paths() -> Option<(PathBuf, PathBuf)> {
    let directory = crate::settings::app_data_dir()?;
    Some((directory.join(OWNER_FILE), directory.join(LOCK_FILE)))
}

/// Wait for the driver lock, calling `waiting` meanwhile. A lock that cannot
/// be opened does not stop the driver change, as a record that cannot be
/// written does not.
fn hold(lock: &Path, mut waiting: impl FnMut()) -> Option<super::lock::Locked> {
    loop {
        match super::lock::try_lock(lock) {
            Ok(locked) => return Some(locked),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                waiting();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => {
                tracing::warn!(%error, "Could not lock the virtual display driver");
                return None;
            }
        }
    }
}

/// Make a driver change for this host: turning the display on records this
/// process as its owner first, and a completed turn-off removes the record if
/// it names this process. Another host's record stays, for its reporter.
/// `progress` runs while another change holds the driver, then goes to `run`.
pub(crate) fn change<P: FnMut()>(enabled: bool, progress: P, run: impl FnOnce(P) -> bool) -> bool {
    match paths() {
        Some((path, lock)) => change_at(&path, &lock, enabled, progress, run),
        None => run(progress),
    }
}

fn change_at<P: FnMut()>(
    path: &Path,
    lock: &Path,
    enabled: bool,
    mut progress: P,
    run: impl FnOnce(P) -> bool,
) -> bool {
    let _driver = hold(lock, &mut progress);
    if enabled {
        if let Err(error) = record(path, ProcessId::current()) {
            tracing::warn!(%error, "Could not record the virtual display's owner");
        }
    }
    let done = run(progress);
    if done && !enabled {
        if let Err(error) = delete_if_owned(path, ProcessId::current(), || true, true) {
            tracing::warn!(%error, "Could not delete the virtual display's owner record");
        }
    }
    done
}

/// Replace the record in one step, so a reader never sees half of it.
fn record(path: &Path, owner: ProcessId) -> io::Result<()> {
    let temp = path.with_extension(format!("{owner}.tmp"));
    fs::write(&temp, owner.to_string())?;
    let replaced = with_retries(|| fs::rename(&temp, path));
    if replaced.is_err() {
        let _ = fs::remove_file(&temp);
    }
    replaced
}

/// A reporter holds the record open for a moment while it checks and deletes
/// it. Where Windows cannot replace an open file (FAT volumes), the replace
/// fails meanwhile; try again for a few milliseconds.
fn with_retries(mut attempt: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut tries = 1;
    loop {
        match attempt() {
            Err(_) if tries < 10 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            result => return result,
        }
    }
}

fn owner_at(path: &Path) -> Option<ProcessId> {
    ProcessId::from_name(fs::read_to_string(path).ok()?.trim())
}

/// Turn the display off after `ended` exited abnormally, if it owned the
/// display and no other host of this install runs. `Ok` says whether
/// `disable` succeeded; `Err` says what happened instead, as a log note.
pub(crate) fn disable_for_ended(
    ended: ProcessId,
    other_host: impl FnMut() -> Option<ProcessId>,
    disable: impl FnOnce() -> bool,
) -> Result<bool, String> {
    let (path, lock) = paths().ok_or("there is no app data folder")?;
    disable_for_ended_at(&path, &lock, ended, other_host, disable)
}

fn disable_for_ended_at(
    path: &Path,
    lock: &Path,
    ended: ProcessId,
    mut other_host: impl FnMut() -> Option<ProcessId>,
    disable: impl FnOnce() -> bool,
) -> Result<bool, String> {
    let check = |other_host: &mut dyn FnMut() -> Option<ProcessId>| {
        match owner_at(path) {
            None => return Err(NO_OWNER.to_string()),
            Some(owner) if owner != ended => {
                return Err(format!(
                    "left the virtual display on: host {owner} turned it on"
                ))
            }
            Some(_) => {}
        }
        match other_host() {
            Some(other) => Err(format!(
                "left the virtual display on: EternalMonitor host {other} is running"
            )),
            None => Ok(()),
        }
    };
    check(&mut other_host)?;
    // Again just before the driver changes: the process scan takes a while,
    // and a replacement host may have started or turned the display on. From
    // here a host that turns it on waits until the turn-off is done.
    let _driver = hold(lock, || {});
    check(&mut other_host)?;
    let done = disable();
    if done {
        if let Err(error) = delete_if_owned(path, ended, || true, true) {
            return Err(format!(
                "turned the virtual display off, but could not delete its owner record: {error}"
            ));
        }
        if let Some(owner) = owner_at(path) {
            return Err(format!(
                "turned the virtual display off, but host {owner} turned it on meanwhile; \
                 its record stays"
            ));
        }
    }
    Ok(done)
}

/// What `delete_if_owned` did when it could read the record, or knew there
/// was none. An I/O failure is its `Err` instead.
#[derive(Debug, PartialEq, Eq)]
enum Removal {
    Deleted,
    Missing,
    /// The record names another process, or was replaced before deletion.
    OtherOwner,
    /// `before_delete` stopped the deletion.
    Stopped,
}

/// Delete the record only if it names `owner`, reading and deleting it
/// through one handle. A host that replaces the record meanwhile puts a new
/// file in its place; the handle still refers to the old one, so the new
/// record is never deleted. `before_delete` runs between the two steps and
/// stops the deletion by returning false, for tests. `posix` asks for the
/// deletion to take the name away at once where the volume supports it.
#[cfg(windows)]
fn delete_if_owned(
    path: &Path,
    owner: ProcessId,
    before_delete: impl FnOnce() -> bool,
    posix: bool,
) -> io::Result<Removal> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileDispositionInfo, FileDispositionInfoEx, SetFileInformationByHandle,
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
        FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS,
    };
    // GENERIC_READ | DELETE, shared with everything, so a host replacing the
    // record is not turned away.
    let mut file = match fs::OpenOptions::new()
        .access_mode(0x8000_0000 | 0x0001_0000)
        .share_mode(0x7)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Removal::Missing),
        Err(error) => return Err(error),
    };
    if let Some(kept) = check_owner(&mut file, owner, before_delete)? {
        return Ok(kept);
    }
    let handle = HANDLE(file.as_raw_handle());
    let size_of = |size: usize| size as u32;
    if posix {
        let info = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
                FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
            ),
        };
        let deleted = unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfoEx,
                &info as *const _ as *const core::ffi::c_void,
                size_of(std::mem::size_of_val(&info)),
            )
        };
        if deleted.is_ok() {
            return Ok(Removal::Deleted);
        }
    }
    // Volumes without POSIX deletion: the name goes when this handle closes.
    let info = FILE_DISPOSITION_INFO {
        DeleteFile: true.into(),
    };
    unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            &info as *const _ as *const core::ffi::c_void,
            size_of(std::mem::size_of_val(&info)),
        )
    }
    .map(|()| Removal::Deleted)
    .map_err(io::Error::from)
}

/// Read the opened record. `None` when it names `owner` and may be deleted.
fn check_owner(
    file: &mut fs::File,
    owner: ProcessId,
    before_delete: impl FnOnce() -> bool,
) -> io::Result<Option<Removal>> {
    use std::io::Read;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(if ProcessId::from_name(text.trim()) != Some(owner) {
        Some(Removal::OtherOwner)
    } else if !before_delete() {
        Some(Removal::Stopped)
    } else {
        None
    })
}

/// Unix has no deletion through a handle; the reporter runs only on Windows.
/// For tests, this deletes the path only while it is still the opened file.
#[cfg(not(windows))]
fn delete_if_owned(
    path: &Path,
    owner: ProcessId,
    before_delete: impl FnOnce() -> bool,
    _posix: bool,
) -> io::Result<Removal> {
    use std::os::unix::fs::MetadataExt;
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Removal::Missing),
        Err(error) => return Err(error),
    };
    if let Some(kept) = check_owner(&mut file, owner, before_delete)? {
        return Ok(kept);
    }
    let opened = file.metadata().map(|m| (m.dev(), m.ino()))?;
    let named = fs::metadata(path).map(|m| (m.dev(), m.ino())).ok();
    if named != Some(opened) {
        return Ok(Removal::OtherOwner);
    }
    fs::remove_file(path).map(|()| Removal::Deleted)
}

/// A file's identity: its volume and its index on that volume. Paths that
/// differ in case, prefix (`\\?\`), short names or links name one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileId {
    volume: u32,
    index: u64,
}

/// This host's executable: its file and the names a process image path can
/// end in when it is that file, the long and the 8.3 short one.
struct ThisExe {
    id: FileId,
    names: Vec<String>,
}

/// Whether a process whose image is `image` may run this executable. Only
/// images with one of its names are compared by identity; one that cannot be
/// resolved may be it.
fn may_be(image: &str, exe: &ThisExe, file_id: impl FnOnce(&str) -> Option<FileId>) -> bool {
    let name = image
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image)
        .to_lowercase();
    exe.names.contains(&name) && file_id(image).is_none_or(|id| id == exe.id)
}

#[cfg(windows)]
fn file_id(path: &str) -> Option<FileId> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    // FILE_READ_ATTRIBUTES, shared with everything a running image allows.
    let file = fs::OpenOptions::new()
        .access_mode(0x80)
        .share_mode(0x7)
        .open(path)
        .ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some(FileId {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

#[cfg(windows)]
impl ThisExe {
    fn current() -> Option<Self> {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{GetLongPathNameW, GetShortPathNameW};
        let exe = std::env::current_exe().ok()?;
        let wide = HSTRING::from(exe.as_os_str());
        let mut names = vec![exe.file_name()?.to_string_lossy().to_lowercase()];
        for convert in [GetLongPathNameW, GetShortPathNameW] {
            let mut buffer = [0u16; 1024];
            let length = unsafe { convert(&wide, Some(&mut buffer)) } as usize;
            if length > 0 && length < buffer.len() {
                let path = String::from_utf16_lossy(&buffer[..length]);
                let name = path.rsplit('\\').next().unwrap_or(&path).to_lowercase();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        Some(Self {
            id: file_id(&exe.to_string_lossy())?,
            names,
        })
    }
}

/// Every process id, or `None` when the list may be incomplete.
#[cfg(windows)]
fn process_ids() -> Option<Vec<u32>> {
    use windows::Win32::System::ProcessStatus::EnumProcesses;
    let mut pids = vec![0u32; 4096];
    while pids.len() <= 65_536 {
        let mut bytes = 0u32;
        unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut bytes) }.ok()?;
        let count = bytes as usize / 4;
        // A full buffer may have left processes out.
        if count < pids.len() {
            pids.truncate(count);
            return Some(pids);
        }
        pids = vec![0u32; pids.len() * 2];
    }
    None
}

/// A running process that may run this executable other than `ended`, this
/// process and the failure reporters in `reporters`. When unsure, it names
/// process 0: another host may be using the display. Exited processes are
/// still listed while someone holds a handle to them; they do not count.
#[cfg(windows)]
pub(crate) fn other_host(ended: ProcessId, reporters: &[ProcessId]) -> Option<ProcessId> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let unknown = Some(ProcessId { pid: 0, start: 0 });
    let (Some(exe), Some(pids)) = (ThisExe::current(), process_ids()) else {
        return unknown;
    };
    let me = std::process::id();
    pids.into_iter()
        .filter(|&pid| pid != 0 && pid != me)
        .find_map(|pid| {
            // A host of this user can always be opened; processes of other
            // users and protected ones cannot be ours.
            let process =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
            let mut name = [0u16; 1024];
            let mut length = name.len() as u32;
            let image = unsafe {
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(name.as_mut_ptr()),
                    &mut length,
                )
            }
            .is_ok()
            .then(|| String::from_utf16_lossy(&name[..length as usize]));
            let start = super::process::start_time_of(process);
            unsafe {
                let _ = CloseHandle(process);
            }
            // No image: System and the kernel's own processes.
            let id = ProcessId { pid, start: start? };
            (id != ended && !reporters.contains(&id) && may_be(&image?, &exe, file_id))
                .then_some(id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "eternal-display-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn turning_the_display_on_records_the_owner_and_turning_it_off_clears_it() {
        let path = owner_file("change");
        let lock = owner_file("change-lock");
        assert!(change_at(&path, &lock, true, || {}, |_| true));
        assert_eq!(owner_at(&path), Some(ProcessId::current()));
        // A failed turn-off leaves the owner, so its reporter still cleans up.
        assert!(!change_at(&path, &lock, false, || {}, |_| false));
        assert_eq!(owner_at(&path), Some(ProcessId::current()));
        assert!(change_at(&path, &lock, false, || {}, |_| true));
        assert!(!path.exists());
        // A record or lock that cannot be written does not stop the driver change.
        let missing = owner_file("missing").join("folder");
        assert!(change_at(
            &missing.join(OWNER_FILE),
            &missing.join(LOCK_FILE),
            true,
            || panic!("waited"),
            |_| true
        ));
        fs::remove_file(lock).unwrap();
    }

    #[test]
    fn a_quit_whose_turn_off_timed_out_is_finished_by_the_reporter() {
        use crate::startup::reporter::{cleans_up, owner_file as reporter_file, DISPLAY};
        let path = owner_file("quit-timeout");
        let lock = owner_file("quit-timeout-lock");
        let crash = owner_file("quit-timeout-crash");
        fs::create_dir(&crash).unwrap();
        let host = ProcessId::current();
        // The host turned the display on; at quit its Disable timed out
        // behind an Enable that was still running.
        assert!(change_at(&path, &lock, true, || {}, |_| true));
        let marker = reporter_file(&crash, host, DISPLAY);
        crate::startup::display_off_at_quit(|| {
            let done = change_at(&path, &lock, false, || {}, |_| false);
            if !done {
                // What reporter::mark writes for a host with a reporter.
                fs::write(&marker, b"").unwrap();
            }
            done
        });
        assert_eq!(owner_at(&path), Some(host));
        // The host exits normally; its reporter cleans up all the same.
        assert!(
            cleans_up(Some(0), marker.exists()),
            "the reporter skipped the cleanup"
        );
        let mut disabled = false;
        let result = disable_for_ended_at(
            &path,
            &lock,
            host,
            || None,
            || {
                disabled = true;
                true
            },
        );
        assert_eq!(result, Ok(true));
        assert!(disabled);
        assert!(!path.exists());
        // A normal quit that turned the display off leaves nothing to do.
        assert!(!cleans_up(Some(0), false));
        assert!(cleans_up(Some(1), false));
        fs::remove_dir_all(crash).unwrap();
        fs::remove_file(lock).unwrap();
    }

    #[test]
    fn a_hosts_own_turn_off_leaves_another_hosts_record() {
        let path = owner_file("startup");
        let lock = owner_file("startup-lock");
        // Host A turned the display on; host B starts and turns it off.
        let host_a = ProcessId { pid: 7, start: 1 };
        record(&path, host_a).unwrap();
        assert!(change_at(&path, &lock, false, || {}, |_| true));
        assert_eq!(owner_at(&path), Some(host_a));
        fs::remove_file(path).unwrap();
        fs::remove_file(lock).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn an_exited_copy_still_held_open_is_not_another_host() {
        let _copies = super::super::process::tests::RUNS_THIS_EXE.lock();
        // The test executable stands in for the host's; `--list` exits at once.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        // `child` still holds its handle, so the process is still listed.
        let ended = ProcessId { pid: 7, start: 1 };
        assert_eq!(other_host(ended, &[]), None);
        drop(child);
    }

    #[cfg(windows)]
    #[test]
    fn another_spelling_of_this_executable_is_the_same_file() {
        let exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let this = ThisExe::current().unwrap();
        let spelled = format!(r"\\?\{}", exe.to_uppercase());
        assert_eq!(file_id(&spelled), Some(this.id));
        assert!(may_be(&spelled, &this, file_id));
        assert!(!may_be(r"C:\Windows\System32\cmd.exe", &this, file_id));
    }

    #[test]
    fn the_reporter_turns_off_only_its_own_hosts_display_with_no_other_host_running() {
        let path = owner_file("ended");
        let lock = owner_file("ended-lock");
        let ended = ProcessId { pid: 7, start: 1 };
        let other = ProcessId { pid: 8, start: 1 };
        // A replacement host turned it on.
        record(&path, other).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, &lock, ended, || None, || panic!("disabled")),
            Err("left the virtual display on: host 8-1 turned it on".to_string())
        );
        // The ended host turned it on, but another host is running.
        record(&path, ended).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, &lock, ended, || Some(other), || panic!("disabled")),
            Err("left the virtual display on: EternalMonitor host 8-1 is running".to_string())
        );
        assert_eq!(owner_at(&path), Some(ended));
        assert_eq!(
            disable_for_ended_at(&path, &lock, ended, || None, || true),
            Ok(true)
        );
        assert!(!path.exists());
        // Nobody turned it on.
        assert_eq!(
            disable_for_ended_at(&path, &lock, ended, || None, || panic!("disabled")),
            Err(NO_OWNER.to_string())
        );
        fs::remove_file(lock).unwrap();
    }

    #[test]
    fn a_host_that_starts_during_the_check_keeps_its_display() {
        let path = owner_file("starting");
        let lock = owner_file("starting-lock");
        let ended = ProcessId { pid: 7, start: 1 };
        let replacement = ProcessId { pid: 8, start: 1 };
        record(&path, ended).unwrap();
        // B is not running at the first scan; by the second it has turned
        // the display on and written its record.
        let mut scans = 0;
        let result = disable_for_ended_at(
            &path,
            &lock,
            ended,
            || {
                scans += 1;
                if scans == 1 {
                    record(&path, replacement).unwrap();
                }
                None
            },
            || panic!("disabled"),
        );
        assert_eq!(
            result,
            Err("left the virtual display on: host 8-1 turned it on".to_string())
        );
        assert_eq!(owner_at(&path), Some(replacement));
        fs::remove_file(path).unwrap();
        fs::remove_file(lock).unwrap();
    }

    #[test]
    fn a_host_that_turns_the_display_on_during_the_turn_off_keeps_its_record() {
        let path = owner_file("during");
        let lock = owner_file("during-lock");
        let ended = ProcessId { pid: 7, start: 1 };
        let replacement = ProcessId { pid: 8, start: 1 };
        record(&path, ended).unwrap();
        let result = disable_for_ended_at(
            &path,
            &lock,
            ended,
            || None,
            || {
                record(&path, replacement).unwrap();
                true
            },
        );
        assert_eq!(
            result,
            Err(
                "turned the virtual display off, but host 8-1 turned it on meanwhile; its record stays"
                    .to_string()
            )
        );
        assert_eq!(owner_at(&path), Some(replacement));
        assert_eq!(files_beside(&path), 1);
        fs::remove_file(path).unwrap();
        fs::remove_file(lock).unwrap();
    }

    #[test]
    fn a_host_that_turns_the_display_on_during_a_reporters_turn_off_keeps_it_on() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};
        let path = owner_file("overlap");
        let lock = owner_file("overlap-lock");
        let ended = ProcessId { pid: 7, start: 1 };
        record(&path, ended).unwrap();
        let display_on = Arc::new(AtomicBool::new(true));
        // The ended host's reporter passed both checks and is turning the
        // display off.
        let (paused_tx, paused) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel::<()>();
        let reporter = {
            let (path, lock, display_on) = (path.clone(), lock.clone(), display_on.clone());
            std::thread::spawn(move || {
                disable_for_ended_at(
                    &path,
                    &lock,
                    ended,
                    || None,
                    || {
                        paused_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        display_on.store(false, Ordering::SeqCst);
                        true
                    },
                )
            })
        };
        paused.recv().unwrap();
        // A replacement host turns it on meanwhile. Once it is done or
        // waiting for the driver, the reporter's turn-off finishes.
        let (event_tx, event) = mpsc::channel();
        let host = {
            let (path, lock, display_on) = (path.clone(), lock.clone(), display_on.clone());
            std::thread::spawn(move || {
                let waiting = event_tx.clone();
                let done = change_at(
                    &path,
                    &lock,
                    true,
                    move || {
                        let _ = waiting.send(());
                    },
                    |_| {
                        display_on.store(true, Ordering::SeqCst);
                        true
                    },
                );
                let _ = event_tx.send(());
                done
            })
        };
        event.recv().unwrap();
        resume.send(()).unwrap();
        let reported = reporter.join().unwrap();
        assert!(host.join().unwrap());
        assert!(display_on.load(Ordering::SeqCst));
        assert_eq!(reported, Ok(true));
        assert_eq!(owner_at(&path), Some(ProcessId::current()));
        fs::remove_file(path).unwrap();
        fs::remove_file(lock).unwrap();
    }

    /// Files named like the record, the record included.
    fn files_beside(path: &Path) -> usize {
        let prefix = path.file_name().unwrap().to_string_lossy().into_owned();
        fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .count()
    }

    #[test]
    fn the_reporter_deletes_its_hosts_record() {
        for posix in [true, false] {
            let path = owner_file("own");
            let ended = ProcessId { pid: 7, start: 1 };
            record(&path, ended).unwrap();
            assert_eq!(
                delete_if_owned(&path, ended, || true, posix).unwrap(),
                Removal::Deleted
            );
            assert!(!path.exists());
            // Nothing to delete, or another host's record.
            assert_eq!(
                delete_if_owned(&path, ended, || true, posix).unwrap(),
                Removal::Missing
            );
            record(&path, ProcessId { pid: 8, start: 1 }).unwrap();
            assert_eq!(
                delete_if_owned(&path, ended, || true, posix).unwrap(),
                Removal::OtherOwner
            );
            assert!(path.exists());
            fs::remove_file(path).unwrap();
        }
    }

    /// A record in its own folder that this process can read but not delete.
    /// Returns a closure that makes it deletable again.
    fn undeletable_record(name: &str, owner: ProcessId) -> (PathBuf, impl FnOnce()) {
        let folder = owner_file(name);
        fs::create_dir(&folder).unwrap();
        let path = folder.join(OWNER_FILE);
        record(&path, owner).unwrap();
        let set_locked = |path: &Path, locked: bool| {
            // Windows refuses to delete a read-only file; Unix needs the
            // folder's write permission instead.
            #[cfg(windows)]
            let target = path.to_path_buf();
            #[cfg(not(windows))]
            let target = path.parent().unwrap().to_path_buf();
            let mut permissions = fs::metadata(&target).unwrap().permissions();
            #[cfg(not(windows))]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(if locked { 0o555 } else { 0o755 });
            }
            #[cfg(windows)]
            permissions.set_readonly(locked);
            fs::set_permissions(&target, permissions).unwrap();
        };
        set_locked(&path, true);
        let unlock_path = path.clone();
        (path, move || {
            set_locked(&unlock_path, false);
            fs::remove_dir_all(unlock_path.parent().unwrap()).unwrap();
        })
    }

    #[test]
    fn a_record_that_cannot_be_deleted_is_an_error_not_another_owner() {
        let ended = ProcessId { pid: 7, start: 1 };
        for posix in [true, false] {
            let (path, unlock) = undeletable_record("locked", ended);
            assert!(
                delete_if_owned(&path, ended, || true, posix).is_err(),
                "posix={posix}"
            );
            assert_eq!(owner_at(&path), Some(ended));
            unlock();
        }
        // The reporter says the deletion failed, not that a host turned the
        // display on meanwhile.
        let (path, unlock) = undeletable_record("locked-reporter", ended);
        let lock = owner_file("locked-reporter-lock");
        let result = disable_for_ended_at(&path, &lock, ended, || None, || true);
        unlock();
        fs::remove_file(lock).unwrap();
        let note = result.unwrap_err();
        assert!(
            note.starts_with(
                "turned the virtual display off, but could not delete its owner record: "
            ),
            "{note}"
        );
    }

    #[test]
    fn a_record_replaced_between_reading_and_deleting_survives() {
        // With POSIX deletion and without it, as on FAT volumes.
        for posix in [true, false] {
            let path = owner_file("replaced");
            let ended = ProcessId { pid: 7, start: 1 };
            let replacement = ProcessId { pid: 8, start: 1 };
            record(&path, ended).unwrap();
            let _ = delete_if_owned(
                &path,
                ended,
                || {
                    record(&path, replacement).unwrap();
                    true
                },
                posix,
            );
            assert_eq!(owner_at(&path), Some(replacement), "posix={posix}");
            assert_eq!(files_beside(&path), 1, "posix={posix}");
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn a_reporter_stopped_midway_leaves_every_record() {
        let path = owner_file("stopped");
        let ended = ProcessId { pid: 7, start: 1 };
        let replacement = ProcessId { pid: 8, start: 1 };
        record(&path, ended).unwrap();
        // Ended, as by Task Manager, after reading its host's record.
        assert_eq!(
            delete_if_owned(&path, ended, || false, true).unwrap(),
            Removal::Stopped
        );
        assert_eq!(owner_at(&path), Some(ended));
        // Ended after a replacement host wrote its record.
        assert_eq!(
            delete_if_owned(
                &path,
                ended,
                || {
                    record(&path, replacement).unwrap();
                    false
                },
                true
            )
            .unwrap(),
            Removal::Stopped
        );
        assert_eq!(owner_at(&path), Some(replacement));
        // Ended while handling a record that names another host.
        assert_eq!(
            delete_if_owned(&path, ended, || false, true).unwrap(),
            Removal::OtherOwner
        );
        assert_eq!(owner_at(&path), Some(replacement));
        assert_eq!(files_beside(&path), 1);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn writing_a_record_tries_again_for_a_few_milliseconds() {
        let mut failures = 3;
        let started = std::time::Instant::now();
        assert!(with_retries(|| {
            if failures == 0 {
                Ok(())
            } else {
                failures -= 1;
                Err(io::ErrorKind::PermissionDenied.into())
            }
        })
        .is_ok());
        let mut attempts = 0;
        let result = with_retries(|| {
            attempts += 1;
            Err(io::ErrorKind::PermissionDenied.into())
        });
        assert!(result.is_err());
        assert_eq!(attempts, 10);
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[cfg(windows)]
    #[test]
    fn a_host_replaces_its_record_while_a_reporter_holds_the_old_one() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = owner_file("held");
        record(&path, ProcessId { pid: 7, start: 1 }).unwrap();
        // Opened as the reporter opens it.
        let held = fs::OpenOptions::new()
            .access_mode(0x8000_0000 | 0x0001_0000)
            .share_mode(0x7)
            .open(&path)
            .unwrap();
        let replacement = ProcessId { pid: 8, start: 1 };
        record(&path, replacement).unwrap();
        assert_eq!(owner_at(&path), Some(replacement));
        drop(held);
        assert_eq!(owner_at(&path), Some(replacement));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn only_images_named_like_this_executable_are_compared_and_unknown_ones_count() {
        let id = FileId {
            volume: 0xAB,
            index: 42,
        };
        let exe = ThisExe {
            id,
            names: vec!["eternal-host.exe".into(), "eterna~1.exe".into()],
        };
        let same = |_: &str| Some(id);
        let other = |_: &str| Some(FileId { index: 43, ..id });
        let unknown = |_: &str| None;
        // A short name, a long-path prefix and another case of this file.
        assert!(may_be(r"D:\AGENTW~1\EM-STA~1\ETERNA~1.EXE", &exe, same));
        assert!(may_be(r"\\?\D:\AgentWork\Eternal-Host.EXE", &exe, same));
        // Another copy of the executable is another install.
        assert!(!may_be(r"D:\copy\eternal-host.exe", &exe, other));
        // Named like it but not resolvable: it may be a host.
        assert!(may_be(r"D:\gone\eternal-host.exe", &exe, unknown));
        // Other programs are never opened.
        assert!(!may_be(
            r"C:\Windows\explorer.exe",
            &exe,
            |_: &str| -> Option<FileId> { panic!("opened") }
        ));
    }
}
