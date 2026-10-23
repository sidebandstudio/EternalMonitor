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
use periodic::Periodic;

const MAX_FILES: usize = 512;
const BATCH_SIZE: usize = 4;
/// Longest a normal quit waits for uploads. Unsent batches stay in the outbox.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// Events another process adds to a launch number from here, clear of the
/// launch's own sequence, which the backend keeps unique per launch.
pub const FOREIGN_SEQUENCE_BASE: u64 = 1 << 40;
static SENDER: OnceLock<mpsc::SyncSender<Queued>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(true);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static STATUS: Mutex<String> = Mutex::new(String::new());
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

/// The upload thread. Dropping it waits up to [`SHUTDOWN_WAIT`] for the thread
/// to finish, which bounds the wait for a request in flight. If the thread has
/// not finished by then, the queued lines are written to the outbox from here,
/// after any final write the thread has begun; those disk writes are not
/// bounded.
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
        if self.done.recv_timeout(SHUTDOWN_WAIT).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            return;
        }
        // The thread is inside a request and holds no lock; keep what it has
        // not written yet and let the process exit without it.
        #[cfg(test)]
        tests::GAVE_UP.with(|gave_up| gave_up.set(true));
        let Ok(receiver) = self.receiver.lock() else {
            return;
        };
        let events = events(receiver.try_iter(), &self.periodic);
        for chunk in events.chunks(BATCH_SIZE) {
            if let Ok(batch) = batch(chunk.to_vec()) {
                let _ = persist(&self.directory, &batch);
            }
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
    events.push(event(message, level));
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
        message: redact_upload(&crate::logging::redact_support_text(line), &NAMES),
    }
}

/// What uploads leave out: the user's profile folder, which is their user
/// name, and the PC's name, which is often its owner's.
struct Names {
    profile: Option<regex::Regex>,
    /// Lowercase, for a whole-token match in any case.
    pc: Option<String>,
}
impl Names {
    fn new(profile: Option<&str>, pc: Option<&str>) -> Self {
        Self {
            profile: profile.and_then(profile_pattern),
            pc: pc
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_lowercase),
        }
    }
}
static NAMES: once_cell::sync::Lazy<Names> = once_cell::sync::Lazy::new(|| {
    let profile = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"));
    let pc = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME"));
    Names::new(profile.ok().as_deref(), pc.ok().as_deref())
});

/// This profile's path, in any case and with either separator, when the
/// folder name ends there: at a separator, a quote or the end of the line.
/// A space or letter after it is a longer folder name, such as `A Smith` for
/// profile `A`, which the general rule takes whole.
fn profile_pattern(path: &str) -> Option<regex::Regex> {
    let path = path.trim().trim_end_matches(['\\', '/']);
    let (parent, folder) = path.rsplit_once(['\\', '/'])?;
    if folder.is_empty() {
        return None;
    }
    let parent: Vec<_> = parent.split(['\\', '/']).map(regex::escape).collect();
    regex::Regex::new(&format!(
        r#"(?i)({}[\\/]+){}([\\/"']|$)"#,
        parent.join(r"[\\/]+"),
        regex::escape(folder)
    ))
    .ok()
}

/// Lines whose `device=` is the PC's audio output. Its name can be the
/// owner's ("Maria's AirPods"); the WASAPI line also logs the form factor and
/// bus, which uploads keep.
const AUDIO_DEVICE_LINES: [&str; 3] = [
    "WASAPI loopback active",
    "PC audio session started",
    "Audio stream stats",
];

fn redact_audio_device(text: &str) -> std::borrow::Cow<'_, str> {
    static DEVICE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r" device=.*?( [a-z][a-z0-9_]*=|$)").expect("device field pattern")
    });
    let message = crate::log_repeat::split_stamp(text)
        .and_then(|(_, rest)| rest.trim_start().split_once(' '))
        .map(|(_, message)| message);
    if message.is_some_and(|m| AUDIO_DEVICE_LINES.iter().any(|line| m.starts_with(line))) {
        DEVICE.replace(text, " device=[AUDIO-DEVICE]$1")
    } else {
        text.into()
    }
}

