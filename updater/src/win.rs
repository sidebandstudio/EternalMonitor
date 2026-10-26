//! Windows-only defenses for running the installer.
//!
//! Threat model (see SECURITY.md): network attackers are fully covered by
//! HTTPS, the host allowlist and pinned hashes in the host. For malware already
//! running as the same user we add standard defense in depth, not a fix for
//! every same-user race: Microsoft documents UAC as not a security boundary,
//! and such malware could request elevation itself.
#![cfg(windows)]

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_NAME_NORMALIZED, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    QueryFullProcessImageNameW, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
};

/// Called first thing in `main`: DLLs this process loads later come only from
/// System32, never from the update folder it runs in.
pub fn restrict_dll_search() {
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// Volume serial number and file index: the same file whatever path reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId {
    volume: u32,
    index: u64,
}

fn info(handle: HANDLE) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(io::Error::other)?;
    Ok(info)
}

pub fn file_id(file: &File) -> io::Result<FileId> {
    let info = info(HANDLE(file.as_raw_handle()))?;
    Ok(FileId {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

/// The normalized final path of an open handle, without the `\\?\` prefix.
pub fn final_path(handle: HANDLE) -> io::Result<String> {
    let mut buffer = vec![0u16; 32_768];
    let length =
        unsafe { GetFinalPathNameByHandleW(handle, &mut buffer, FILE_NAME_NORMALIZED) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    let text = String::from_utf16_lossy(&buffer[..length]);
    Ok(text
        .strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(text))
}

/// Opens a directory without following a reparse point and fails if it is
/// one. `share` decides what others may still do while we hold it.
fn open_dir(path: &Path, share: u32) -> io::Result<OwnedHandle> {
    let name = wide(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            0x0000_0080, // FILE_READ_ATTRIBUTES
            windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(share),
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(io::Error::other)?;
    let owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if info(handle)?.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a link", path.display()),
        ));
    }
    Ok(owned)
}

const SHARE_ALL: u32 = 0x7; // read | write | delete
const SHARE_NO_DELETE: u32 = 0x3; // read | write

/// Pins the update folder for as long as the returned handle lives.
///
/// Every folder from the drive down must be a real folder, not a junction or
/// symbolic link; the update folder itself is held so it can't be renamed,
/// deleted or replaced, and its real path must be the one we were given.
/// Ancestors are checked but not held (holding `AppData` against deletion
/// would disturb other programs). They can't be swapped for a link later:
/// Windows refuses to rename a folder while a file inside it is open, and the
/// helper keeps the installer open until Setup has exited.
pub fn pin_dir(dir: &Path) -> Result<OwnedHandle, String> {
    let mut ancestors: Vec<&Path> = dir.ancestors().collect();
    ancestors.reverse();
    for part in &ancestors[..ancestors.len() - 1] {
        // A bare drive root ("C:\") can't be a link.
        if part.parent().is_none() || part.as_os_str().is_empty() {
            continue;
        }
        open_dir(part, SHARE_ALL).map_err(|error| format!("{}: {error}", part.display()))?;
    }
    let held =
        open_dir(dir, SHARE_NO_DELETE).map_err(|error| format!("{}: {error}", dir.display()))?;
    let real = final_path(HANDLE(held.as_raw_handle())).map_err(|e| e.to_string())?;
    let expected = dir.display().to_string();
    let expected = expected.strip_prefix(r"\\?\").unwrap_or(&expected);
    if !real.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "the update folder resolves to {real}, not {expected}"
        ));
    }
    Ok(held)
}

/// A started process that hasn't run any of its own code yet.
pub struct Suspended {
    pub process: OwnedHandle,
    thread: OwnedHandle,
}

