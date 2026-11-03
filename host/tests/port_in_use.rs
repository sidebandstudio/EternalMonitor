//! Another program holds the stream port when the host starts. The host says
//! so once, waits without a restart storm and without parking in Failed,
//! and starts streaming within about 2 s of the port's release. Runs the
//! real binary headless.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn wait_for(log: &Path, line: &str, limit: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if read(log).contains(line) {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn a_held_stream_port_is_waited_for_and_the_stream_starts_once_free() {
    let dir = std::env::temp_dir().join(format!("em-port-in-use-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir
        .join("EternalMonitor")
        .join("logs")
        .join("eternal-host-session.log");

    // Bound as the stream binds it.
    let holder = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = holder.local_addr().unwrap().port();
    let mut host = Command::new(env!("CARGO_BIN_EXE_eternal-host"))
        .arg(port.to_string())
        .env("APPDATA", &dir)
        .env("ETERNAL_HEADLESS", "1")
        .env("ETERNAL_CAPTURE", "synthetic")
        .env("ETERNAL_SYNTH_SIZE", "320x180")
        .env("ETERNAL_ENCODER", "libx264")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let result = std::panic::catch_unwind(|| {
        let noticed = "Another program is using the stream port";
        assert!(
            wait_for(&log, noticed, Duration::from_secs(60)).is_some(),
            "the held port was never reported:\n{}",
            read(&log)
        );
        // Several checks while it stays held.
        std::thread::sleep(Duration::from_secs(7));
        let waiting = read(&log);
        for line in [
            "UDP transport ready",
            "automatic restart scheduled",
            "giving up until a manual Start",
        ] {
            assert!(!waiting.contains(line), "{line:?} while held:\n{waiting}");
        }
        assert_eq!(waiting.matches(noticed).count(), 1, "said once:\n{waiting}");

        drop(holder);
        let started = wait_for(&log, "UDP transport ready", Duration::from_secs(10))
            .unwrap_or_else(|| panic!("the stream never started:\n{}", read(&log)));
        eprintln!("streaming started {started:?} after the port was released");
        assert!(started < Duration::from_millis(3_500), "{started:?}");
        assert!(read(&log).contains("The stream port is free; starting the stream"));
    });
    let _ = host.kill();
    let _ = host.wait();
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
