use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata, Subscriber};
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::layer::{Context, Filter};
use tracing_subscriber::registry::LookupSpan;

const LOG_BUFFER_LIMIT: usize = 200;
const OUTPUT_BUFFER_LIMIT: usize = 256;
const SESSION_LOG_FILE_NAME: &str = "eternal-host-session.log";

static LOG_BUFFER: Lazy<Arc<Mutex<VecDeque<String>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(LOG_BUFFER_LIMIT))));
static SESSION_LOG_PATH: Lazy<PathBuf> = Lazy::new(resolve_session_log_path);
fn open_session_log() -> Option<File> {
    let path = SESSION_LOG_PATH.clone();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let rotated = match rotate_session_logs(&path) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("Could not rotate session logs: {error}");
            false
        }
    };
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(rotated)
        .append(!rotated)
        .open(&path)
        .ok()
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
}

impl MemoryLogWriter {
    pub fn start() -> (Self, Option<WorkerGuard>) {
        let (session_file, guard) = match open_session_log() {
            Some(file) => {
                let (writer, guard) = non_blocking_output(file);
                (Some(writer), Some(guard))
            }
            None => (None, None),
        };
        (
            Self {
                shared: LOG_BUFFER.clone(),
                session_file,
                current_line: Vec::new(),
            },
            guard,
        )
    }

    fn push_line(&mut self) {
        if self.current_line.is_empty() {
            return;
        }

        let line = String::from_utf8_lossy(&self.current_line)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        self.current_line.clear();

        if line.is_empty() {
            return;
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
            self.current_line.push(byte);
            if byte == b'\n' {
                self.push_line();
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

pub fn session_log_text() -> Option<String> {
    match std::fs::read_to_string(session_log_path()) {
        Ok(text) if !text.trim().is_empty() => Some(text),
        _ => recent_log_text(LOG_BUFFER_LIMIT),
    }
}

pub fn session_log_path() -> PathBuf {
    SESSION_LOG_PATH.clone()
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
        if event.metadata().target() != "mdns_sd" {
            return true;
        }
        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);
        let Some(message) = visitor.message else {
            return true;
        };
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
}

impl MessageVisitor {
    fn new() -> Self {
        Self { message: None }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

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
