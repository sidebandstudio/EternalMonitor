//! A burst's repeat count must reach the session log about 30 s after the
//! burst began, even if nothing else is logged. Its own test binary because
//! the session log path is resolved once per process from APPDATA.

use std::io::Write;
use std::time::{Duration, Instant};

#[test]
fn a_burst_summary_is_written_without_further_log_traffic() {
    let dir = std::env::temp_dir().join(format!("em-log-timer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("APPDATA", &dir);

    let (mut writer, _guard) = eternal_host::logging::MemoryLogWriter::start();
    for _ in 0..20 {
        writer
            .write_all(b"2026-10-21T03:10:19.347613Z  WARN Desktop duplication access lost\n")
            .unwrap();
    }
    writer.flush().unwrap();

    let path = eternal_host::logging::session_log_path();
    let deadline =
        Instant::now() + eternal_host::log_repeat::REPEAT_WINDOW + Duration::from_secs(5);
    let log = loop {
        let log = std::fs::read_to_string(&path).unwrap_or_default();
        if log.contains("[repeated 19 more times") || Instant::now() > deadline {
            break log;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let _ = std::fs::remove_dir_all(&dir);
    assert!(log.contains("[repeated 19 more times"), "{log}");
}
