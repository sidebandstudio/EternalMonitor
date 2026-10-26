//! The update helper and the app it starts again inherit only what they are
//! given. The host holds inheritable handles it never meant to pass on (a
//! socket, driver objects); a child that inherits them keeps them alive.
//!
//! The test runs its own binary as the child, through the real start paths.
//! The child finds the variable below, checks whether the parent's
//! inheritable event is open in it and writes the answer to a file.
#![cfg(windows)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eternal_updater::win;
use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, CompareObjectHandles, GetHandleInformation, HANDLE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Threading::{CreateEventW, OpenEventW, SYNCHRONIZATION_SYNCHRONIZE};

const PROBE: &str = "EM_INHERIT_PROBE";
/// What a child prints on its standard output, to show the pipe still works.
const PIPE_OK: &str = "inherit-probe: pipe ok";

#[test]
fn children_inherit_only_what_they_are_given() {
    if let Ok(probe) = std::env::var(PROBE) {
        return child(&probe);
    }
    let name = format!("Local\\em-inherit-probe-{}", std::process::id());
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    // Inheritable, like the leaked socket: any child started with handle
    // inheritance on and no handle list gets a copy.
    let event = unsafe {
        CreateEventW(
            Some(&attributes as *const _),
            true,
            false,
            &HSTRING::from(name.as_str()),
        )
    }
    .unwrap();
    let dir = std::env::temp_dir().join(format!("eternal-inherit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = std::env::current_exe().unwrap();
    let probe = |result: &Path| format!("{}|{name}|{}", event.0 as usize, result.display());

    // The host starting the helper: its stdout pipe, and nothing else.
    let helper = dir.join("helper.txt");
    std::env::set_var(PROBE, probe(&helper));
    let verified = eternal_updater::open_locked(&exe).unwrap();
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let (process, mut stdout) = win::spawn_verified_piped(
        &exe,
        &format!("\"{}\"", exe.display()),
        &verified,
        CREATE_NO_WINDOW,
    )
    .unwrap();
    let mut printed = String::new();
    stdout.read_to_string(&mut printed).unwrap();
    assert!(win::wait(&process, Some(Duration::from_secs(30))).is_some());
    assert!(printed.contains(PIPE_OK), "{printed}");

    // The helper starting the app again: nothing at all.
    let relaunched = dir.join("relaunched.txt");
    std::env::set_var(PROBE, probe(&relaunched));
    eternal_updater::relaunch(&exe, &[]).unwrap();

    let answers = (answer(&helper), answer(&relaunched));
    assert_eq!(
        (answers.0.as_str(), answers.1.as_str()),
        ("not inherited", "not inherited"),
        "(helper start, relaunch)"
    );

    std::env::remove_var(PROBE);
    unsafe { CloseHandle(event) }.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// Waits for the child's answer.
fn answer(path: &PathBuf) -> String {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.is_empty() {
                return text;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no answer in {}", path.display());
}

/// In the child: is the parent's event open here under the same value?
fn child(probe: &str) {
    let mut parts = probe.splitn(3, '|');
    let value: usize = parts.next().unwrap().parse().unwrap();
    let name = parts.next().unwrap();
    let result = parts.next().unwrap();
    // Look for the value before opening a handle of our own: that handle
    // could otherwise get the same value and compare equal to itself.
    let mut flags = 0;
    let open = unsafe { GetHandleInformation(HANDLE(value as *mut _), &mut flags) }.is_ok();
    let inherited = open && {
        let ours = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) }
            .unwrap();
        unsafe { CompareObjectHandles(HANDLE(value as *mut _), ours) }.as_bool()
    };
    let _ = writeln!(std::io::stdout().lock(), "{PIPE_OK}");
    let _ = std::io::stdout().flush();
    let text = if inherited {
        "inherited"
    } else {
        "not inherited"
    };
    let partial = format!("{result}.part");
    std::fs::write(&partial, text).unwrap();
    std::fs::rename(partial, result).unwrap();
}
