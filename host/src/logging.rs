use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata, Subscriber};
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::layer::{Context, Filter};
use tracing_subscriber::registry::LookupSpan;

use crate::log_repeat::RepeatCollapser;

const LOG_BUFFER_LIMIT: usize = 500;
const LOG_FILE_LIMIT: u64 = 2 * 1024 * 1024;
const LOG_LINE_LIMIT: usize = 16 * 1024;
const OUTPUT_BUFFER_LIMIT: usize = 256;
const SESSION_LOG_FILE_NAME: &str = "eternal-host-session.log";

static LOG_BUFFER: Lazy<Arc<Mutex<VecDeque<String>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(LOG_BUFFER_LIMIT))));
static STARTUP_LOG: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));
static OUTPUT_ERRORS: Lazy<Mutex<Option<tracing_appender::non_blocking::ErrorCounter>>> =
    Lazy::new(|| Mutex::new(None));
static STORAGE_FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_STORAGE_ERROR: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));
static SESSION_LOG_PATH: Lazy<PathBuf> = Lazy::new(resolve_session_log_path);
static REPEATS: Lazy<Arc<Mutex<RepeatCollapser>>> = Lazy::new(Default::default);
/// The production writer, kept so repeat summaries can be written on a timer
/// and at shutdown when no other log line arrives to carry them.
static SUMMARY_WRITER: std::sync::OnceLock<Mutex<MemoryLogWriter>> = std::sync::OnceLock::new();
fn open_session_log() -> Option<RollingLogFile> {
    let path = SESSION_LOG_PATH.clone();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let rotated = match rotate_session_logs(&path) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("Could not rotate session logs: {error}");
            note_storage_error(&error);
            false
        }
    };
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(rotated)
        .append(!rotated)
        .open(&path);
    if let Err(error) = &file {
        note_storage_error(error);
    }
    file.ok().map(|file| RollingLogFile {
        bytes: file.metadata().map_or(0, |m| m.len()),
        file: Some(file),
        path,
        limit: LOG_FILE_LIMIT,
    })
}

/// Rotation runs on the output worker, never on capture/transport/GUI threads.
struct RollingLogFile {
    file: Option<File>,
    path: PathBuf,
    bytes: u64,
    limit: u64,
}

impl RollingLogFile {
    fn write_inner(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.file.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            self.bytes = file.metadata()?.len();
            self.file = Some(file);
        }
        if self.bytes > 0 && self.bytes + bytes.len() as u64 > self.limit {
            self.file.take(); // Windows cannot rename an open file.
            let rotated = rotate_session_logs(&self.path).is_ok();
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(rotated)
                .append(!rotated)
                .open(&self.path)?;
            self.bytes = file.metadata()?.len();
            self.file = Some(file);
            if !rotated {
                return Err(io::Error::other("Support log rotation failed"));
            }
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("Support log unavailable"))?;
        let written = file.write(bytes)?;
        self.bytes += written as u64;
        Ok(written)
    }
}

impl Write for RollingLogFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.write_inner(bytes);
        if let Err(error) = &result {
            note_storage_error(error);
        }
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

fn note_storage_error(error: &io::Error) {
    STORAGE_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    *LAST_STORAGE_ERROR.lock() = Some(error.to_string());
}

pub fn redact_support_text(text: &str) -> String {
    static SECRETS: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(
        r#"(?i)\b(pairing_code|pairingCode|auth_token(?:_hex)?|authToken|ingest_key|write_key|token)("?\s*[=:]\s*)("[^"]*"|\[[^\]]*\]|[^\s&,;]+)"#
    ).expect("support redaction pattern")
    });
    SECRETS.replace_all(text, "$1$2[REDACTED]").into_owned()
}

/// A stalled console or disk must not hold a capture, transport or GUI thread.
/// Keep the guard in main so normal shutdown drains the bounded output queue.
pub fn non_blocking_output(writer: impl Write + Send + 'static) -> (NonBlocking, WorkerGuard) {
    NonBlockingBuilder::default()
        .buffered_lines_limit(OUTPUT_BUFFER_LIMIT)
        .lossy(true)
        .finish(writer)
}