/// The PC's name where the host logs it: the mDNS registration's `host=` and
/// instance name, and mDNS host names (`NAME.local.`). Only a whole token
/// equal to the name, in any case, so a PC named `Desktop` or `INFO` leaves
/// "Desktop duplication" and the level alone.
fn redact_pc_name<'a>(text: &'a str, pc: &str) -> std::borrow::Cow<'a, str> {
    static CARRIERS: once_cell::sync::Lazy<[regex::Regex; 2]> = once_cell::sync::Lazy::new(|| {
        [
            regex::Regex::new(r"(\bhost=|\bEternalMonitor \()([^\s()]+)()"),
            regex::Regex::new(r"()\b([A-Za-z0-9_-]+)(\.local\b)"),
        ]
        .map(|pattern| pattern.expect("PC name pattern"))
    });
    let mut text = std::borrow::Cow::Borrowed(text);
    for carrier in CARRIERS.iter() {
        let replaced = carrier.replace_all(&text, |captures: &regex::Captures| {
            let token = &captures[2];
            let token = if token.to_lowercase() == pc {
                "[PC-NAME]"
            } else {
                token
            };
            format!("{}{token}{}", &captures[1], &captures[3])
        });
        if let std::borrow::Cow::Owned(replaced) = replaced {
            text = replaced.into();
        }
    }
    text
}

