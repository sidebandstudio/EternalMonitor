//! The host is a Windows GUI program, so opening it from the Start menu shows
//! no console window. It has no console of its own: started from a terminal
//! it joins that terminal's console, so its log lines print there and Ctrl+C
//! reaches it, as when it was a console program.

/// Join the console of the terminal that started the host, if any. Output
/// redirected to a file or pipe stays there; a host started by Explorer,
/// Setup or the update helper has no console to join and gets none.
pub fn join_parent_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_DISK, FILE_TYPE_PIPE};
        use windows::Win32::System::Console::{
            AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
        };
        let redirected = GetStdHandle(STD_OUTPUT_HANDLE)
            .is_ok_and(|output| [FILE_TYPE_DISK, FILE_TYPE_PIPE].contains(&GetFileType(output)));
        // Redirected runs don't attach: on the test runner's console, Stop-Host's
        // Ctrl+C would reach the runner too. A headless one gets its own console.
        if !redirected {
            keeping_std_handles(|| AttachConsole(ATTACH_PARENT_PROCESS));
        }
    }
}

/// Give a headless host a console of its own when it has none, so automation
/// can stop it with Ctrl+C. The window follows how the host was started (the
/// test runner starts it hidden), as a console program's did.
pub fn ensure_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Console::{AllocConsole, GetConsoleWindow};
        if GetConsoleWindow().0.is_null() {
            keeping_std_handles(|| AllocConsole());
        }
    }
}

/// Connecting to a console points every standard handle at it. Keep the ones
/// the host was started with, so output redirected to a file stays there.
#[cfg(windows)]
unsafe fn keeping_std_handles(connect: impl FnOnce() -> windows::core::Result<()>) {
    use windows::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    let streams = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    let given = streams.map(|stream| GetStdHandle(stream).ok());
    if connect().is_ok() {
        for (stream, handle) in streams.into_iter().zip(given) {
            if let Some(handle) = handle {
                let _ = SetStdHandle(stream, handle);
            }
        }
    }
}