fn rotate_session_logs(path: &Path) -> io::Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    let suffix = |number: u8| {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{number}"));
        PathBuf::from(name)
    };
    let older = suffix(2);
    if older.try_exists()? {
        fs::remove_file(&older)?;
    }
    let previous = suffix(1);
    if previous.try_exists()? {
        fs::rename(&previous, &older)?;
    }
    fs::rename(path, previous)
}

#[derive(Clone)]
pub struct MemoryLogWriter {
    shared: Arc<Mutex<VecDeque<String>>>,
    session_file: Option<NonBlocking>,
    current_line: Vec<u8>,
    repeats: Option<Arc<Mutex<RepeatCollapser>>>,
}

/// Keeps the session log open. Dropping it writes pending repeat summaries
/// first, so a burst that was still being counted at exit keeps its count;
/// the inner guard then drains the log file.
pub struct SessionLogGuard(#[allow(dead_code)] Option<WorkerGuard>);

impl Drop for SessionLogGuard {
    fn drop(&mut self) {
        flush_repeat_summaries();
    }
}

/// Writes the summaries of every burst still being counted. Called at
/// shutdown (before support uploads stop, and before the session log closes).
pub fn flush_repeat_summaries() {
    if let Some(writer) = SUMMARY_WRITER.get() {
        writer.lock().emit_repeat_summaries(None);
    }
}

impl MemoryLogWriter {
    pub fn start() -> (Self, SessionLogGuard) {
        let (session_file, guard) = match open_session_log() {
            Some(file) => {
                let (writer, guard) = non_blocking_output(file);
                *OUTPUT_ERRORS.lock() = Some(writer.error_counter());
                (Some(writer), Some(guard))
            }
            None => (None, None),
        };
        let writer = Self {
            shared: LOG_BUFFER.clone(),
            session_file,
            current_line: Vec::new(),
            repeats: Some(REPEATS.clone()),
        };
        if SUMMARY_WRITER.set(Mutex::new(writer.clone())).is_ok() {
            // A burst's summary is due 30 s after it began. Without this tick
            // it waited for the next log line, which may never come.
            let _ = std::thread::Builder::new()
                .name("log-repeats".into())
                .spawn(|| loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if let Some(writer) = SUMMARY_WRITER.get() {
                        writer
                            .lock()
                            .emit_repeat_summaries(Some(std::time::Instant::now()));
                    }
                });
        }
        (writer, SessionLogGuard(guard))
    }

    /// Writes summaries of bursts whose window ended by `due`, or of every
    /// open burst when `due` is `None`.
    fn emit_repeat_summaries(&mut self, due: Option<std::time::Instant>) {
        let Some(repeats) = self.repeats.clone() else {
            return;
        };
        let lines = match due {
            Some(now) => repeats.lock().flush_due(now),
            None => repeats.lock().flush_all(),
        };
        for line in lines {
            self.emit(line);
        }
    }

    fn push_line(&mut self) {
        if self.current_line.is_empty() {
            return;
        }

        let line = String::from_utf8_lossy(&self.current_line)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        self.current_line.clear();
        let line = redact_support_text(&line);
        if let Some(repeats) = self.repeats.clone() {
            let lines = repeats.lock().push(&line, std::time::Instant::now());
            for line in lines {
                self.emit(line);
            }
        } else {
            self.emit(line);
        }
    }

    fn emit(&mut self, line: String) {
        crate::telemetry::record(&line);

        if line.is_empty() {
            return;
        }

        if Arc::ptr_eq(&self.shared, &LOG_BUFFER) {
            let mut startup = STARTUP_LOG.lock();
            if startup.len() < 200 {
                startup.push(line.clone());
            }
        }
        {
            let mut shared = self.shared.lock();
            if shared.len() >= LOG_BUFFER_LIMIT {
                shared.pop_front();
            }
            shared.push_back(line.clone());
        }

        if let Some(file) = &mut self.session_file {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            let _ = file.write_all(&bytes);
        }
    }
}

impl io::Write for MemoryLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &byte in buf {
            if byte == b'\n' {
                self.push_line();
            } else if self.current_line.len() < LOG_LINE_LIMIT {
                self.current_line.push(byte);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.push_line();
        Ok(())
    }
}

