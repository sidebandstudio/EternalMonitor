//! A normal quit must write pending repeat counts to the session log. The
//! host ends with process::exit, which skips destructors, so this runs the
//! real binary and quits it the way an installer or the user does.
#![cfg(unix)]

use std::net::UdpSocket;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const IGNORED: &str = "Ignored unexpected datagram";

fn wait_for(log: &Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !std::fs::read_to_string(log).is_ok_and(|log| log.contains(text)) {
        assert!(Instant::now() < deadline, "{text:?} never reached {log:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn quitting_writes_pending_repeat_counts() {
    let dir = std::env::temp_dir().join(format!("em-quit-flush-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let app_dir = dir.join("EternalMonitor");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(app_dir.join("diagnostics-upload-disabled"), "test").unwrap();
    let log = app_dir.join("logs").join("eternal-host-session.log");
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut host = Command::new(env!("CARGO_BIN_EXE_eternal-host"))
        .arg(port.to_string())
        .env("APPDATA", &dir)
        .env("ETERNAL_HEADLESS", "1")
        .env("ETERNAL_CAPTURE", "synthetic")
        .env("ETERNAL_SYNTH_SIZE", "320x180")
        .env("ETERNAL_ENCODER", "libx264")
        .env("RUST_LOG", "info,eternal_host::transport=debug")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&log, "UDP transport ready");

    // One datagram the host ignores, then 19 identical ones collapsed into
    // a count that is still pending when the host quits.
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.send_to(b"junk", ("127.0.0.1", port)).unwrap();
    wait_for(&log, IGNORED);
    for _ in 0..19 {
        sender.send_to(b"junk", ("127.0.0.1", port)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(500));
    let status = Command::new("kill")
        .args(["-TERM", &host.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(20);
    let exit = loop {
        if let Some(exit) = host.try_wait().unwrap() {
            break exit;
        }
        if Instant::now() > deadline {
            let _ = host.kill();
            panic!("the host did not quit");
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let text = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(exit.success(), "{exit:?}");
    assert_eq!(
        text.matches(IGNORED).count(),
        2,
        "first line and summary:\n{text}"
    );
    assert!(text.contains("[repeated 19 more times"), "{text}");
}
