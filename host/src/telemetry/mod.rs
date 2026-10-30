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

mod periodic;
mod redact;
use periodic::Periodic;
#[cfg(windows)]
pub(crate) use redact::note_audio_device;
pub(crate) use redact::note_client_device;
#[cfg(windows)]
pub(crate) use redact::note_hardware;

const MAX_FILES: usize = 512;
/// The longest line or message that uploads. Redaction slows down faster
/// than the length grows on some punctuation-heavy lines, and a fatal
/// report runs it on the main thread.
const MAX_LINE_BYTES: usize = 8192;
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
/// What Settings tells the user, in plain words; [`STATUS`] is for support.
static SHARING_STATE: Mutex<Sharing> = Mutex::new(Sharing::Unavailable);
/// When the backend last acknowledged a batch.
static LAST_SENT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// The sharing preference could not be saved.
static SAVE_FAILED: AtomicBool = AtomicBool::new(false);
static IDENTITY: OnceLock<(String, String)> = OnceLock::new();
static OUTBOX: OnceLock<PathBuf> = OnceLock::new();
/// The uploader's summary window, for fatal reports.
static PERIODIC: OnceLock<std::sync::Arc<Mutex<Periodic>>> = OnceLock::new();
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

#[derive(Clone, Copy)]
enum Sharing {
    /// This build has no upload endpoint.
    Unavailable,
    /// The outbox could not be written.
    Stopped,
    Ready,
    /// The last upload failed; it is retried.
    Retrying,
}
fn set_sharing(sharing: Sharing) {
    *lock(&SHARING_STATE) = sharing;
}
/// What Settings shows about uploads, without field names or ids.
pub fn summary() -> String {
    let last_sent = lock(&LAST_SENT).map(|at| at.elapsed());
    plain_summary(
        *lock(&SHARING_STATE),
        enabled(),
        last_sent,
        DROPPED.load(Ordering::Relaxed),
        SAVE_FAILED.load(Ordering::Relaxed),
    )
}
fn plain_summary(
    sharing: Sharing,
    enabled: bool,
    last_sent: Option<Duration>,
    dropped: u64,
    save_failed: bool,
) -> String {
    let mut text = match sharing {
        Sharing::Unavailable => "Sharing diagnostics isn't available in this build.".to_string(),
        Sharing::Stopped => {
            "Sharing diagnostics stopped: they could not be saved on this PC.".to_string()
        }
        _ if !enabled => "Sharing diagnostics: off.".to_string(),
        Sharing::Retrying => "Sharing diagnostics: on. Waiting for a connection.".to_string(),
        Sharing::Ready => match last_sent {
            Some(ago) => format!("Sharing diagnostics: on. Last sent {}.", ago_text(ago)),
            None => "Sharing diagnostics: on. Nothing sent yet.".to_string(),
        },
    };
    if enabled && dropped > 0 && !matches!(sharing, Sharing::Unavailable) {
        text.push_str(" Some diagnostics were dropped while offline.");
    }
    if save_failed {
        text.push_str(" This setting could not be saved.");
    }
    text
}
fn ago_text(ago: Duration) -> String {
    let minutes = ago.as_secs() / 60;
    let (count, unit) = match minutes {
        0 => return "just now".to_string(),
        1..60 => (minutes, "minute"),
        60..1440 => (minutes / 60, "hour"),
        _ => (minutes / 1440, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
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
            SAVE_FAILED.store(true, Ordering::Relaxed);
            set_status("Could not save automatic upload preference");
            return;
        }
    }
    SAVE_FAILED.store(false, Ordering::Relaxed);
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

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn events_of(lines: Vec<String>) -> impl Iterator<Item = Event> {
    lines.into_iter().map(|line| event(&line, level(&line)))
}

/// The log lines among `queued`, as events, with the periodic stats in a
/// summary. The open summary window closes too: the uploader is stopping.
fn events(queued: impl Iterator<Item = Queued>, periodic: &Mutex<Periodic>) -> Vec<Event> {
    let mut periodic = lock(periodic);
    let now = std::time::Instant::now();
    let mut lines = Vec::new();
    for queued in queued {
        if let Queued::Line(line) = queued {
            lines.extend(periodic.push(&line, now));
        }
    }
    lines.extend(periodic.flush());
    events_of(lines).collect()
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
    /// The summary window the thread is filling, closed by whichever of the
    /// thread and the shutdown fallback writes the last events.
    periodic: std::sync::Arc<Mutex<Periodic>>,
    directory: PathBuf,
    done: mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Uploader {
    fn spawn(config: Config, directory: PathBuf) -> (mpsc::SyncSender<Queued>, Self) {
        let (sender, receiver) = mpsc::sync_channel(256);
        let receiver = std::sync::Arc::new(Mutex::new(receiver));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let periodic = std::sync::Arc::new(Mutex::new(Periodic::default()));
        let (done_tx, done) = mpsc::channel();
        let thread = {
            let (receiver, periodic, stop, directory) = (
                receiver.clone(),
                periodic.clone(),
                stop.clone(),
                directory.clone(),
            );
            std::thread::spawn(move || {
                if run(config, directory, receiver, periodic, stop).is_err() {
                    set_sharing(Sharing::Stopped);
                    set_status("Automatic upload stopped: local outbox unavailable");
                }
                let _ = done_tx.send(());
            })
        };
        let uploader = Self {
            stop,
            wake: sender.clone(),
            receiver,
            periodic,
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
        #[cfg(test)]
        tests::before_quit_wait(&self.directory);
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
        let (receiver, periodic, directory, sharing) = (
            self.receiver.clone(),
            self.periodic.clone(),
            self.directory.clone(),
            enabled(),
        );
        let (written_tx, written) = mpsc::channel();
        let _ = std::thread::Builder::new().spawn(move || {
            keep_queued(&receiver, &periodic, &directory, sharing);
            let _ = written_tx.send(());
        });
        let _ = written.recv_timeout(FINAL_WRITE_WAIT);
    }
}
/// The quit's fallback: writes the lines still queued and the open summary
/// window, or clears everything pending, the window too, when sharing is
/// off. If the upload thread is writing the queue itself, this waits for it
/// and finds nothing left.
fn keep_queued(
    receiver: &Mutex<mpsc::Receiver<Queued>>,
    periodic: &Mutex<Periodic>,
    directory: &Path,
    sharing: bool,
) {
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
        *lock(periodic) = Periodic::default();
        return;
    }
    let events = events(receiver.try_iter(), periodic);
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
                set_sharing(Sharing::Stopped);
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
        let _ = PERIODIC.set(uploader.periodic.clone());
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
            if sender
                .try_send(Queued::Line(queued(capped(line), &redact::NAMES.read())))
                .is_err()
            {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
/// `line` as the queue keeps it: redacted with the device names known now,
/// so a name the registry lets go later stays out of it. A session summary
/// stays as written. The upload redacts it again, which changes nothing.
fn queued(line: &str, names: &redact::Names) -> String {
    match session_summary(line) {
        Some(_) => line.to_owned(),
        None => redact::redact(line, names),
    }
}
/// `line`'s first [`MAX_LINE_BYTES`], whole characters only.
fn capped(line: &str) -> &str {
    let mut end = line.len().min(MAX_LINE_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}
/// Queue one event (`level` is `error` or `info`) on disk now, after the
/// open periodic summary. The upload thread sends them, or the next launch
/// does if this process ends first. Ok when the event is saved or uploads
/// are off.
pub fn queue_now(message: &str, level: &str) -> std::io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    let Some(directory) = OUTBOX.get() else {
        return Ok(());
    };
    let queued = queue_into(directory, PERIODIC.get().map(|x| &**x), message, level);
    if queued.is_err() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
    queued
}
/// The open summary window goes first, in the same batch: a crash after
/// this skips the uploader's shutdown, which would otherwise write it.
fn queue_into(
    directory: &Path,
    periodic: Option<&Mutex<Periodic>>,
    message: &str,
    level: &str,
) -> std::io::Result<()> {
    let mut events: Vec<Event> = periodic.map_or_else(Vec::new, |periodic| {
        events_of(lock(periodic).flush()).collect()
    });
    events.push(event(capped(message), level));
    persist(directory, &batch(events)?)
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
    let mut event = event(capped(message), level);
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
/// The message of a session summary line (`crate::diagnostics::session_summary`):
/// the backend copies category `summary` events whose message is exactly that.
pub(crate) fn session_summary(line: &str) -> Option<&str> {
    let (_stamp, rest) = line.split_once(' ')?;
    rest.trim_start()
        .strip_prefix("INFO ")
        .filter(|message| message.starts_with("session_summary v="))
}
/// What uploads say for `line`.
#[cfg(test)]
pub(crate) fn uploaded(line: &str) -> String {
    event(line, level(line)).message
}
fn event(line: &str, level: &str) -> Event {
    event_with(line, level, &redact::NAMES.read())
}
/// A session summary uploads as written. Other lines leave out the user's
/// and the PC's names (see [`redact`]).
fn event_with(line: &str, level: &str, names: &redact::Names) -> Event {
    let (category, message) = match session_summary(line) {
        Some(summary) => ("summary", crate::logging::redact_support_text(summary)),
        None => (
            "host",
            redact::redact(&crate::logging::redact_support_text(line), names),
        ),
    };
    Event {
        sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
        at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        level: level.into(),
        category: category.into(),
        message,
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
    *lock(&LAST_SENT) = Some(std::time::Instant::now());
    Ok(())
}
fn run(
    config: Config,
    directory: PathBuf,
    receiver: std::sync::Arc<Mutex<mpsc::Receiver<Queued>>>,
    periodic: std::sync::Arc<Mutex<Periodic>>,
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
    set_sharing(Sharing::Ready);
    set_status("Automatic upload enabled; waiting for events");
    loop {
        if stop.load(Ordering::SeqCst) {
            return finish(&directory, &receiver, &periodic, pending);
        }
        let received = match receiver.lock() {
            Ok(receiver) => receiver.recv_timeout(receive_timeout(&directory)),
            Err(_) => break,
        };
        match received {
            Ok(Queued::Wake) => {}
            Ok(Queued::Line(line)) => {
                let lines = lock(&periodic).push(&line, std::time::Instant::now());
                pending.extend(events_of(lines));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if stop.load(Ordering::SeqCst) {
            return finish(&directory, &receiver, &periodic, pending);
        }
        if !enabled() {
            pending.clear();
            *lock(&periodic) = Periodic::default();
            for path in files(&directory)? {
                fs::remove_file(path)?;
            }
            if let Ok(receiver) = receiver.lock() {
                while receiver.try_recv().is_ok() {}
            }
            continue;
        }
        let due = lock(&periodic).flush_due(std::time::Instant::now());
        pending.extend(events_of(due));
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
                set_sharing(Sharing::Retrying);
                set_status(format!(
                    "Automatic upload waiting to retry; pending_batches={}",
                    files(&directory)?.len()
                ));
                backoff = (backoff * 2).min(300);
            } else {
                set_sharing(Sharing::Ready);
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
/// How long the upload thread waits for a line before it checks its upload
/// schedule. A test of the quit's wake makes it wait far longer.
fn receive_timeout(_directory: &Path) -> Duration {
    #[cfg(test)]
    if let Some(timeout) = tests::receive_timeout(_directory) {
        return timeout;
    }
    Duration::from_millis(100)
}
/// Writes the events this thread holds and those still queued, all under the
/// receiver lock. The quit's fallback takes the same lock, so each queued
/// line is written once.
fn finish(
    directory: &Path,
    receiver: &Mutex<mpsc::Receiver<Queued>>,
    periodic: &Mutex<Periodic>,
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
    pending.extend(events(receiver.try_iter(), periodic));
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

    /// Holds each upload until `answer` is set, for at most a minute, then
    /// acknowledges its batch. The counter says how many requests have
    /// arrived.
    fn held_server(
        answer: std::sync::Arc<AtomicBool>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let arrived = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = arrived.clone();
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (counter, answer) = (counter.clone(), answer.clone());
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
                    let held = std::time::Instant::now();
                    while !answer.load(Ordering::SeqCst) && held.elapsed() < Duration::from_secs(60)
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
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
    /// stalls the outbox, which holds every outbox write in the process, and
    /// one that floods it: on a slow disk its writes kept the write lock
    /// from other tests' threads for over a minute.
    static SHARING: std::sync::RwLock<()> = std::sync::RwLock::new(());
    fn sharing_on() -> std::sync::RwLockReadGuard<'static, ()> {
        SHARING
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// How long a test waits for a step that takes milliseconds. Only a
    /// hung test waits this long: a single small write on mainpc's shared
    /// disk has taken 27 s.
    const PATIENCE: Duration = Duration::from_secs(120);

    /// The upload thread is in its first request: the server has seen it.
    fn wait_for_upload(arrived: &std::sync::atomic::AtomicUsize) {
        let deadline = std::time::Instant::now() + PATIENCE;
        while arrived.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "no upload started");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Sets `answer` once the quit has told the upload thread to stop, so
    /// the request in flight ends after the quit began on any machine.
    fn answer_when_stopped(stop: std::sync::Arc<AtomicBool>, answer: std::sync::Arc<AtomicBool>) {
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            answer.store(true, Ordering::SeqCst);
        });
    }

    /// Whether the upload thread has ended, waiting at most [`PATIENCE`]. `queue` is
    /// a copy of its queue taken before the quit; the thread and the quit's
    /// fallback hold the only others.
    fn thread_ended(queue: &std::sync::Arc<Mutex<mpsc::Receiver<Queued>>>) -> bool {
        let deadline = std::time::Instant::now() + PATIENCE;
        while std::sync::Arc::strong_count(queue) > 1 {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    struct Quit {
        waited: Duration,
        directory: PathBuf,
        gave_up: bool,
        /// Requests the server has seen.
        arrived: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        queue: std::sync::Arc<Mutex<mpsc::Receiver<Queued>>>,
    }

    /// Quit as main does while the first upload is in flight. With `answered`
    /// the server acknowledges it once the quit has begun; otherwise it
    /// stalls past the request's own timeout.
    fn quit_during_upload(answered: bool) -> Quit {
        let _sharing = sharing_on();
        let directory = outbox_with_batches(8);
        let answer = std::sync::Arc::new(AtomicBool::new(false));
        let (endpoint, arrived) = held_server(answer.clone());
        let config = Config {
            endpoint,
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        wait_for_upload(&arrived);
        sender
            .send(Queued::Line(
                "2026-10-21T17:11:31Z  INFO EternalMonitor shutting down".into(),
            ))
            .unwrap();
        if answered {
            answer_when_stopped(uploader.stop.clone(), answer);
        }
        let queue = uploader.receiver.clone();
        GAVE_UP.with(|gave_up| gave_up.set(false));
        let started = std::time::Instant::now();
        drop(uploader);
        Quit {
            waited: started.elapsed(),
            directory,
            gave_up: GAVE_UP.with(|gave_up| gave_up.get()),
            arrived,
            queue,
        }
    }

    /// The outbox's messages once `kept` holds for them, or after [`PATIENCE`]. The
    /// quit's fallback writes on its own thread and may finish after it.
    fn outbox_messages_once(directory: &Path, kept: impl Fn(&[String]) -> bool) -> Vec<String> {
        let deadline = std::time::Instant::now() + PATIENCE;
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
        let quit = quit_during_upload(true);
        // Once the thread is done, the request in flight is the only one it
        // sent: it started no other upload after the quit. The time the quit
        // took no longer stands in for this; it measured the runner's speed.
        assert!(thread_ended(&quit.queue), "the upload thread kept going");
        assert_eq!(quit.arrived.load(Ordering::SeqCst), 1);
        let messages = outbox_messages_once(&quit.directory, shut_down);
        assert!(shut_down(&messages), "{messages:?}");
        // The rest wait for next launch.
        assert!(messages.iter().filter(|m| *m == "queued").count() >= 7);
        fs::remove_dir_all(quit.directory).unwrap();
    }

    /// A point in the upload code where a test holds the threads of one
    /// outbox until it lets them go, and learns that one got there.
    struct Hold {
        directory: PathBuf,
        reached: AtomicBool,
        released: AtomicBool,
    }
    impl Hold {
        fn new(directory: &Path) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                directory: directory.to_path_buf(),
                reached: AtomicBool::new(false),
                released: AtomicBool::new(false),
            })
        }
        /// Lets the threads go after `backstop` if the test has not, so a
        /// quit that waits for them fails instead of hanging.
        fn release_after(self: &std::sync::Arc<Self>, backstop: Duration) {
            let hold = self.clone();
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + backstop;
                while !hold.released() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                hold.release();
            });
        }
        fn release(&self) {
            self.released.store(true, Ordering::SeqCst);
        }
        fn released(&self) -> bool {
            self.released.load(Ordering::SeqCst)
        }
        /// Whether a thread got here, waiting at most [`PATIENCE`].
        fn wait_until_reached(&self) -> bool {
            let deadline = std::time::Instant::now() + PATIENCE;
            while !self.reached.load(Ordering::SeqCst) {
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            true
        }
        /// The hold in `slot`, if it is for `directory`.
        fn for_outbox(
            slot: &Mutex<Option<std::sync::Arc<Self>>>,
            directory: &Path,
        ) -> Option<std::sync::Arc<Self>> {
            let hold = slot.lock().unwrap().clone();
            hold.filter(|hold| hold.directory == directory)
        }
        /// Holds the calling thread here while a test keeps `slot` on.
        fn wait_at(slot: &Mutex<Option<std::sync::Arc<Self>>>, directory: &Path) {
            if let Some(hold) = Self::for_outbox(slot, directory) {
                hold.reached.store(true, Ordering::SeqCst);
                while !hold.released() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Outbox whose final write pauses, to hold a drained queue past the
    /// shutdown deadline.
    static PAUSED_FINAL_WRITE: Mutex<Option<std::sync::Arc<Hold>>> = Mutex::new(None);
    pub(super) fn pause_before_final_write(directory: &Path) {
        Hold::wait_at(&PAUSED_FINAL_WRITE, directory);
    }

    /// Outbox whose writes block, holding the write lock, while a test
    /// keeps it here: a stalled disk.
    static STALLED_WRITE: Mutex<Option<std::sync::Arc<Hold>>> = Mutex::new(None);
    pub(super) fn stall_while_held(directory: &Path) {
        Hold::wait_at(&STALLED_WRITE, directory);
    }

    /// Outbox whose quit starts its deadline only once a thread reached this
    /// hold.
    static QUIT_AFTER: Mutex<Option<std::sync::Arc<Hold>>> = Mutex::new(None);
    pub(super) fn before_quit_wait(directory: &Path) {
        if let Some(hold) = Hold::for_outbox(&QUIT_AFTER, directory) {
            hold.wait_until_reached();
        }
    }

    /// Outbox whose upload thread waits an hour for each line. Reaching the
    /// hold means the thread is waiting.
    static IDLE: Mutex<Option<std::sync::Arc<Hold>>> = Mutex::new(None);
    pub(super) fn receive_timeout(directory: &Path) -> Option<Duration> {
        Hold::for_outbox(&IDLE, directory).map(|hold| {
            hold.reached.store(true, Ordering::SeqCst);
            Duration::from_secs(3600)
        })
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
        let stall = Hold::new(&directory);
        stall.release_after(PATIENCE);
        *STALLED_WRITE.lock().unwrap() = Some(stall.clone());
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        sender
            .send(Queued::Line("2026-10-21T17:11:31Z  INFO first line".into()))
            .unwrap();
        assert!(stall.wait_until_reached(), "the worker never wrote");
        sender
            .send(Queued::Line("2026-10-21T17:11:31Z  INFO last line".into()))
            .unwrap();
        GAVE_UP.with(|gave_up| gave_up.set(false));
        let started = std::time::Instant::now();
        drop(uploader);
        let waited = started.elapsed();
        // The disk recovers only after the quit returned, so its return
        // proves it did not wait for the write. Measuring it against
        // SHUTDOWN_WAIT plus a margin measured the runner's speed instead.
        assert!(
            !stall.released(),
            "quit waited for the stalled write: {waited:?}"
        );
        assert!(
            GAVE_UP.with(|gave_up| gave_up.get()),
            "quit did not stop waiting for the worker"
        );
        // So the quit waited only on its two bounded waits: SHUTDOWN_WAIT
        // for the worker, then FINAL_WRITE_WAIT for the fallback's write.
        // Pin them, so lengthening either fails here.
        assert_eq!(
            (SHUTDOWN_WAIT, FINAL_WRITE_WAIT),
            (Duration::from_secs(2), Duration::from_millis(250))
        );
        stall.release();
        *STALLED_WRITE.lock().unwrap() = None;
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
        let answer = std::sync::Arc::new(AtomicBool::new(false));
        let (endpoint, arrived) = held_server(answer.clone());
        let config = Config {
            endpoint,
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        // The acknowledgement arrives after the quit began, and the worker
        // is still writing the drained queue when the wait runs out: the
        // quit's deadline starts once the worker holds the drained queue,
        // and the write stalls until the quit has returned.
        let final_write = Hold::new(&directory);
        final_write.release_after(PATIENCE);
        *PAUSED_FINAL_WRITE.lock().unwrap() = Some(final_write.clone());
        *QUIT_AFTER.lock().unwrap() = Some(final_write.clone());
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        wait_for_upload(&arrived);
        for index in 0..10 {
            sender
                .send(Queued::Line(format!(
                    "2026-10-21T17:11:31Z  INFO last line {index}"
                )))
                .unwrap();
        }
        answer_when_stopped(uploader.stop.clone(), answer);
        GAVE_UP.with(|gave_up| gave_up.set(false));
        let started = std::time::Instant::now();
        drop(uploader);
        let waited = started.elapsed();
        assert!(
            !final_write.released(),
            "quit waited for the final write: {waited:?}"
        );
        assert!(
            GAVE_UP.with(|gave_up| gave_up.get()),
            "the worker finished before the deadline"
        );
        final_write.release();
        *PAUSED_FINAL_WRITE.lock().unwrap() = None;
        *QUIT_AFTER.lock().unwrap() = None;
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

    /// Quits an uploader whose request in flight never finishes, so the quit
    /// wait runs out, with periodic lines queued and a summary window open.
    /// Returns what the outbox holds afterwards.
    fn quit_stalled_with_an_open_window(sharing: bool) -> Vec<String> {
        let directory = outbox_with_batches(1);
        let lines = streaming_lines(2);
        let mut window = Periodic::default();
        for line in &lines[..3] {
            assert!(window.push(line, std::time::Instant::now()).is_empty());
        }
        assert!(window.is_open());
        let (sender, receiver) = mpsc::sync_channel(256);
        for line in &lines[3..] {
            sender.send(Queued::Line(line.clone())).unwrap();
        }
        sender
            .send(Queued::Line("2026-10-21T17:11:31Z  INFO last line".into()))
            .unwrap();
        let (_request, done) = mpsc::channel();
        let uploader = Uploader {
            stop: std::sync::Arc::new(AtomicBool::new(false)),
            wake: sender,
            receiver: std::sync::Arc::new(Mutex::new(receiver)),
            periodic: std::sync::Arc::new(Mutex::new(window)),
            directory: directory.clone(),
            done,
            thread: None,
        };
        ENABLED.store(sharing, Ordering::Relaxed);
        drop(uploader);
        ENABLED.store(true, Ordering::Relaxed);
        let messages = outbox_messages_once(&directory, |messages| {
            messages.is_empty() != sharing
                && (!sharing
                    || messages
                        .iter()
                        .any(|m| m.contains("Periodic stats summary")))
        });
        fs::remove_dir_all(directory).unwrap();
        messages
    }

    #[test]
    fn quitting_with_sharing_off_during_a_stalled_upload_keeps_nothing() {
        let _sharing = SHARING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Neither the queued lines, the outbox, nor the open summary window.
        let messages = quit_stalled_with_an_open_window(false);
        assert!(messages.is_empty(), "{messages:?}");
    }

    /// The same quit with sharing on writes the open window as a summary, so
    /// the test above would see one if sharing off let it through.
    #[test]
    fn quitting_during_a_stalled_upload_keeps_the_open_window() {
        let _sharing = SHARING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let messages = quit_stalled_with_an_open_window(true);
        let summaries: Vec<_> = messages
            .iter()
            .filter(|m| m.contains("INFO Periodic stats summary"))
            .collect();
        assert_eq!(summaries.len(), 1, "{messages:?}");
        assert!(summaries[0].contains(" rr_lines=4 "), "{messages:?}");
        assert!(!messages.iter().any(|m| m.contains("Receiver report")));
        assert!(messages.iter().any(|m| m.ends_with("last line")));
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
        // A slow runner's quit can leave both lines to its fallback thread.
        let mut messages = outbox_messages_once(&directory, |messages| messages.len() >= 2);
        messages.sort();
        assert_eq!(messages, ["", "after the empty line"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_idle_uploader_quits_without_waiting_for_its_receive_timeout() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        // The thread waits an hour for lines, so only the quit's wake can
        // end the wait. Timing the quit against the real 100 ms receive
        // measured the runner's speed instead.
        let idle = Hold::new(&directory);
        *IDLE.lock().unwrap() = Some(idle.clone());
        let config = Config {
            endpoint: "http://127.0.0.1:9/v1/events".into(),
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        let (_sender, uploader) = Uploader::spawn(config, directory.clone());
        assert!(
            idle.wait_until_reached(),
            "the thread never waited for lines"
        );
        let queue = uploader.receiver.clone();
        drop(uploader);
        assert!(
            thread_ended(&queue),
            "the quit did not wake the idle thread"
        );
        *IDLE.lock().unwrap() = None;
        assert!(outbox_messages(&directory).is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quitting_during_a_stalled_upload_is_bounded_and_keeps_the_last_lines() {
        let Quit {
            waited,
            directory,
            gave_up,
            ..
        } = quit_during_upload(false);
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
            .timeout_global(Some(Duration::from_secs(30)))
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
                    .set_read_timeout(Some(Duration::from_secs(30)))
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
            None,
            "EternalMonitor could not start: token=abc",
            "error",
        )
        .unwrap();
        queue_into(
            &directory,
            None,
            "EternalMonitor could not start: again",
            "error",
        )
        .unwrap();
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
    /// A fatal report is redacted on the main thread, before the virtual
    /// display is turned off. A crafted message costs no more than a capped
    /// log line.
    #[test]
    fn a_long_fatal_report_is_capped_before_redaction() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("installation-id"), "ab".repeat(16)).unwrap();
        let message = format!("EternalMonitor could not start: {}", r"=\a".repeat(200_000));
        // Uncapped, this message takes minutes in a release build. Capped,
        // it takes tens of milliseconds, and about a second unoptimized.
        let limit = Duration::from_millis(if cfg!(debug_assertions) { 20_000 } else { 100 });
        let started = std::time::Instant::now();
        queue_into(&directory, None, &message, "error").unwrap();
        assert!(started.elapsed() < limit, "{:?}", started.elapsed());
        let started = std::time::Instant::now();
        queue_foreign(&directory, "launch", 1, &message, "error").unwrap();
        assert!(started.elapsed() < limit, "{:?}", started.elapsed());
        for path in files(&directory).unwrap() {
            let saved: Batch = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            let uploaded = &saved.events[0].message;
            assert!(uploaded.starts_with("EternalMonitor could not start: "));
            assert!(uploaded.len() <= MAX_LINE_BYTES, "{}", uploaded.len());
        }
        fs::remove_dir_all(directory).unwrap();
    }
    /// Settings says what happens in words: no field names, ids or counts.
    #[test]
    fn settings_shows_sharing_in_plain_words() {
        let minutes = |m: u64| Some(Duration::from_secs(m * 60));
        let cases = [
            (
                plain_summary(Sharing::Ready, true, minutes(2), 0, false),
                "Sharing diagnostics: on. Last sent 2 minutes ago.",
            ),
            (
                plain_summary(
                    Sharing::Ready,
                    true,
                    Some(Duration::from_secs(20)),
                    0,
                    false,
                ),
                "Sharing diagnostics: on. Last sent just now.",
            ),
            (
                plain_summary(Sharing::Ready, true, minutes(61), 0, false),
                "Sharing diagnostics: on. Last sent 1 hour ago.",
            ),
            (
                plain_summary(Sharing::Ready, true, minutes(3 * 1440), 0, false),
                "Sharing diagnostics: on. Last sent 3 days ago.",
            ),
            (
                plain_summary(Sharing::Ready, true, None, 0, false),
                "Sharing diagnostics: on. Nothing sent yet.",
            ),
            (
                plain_summary(Sharing::Retrying, true, minutes(1), 3, false),
                "Sharing diagnostics: on. Waiting for a connection. \
                 Some diagnostics were dropped while offline.",
            ),
            (
                plain_summary(Sharing::Ready, false, minutes(1), 3, false),
                "Sharing diagnostics: off.",
            ),
            (
                plain_summary(Sharing::Ready, true, minutes(1), 0, true),
                "Sharing diagnostics: on. Last sent 1 minute ago. This setting could not be saved.",
            ),
            (
                plain_summary(Sharing::Unavailable, true, None, 0, false),
                "Sharing diagnostics isn't available in this build.",
            ),
            (
                plain_summary(Sharing::Stopped, true, None, 0, false),
                "Sharing diagnostics stopped: they could not be saved on this PC.",
            ),
        ];
        for (shown, expected) in cases {
            assert_eq!(shown, expected);
            assert!(!shown.contains(['=', '_']), "{shown}");
        }
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
    fn a_session_summary_uploads_alone_in_its_own_category() {
        let summary = "session_summary v=1 role=pc session_id=7 nonce=00000000e2e00001 \
                       transport=wifi mode=mirror stage=accepted outcome=user_ended failure=none \
                       fallback=none end=goodbye ttff_ms=- duration_ms=900 os=win:0.0 hw=pc gpu=- \
                       hybrid=-";
        let uploaded = event(
            &format!("2026-10-28T02:03:04.123456Z  INFO {summary}"),
            "info",
        );
        assert_eq!(
            (uploaded.category.as_str(), uploaded.message.as_str()),
            ("summary", summary)
        );
        // Only the line's own message, at info.
        for line in [
            format!("2026-10-28T02:03:04Z  WARN {summary}"),
            format!("2026-10-28T02:03:04Z  INFO Client session established device=\"x {summary}\""),
            summary.to_string(),
        ] {
            let uploaded = event(&line, level(&line));
            // The iPad's name, the rest of its line, is left out (see `redact`).
            let message = match line.split_once(" device=") {
                Some((head, _)) => format!("{head} device=<device-name>"),
                None => line.clone(),
            };
            assert_eq!(
                (uploaded.category.as_str(), uploaded.message),
                ("host", message)
            );
        }
    }
    #[test]
    fn uploads_name_a_device_by_its_kind() {
        let line = "2026-10-30T09:12:00.123456Z  INFO WASAPI loopback active \
                    device=Headphones (Maria's AirPods) rate=48000 channels=2";
        let uploaded = event(line, level(line));
        assert_eq!(
            uploaded.message,
            "2026-10-30T09:12:00.123456Z  INFO WASAPI loopback active \
             device=<headphones> rate=48000 channels=2"
        );
    }
    /// Lines a streaming host logs over `seconds`, at their real rates.
    fn streaming_lines(seconds: u32) -> Vec<String> {
        let mut lines = Vec::new();
        for half in 0..seconds * 2 {
            let stamp = format!("2026-10-29T17:46:{:02}.{}00000Z", half / 2, half % 2 * 5);
            if half % 20 == 0 {
                lines.push(format!(
                    "{stamp}  INFO Support stream summary session_id=Some(7) running=true \
                     captured={half} capture_fps=60.0"
                ));
            }
            lines.push(format!(
                "{stamp}  INFO Receiver report epoch=1 complete={half} dropped=0 repaired=0 \
                 nacks=0 jitter_us=190 decode_fps_x10=558 decode_depth=0 retransmits=0 \
                 frags_received={half} frags_lost=0 rtt_ms_x10=31 latency_ms_x10=0 audio_lost=0"
            ));
            if half % 2 == 0 {
                lines.push(format!(
                    "{stamp}  INFO Audio stream stats device=Speakers kbps=96.0 \
                     packets_per_sec=50.0 quiet_packets=0 dropped=0"
                ));
            }
        }
        lines
    }

    #[test]
    fn periodic_stats_reach_the_outbox_as_one_summary() {
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
        let lines = streaming_lines(30);
        assert_eq!(lines.len(), 60 + 30 + 3);
        for line in lines {
            sender.send(Queued::Line(line)).unwrap();
        }
        let other = "2026-10-29T17:46:30.000000Z  WARN Client liveness expired";
        sender.send(Queued::Line(other.into())).unwrap();
        drop(uploader);
        let messages = outbox_messages_once(&directory, |messages| messages.len() >= 2);
        assert!(
            !messages.iter().any(|m| m.contains("Receiver report")
                || m.contains("Audio stream stats")
                || m.contains("Support stream summary")),
            "{messages:?}"
        );
        let summaries: Vec<_> = messages
            .iter()
            .filter(|m| m.contains("INFO Periodic stats summary"))
            .collect();
        assert_eq!(summaries.len(), 1, "{messages:?}");
        assert!(summaries[0].contains(" session_id=7 keyframe_requests=0 rr_lines=60 "));
        assert!(summaries[0].contains(" rr_rtt_ms_x10=31/31/31 "));
        assert!(summaries[0].contains(" audio_lines=30 "));
        assert!(summaries[0].contains(" stream_lines=3 "));
        assert!(!summaries[0].contains("Speakers"), "no device names");
        assert!(messages.iter().any(|m| m == other));
        assert_eq!(messages.len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    /// A crash ends the process without the uploader's shutdown: the fatal
    /// report carries the open summary window, in order before it.
    #[test]
    fn a_fatal_report_carries_the_open_summary_window() {
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
        for line in streaming_lines(5) {
            sender.send(Queued::Line(line)).unwrap();
        }
        // Wait for the thread to take every line: 10 reports, 5 audio lines
        // and a stream summary.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while lock(&uploader.periodic).lines() < 16 {
            assert!(std::time::Instant::now() < deadline, "lines not taken");
            std::thread::sleep(Duration::from_millis(10));
        }
        let fatal = r"EternalMonitor stopped unexpectedly: Host panic at C:\Users\bob\src\x.rs";
        queue_into(&directory, Some(&uploader.periodic), fatal, "error").unwrap();
        let batches: Vec<Batch> = files(&directory)
            .unwrap()
            .iter()
            .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
            .collect();
        let with_fatal = batches
            .iter()
            .find(|b| b.events.iter().any(|e| e.level == "error"))
            .expect("fatal event saved");
        let messages: Vec<_> = with_fatal
            .events
            .iter()
            .map(|e| e.message.as_str())
            .collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].contains("INFO Periodic stats summary"),
            "{messages:?}"
        );
        assert!(messages[0].contains(" rr_lines=10 "), "{messages:?}");
        assert_eq!(
            messages[1],
            r"EternalMonitor stopped unexpectedly: Host panic at C:\Users\%USERNAME%\src\x.rs"
        );
        drop(uploader);
        let summaries = outbox_messages(&directory)
            .iter()
            .filter(|m| m.contains("Periodic stats summary"))
            .count();
        assert_eq!(summaries, 1, "the window is not uploaded twice");
        fs::remove_dir_all(directory).unwrap();
    }

    /// The failure reporter relays another launch's event: it leaves names
    /// out too.
    #[test]
    fn relayed_reporter_events_leave_out_profile_paths() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("installation-id"), "ab".repeat(16)).unwrap();
        queue_foreign(
            &directory,
            "launch",
            1,
            r"EternalMonitor stopped unexpectedly: exception in \\?\C:\Users\Maria\AppData\Local\x.dll",
            "error",
        )
        .unwrap();
        let saved: Batch =
            serde_json::from_slice(&fs::read(&files(&directory).unwrap()[0]).unwrap()).unwrap();
        assert_eq!(
            saved.events[0].message,
            r"EternalMonitor stopped unexpectedly: exception in \\?\C:\Users\%USERNAME%\AppData\Local\x.dll"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    /// The startup network line uploads byte for byte, even when the PC or
    /// user is named like one of its tokens.
    #[test]
    fn the_network_line_is_never_redacted() {
        let names = redact::Names::new(Some(r"C:\Users\vpn"), Some("vpn"), Some("wifi"));
        let adapters = [
            crate::network::Adapter {
                kind: crate::network::Kind::Wifi,
                up: true,
                gateway: true,
                ipv4: vec![std::net::Ipv4Addr::new(192, 168, 1, 20)],
            },
            crate::network::Adapter {
                kind: crate::network::Kind::Vpn,
                up: true,
                gateway: false,
                ipv4: vec![std::net::Ipv4Addr::new(100, 80, 1, 2)],
            },
        ];
        let line = crate::network::summary(
            Some(&adapters),
            Some(std::net::Ipv4Addr::new(100, 80, 1, 2).into()),
        );
        let stamped = format!("2026-10-29T17:46:01.000000Z  INFO {line}");
        assert_eq!(event_with(&stamped, "info", &names).message, stamped);
    }

    /// Names that are also the summary's own tokens leave it byte for byte,
    /// while the same words elsewhere are names.
    /// A line is redacted as it is queued: a device name the registry lets
    /// go later stays out of its upload.
    #[test]
    fn a_queued_line_keeps_out_a_device_name_seen_before_many_others() {
        let mut names = redact::Names::new(None, None, None);
        let mut queue = Vec::new();
        for i in 0..41 {
            let name = format!("Zoe's iPad {i}");
            names.note_device(&name, "<device-name>");
            queue.push(queued(
                &format!("2026-10-30T09:12:00.000000Z  INFO Pairing approved for {name}"),
                &names,
            ));
        }
        for i in 0..64 {
            names.note_device(&format!("Kim's iPad {i}"), "<device-name>");
        }
        for line in queue {
            assert_eq!(
                event_with(&line, "info", &names).message,
                "2026-10-30T09:12:00.000000Z  INFO Pairing approved for <device-name>"
            );
        }
    }
    #[test]
    fn a_session_summary_is_never_redacted() {
        let names = redact::Names::new(Some(r"C:\Users\steady"), Some("mirror"), Some("wifi"));
        let summary = "session_summary v=1 role=pc session_id=7 nonce=00000000e2e00001 \
                       transport=wifi mode=mirror stage=steady outcome=streamed failure=none \
                       fallback=none end=goodbye ttff_ms=210 duration_ms=90000 \
                       os=win:26200.9457 hw=pc gpu=Intel+NVIDIA hybrid=true";
        let uploaded = event_with(
            &format!("2026-10-29T17:46:01.000000Z  INFO {summary}"),
            "info",
            &names,
        );
        assert_eq!(
            (uploaded.category.as_str(), uploaded.message.as_str()),
            ("summary", summary)
        );
        let other = event_with(
            "2026-10-29T17:46:01.000000Z  INFO Display mode mirror on wifi",
            "info",
            &names,
        );
        assert_eq!(
            other.message,
            "2026-10-29T17:46:01.000000Z  INFO Display mode %USERNAME% on %COMPUTERNAME%"
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
        let _sharing = SHARING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