pub fn recent_log_text(limit: usize) -> Option<String> {
    let shared = LOG_BUFFER.lock();
    if shared.is_empty() {
        return None;
    }

    let count = limit.max(1);
    let start = shared.len().saturating_sub(count);
    Some(
        shared
            .iter()
            .skip(start)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn read_log_tail(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let offset = file.metadata()?.len().saturating_sub(LOG_FILE_LIMIT);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(LOG_FILE_LIMIT).read_to_end(&mut bytes)?;
    // A legacy log may predate size limits. Skip its partial first line, including
    // any secret whose field name was outside the retained tail.
    let start = if offset > 0 {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1)
    } else {
        0
    };
    Ok(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

pub fn session_log_text() -> Option<String> {
    let dropped = OUTPUT_ERRORS
        .lock()
        .as_ref()
        .map_or(0, |counter| counter.dropped_lines());
    let storage_failures = STORAGE_FAILURES.load(std::sync::atomic::Ordering::Relaxed);
    let storage_error = LAST_STORAGE_ERROR
        .lock()
        .clone()
        .unwrap_or_else(|| "none".to_string());
    let mut report = format!("EternalMonitor support report, schema 1\n{}\nOutput queue dropped lines: {dropped}\nStorage failures: {storage_failures}, last error: {storage_error}\nLogs may contain PC/device names and local network addresses.\n\n--- Startup evidence ---\n{}\n",
        crate::diagnostics::system_report(), STARTUP_LOG.lock().join("\n"));
    let path = session_log_path();
    for suffix in [".2", ".1", ""] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        if let Ok(text) = read_log_tail(&PathBuf::from(name)) {
            report.push_str(&format!("\n--- Log{suffix} ---\n{text}"));
        }
    }
    // Include queued lines that may not have reached disk yet, especially the latest error.
    if let Some(recent) = recent_log_text(LOG_BUFFER_LIMIT) {
        report.push_str(&format!("\n--- Recent memory log ---\n{recent}\n"));
    }
    report.push_str(&format!(
        "\nAutomatic upload: {}\n",
        crate::telemetry::status()
    ));
    Some(redact_support_text(&report))
}

pub fn session_log_path() -> PathBuf {
    SESSION_LOG_PATH.clone()
}

/// Release the log and upload guards in the order a normal quit needs. A
/// session-log guard may emit final lines, such as repeat summaries, while it
/// drops; those reach support only while the upload guard still queues lines.
/// The upload guard then writes what is queued to the outbox, within its
/// bound, and console output goes last.
pub fn flush_before_exit<S, U, C>(session_log: S, uploads: U, console: C) {
    drop(session_log);
    drop(uploads);
    drop(console);
}

fn resolve_session_log_path() -> PathBuf {
    // Prefer %APPDATA%/EternalMonitor/logs so the log is writable even when the app is installed
    // read-only under Program Files; fall back to the exe directory only if APPDATA is unavailable.
    let logs_dir = crate::settings::app_data_dir()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(PathBuf::from))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("logs");
    logs_dir.join(SESSION_LOG_FILE_NAME)
}

// --- mDNS interface-failure deduper -----------------------------------------
//
// The `mdns-sd` crate emits one WARN per send attempt per interface when an
// interface (often Tailscale or other tunnels) refuses the multicast send.
// At ~10s probe cadence on multiple interfaces this floods the log. We keep a
// shared HashSet of interface markers seen so far and let only the FIRST
// failure per interface through.

static SUPPRESSED_MDNS_INTERFACES: Lazy<Arc<StdMutex<HashSet<String>>>> =
    Lazy::new(|| Arc::new(StdMutex::new(HashSet::new())));

#[derive(Clone)]
pub struct MdnsDedupFilter;

impl MdnsDedupFilter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MdnsDedupFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Filter<S> for MdnsDedupFilter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, _metadata: &Metadata<'_>, _ctx: &Context<'_, S>) -> bool {
        true
    }

    fn event_enabled(&self, event: &Event<'_>, _ctx: &Context<'_, S>) -> bool {
        let target = event.metadata().target();
        if target != "log" && target != "mdns_sd" && !target.starts_with("mdns_sd::") {
            return true;
        }
        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);
        // mdns-sd uses the log facade. Its tracing events carry the original
        // module in log.target rather than in the event's static metadata.
        let target = visitor.log_target.as_deref().unwrap_or(target);
        if target != "mdns_sd" && !target.starts_with("mdns_sd::") {
            return true;
        }
        let Some(message) = visitor.message else {
            return true;
        };
        // mdns-sd 0.12.0's register_service emits this ERROR after a successful
        // announcement, immediately before setting ServiceStatus::Announced.
        // Our discovery module already logs registration. Keep actual errors.
        if *event.metadata().level() == tracing::Level::ERROR
            && message.starts_with("Announce service ")
            && message
                .rsplit_once(" on ")
                .is_some_and(|(_, address)| address.parse::<std::net::IpAddr>().is_ok())
        {
            return false;
        }
        // mdns-sd's send failure messages look like:
        //   "Failed to send to ... interface ..."
        // or in raw socket form contain a zone id like "%5". Bucket on either.
        if !message.contains("Failed to send") {
            return true;
        }
        let bucket = interface_bucket(&message);
        let mut set = match SUPPRESSED_MDNS_INTERFACES.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // First failure per interface: insert returns true → emit.
        // Subsequent failures on the same interface: insert returns false → drop.
        set.insert(bucket)
    }
}

