//! Automatic support uploads. A bounded channel protects media threads; a disk outbox
//! retains unacknowledged events across launches. No dashboard credential belongs here.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_FILES: usize = 512;
const BATCH_SIZE: usize = 4;
/// Longest a normal quit waits for uploads. Unsent batches stay in the outbox.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// After that, how long a quit waits for the last queued lines to reach the
/// outbox. A few small files take milliseconds.
const FINAL_WRITE_WAIT: Duration = Duration::from_millis(250);
/// Events another process adds to a launch number from here, clear of the
/// launch's own sequence, which the backend keeps unique per launch.
pub const FOREIGN_SEQUENCE_BASE: u64 = 1 << 40;
static SENDER: OnceLock<mpsc::SyncSender<Queued>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(true);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static STATUS: Mutex<String> = Mutex::new(String::new());
static IDENTITY: OnceLock<(String, String)> = OnceLock::new();
static OUTBOX: OnceLock<PathBuf> = OnceLock::new();
// Shared by the upload thread and fatal reports, so events never reuse a number.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub ingest_key: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_http: bool,
}
fn default_enabled() -> bool {
    true
}
impl Config {
    fn load() -> Option<Self> {
        let path = crate::settings::app_data_dir()?.join("diagnostics-upload.json");
        let mut config: Self =
            fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .or_else(|| {
                    Some(Self {
                        endpoint: std::env::var("ETERNAL_DIAGNOSTICS_ENDPOINT").ok().or_else(
                            || option_env!("ETERNAL_DIAGNOSTICS_ENDPOINT").map(str::to_owned),
                        )?,
                        ingest_key: std::env::var("ETERNAL_DIAGNOSTICS_INGEST_KEY")
                            .ok()
                            .or_else(|| {
                                option_env!("ETERNAL_DIAGNOSTICS_INGEST_KEY").map(str::to_owned)
                            })?,
                        enabled: true,
                        allow_http: false,
                    })
                })?;
        if let Ok(value) = std::env::var("ETERNAL_DIAGNOSTICS_ENDPOINT") {
            config.endpoint = value;
        }
        if let Ok(value) = std::env::var("ETERNAL_DIAGNOSTICS_INGEST_KEY") {
            config.ingest_key = value;
        }
        config.allow_http = std::env::var("ETERNAL_DIAGNOSTICS_ALLOW_HTTP").is_ok_and(|x| x == "1");
        config.valid().then_some(config)
    }
    fn valid(&self) -> bool {
        let uri: Result<ureq::http::Uri, _> = self.endpoint.parse();
        uri.is_ok_and(|uri| {
            let scheme = uri.scheme_str().unwrap_or_default();
            let authority = uri.authority().map_or("", |x| x.as_str());
            (scheme == "https" || (scheme == "http" && self.allow_http))
                && !authority.is_empty()
                && !authority.contains('@')
                && uri.query().is_none()
        }) && !self.ingest_key.is_empty()
            && !self.ingest_key.contains(['\r', '\n'])
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Event {
    sequence: u64,
    at_ms: u64,
    level: String,
    category: String,
    message: String,
}
#[derive(Serialize, Deserialize)]
struct Batch {
    schema: u32,
    batch_id: String,
    installation_id: String,
    launch_id: String,
    platform: String,
    app_version: String,
    build: String,
    events: Vec<Event>,
}

fn id() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn set_status(text: impl Into<String>) {
    if let Ok(mut value) = STATUS.lock() {
        *value = text.into();
    }
}
pub fn status() -> String {
    format!(
        "{}{}; queue_dropped={}",
        STATUS.lock().map(|x| x.clone()).unwrap_or_default(),
        IDENTITY
            .get()
            .map_or_else(String::new, |(installation, launch)| {
                format!("; installation_id={installation}; launch_id={launch}")
            }),
        DROPPED.load(Ordering::Relaxed)
    )
}

/// This launch's upload id, once uploads are configured.
pub fn launch_id() -> Option<String> {
    IDENTITY.get().map(|(_, launch)| launch.clone())
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}
pub fn set_enabled(value: bool) {
    let path = crate::settings::app_data_dir().map(|p| p.join("diagnostics-upload-disabled"));
    if let Some(path) = path {
        let result = if value {
            if path.exists() {
                fs::remove_file(path)
            } else {
                Ok(())
            }
        } else {
            fs::write(path, b"disabled")
        };
        if result.is_err() {
            set_status("Could not save automatic upload preference");
            return;
        }
    }
    ENABLED.store(value, Ordering::Relaxed);
    set_status(if value {
        "Automatic upload enabled"
    } else {
        "Automatic upload disabled"
    });
}

/// What the upload thread receives. A log line, even an empty one, is always
/// an event; only the shutdown sends `Wake`.
enum Queued {
    Line(String),
    Wake,
}

/// The log lines among `queued`, as events.
fn events(queued: impl Iterator<Item = Queued>) -> impl Iterator<Item = Event> {
    queued.filter_map(|queued| match queued {
        Queued::Line(line) => Some(event(&line, level(&line))),
        Queued::Wake => None,
    })
}

pub struct UploadGuard {
    _uploader: Option<Uploader>,
}

/// The upload thread. Dropping it waits at most [`SHUTDOWN_WAIT`] and
/// [`FINAL_WRITE_WAIT`]; if a request is still in flight, another thread
/// writes the queued lines to the outbox.
struct Uploader {
    stop: std::sync::Arc<AtomicBool>,
    /// Wakes a thread waiting for lines, so an idle quit does not wait out
    /// its receive timeout.
    wake: mpsc::SyncSender<Queued>,
    receiver: std::sync::Arc<Mutex<mpsc::Receiver<Queued>>>,
    directory: PathBuf,
    done: mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Uploader {
    fn spawn(config: Config, directory: PathBuf) -> (mpsc::SyncSender<Queued>, Self) {
        let (sender, receiver) = mpsc::sync_channel(256);
        let receiver = std::sync::Arc::new(Mutex::new(receiver));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (done_tx, done) = mpsc::channel();
        let thread = {
            let (receiver, stop, directory) = (receiver.clone(), stop.clone(), directory.clone());
            std::thread::spawn(move || {
                if run(config, directory, receiver, stop).is_err() {
                    set_status("Automatic upload stopped: local outbox unavailable");
                }
                let _ = done_tx.send(());
            })
        };
        let uploader = Self {
            stop,
            wake: sender.clone(),
            receiver,
            directory,
            done,
            thread: Some(thread),
        };
        (sender, uploader)
    }
}
impl Drop for Uploader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(Queued::Wake);
        if self.done.recv_timeout(SHUTDOWN_WAIT).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            return;
        }
        // The thread is stuck in a request or a write. Keep what it has not
        // written yet, but from another thread: the disk can stall as well,
        // and writing here made the quit last as long as the stall.
        #[cfg(test)]
        tests::GAVE_UP.with(|gave_up| gave_up.set(true));
        let (receiver, directory, sharing) =
            (self.receiver.clone(), self.directory.clone(), enabled());
        let (written_tx, written) = mpsc::channel();
        let _ = std::thread::Builder::new().spawn(move || {
            keep_queued(&receiver, &directory, sharing);
            let _ = written_tx.send(());
        });
        let _ = written.recv_timeout(FINAL_WRITE_WAIT);
    }
}
/// The quit's fallback: writes the lines still queued, or clears everything
/// pending when sharing is off. If the upload thread is writing the queue
/// itself, this waits for it and finds nothing left.
fn keep_queued(receiver: &Mutex<mpsc::Receiver<Queued>>, directory: &Path, sharing: bool) {
    let Ok(receiver) = receiver.lock() else {
        return;
    };
    if !sharing {
        // Sharing was turned off during the stalled request. Clear what is
        // pending, as the thread does.
        if let Ok(paths) = files(directory) {
            for path in paths {
                let _ = fs::remove_file(path);
            }
        }
        while receiver.try_recv().is_ok() {}
        return;
    }
    let events: Vec<Event> = events(receiver.try_iter()).collect();
    for chunk in events.chunks(BATCH_SIZE) {
        if let Ok(batch) = batch(chunk.to_vec()) {
            let _ = persist(directory, &batch);
        }
    }
}
impl UploadGuard {
    pub fn start() -> Self {
        ENABLED.store(
            !crate::settings::app_data_dir()
                .is_some_and(|p| p.join("diagnostics-upload-disabled").exists()),
            Ordering::Relaxed,
        );
        let Some(config) = Config::load() else {
            set_status("Automatic upload is not configured");
            return Self { _uploader: None };
        };
        if !config.enabled {
            set_status("Automatic upload is disabled");
            return Self { _uploader: None };
        }
        let directory = crate::settings::app_data_dir()
            .unwrap()
            .join("diagnostics-outbox");
        // Ready before any thread runs, so a fatal report can queue at once.
        let identity = match identity(&directory) {
            Ok(identity) => identity,
            Err(_) => {
                set_status("Automatic upload stopped: local outbox unavailable");
                return Self { _uploader: None };
            }
        };
        if SENDER.get().is_some() {
            return Self { _uploader: None };
        }
        let _ = IDENTITY.set(identity);
        let _ = OUTBOX.set(directory.clone());
        let (sender, uploader) = Uploader::spawn(config, directory);
        let _ = SENDER.set(sender);
        Self {
            _uploader: Some(uploader),
        }
    }
}
impl Drop for UploadGuard {
    fn drop(&mut self) {
        // Counts of bursts still being collapsed belong in the last upload.
        // The uploader drops after this, within its bound.
        crate::logging::flush_repeat_summaries();
    }
}
pub fn record(line: &str) {
    if enabled() {
        if let Some(sender) = SENDER.get() {
            let mut end = line.len().min(8192);
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            if sender
                .try_send(Queued::Line(line[..end].to_owned()))
                .is_err()
            {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
/// Queue one event (`level` is `error` or `info`) on disk now. The upload
/// thread sends it, or the next launch does if this process ends first. Ok
/// when the event is saved or uploads are off.
pub fn queue_now(message: &str, level: &str) -> std::io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    let Some(directory) = OUTBOX.get() else {
        return Ok(());
    };
    let queued = queue_into(directory, message, level);
    if queued.is_err() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
    queued
}
fn queue_into(directory: &Path, message: &str, level: &str) -> std::io::Result<()> {
    persist(directory, &batch(vec![event(message, level)])?)
}
/// Queue an event for another launch of this installation, from the failure
/// reporter process. `index` keeps several such events distinct. Ok when the
/// event is saved or uploads are off.
pub fn queue_for_launch(
    launch_id: &str,
    index: u64,
    message: &str,
    level: &str,
) -> std::io::Result<()> {
    let Some(base) = crate::settings::app_data_dir() else {
        return Ok(());
    };
    if base.join("diagnostics-upload-disabled").exists()
        || !Config::load().is_some_and(|c| c.enabled)
    {
        return Ok(());
    }
    queue_foreign(
        &base.join("diagnostics-outbox"),
        launch_id,
        index,
        message,
        level,
    )
}
fn queue_foreign(
    directory: &Path,
    launch_id: &str,
    index: u64,
    message: &str,
    level: &str,
) -> std::io::Result<()> {
    let installation_id = fs::read_to_string(directory.join("installation-id"))?;
    let mut event = event(message, level);
    event.sequence = FOREIGN_SEQUENCE_BASE + index;
    let mut batch = batch(vec![event])?;
    batch.installation_id = installation_id.trim().to_string();
    batch.launch_id = launch_id.to_string();
    persist(directory, &batch)
}
fn identity(directory: &Path) -> std::io::Result<(String, String)> {
    fs::create_dir_all(directory)?;
    let identity_path = directory.join("installation-id");
    let installation_id = match fs::read_to_string(&identity_path) {
        Ok(id) if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) => id,
        _ => {
            let id = id()?;
            fs::write(identity_path, &id)?;
            id
        }
    };
    Ok((installation_id, id()?))
}
/// Lines drained at shutdown keep their level: an error logged just before
/// exit is usually the one that explains the exit.
fn level(line: &str) -> &'static str {
    if line.contains(" ERROR ") {
        "error"
    } else if line.contains(" WARN ") {
        "warning"
    } else {
        "info"
    }
}
fn event(line: &str, level: &str) -> Event {
    Event {
        sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
        at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        level: level.into(),
        category: "host".into(),
        message: crate::logging::redact_support_text(line),
    }
}
fn batch(events: Vec<Event>) -> std::io::Result<Batch> {
    let (installation_id, launch_id) = IDENTITY.get().cloned().unwrap_or_default();
    Ok(Batch {
        schema: 1,
        batch_id: id()?,
        installation_id,
        launch_id,
        platform: std::env::consts::OS.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        build: env!("ETERNAL_BUILD_REVISION").into(),
        events,
    })
}
fn files(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|x| x.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    Ok(paths)
}
fn persist(directory: &Path, batch: &Batch) -> std::io::Result<()> {
    // The upload thread, a bounded shutdown and fatal reports may all write.
    static PERSIST: Mutex<()> = Mutex::new(());
    let _guard = PERSIST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    #[cfg(test)]
    tests::stall_while_held(directory);
    let bytes = serde_json::to_vec(batch)?;
    let path = directory.join(format!("{}-{}.json", batch.events[0].at_ms, batch.batch_id));
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)?;
    let paths = files(directory)?;
    let mut total: u64 = paths
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    let mut count = paths.len();
    for path in paths.iter() {
        if count <= MAX_FILES && total <= 8 * 1024 * 1024 {
            break;
        }
        total = total.saturating_sub(fs::metadata(path).map_or(0, |m| m.len()));
        count -= 1;
        let lost = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Batch>(&bytes).ok())
            .map_or(1, |batch| batch.events.len());
        fs::remove_file(path)?;
        DROPPED.fetch_add(lost as u64, Ordering::Relaxed);
    }
    Ok(())
}
fn upload(
    agent: &ureq::Agent,
    config: &Config,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let batch: Batch = match serde_json::from_slice(&bytes) {
        Ok(batch) => batch,
        Err(_) => {
            fs::remove_file(path)?;
            DROPPED.fetch_add(1, Ordering::Relaxed);
            set_status("A damaged upload batch was discarded");
            return Ok(());
        }
    };
    let mut response = agent
        .post(&config.endpoint)
        .header("Authorization", format!("Bearer {}", config.ingest_key))
        .header("Content-Type", "application/json")
        .send(bytes)?;
    if response.status() != ureq::http::StatusCode::OK {
        return Err("Upload did not return HTTP 200".into());
    }
    let ack: serde_json::Value = response.body_mut().with_config().limit(4096).read_json()?;
    if ack.get("accepted_batch").and_then(|x| x.as_str()) != Some(&batch.batch_id) {
        return Err("Upload acknowledgement mismatch".into());
    }
    fs::remove_file(path)?;
    Ok(())
}
fn run(
    config: Config,
    directory: PathBuf,
    receiver: std::sync::Arc<Mutex<mpsc::Receiver<Queued>>>,
    stop: std::sync::Arc<AtomicBool>,
) -> std::io::Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .max_redirects(0)
        .build()
        .into();
    let mut pending = Vec::new();
    let mut next = std::time::Instant::now();
    let mut backoff = 5;
    let mut last_persist = std::time::Instant::now();
    set_status("Automatic upload enabled; waiting for events");
    loop {
        if stop.load(Ordering::SeqCst) {
            return finish(&directory, &receiver, pending);
        }
        let received = match receiver.lock() {
            Ok(receiver) => receiver.recv_timeout(Duration::from_millis(100)),
            Err(_) => break,
        };
        match received {
            Ok(Queued::Wake) => {}
            Ok(Queued::Line(line)) => pending.push(event(&line, level(&line))),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if stop.load(Ordering::SeqCst) {
            return finish(&directory, &receiver, pending);
        }
        if !enabled() {
            pending.clear();
            for path in files(&directory)? {
                fs::remove_file(path)?;
            }
            if let Ok(receiver) = receiver.lock() {
                while receiver.try_recv().is_ok() {}
            }
            continue;
        }
        if pending.len() >= BATCH_SIZE
            || (!pending.is_empty() && last_persist.elapsed() >= Duration::from_secs(1))
        {
            persist(&directory, &batch(std::mem::take(&mut pending))?)?;
            last_persist = std::time::Instant::now();
        }
        if std::time::Instant::now() >= next {
            // Nothing stays only in memory while a request is in flight.
            if !pending.is_empty() {
                persist(&directory, &batch(std::mem::take(&mut pending))?)?;
                last_persist = std::time::Instant::now();
            }
            let mut failed = false;
            for path in files(&directory)?.into_iter().take(8) {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if upload(&agent, &config, &path).is_err() {
                    failed = true;
                    break;
                }
            }
            if failed {
                set_status(format!(
                    "Automatic upload waiting to retry; pending_batches={}",
                    files(&directory)?.len()
                ));
                backoff = (backoff * 2).min(300);
            } else {
                set_status(format!(
                    "Automatic upload connected; pending_batches={}",
                    files(&directory)?.len()
                ));
                backoff = 5;
            }
            next = std::time::Instant::now() + Duration::from_secs(backoff);
        }
    }
    Ok(())
}
/// Writes the events this thread holds and those still queued, all under the
/// receiver lock. The quit's fallback takes the same lock, so each queued
/// line is written once.
fn finish(
    directory: &Path,
    receiver: &Mutex<mpsc::Receiver<Queued>>,
    mut pending: Vec<Event>,
) -> std::io::Result<()> {
    let Ok(receiver) = receiver.lock() else {
        return Ok(());
    };
    if !enabled() {
        for path in files(directory)? {
            fs::remove_file(path)?;
        }
        while receiver.try_recv().is_ok() {}
        return Ok(());
    }
    pending.extend(events(receiver.try_iter()));
    #[cfg(test)]
    tests::pause_before_final_write(directory);
    for chunk in pending.chunks(BATCH_SIZE) {
        persist(directory, &batch(chunk.to_vec())?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn events_keep_their_level_on_every_path() {
        let [warn, error, info] = [
            "2026-10-21T03:10:19.347613Z  WARN Desktop duplication access lost",
            "2026-10-21T03:04:31.464409Z ERROR Capture loop exited with error",
            "2026-10-21T03:04:31.478265Z  INFO Pipeline ended generation=7",
        ]
        .map(|line| event(line, level(line)));
        assert_eq!(
            [warn.level, error.level, info.level],
            ["warning", "error", "info"]
        );
    }
    thread_local! {
        /// Set on the thread whose `Uploader` drop stopped waiting for its thread.
        pub(super) static GAVE_UP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Serves each upload after `delay`, acknowledging its batch. The counter
    /// says how many requests have arrived.
    fn slow_server(delay: Duration) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let arrived = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = arrived.clone();
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let counter = counter.clone();
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let body = loop {
                        let Ok(count) = stream.read(&mut buffer) else {
                            return;
                        };
                        if count == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                        let Some(end) = bytes.windows(4).position(|x| x == b"\r\n\r\n") else {
                            continue;
                        };
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let size: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + size {
                            break bytes[end + 4..end + 4 + size].to_vec();
                        }
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(delay);
                    let batch: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let reply =
                        serde_json::json!({"accepted_batch": batch["batch_id"]}).to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                });
            }
        });
        (format!("http://{address}/v1/events"), arrived)
    }

    fn outbox_with_batches(count: usize) -> PathBuf {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        for _ in 0..count {
            persist(&directory, &batch(vec![event("queued", "info")]).unwrap()).unwrap();
        }
        directory
    }

    /// The sharing setting is process-wide. Tests that need it on hold this;
    /// a test that turns it off holds it exclusively. So does a test that
    /// stalls the outbox, which holds every outbox write in the process.
    static SHARING: std::sync::RwLock<()> = std::sync::RwLock::new(());
    fn sharing_on() -> std::sync::RwLockReadGuard<'static, ()> {
        SHARING
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Quit as main does while the first upload is in flight. Returns how long
    /// the quit took and whether it stopped waiting for the upload thread.
    fn quit_waits(delay: Duration) -> (Duration, PathBuf, bool) {
        let _sharing = sharing_on();
        let directory = outbox_with_batches(8);
        let (endpoint, arrived) = slow_server(delay);
        let config = Config {
            endpoint,
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while arrived.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "no upload started");
            std::thread::sleep(Duration::from_millis(10));
        }
        sender
            .send(Queued::Line(
                "2026-10-21T17:11:31Z  INFO EternalMonitor shutting down".into(),
            ))
            .unwrap();
        GAVE_UP.with(|gave_up| gave_up.set(false));
        let started = std::time::Instant::now();
        drop(uploader);
        let waited = started.elapsed();
        (waited, directory, GAVE_UP.with(|gave_up| gave_up.get()))
    }

    /// The outbox's messages once `kept` holds for them, or after 10 s. The
    /// quit's fallback writes on its own thread and may finish after it.
    fn outbox_messages_once(directory: &Path, kept: impl Fn(&[String]) -> bool) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let messages = outbox_messages(directory);
            if kept(&messages) || std::time::Instant::now() >= deadline {
                return messages;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn shut_down(messages: &[String]) -> bool {
        messages
            .iter()
            .any(|m| m.ends_with("EternalMonitor shutting down"))
    }

    fn outbox_messages(directory: &Path) -> Vec<String> {
        files(directory)
            .unwrap()
            .iter()
            .flat_map(|path| {
                serde_json::from_slice::<Batch>(&fs::read(path).unwrap())
                    .unwrap()
                    .events
                    .into_iter()
                    .map(|e| e.message)
            })
            .collect()
    }

    #[test]
    fn quitting_stops_between_slow_uploads_and_keeps_the_rest() {
        let (waited, directory, _) = quit_waits(Duration::from_millis(1500));
        assert!(
            waited < Duration::from_millis(2500),
            "quit waited {waited:?}"
        );
        // A slow runner can still be writing when the bounded quit returns.
        let messages = outbox_messages_once(&directory, shut_down);
        assert!(shut_down(&messages), "{messages:?}");
        // At most the request in flight was sent; the rest wait for next launch.
        assert!(messages.iter().filter(|m| *m == "queued").count() >= 7);
        fs::remove_dir_all(directory).unwrap();
    }

    /// Outbox whose final write pauses, to hold a drained queue past the
    /// shutdown deadline.
    static PAUSED_OUTBOX: Mutex<Option<(PathBuf, Duration)>> = Mutex::new(None);
    pub(super) fn pause_before_final_write(directory: &Path) {
        let pause = PAUSED_OUTBOX.lock().unwrap().clone();
        if let Some((paused, pause)) = pause {
            if paused == directory {
                std::thread::sleep(pause);
            }
        }
    }

    /// Outbox whose writes block, holding the write lock, while a test
    /// keeps it here: a stalled disk.
    static STALLED_OUTBOX: Mutex<Option<PathBuf>> = Mutex::new(None);
    pub(super) fn stall_while_held(directory: &Path) {
        while STALLED_OUTBOX.lock().unwrap().as_deref() == Some(directory) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn quitting_while_an_outbox_write_stalls_is_bounded() {
        let _sharing = SHARING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let config = Config {
            endpoint: "http://127.0.0.1:9/v1/events".into(),
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        // The worker's first write stalls while it holds the outbox lock.
        *STALLED_OUTBOX.lock().unwrap() = Some(directory.clone());
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        sender
            .send(Queued::Line("2026-10-21T17:11:31Z  INFO first line".into()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        sender
            .send(Queued::Line("2026-10-21T17:11:31Z  INFO last line".into()))
            .unwrap();
        // The disk recovers long after the quit should have returned.
        let release = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(5));
            *STALLED_OUTBOX.lock().unwrap() = None;
        });
        let started = std::time::Instant::now();
        drop(uploader);
        let waited = started.elapsed();
        assert!(
            STALLED_OUTBOX.lock().unwrap().is_some(),
            "quit waited for the stalled write: {waited:?}"
        );
        assert!(
            waited < SHUTDOWN_WAIT + Duration::from_millis(600),
            "quit waited {waited:?}"
        );
        release.join().unwrap();
        let messages = outbox_messages_once(&directory, |messages| {
            ["first line", "last line"]
                .iter()
                .all(|line| messages.iter().any(|m| m.ends_with(line)))
        });
        assert_eq!(messages.len(), 2, "{messages:?}");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn quitting_near_the_deadline_keeps_every_drained_event() {
        let _sharing = sharing_on();
        let directory = outbox_with_batches(1);
        let config = Config {
            endpoint: slow_server(Duration::from_millis(1700)).0,
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        // The acknowledgement arrives just before the deadline, and the
        // worker is still writing the drained queue when the wait runs out.
        // The write stalls far longer than a quit may wait.
        *PAUSED_OUTBOX.lock().unwrap() = Some((directory.clone(), Duration::from_secs(5)));
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        std::thread::sleep(Duration::from_millis(300));
        for index in 0..10 {
            sender
                .send(Queued::Line(format!(
                    "2026-10-21T17:11:31Z  INFO last line {index}"
                )))
                .unwrap();
        }
        let started = std::time::Instant::now();
        drop(uploader);
        let waited = started.elapsed();
        assert!(
            waited < SHUTDOWN_WAIT + Duration::from_millis(600),
            "quit waited {waited:?}"
        );
        // Whoever drained the queue writes all of it.
        let kept = |messages: &[String]| {
            (0..10)
                .filter(|index| {
                    let line = format!("last line {index}");
                    messages.iter().any(|m| m.ends_with(&line))
                })
                .count()
        };
        let messages = outbox_messages_once(&directory, |messages| kept(messages) == 10);
        assert_eq!(kept(&messages), 10, "{messages:?}");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn quitting_with_sharing_off_during_a_stalled_upload_keeps_nothing() {
        let _sharing = SHARING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = outbox_with_batches(1);
        let (sender, receiver) = mpsc::sync_channel(256);
        sender
            .send(Queued::Line("2026-10-21T17:11:31Z  INFO last line".into()))
            .unwrap();
        // The request in flight never finishes, so the quit wait runs out.
        let (_request, done) = mpsc::channel();
        let uploader = Uploader {
            stop: std::sync::Arc::new(AtomicBool::new(false)),
            wake: sender,
            receiver: std::sync::Arc::new(Mutex::new(receiver)),
            directory: directory.clone(),
            done,
            thread: None,
        };
        ENABLED.store(false, Ordering::Relaxed);
        drop(uploader);
        ENABLED.store(true, Ordering::Relaxed);
        let messages = outbox_messages_once(&directory, |messages| messages.is_empty());
        assert!(messages.is_empty(), "{messages:?}");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_empty_log_line_is_an_event_and_not_the_shutdown_wake() {
        let _sharing = sharing_on();
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let config = Config {
            endpoint: "http://127.0.0.1:9/v1/events".into(),
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        for line in ["", "after the empty line"] {
            sender.send(Queued::Line(line.into())).unwrap();
        }
        drop(uploader);
        let mut messages = outbox_messages(&directory);
        messages.sort();
        assert_eq!(messages, ["", "after the empty line"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_idle_uploader_quits_without_waiting_for_its_receive_timeout() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let mut waits = Vec::new();
        for attempt in 0..10 {
            let config = Config {
                endpoint: "http://127.0.0.1:9/v1/events".into(),
                ingest_key: "write-only".into(),
                enabled: true,
                allow_http: true,
            };
            let (_sender, uploader) = Uploader::spawn(config, directory.clone());
            // Quit at different points of the thread's 100 ms receive.
            std::thread::sleep(Duration::from_millis(150 + attempt * 7));
            let started = std::time::Instant::now();
            drop(uploader);
            waits.push(started.elapsed());
        }
        waits.sort();
        assert!(waits[5] < Duration::from_millis(20), "{waits:?}");
        assert!(outbox_messages(&directory).is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quitting_during_a_stalled_upload_is_bounded_and_keeps_the_last_lines() {
        let (waited, directory, gave_up) = quit_waits(Duration::from_secs(30));
        // What bounds the quit: it stops waiting for the stalled upload
        // thread after SHUTDOWN_WAIT instead of joining it. Waiting for the
        // request to time out (5 s) or for the thread would fail this.
        assert!(gave_up, "quit waited for the stalled upload, {waited:?}");
        // The elapsed time is only a backstop for a hang: on a loaded PC
        // the outbox writes after the wait took 2.7 s. The quits this guards
        // against took 12.9 s and 30 s.
        assert!(waited < Duration::from_secs(10), "quit waited {waited:?}");
        let messages = outbox_messages_once(&directory, shut_down);
        assert!(shut_down(&messages), "{messages:?}");
        assert_eq!(messages.iter().filter(|m| *m == "queued").count(), 8);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn retains_batch_until_http_200_and_matching_acknowledgement() {
        use std::io::{Read, Write};
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("batch.json");
        let batch = Batch {
            schema: 1,
            batch_id: id().unwrap(),
            installation_id: id().unwrap(),
            launch_id: id().unwrap(),
            platform: "windows".into(),
            app_version: "0.3.0".into(),
            build: "test".into(),
            events: vec![],
        };
        fs::write(&path, serde_json::to_vec(&batch).unwrap()).unwrap();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .max_redirects(0)
            .build()
            .into();
        for (status, acknowledgement, accepted) in [
            (200, "wrong-batch", false),
            (202, batch.batch_id.as_str(), false),
            (200, batch.batch_id.as_str(), true),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let body = serde_json::json!({"accepted_batch": acknowledgement}).to_string();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0, "request ended before body");
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let size: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse().ok())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break;
                        }
                    }
                }
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let config = Config {
                endpoint: format!("http://{address}/v1/events"),
                ingest_key: "write-only".into(),
                enabled: true,
                allow_http: true,
            };
            assert_eq!(upload(&agent, &config, &path).is_ok(), accepted);
            server.join().unwrap();
            assert_eq!(path.exists(), !accepted);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reporter_events_join_the_crashed_launch_outside_its_sequence() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("installation-id"), "ab".repeat(16)).unwrap();
        let launch = "cd".repeat(16);
        queue_foreign(
            &directory,
            &launch,
            1,
            "EternalMonitor stopped unexpectedly: crash",
            "error",
        )
        .unwrap();
        let saved: Batch =
            serde_json::from_slice(&fs::read(&files(&directory).unwrap()[0]).unwrap()).unwrap();
        assert_eq!(saved.launch_id, launch);
        assert_eq!(saved.installation_id, "ab".repeat(16));
        assert_eq!(saved.events[0].sequence, FOREIGN_SEQUENCE_BASE + 1);
        assert_eq!(saved.events[0].level, "error");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fatal_reports_queue_as_errors_with_their_own_sequence_numbers() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        queue_into(
            &directory,
            "EternalMonitor could not start: token=abc",
            "error",
        )
        .unwrap();
        queue_into(&directory, "EternalMonitor could not start: again", "error").unwrap();
        let saved: Vec<Batch> = files(&directory)
            .unwrap()
            .iter()
            .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
            .collect();
        assert_eq!(saved.len(), 2);
        for batch in &saved {
            assert_eq!(batch.events.len(), 1);
            assert_eq!(batch.events[0].level, "error");
            assert_eq!(batch.events[0].category, "host");
        }
        assert!(saved
            .iter()
            .any(|b| b.events[0].message == "EternalMonitor could not start: token=[REDACTED]"));
        assert_ne!(saved[0].events[0].sequence, saved[1].events[0].sequence);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn drained_events_keep_their_level() {
        assert_eq!(level("2026-10-20T20:47:20Z ERROR Host panic"), "error");
        assert_eq!(
            level("2026-10-20T20:47:20Z  WARN Missing downlevel flags"),
            "warning"
        );
        assert_eq!(
            level("2026-10-20T20:47:20Z  INFO EternalMonitor shutting down"),
            "info"
        );
    }
    #[test]
    fn rejects_plaintext_and_credentials_in_urls() {
        let mut c = Config {
            endpoint: "https://logs.example/v1/events".into(),
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: false,
        };
        assert!(c.valid());
        for url in [
            "http://logs.example/v1/events",
            "https://secret@logs.example/v1/events",
            "https://logs.example/v1/events?key=secret",
            "relative",
        ] {
            c.endpoint = url.into();
            assert!(!c.valid());
        }
    }
    #[test]
    fn outbox_survives_reopen_and_is_bounded() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let mut batch = Batch {
            schema: 1,
            batch_id: id().unwrap(),
            installation_id: id().unwrap(),
            launch_id: id().unwrap(),
            platform: "windows".into(),
            app_version: "0.3.0".into(),
            build: "test".into(),
            events: vec![Event {
                sequence: 1,
                at_ms: 1,
                level: "error".into(),
                category: "decode".into(),
                message: "status=-9".into(),
            }],
        };
        for _ in 0..MAX_FILES + 1 {
            batch.batch_id = id().unwrap();
            persist(&directory, &batch).unwrap();
        }
        let paths = files(&directory).unwrap();
        assert_eq!(paths.len(), MAX_FILES);
        let saved: Batch = serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
        assert_eq!(saved.events[0].message, "status=-9");
        fs::remove_dir_all(directory).unwrap();
    }
}
