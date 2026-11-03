//! "Restart EternalMonitor" with the real binary: the copy signals the old
//! window first, then waits for the running host's single-instance mutex
//! without touching its session log, and becomes the host once that host
//! ends. The test plays the old window. Headless, like `second_launch`.
#![cfg(windows)]

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn host(app_data: &Path, args: &[String]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_eternal-host"));
    command
        .args(args)
        .env("APPDATA", app_data)
        .env("ETERNAL_HEADLESS", "1")
        .env("ETERNAL_CAPTURE", "synthetic")
        .env("ETERNAL_SYNTH_SIZE", "320x180")
        .env("ETERNAL_ENCODER", "libx264")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn wait_for(what: &str, seconds: u64, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_restart_copy_signals_then_waits_for_the_running_host() {
    let dir: PathBuf = std::env::temp_dir().join(format!("em-restart-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let logs = dir.join("EternalMonitor").join("logs");
    let log = logs.join("eternal-host-session.log");

    let mut old = Kill(host(&dir, &[free_port().to_string()]).spawn().unwrap());
    wait_for("the first host never got ready", 60, || {
        read(&log).contains("UDP transport ready")
    });

    // The old window's side: the readiness event, then the copy.
    let nonce = format!("{:032x}", std::process::id() as u128 * 7919 + 0xe7e7);
    let event = unsafe {
        CreateEventW(
            None,
            true,
            false,
            &HSTRING::from(format!(r"Local\EternalMonitor-ready-{nonce}")),
        )
    }
    .unwrap();
    let args = [
        free_port().to_string(),
        "--relaunch-wait".into(),
        nonce,
        "--relaunch-for".into(),
        "plain".into(),
    ];
    let mut copy = Kill(host(&dir, &args).spawn().unwrap());
    let signalled = unsafe { WaitForSingleObject(event, 30_000) } == WAIT_OBJECT_0;
    unsafe { CloseHandle(event) }.unwrap();
    assert!(signalled, "the copy never signalled");

    // While the old host runs, the copy waits and leaves its log alone.
    std::thread::sleep(Duration::from_secs(3));
    assert!(copy.0.try_wait().unwrap().is_none(), "the copy exited");
    assert!(
        old.0.try_wait().unwrap().is_none(),
        "the copy ended the host"
    );
    let before = read(&log);
    assert!(before.contains("UDP transport ready"), "log replaced");
    assert!(!before.contains("Started by Restart EternalMonitor"));

    // The old host ends (abandoning its mutex): the copy becomes the host.
    let _ = old.0.kill();
    let _ = old.0.wait();
    wait_for("the copy never became the host", 60, || {
        read(&log).contains("Started by Restart EternalMonitor")
    });
    wait_for("the copy never started streaming", 60, || {
        read(&log).contains("UDP transport ready")
    });
    assert!(copy.0.try_wait().unwrap().is_none(), "the copy exited");
    drop(copy);
    drop(old);
    let _ = std::fs::remove_dir_all(&dir);
}