fn interface_bucket(message: &str) -> String {
    if let Some(idx) = message.find("interface") {
        let rest = &message[idx + "interface".len()..];
        let trimmed = rest.trim_start_matches([':', ' ']);
        let end = trimmed.find([',', '"', ')']).unwrap_or(trimmed.len());
        return trimmed[..end].trim().to_string();
    }
    if let Some(pct) = message.find('%') {
        let rest = &message[pct + 1..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .unwrap_or(rest.len());
        if end > 0 {
            return format!("%{}", &rest[..end]);
        }
    }
    // Fallback: dedupe by the verb prefix so all "Failed to send" messages
    // collapse to a single line if we can't extract an interface name.
    "generic".to_string()
}

struct MessageVisitor {
    message: Option<String>,
    log_target: Option<String>,
}

impl MessageVisitor {
    fn new() -> Self {
        Self {
            message: None,
            log_target: None,
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{:?}", value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(value.to_string());
        } else if field.name() == "log.target" {
            self.log_target = Some(value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn mdns_success_announcements_do_not_hide_real_errors() {
        use tracing_subscriber::prelude::*;
        let shared = Arc::new(Mutex::new(VecDeque::new()));
        let writer = MemoryLogWriter {
            shared: shared.clone(),
            session_file: None,
            current_line: Vec::new(),
            repeats: None,
        };
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .with_filter(MdnsDedupFilter::new()),
        );
        tracing::subscriber::with_default(subscriber, || {
            // The log-to-tracing bridge stores the original module in log.target.
            tracing::error!(target: "log", { log.target = "mdns_sd::service_daemon" },
                "Announce service Monitor._eternaldisplay._udp.local. on 10.0.0.1");
            tracing::error!(target: "mdns_sd", "Announce service Monitor._eternaldisplay._udp.local. on ::1");
            tracing::error!(target: "log", { log.target = "mdns_sd::service_daemon" }, "discovery socket failed");
            tracing::error!(target: "mdns_sd", "Announce service failed on invalid interface");
            tracing::error!(target: "another_crate", "Announce service Other._udp.local. on 10.0.0.2");
            tracing::info!(target: "mdns_sd", "Announce service Info._udp.local. on 10.0.0.3");
        });
        let lines = shared.lock();
        assert_eq!(lines.len(), 4, "unexpected filtered logs: {lines:?}");
        for expected in [
            "discovery socket failed",
            "invalid interface",
            "Other._udp.local.",
            "Info._udp.local.",
        ] {
            assert!(
                lines.iter().any(|line| line.contains(expected)),
                "lost {expected}"
            );
        }
    }

    #[test]
    fn support_redacts_credentials_but_keeps_error_codes_and_session_ids() {
        let text = r#"pairing_code=123456 auth_token_hex="00112233445566778899aabbccddeeff" token=[1, 2, 3] pairingCode:654321 status=-12909 session_id=123456 "auth_token_hex": "json-secret""#;
        let redacted = redact_support_text(text);
        for secret in [
            "001122",
            "654321",
            "[1, 2, 3]",
            "pairing_code=123456",
            "json-secret",
        ] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        assert!(redacted.contains("status=-12909 session_id=123456"));
    }

    #[test]
    fn quit_drops_session_log_guards_while_uploads_still_queue() {
        struct Guard(&'static str, Arc<Mutex<Vec<&'static str>>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.1.lock().push(self.0);
            }
        }
        let order = Arc::new(Mutex::new(Vec::new()));
        flush_before_exit(
            Guard("session log", order.clone()),
            Guard("uploads", order.clone()),
            Guard("console", order.clone()),
        );
        assert_eq!(*order.lock(), ["session log", "uploads", "console"]);
    }

    #[test]
    fn a_burst_summary_is_written_when_its_window_ends_without_more_traffic() {
        let shared = Arc::new(Mutex::new(VecDeque::new()));
        let mut writer = MemoryLogWriter {
            shared: shared.clone(),
            session_file: None,
            current_line: Vec::new(),
            repeats: Some(Default::default()),
        };
        for _ in 0..20 {
            writer
                .write_all(b"2026-10-21T03:10:19.347613Z  WARN access lost\n")
                .unwrap();
        }
        assert_eq!(shared.lock().len(), 1);
        // The ticker's call before the window ends writes nothing.
        writer.emit_repeat_summaries(Some(std::time::Instant::now()));
        assert_eq!(shared.lock().len(), 1);
        writer.emit_repeat_summaries(Some(
            std::time::Instant::now() + crate::log_repeat::REPEAT_WINDOW,
        ));
        let lines: Vec<_> = shared.lock().iter().cloned().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("access lost [repeated 19 more times"));
    }

    #[test]
    fn oversized_line_is_bounded_and_does_not_leak_credential_tail() {
        let shared = Arc::new(Mutex::new(VecDeque::new()));
        let mut writer = MemoryLogWriter {
            shared: shared.clone(),
            session_file: None,
            current_line: Vec::new(),
            repeats: None,
        };
        writer
            .write_all(
                format!(
                    "auth_token={}\nnext event\n",
                    "secret".repeat(LOG_LINE_LIMIT)
                )
                .as_bytes(),
            )
            .unwrap();
        let lines = shared.lock();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "auth_token=[REDACTED]");
        assert_eq!(lines[1], "next event");
    }

    #[test]
    fn legacy_oversized_log_export_is_bounded_and_skips_partial_first_line() {
        let path = std::env::temp_dir().join(format!(
            "eternal-tail-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut text = "auth_token=".to_string();
        text.push_str(&"s".repeat(LOG_FILE_LIMIT as usize));
        text.push_str("\nlatest error status=-123\n");
        fs::write(&path, text).unwrap();
        assert_eq!(read_log_tail(&path).unwrap(), "latest error status=-123\n");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn storage_failure_is_visible_and_writer_can_recover() {
        let directory = std::env::temp_dir().join(format!(
            "eternal-storage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("blocked");
        fs::create_dir_all(&path).unwrap();
        let mut output = RollingLogFile {
            file: None,
            path: path.clone(),
            bytes: 0,
            limit: 64,
        };
        assert!(output.write_all(b"failed\n").is_err());
        assert!(LAST_STORAGE_ERROR.lock().is_some());
        fs::remove_dir(&path).unwrap();
        output.write_all(b"recovered\n").unwrap();
        drop(output);
        assert_eq!(fs::read_to_string(&path).unwrap(), "recovered\n");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn running_log_rotates_by_size_and_keeps_latest_events() {
        let dir = std::env::temp_dir().join(format!(
            "eternal-size-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SESSION_LOG_FILE_NAME);
        let mut output = RollingLogFile {
            file: Some(File::create(&path).unwrap()),
            path: path.clone(),
            bytes: 0,
            limit: 64,
        };
        for n in 0..100 {
            writeln!(output, "event-{n:03}").unwrap();
        }
        output.flush().unwrap();
        drop(output);
        assert!(fs::read_to_string(&path).unwrap().contains("event-099"));
        let files: Vec<_> = fs::read_dir(&dir).unwrap().map(Result::unwrap).collect();
        assert_eq!(files.len(), 3);
        for file in files {
            assert!(file.metadata().unwrap().len() <= 64);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_ffmpeg_errors_include_formatted_driver_details_in_support_logs() {
        use tracing_subscriber::prelude::*;
        let shared = Arc::new(Mutex::new(VecDeque::new()));
        let writer = MemoryLogWriter {
            shared: shared.clone(),
            session_file: None,
            current_line: Vec::new(),
            repeats: None,
        };
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(move || writer.clone()),
        );
        tracing::subscriber::with_default(subscriber, || {
            crate::diagnostics::install_ffmpeg_logging();
            let format =
                std::ffi::CString::new("support driver probe status=%d device=%s\n").unwrap();
            let device = std::ffi::CString::new("test-gpu").unwrap();
            unsafe {
                ffmpeg_next::ffi::av_log(
                    std::ptr::null_mut(),
                    ffmpeg_next::ffi::AV_LOG_ERROR,
                    format.as_ptr(),
                    -123i32,
                    device.as_ptr(),
                );
            }
        });
        assert!(shared
            .lock()
            .iter()
            .any(|line| line.contains("support driver probe status=-123 device=test-gpu")));
    }

    struct BlockedOutput {
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl Write for BlockedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_output_drops_overflow_without_blocking_recent_logs() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (output, guard) = non_blocking_output(BlockedOutput {
            entered: Some(entered_tx),
            release: release_rx,
        });
        let errors = output.error_counter();
        let shared = Arc::new(Mutex::new(VecDeque::new()));
        let mut writer = MemoryLogWriter {
            shared: shared.clone(),
            session_file: Some(output),
            current_line: Vec::new(),
            repeats: None,
        };
        writer.write_all(b"first\n").unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            for line in 0..OUTPUT_BUFFER_LIMIT * 2 {
                writeln!(writer, "line {line}").unwrap();
            }
            done_tx.send(()).unwrap();
        });
        let completed = done_rx.recv_timeout(Duration::from_secs(2));
        // Always release the simulated disk before asserting, even on failure.
        release_tx.send(()).unwrap();
        producer.join().unwrap();
        drop(guard);
        assert!(completed.is_ok(), "log output blocked the producer");
        assert!(
            errors.dropped_lines() > 0,
            "the bounded queue did not overflow"
        );
        let recent = shared.lock();
        assert_eq!(recent.len(), LOG_BUFFER_LIMIT);
        assert_eq!(
            recent.back().unwrap(),
            &format!("line {}", OUTPUT_BUFFER_LIMIT * 2 - 1)
        );
    }

    #[test]
    fn output_guard_flushes_complete_lines_on_shutdown() {
        #[derive(Clone)]
        struct RecordedOutput(Arc<Mutex<Vec<u8>>>);
        impl Write for RecordedOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let recorded = RecordedOutput(Arc::new(Mutex::new(Vec::new())));
        let (mut writer, guard) = non_blocking_output(recorded.clone());
        writer.write_all(b"first\nlast\n").unwrap();
        drop(guard);
        assert_eq!(&*recorded.0.lock(), b"first\nlast\n");
    }

    #[test]
    fn rotation_keeps_two_previous_sessions_and_preserves_them_without_a_current_log() {
        let dir = std::env::temp_dir().join(format!(
            "eternal-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SESSION_LOG_FILE_NAME);
        for session in 0..4 {
            rotate_session_logs(&path).unwrap();
            fs::write(&path, session.to_string()).unwrap();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "3");
        assert_eq!(
            fs::read_to_string(dir.join(format!("{SESSION_LOG_FILE_NAME}.1"))).unwrap(),
            "2"
        );
        assert_eq!(
            fs::read_to_string(dir.join(format!("{SESSION_LOG_FILE_NAME}.2"))).unwrap(),
            "1"
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 3);
        fs::remove_file(&path).unwrap();
        rotate_session_logs(&path).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(format!("{SESSION_LOG_FILE_NAME}.1"))).unwrap(),
            "2"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