/// Uploads leave out the names in [`Names`] and audio device names. Support
/// identifies a PC by its installation id. The session log and "Copy logs"
/// keep them all.
fn redact_upload(text: &str, names: &Names) -> String {
    let text = redact_audio_device(text);
    let text = match &names.profile {
        Some(profile) => profile.replace_all(&text, "${1}[USER]${2}").into_owned(),
        None => text.into_owned(),
    };
    // Every other profile folder not yet [USER]. A Windows user name may
    // contain spaces but no path separator, so a segment that ends at one is
    // taken whole; at the end of a path, only up to the next space.
    static PROFILE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(
            r#"(?i)(\b[a-z]:[\\/]+(?:users|documents and settings)[\\/]+|/(?:Users|home)/)(?:([^\\/\r\n"'<>|:*?=]+)([\\/])|([^\\/\s"'<>|:*?=]+))"#,
        )
        .expect("profile path pattern")
    });
    let text = PROFILE.replace_all(&text, |captures: &regex::Captures| {
        let folder = captures
            .get(2)
            .or(captures.get(4))
            .map_or("", |m| m.as_str());
        if folder == "[USER]" {
            return captures[0].to_owned();
        }
        format!(
            "{}[USER]{}",
            &captures[1],
            captures.get(3).map_or("", |separator| separator.as_str())
        )
    });
    match &names.pc {
        Some(pc) => redact_pc_name(&text, pc).into_owned(),
        None => text.into_owned(),
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
    set_status("Automatic upload enabled; waiting for events");
    loop {
        if stop.load(Ordering::SeqCst) {
            return finish(&directory, &receiver, &periodic, pending);
        }
        let received = match receiver.lock() {
            Ok(receiver) => receiver.recv_timeout(Duration::from_millis(100)),
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
/// receiver lock. The shutdown fallback takes that lock, so it cannot find the
/// queue empty and let the process exit while drained events are unwritten.
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

    /// Quit as main does while the first upload is in flight. Returns how long
    /// the quit took and whether it stopped waiting for the upload thread.
    fn quit_waits(delay: Duration) -> (Duration, PathBuf, bool) {
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

    /// Lines a streaming host logs over `seconds`, at their real rates.
    fn streaming_lines(seconds: u32) -> Vec<String> {
        let mut lines = Vec::new();
        for half in 0..seconds * 2 {
            let stamp = format!("2026-10-22T17:46:{:02}.{}00000Z", half / 2, half % 2 * 5);
            if half % 20 == 0 {
                lines.push(format!(
                    "{stamp}  INFO Support stream summary session_id=Some(7) running=true \
                     captured={half} capture_fps=60.0"
                ));
            }
            lines.push(format!(
                "{stamp}  INFO Receiver report epoch=1 complete={half} dropped=0 repaired=0 \
                 nacks=0 jitter_us=190 decode_fps_x10=558 decode_depth=0 retransmits=0"
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
        let other = "2026-10-22T17:46:30.000000Z  WARN Client liveness expired";
        sender.send(Queued::Line(other.into())).unwrap();
        drop(uploader);
        let messages = outbox_messages(&directory);
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
        assert!(summaries[0].contains(" session_id=7 rr_lines=60 "));
        assert!(summaries[0].contains(" audio_lines=30 "));
        assert!(summaries[0].contains(" stream_lines=3 "));
        assert!(!summaries[0].contains("Speakers"), "no device names");
        assert!(messages.iter().any(|m| m == other));
        assert_eq!(messages.len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uploads_leave_out_the_windows_user_name_and_keep_it_locally() {
        let line = r"2026-10-22T15:56:27.075652Z  INFO Session log file initialized path=C:\Users\Maria Rossi\AppData\Roaming\EternalMonitor\logs\eternal-host-session.log";
        assert_eq!(
            event(line, "info").message,
            r"2026-10-22T15:56:27.075652Z  INFO Session log file initialized path=C:\Users\[USER]\AppData\Roaming\EternalMonitor\logs\eternal-host-session.log"
        );
        // The session log and "Copy logs" keep the full path.
        assert_eq!(crate::logging::redact_support_text(line), line);
    }

    #[test]
    fn every_profile_path_form_loses_its_user_name() {
        for (text, uploaded) in [
            (
                r"Host panic panic=panicked at C:\Users\bob\.cargo\registry\src\renderer.rs:984:17:",
                r"Host panic panic=panicked at C:\Users\[USER]\.cargo\registry\src\renderer.rs:984:17:",
            ),
            (
                r"profile=c:/users/Bob Smith/Desktop next=1",
                r"profile=c:/users/[USER]/Desktop next=1",
            ),
            (r"home=C:\Users\Bob Smith", r"home=C:\Users\[USER] Smith"),
            (r"home=C:\Users\bob next=1", r"home=C:\Users\[USER] next=1"),
            (
                "panicked at /Users/bob/src/main.rs:1:1",
                "panicked at /Users/[USER]/src/main.rs:1:1",
            ),
            ("dir=/home/bob/.config", "dir=/home/[USER]/.config"),
            (
                r"path=C:\Program Files\EternalMonitor",
                r"path=C:\Program Files\EternalMonitor",
            ),
        ] {
            assert_eq!(redact_upload(text, &Names::new(None, None)), uploaded);
        }
    }

    #[test]
    fn uploads_leave_out_the_pc_name_only_where_it_is_carried() {
        let mdns = |name: &str| {
            format!(
                "2026-10-22T18:04:26.445159Z  INFO mDNS service registering \u{2014} advertising \
                 on all non-loopback addresses service_type=\"_eternaldisplay._udp.local.\" \
                 port=9876 host={name} instance=EternalMonitor ({name}) addrs=[]"
            )
        };
        // A name that is also a word or a level marker leaves the rest alone.
        for (name, line) in [
            ("Desktop", "2026-10-22T18:04:26.445159Z  WARN Desktop duplication access lost \u{2014} reinitializing"),
            ("INFO", "2026-10-22T18:04:26.445159Z  INFO Encoder opened codec=h264_nvenc"),
            ("INFO", "2026-10-22T18:04:26.445159Z  INFO Client session established device=INFO"),
        ] {
            assert_eq!(redact_upload(line, &Names::new(None, Some(name))), line);
        }
        let redacted = mdns("[PC-NAME]");
        for name in ["MARIA-LAPTOP", "Desktop", "INFO"] {
            let names = Names::new(None, Some(name));
            assert_eq!(redact_upload(&mdns(name), &names), redacted, "{name}");
            // A lowercase copy, as in an mDNS host name, is the same name.
            assert_eq!(
                redact_upload(
                    &format!("Announce {}.local. on 192.0.2.1", name.to_lowercase()),
                    &names
                ),
                "Announce [PC-NAME].local. on 192.0.2.1",
                "{name}"
            );
        }
        assert!(Names::new(None, Some("  ")).pc.is_none());
    }

    #[test]
    fn uploads_leave_out_the_whole_profile_folder() {
        let names = Names::new(Some(r"C:\Users\Name Smith"), None);
        for (text, uploaded) in [
            (r"home=C:\Users\Name Smith", r"home=C:\Users\[USER]"),
            (
                r"path=C:\Users\Name Smith\AppData\Roaming\EternalMonitor\settings.json",
                r"path=C:\Users\[USER]\AppData\Roaming\EternalMonitor\settings.json",
            ),
            (
                r"dir=c:/users/name smith/Desktop next=1",
                r"dir=c:/users/[USER]/Desktop next=1",
            ),
        ] {
            assert_eq!(redact_upload(text, &names), uploaded);
        }
    }

    /// The exact profile counts only where its folder name ends, so a longer
    /// folder that starts with it goes whole through the general rule.
    #[test]
    fn a_profile_that_starts_another_folder_name_does_not_cut_it() {
        let names = Names::new(Some(r"C:\Users\A"), None);
        let wrong = [
            (
                r"path=C:\Users\A Smith\Desktop\x",
                r"path=C:\Users\[USER]\Desktop\x",
            ),
            (r"C:\Users\A\x", r"C:\Users\[USER]\x"),
            (r"C:\Users\Al\x", r"C:\Users\[USER]\x"),
            (r"C:\Users\A Smith\x", r"C:\Users\[USER]\x"),
            (r"home=C:\Users\A", r"home=C:\Users\[USER]"),
            (
                r#"path="C:\Users\A" next=1"#,
                r#"path="C:\Users\[USER]" next=1"#,
            ),
            (r"path='C:\Users\A'", r"path='C:\Users\[USER]'"),
            (r"path=C:\Users\[USER]\x", r"path=C:\Users\[USER]\x"),
        ]
        .into_iter()
        .filter(|(text, uploaded)| redact_upload(text, &names) != *uploaded)
        .map(|(text, _)| (text, redact_upload(text, &names)))
        .collect::<Vec<_>>();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn uploads_name_no_audio_device_but_keep_its_kind() {
        for (line, uploaded) in [
            (
                "2026-10-22T18:00:00.000000Z  INFO WASAPI loopback active device=Headphones (Maria's AirPods) \
                 form_factor=headphones bus=BTHENUM rate=48000 channels=2",
                "2026-10-22T18:00:00.000000Z  INFO WASAPI loopback active device=[AUDIO-DEVICE] \
                 form_factor=headphones bus=BTHENUM rate=48000 channels=2",
            ),
            (
                "2026-10-22T18:00:00.000000Z  INFO PC audio session started (Opus, 48 kHz stereo, 20 ms) \
                 session_id=5 device=Headphones (Maria's AirPods) bitrate=96000",
                "2026-10-22T18:00:00.000000Z  INFO PC audio session started (Opus, 48 kHz stereo, 20 ms) \
                 session_id=5 device=[AUDIO-DEVICE] bitrate=96000",
            ),
            (
                "2026-10-22T18:00:00.000000Z  INFO WASAPI loopback active device=Maria's AirPods",
                "2026-10-22T18:00:00.000000Z  INFO WASAPI loopback active device=[AUDIO-DEVICE]",
            ),
            // Other lines' device fields are not audio devices.
            (
                "2026-10-22T18:00:00.000000Z  INFO Client session established session_id=5 device=iPad",
                "2026-10-22T18:00:00.000000Z  INFO Client session established session_id=5 device=iPad",
            ),
        ] {
            assert_eq!(event(line, "info").message, uploaded);
        }
    }

    /// A crash ends the process without the uploader's shutdown: the fatal
    /// report carries the open summary window, in order before it.
    #[test]
    fn a_fatal_report_carries_the_open_summary_window() {
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
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !lock(&uploader.periodic).is_open() {
            assert!(
                std::time::Instant::now() < deadline,
                "no line reached the window"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Let the thread take the rest of the lines too.
        std::thread::sleep(Duration::from_millis(300));
        let fatal = "EternalMonitor stopped unexpectedly: Host panic";
        queue_into(&directory, Some(&uploader.periodic), fatal, "error").unwrap();
        let batches: Vec<Batch> = files(&directory)
            .unwrap()
            .iter()
            .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
            .collect();
        let with_fatal = batches
            .iter()
            .find(|b| b.events.iter().any(|e| e.message == fatal))
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
        assert_eq!(messages[1], fatal);
        drop(uploader);
        let summaries = outbox_messages(&directory)
            .iter()
            .filter(|m| m.contains("Periodic stats summary"))
            .count();
        assert_eq!(summaries, 1, "the window is not uploaded twice");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quitting_stops_between_slow_uploads_and_keeps_the_rest() {
        let (waited, directory, _) = quit_waits(Duration::from_millis(1500));
        assert!(
            waited < Duration::from_millis(2500),
            "quit waited {waited:?}"
        );
        let messages = outbox_messages(&directory);
        assert!(messages
            .iter()
            .any(|m| m.ends_with("EternalMonitor shutting down")));
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

    #[test]
    fn quitting_near_the_deadline_keeps_every_drained_event() {
        let directory = outbox_with_batches(1);
        let config = Config {
            endpoint: slow_server(Duration::from_millis(1700)).0,
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: true,
        };
        // The acknowledgement arrives just before the deadline, and the
        // worker is still writing the drained queue when the wait runs out.
        *PAUSED_OUTBOX.lock().unwrap() = Some((directory.clone(), Duration::from_millis(1500)));
        let (sender, uploader) = Uploader::spawn(config, directory.clone());
        std::thread::sleep(Duration::from_millis(300));
        for index in 0..10 {
            sender
                .send(Queued::Line(format!(
                    "2026-10-21T17:11:31Z  INFO last line {index}"
                )))
                .unwrap();
        }
        drop(uploader);
        let messages = outbox_messages(&directory);
        let kept = (0..10)
            .filter(|index| {
                let line = format!("last line {index}");
                messages.iter().any(|m| m.ends_with(&line))
            })
            .count();
        assert_eq!(kept, 10, "{messages:?}");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn an_empty_log_line_is_an_event_and_not_the_shutdown_wake() {
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
        let messages = outbox_messages(&directory);
        assert!(messages
            .iter()
            .any(|m| m.ends_with("EternalMonitor shutting down")));
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
