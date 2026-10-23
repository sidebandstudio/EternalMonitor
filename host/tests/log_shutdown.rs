//! Repeat counts must reach the session log when the host shuts down. This is
//! its own test binary because the session log path is resolved once per
//! process from APPDATA.

use std::io::Write;

const LINE: &[u8] = b"2026-10-21T03:10:19.347613Z  WARN Desktop duplication access lost\n";

#[test]
fn repeat_counts_are_written_before_logging_shuts_down() {
    let dir = std::env::temp_dir().join(format!("em-log-shutdown-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("APPDATA", &dir);

    let (mut writer, guard) = eternal_host::logging::MemoryLogWriter::start();
    for _ in 0..20 {
        writer.write_all(LINE).unwrap();
    }
    writer.flush().unwrap();
    // Shutdown: main drops the session log guard last.
    drop(guard);

    let log = std::fs::read_to_string(eternal_host::logging::session_log_path()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        log.matches("Desktop duplication access lost").count(),
        2,
        "the first line and one summary:\n{log}"
    );
    assert!(log.contains("[repeated 19 more times"), "{log}");
}