impl Suspended {
    pub fn resume(self) -> io::Result<OwnedHandle> {
        if unsafe { ResumeThread(HANDLE(self.thread.as_raw_handle())) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(self.process)
    }

    pub fn terminate(self) {
        unsafe {
            let _ = TerminateProcess(HANDLE(self.process.as_raw_handle()), 1);
        }
    }
}

/// The handles a child may inherit, and nothing else
/// (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`). Each must be inheritable.
struct HandleList {
    /// Backing store for the attribute list, aligned for it.
    buffer: Vec<usize>,
    /// Windows reads the list from here when the child starts.
    _handles: Box<[HANDLE]>,
}

impl HandleList {
    fn new(handles: &[HANDLE]) -> io::Result<Self> {
        let mut size = 0;
        // Fails with "insufficient buffer" and reports the size it needs.
        let _ = unsafe {
            InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST::default(),
                1,
                0,
                &mut size,
            )
        };
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) }
            .map_err(io::Error::other)?;
        let this = Self {
            buffer,
            _handles: handles.into(),
        };
        unsafe {
            UpdateProcThreadAttribute(
                this.list(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(this._handles.as_ptr().cast()),
                std::mem::size_of_val(&*this._handles),
                None,
                None,
            )
        }
        .map_err(io::Error::other)?;
        Ok(this)
    }

    fn list(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buffer.as_ptr() as *mut _)
    }
}

impl Drop for HandleList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.list()) }
    }
}

/// Starts `exe` suspended, with `command_line` (program name first). With
/// `stdout`, the child writes to that (inheritable) handle and inherits that
/// handle alone; without it, it inherits nothing. The host holds inheritable
/// handles it never meant to pass on (sockets, graphics driver objects), so
/// inheritance is never left open-ended.
pub fn start_suspended(
    exe: &Path,
    command_line: &str,
    flags: u32,
    stdout: Option<HANDLE>,
) -> io::Result<Suspended> {
    let application = wide(exe.as_os_str());
    let mut command: Vec<u16> = OsStr::new(command_line)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let directory = exe.parent().map(|dir| wide(dir.as_os_str()));
    let mut startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut creation = CREATE_SUSPENDED | PROCESS_CREATION_FLAGS(flags);
    let inherited = match stdout {
        Some(stdout) => {
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdOutput = stdout;
            let list = HandleList::new(&[stdout])?;
            startup.lpAttributeList = list.list();
            creation |= EXTENDED_STARTUPINFO_PRESENT;
            Some(list)
        }
        None => None,
    };
    let mut information = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            PWSTR(command.as_mut_ptr()),
            None,
            None,
            inherited.is_some(),
            creation,
            None,
            directory
                .as_ref()
                .map_or(PCWSTR::null(), |dir| PCWSTR(dir.as_ptr())),
            &startup.StartupInfo,
            &mut information,
        )
    }
    .map_err(io::Error::other)?;
    Ok(Suspended {
        process: unsafe { OwnedHandle::from_raw_handle(information.hProcess.0) },
        thread: unsafe { OwnedHandle::from_raw_handle(information.hThread.0) },
    })
}

/// Opens the image a process was started from, by path, and returns its
/// identity. The path is read from the kernel; the identity, not the path
/// string, is what we compare.
pub fn image_id(process: &OwnedHandle) -> io::Result<FileId> {
    let path = image_path(process)?;
    let image = std::fs::OpenOptions::new()
        .read(true)
        .open(&path)
        .or_else(|_| {
            // The locked file allows only readers that share read access.
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ.0)
                .open(&path)
        })?;
    file_id(&image)
}

/// The path a process was started from, as the kernel reports it.
fn image_path(process: &OwnedHandle) -> io::Result<String> {
    let mut buffer = vec![0u16; 32_768];
    let mut size = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(process.as_raw_handle()),
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    }
    .map_err(io::Error::other)?;
    Ok(String::from_utf16_lossy(&buffer[..size as usize]))
}

/// Every running process whose image is the file `app` names, opened so the
/// caller can wait for it. Its final path matches, or, where the path is
/// spelled another way (a short name, a junction), the file's identity does;
/// another copy of the same program never matches. Comparing paths first
/// does not depend on opening the image, which antivirus scanning can
/// briefly refuse.
pub fn processes_running(app: &Path) -> io::Result<Vec<OwnedHandle>> {
    use windows::Win32::System::ProcessStatus::K32EnumProcesses;
    let (target, target_path) = {
        use std::os::windows::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(app)?;
        (file_id(&file)?, final_path(HANDLE(file.as_raw_handle()))?)
    };
    let mut pids = vec![0u32; 4096];
    let count = loop {
        let mut bytes = 0u32;
        let size = (pids.len() * std::mem::size_of::<u32>()) as u32;
        if !unsafe { K32EnumProcesses(pids.as_mut_ptr(), size, &mut bytes) }.as_bool() {
            return Err(io::Error::last_os_error());
        }
        if bytes < size {
            break bytes as usize / std::mem::size_of::<u32>();
        }
        // A full buffer may have left processes out.
        pids = vec![0u32; pids.len() * 2];
    };
    let me = std::process::id();
    Ok(pids[..count]
        .iter()
        .filter(|&&pid| pid != 0 && pid != me)
        // Processes of other users and protected ones cannot be opened;
        // they cannot be this user's EternalMonitor.
        .filter_map(|&pid| open_process(pid).ok())
        .filter(|process| {
            image_path(process).is_ok_and(|path| path.eq_ignore_ascii_case(&target_path))
                || image_id(process).is_ok_and(|id| id == target)
        })
        .collect())
}

/// Starts `exe` from its own folder with `command_line` (program name first),
/// inheriting no handles. Unlike `std::process::Command`, which passes on
/// every inheritable handle.
pub fn start(exe: &Path, command_line: &str) -> io::Result<()> {
    start_suspended(exe, command_line, 0, None)?
        .resume()
        .map(drop)
}

/// Starts `exe` suspended and lets it run only if Windows mapped the very file
/// `verified` is open on. Otherwise it is killed before running any code.
pub fn start_verified(
    exe: &Path,
    command_line: &str,
    verified: &File,
    flags: u32,
    stdout: Option<HANDLE>,
) -> Result<OwnedHandle, String> {
    let expected =
        file_id(verified).map_err(|e| format!("cannot identify {}: {e}", exe.display()))?;
    let suspended = start_suspended(exe, command_line, flags, stdout)
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    match image_id(&suspended.process) {
        Ok(actual) if actual == expected => suspended
            .resume()
            .map_err(|e| format!("cannot resume {}: {e}", exe.display())),
        Ok(_) => {
            suspended.terminate();
            Err(format!("{} is not the verified file", exe.display()))
        }
        Err(error) => {
            suspended.terminate();
            Err(format!("cannot check what started: {error}"))
        }
    }
}

/// Waits for a process; `None` if it is still running after `timeout`.
pub fn wait(process: &OwnedHandle, timeout: Option<std::time::Duration>) -> Option<u32> {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    let millis = timeout.map_or(INFINITE, |t| {
        t.as_millis().min(u128::from(INFINITE - 1)) as u32
    });
    let handle = HANDLE(process.as_raw_handle());
    if unsafe { WaitForSingleObject(handle, millis) } != WAIT_OBJECT_0 {
        return None;
    }
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(handle, &mut code) }.ok()?;
    Some(code)
}

/// A handle to a running process that can only be waited on. Opened while
/// the process is known to be alive, so a reused process ID can't mislead us.
pub fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    let handle = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
    }
    .map_err(io::Error::other)?;
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

/// Makes a handle inheritable (or not) by child processes.
fn set_inheritable(handle: HANDLE, on: bool) -> io::Result<()> {
    use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
    let value = if on {
        HANDLE_FLAG_INHERIT
    } else {
        HANDLE_FLAGS(0)
    };
    unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, value) }.map_err(io::Error::other)
}

/// Starts `exe` suspended with its stdout connected to the returned reader,
/// lets it run only if it is the file `verified` is open on, and returns the
/// process handle and a reader for its stdout.
pub fn spawn_verified_piped(
    exe: &Path,
    command_line: &str,
    verified: &File,
    flags: u32,
) -> Result<(OwnedHandle, File), String> {
    use std::os::windows::io::IntoRawHandle;
    // A pipe whose write end the child inherits as stdout.
    let (reader, writer) = std::io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
    let writer: OwnedHandle = writer.into();
    let write_handle = HANDLE(writer.as_raw_handle());
    set_inheritable(write_handle, true).map_err(|e| e.to_string())?;
    let started = start_verified(exe, command_line, verified, flags, Some(write_handle));
    // Close our copy of the write end so the reader sees EOF when the child
    // exits, whatever happened.
    drop(writer);
    let process = started?;
    let reader: File =
        unsafe { File::from_raw_handle(OwnedHandle::from(reader).into_raw_handle()) };
    Ok((process, reader))
}

/// Ends a process started for the update that didn't take over.
pub fn terminate(process: &OwnedHandle) {
    unsafe {
        let _ = TerminateProcess(HANDLE(process.as_raw_handle()), 1);
    }
}
