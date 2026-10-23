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
/// and finds nothing left. It runs inside the quit's [`FINAL_WRITE_WAIT`].
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

/// What uploads leave out: the PC's profile folder names, which are user
/// names, and the PC's name, which is often its owner's.
struct Names {
    /// Profile folder names on this PC, longest first, so `A Smith` is tried
    /// before `A`.
    profiles: Vec<String>,
    /// Registered profiles outside a `Users` folder, such as
    /// `D:\Profiles\Name Smith`, as (parent, folder), longest first.
    redirected: Vec<(String, String)>,
    /// Lowercase, for a whole-token match in any case.
    pc: Option<String>,
}
impl Names {
    /// `profiles` are profile paths. Folders directly under a `Users` (or
    /// `home`) folder are local names, never the shared ones; any other
    /// path is a redirected profile, except Windows' own service profiles.
    fn new(profiles: &[&str], pc: Option<&str>) -> Self {
        let mut redirected: Vec<(String, String)> = profiles
            .iter()
            .filter(|path| profile_folder(path).is_none())
            .filter_map(|path| {
                let path = path.trim().trim_end_matches(['\\', '/']);
                let (parent, folder) = path.rsplit_once(['\\', '/'])?;
                let system = path
                    .split(['\\', '/'])
                    .any(|part| part.eq_ignore_ascii_case("windows"));
                (!system && !parent.is_empty() && !folder.is_empty())
                    .then(|| (parent.to_owned(), folder.to_owned()))
            })
            .collect();
        redirected.sort_by_key(|(parent, folder)| {
            std::cmp::Reverse(parent.chars().count() + folder.chars().count())
        });
        redirected.dedup_by(|a, b| {
            a.0.to_lowercase() == b.0.to_lowercase() && a.1.to_lowercase() == b.1.to_lowercase()
        });
        let mut folders: Vec<String> = profiles
            .iter()
            .filter_map(|path| profile_folder(path))
            .filter(|folder| {
                !SHARED_PROFILES
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(folder))
            })
            .map(str::to_owned)
            .collect();
        folders.sort_by_key(|folder| std::cmp::Reverse(folder.chars().count()));
        folders.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
        Self {
            profiles: folders,
            redirected,
            pc: pc
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_lowercase),
        }
    }
}
static NAMES: once_cell::sync::Lazy<Names> = once_cell::sync::Lazy::new(|| {
    let profiles = profile_paths();
    let profiles: Vec<&str> = profiles.iter().map(String::as_str).collect();
    let pc = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME"));
    Names::new(&profiles, pc.ok().as_deref())
});

/// Folders under `Users` that name nobody. Support uses them.
const SHARED_PROFILES: [&str; 4] = ["Default User", "All Users", "Default", "Public"];

/// The folder name of a profile directly under a `Users` or `home` folder.
fn profile_folder(path: &str) -> Option<&str> {
    let path = path.trim().trim_end_matches(['\\', '/']);
    let (parent, folder) = path.rsplit_once(['\\', '/'])?;
    let parent = parent.rsplit(['\\', '/']).next()?;
    let under_users = parent.eq_ignore_ascii_case("users") || parent == "home";
    (under_users && !folder.is_empty()).then_some(folder)
}

/// This user's profile and, on Windows, every profile in the registry's
/// ProfileList (`ProfileImagePath`), which standard users can read. Paths
/// outside `\Users\` are kept: they are redirected profiles.
fn profile_paths() -> Vec<String> {
    #[allow(unused_mut)]
    let mut paths: Vec<String> = ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    #[cfg(windows)]
    paths.extend(windows_profile_list());
    paths
}

#[cfg(windows)]
fn windows_profile_list() -> Vec<String> {
    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE,
        KEY_READ, RRF_RT_REG_SZ,
    };
    let mut paths = Vec::new();
    unsafe {
        let mut list = HKEY::default();
        let opened = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList"),
            0,
            KEY_READ,
            &mut list,
        );
        if opened.is_err() {
            return paths;
        }
        for index in 0.. {
            let mut sid = [0u16; 256];
            let mut length = sid.len() as u32;
            let listed = RegEnumKeyExW(
                list,
                index,
                PWSTR(sid.as_mut_ptr()),
                &mut length,
                None,
                PWSTR::null(),
                None,
                None,
            );
            if listed.is_err() {
                break;
            }
            let subkey: Vec<u16> = sid[..length as usize]
                .iter()
                .copied()
                .chain(Some(0))
                .collect();
            // RRF_RT_REG_SZ also reads REG_EXPAND_SZ, expanded.
            let mut value = [0u16; 1024];
            let mut bytes = std::mem::size_of_val(&value) as u32;
            let read = RegGetValueW(
                list,
                PCWSTR(subkey.as_ptr()),
                w!("ProfileImagePath"),
                RRF_RT_REG_SZ,
                None,
                Some(value.as_mut_ptr().cast()),
                Some(&mut bytes),
            );
            if read.is_ok() {
                let end = value.iter().position(|c| *c == 0).unwrap_or(value.len());
                paths.push(String::from_utf16_lossy(&value[..end]));
            }
        }
        let _ = RegCloseKey(list);
    }
    paths
}

/// `name` at the start of `text`, in any case: the length in bytes it takes.
fn starts_with_name(text: &str, name: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    for expected in name.chars() {
        let (_, found) = chars.next()?;
        if !found.to_lowercase().eq(expected.to_lowercase()) {
            return None;
        }
    }
    Some(chars.next().map_or(text.len(), |(index, _)| index))
}

/// How a profile segment ended.
#[derive(Clone, Copy, PartialEq)]
enum SegmentEnd {
    /// `\` or `/`, a character Windows forbids in file names
    /// (`: * ? " < > |`), or `'`: the segment is exactly the folder name.
    Delimiter,
    /// The end of the line, or a space before the next `word=` field.
    Open,
}

/// The segment after `\Users\` in `rest`: its length in bytes and how it
/// ended.
fn segment(rest: &str) -> (usize, SegmentEnd) {
    static FIELD: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\s+[A-Za-z_][A-Za-z0-9_]*=").expect("field pattern")
    });
    for (index, c) in rest.char_indices() {
        if matches!(
            c,
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\''
        ) {
            return (index, SegmentEnd::Delimiter);
        }
        if c.is_whitespace() && FIELD.is_match(&rest[index..]) {
            return (index, SegmentEnd::Open);
        }
    }
    (rest.len(), SegmentEnd::Open)
}

/// Windows local profile folder names are short: a user name of at most 20
/// characters plus a suffix such as `.DOMAIN` or `.000`.
const LOCAL_FOLDER_CHARS: usize = 32;

/// Replaces profile folder names in `text` with `[USER]`.
///
/// Step 1, registered profiles outside `<drive>:\Users\` (redirected, such as
/// `D:\Profiles\Name Smith`): the exact registered path, longest first, in any
/// case and with either separator, becomes `<parent>\[USER]`.
///
/// Step 2, after every `<drive>:\Users\` (and `/Users/`, `/home/` on macOS and
/// Linux), in any case and with either separator: the segment runs to the
/// first `\` or `/`, character Windows forbids in file names
/// (`: * ? " < > |`), `'`, end of line, or space before a `word=` field.
///
/// - (a) A segment that starts with `[USER]`, or is `Public`, `Default`,
///   `Default User` or `All Users`, stays.
/// - (b) A segment ended by a separator, forbidden character or quote is
///   exactly the folder name: all of it becomes `[USER]`.
/// - (c) Otherwise, a known local profile name, longest first, that starts
///   the segment and is followed by whitespace, `.`, `,`, `;`, `)` or the
///   end is replaced, exactly that name.
/// - (d) Otherwise, a segment of at most 32 characters is replaced whole.
/// - (e) Otherwise, only up to its first whitespace: the documented
///   residual, an unknown, non-local profile followed by free text with no
///   separator.
///
/// With local profile `C:\Users\Name Smith`:
///
/// | Input | Upload |
/// |---|---|
/// | `home=C:\Users\Name Smith: next=1` | `home=C:\Users\[USER]: next=1` |
/// | `home=C:\Users\Name Smith.` | `home=C:\Users\[USER].` |
/// | `home=C:\Users\Name Smith next=1` | `home=C:\Users\[USER] next=1` |
/// | `C:\Users\A Smith\Desktop\x` (local `A` only) | `C:\Users\[USER]\Desktop\x` |
/// | `path=C:\Users\[USER] access denied while opening /config` | unchanged |
/// | `D:\Profiles\Name Smith\AppData\x` (registered) | `D:\Profiles\[USER]\AppData\x` |
/// | `"C:\Users\A"` | `"C:\Users\[USER]"` |
/// | `C:\Users\Public\x` | unchanged |
fn redact_profiles(text: &str, names: &Names) -> String {
    let text = redact_redirected(text, names);
    static PREFIX: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(
            r"(?i)\b[a-z]:[\\/]+(?:users|documents and settings)[\\/]+|/(?:users|home)/",
        )
        .expect("profile prefix pattern")
    });
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for prefix in PREFIX.find_iter(&text) {
        if prefix.start() < copied {
            continue;
        }
        let rest = &text[prefix.end()..];
        let (length, end) = segment(rest);
        let folder = &rest[..length];
        // a. Already redacted, or a shared folder.
        if folder.starts_with("[USER]")
            || SHARED_PROFILES
                .iter()
                .any(|name| name.eq_ignore_ascii_case(folder))
        {
            continue;
        }
        let replaced = if end == SegmentEnd::Delimiter {
            // b. Exactly the folder name.
            length
        } else if let Some(name) = names.profiles.iter().find_map(|name| {
            starts_with_name(folder, name).filter(|&after| {
                rest[after..]
                    .chars()
                    .next()
                    .is_none_or(|c| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ')'))
            })
        }) {
            // c. A known local name.
            name
        } else if folder.chars().count() <= LOCAL_FOLDER_CHARS {
            // d. Short enough to be a local folder name.
            length
        } else {
            // e. The residual.
            folder.find(char::is_whitespace).unwrap_or(length)
        };
        if replaced == 0 {
            continue;
        }
        out.push_str(&text[copied..prefix.end()]);
        out.push_str("[USER]");
        copied = prefix.end() + replaced;
    }
    out.push_str(&text[copied..]);
    out
}

/// Step 1 of [`redact_profiles`]: each registered profile outside a `Users`
/// folder, as an exact path that ends at its folder name.
fn redact_redirected<'a>(text: &'a str, names: &Names) -> std::borrow::Cow<'a, str> {
    let mut text = std::borrow::Cow::Borrowed(text);
    for (parent, folder) in &names.redirected {
        let parent: Vec<_> = parent.split(['\\', '/']).map(regex::escape).collect();
        let Ok(pattern) = regex::Regex::new(&format!(
            r"(?i)({}[\\/]+){}",
            parent.join(r"[\\/]+"),
            regex::escape(folder)
        )) else {
            continue;
        };
        let replaced = pattern.replace_all(&text, |captures: &regex::Captures| {
            let whole = captures.get(0).expect("match");
            // A longer folder that starts with this name is another folder.
            let continues = text[whole.end()..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric);
            if continues {
                whole.as_str().to_owned()
            } else {
                format!("{}[USER]", &captures[1])
            }
        });
        if let std::borrow::Cow::Owned(replaced) = replaced {
            text = replaced.into();
        }
    }
    text
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
    let text = redact_profiles(&redact_audio_device(text), names);
    match &names.pc {
        Some(pc) => redact_pc_name(&text, pc).into_owned(),
        None => text,
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
        let line = r"2026-10-22T15:56:27.075652Z  INFO Session log file initialized path=C:\Users\Maria\AppData\Roaming\EternalMonitor\logs\eternal-host-session.log";
        assert_eq!(
            event(line, "info").message,
            r"2026-10-22T15:56:27.075652Z  INFO Session log file initialized path=C:\Users\[USER]\AppData\Roaming\EternalMonitor\logs\eternal-host-session.log"
        );
        // The session log and "Copy logs" keep the full path.
        assert_eq!(crate::logging::redact_support_text(line), line);
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
            assert_eq!(redact_upload(line, &Names::new(&[], Some(name))), line);
        }
        let redacted = mdns("[PC-NAME]");
        for name in ["MARIA-LAPTOP", "Desktop", "INFO"] {
            let names = Names::new(&[], Some(name));
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
        assert!(Names::new(&[], Some("  ")).pc.is_none());
    }

    /// Every path and device-name input from the reviews. `profiles` stands
    /// in for this PC's profile list.
    #[test]
    fn upload_redaction_table() {
        const NAME_SMITH: &[&str] = &[r"C:\Users\Name Smith"];
        const A_AND_A_SMITH: &[&str] = &[r"C:\Users\A", r"C:\Users\A Smith"];
        const A_ONLY: &[&str] = &[r"C:\Users\A"];
        const REDIRECTED: &[&str] = &[r"C:\Users\Name Smith", r"D:\Profiles\Name Smith"];
        const NONE: &[&str] = &[];
        let cases: &[(&[&str], &str, &str)] = &[
            // The algorithm's table (see `redact_profiles`).
            (NAME_SMITH, r"home=C:\Users\Name Smith: next=1", r"home=C:\Users\[USER]: next=1"),
            (NAME_SMITH, r"home=C:\Users\Name Smith.", r"home=C:\Users\[USER]."),
            (NAME_SMITH, r"home=C:\Users\Name Smith next=1", r"home=C:\Users\[USER] next=1"),
            (A_ONLY, r"C:\Users\A Smith\Desktop\x", r"C:\Users\[USER]\Desktop\x"),
            (NAME_SMITH, r"path=C:\Users\[USER] access denied while opening /config",
                r"path=C:\Users\[USER] access denied while opening /config"),
            (REDIRECTED, r"D:\Profiles\Name Smith\AppData\x", r"D:\Profiles\[USER]\AppData\x"),
            (NAME_SMITH, r#""C:\Users\A""#, r#""C:\Users\[USER]""#),
            (NAME_SMITH, r"C:\Users\Public\x", r"C:\Users\Public\x"),
            // Redirected profiles in any case, either separator, at the end.
            (REDIRECTED, r"dir=d:/profiles/name smith/x", r"dir=d:/profiles/[USER]/x"),
            (REDIRECTED, r"home=D:\Profiles\Name Smith", r"home=D:\Profiles\[USER]"),
            (REDIRECTED, r"home=D:\Profiles\Name Smith: next=1", r"home=D:\Profiles\[USER]: next=1"),
            (NONE, r"home=D:\Profiles\Name Smith\x", r"home=D:\Profiles\Name Smith\x"),
            (NONE, "2026-10-22T18:00:00.000000Z  INFO WASAPI loopback active device=Headphones (Maria's AirPods) form_factor=headphones bus=BTHENUM",
                "2026-10-22T18:00:00.000000Z  INFO WASAPI loopback active device=[AUDIO-DEVICE] form_factor=headphones bus=BTHENUM"),
            (NAME_SMITH, r"home=C:\Users\Name Smith", r"home=C:\Users\[USER]"),
            (NAME_SMITH, r"path=C:\Users\Name Smith\AppData\Roaming\EternalMonitor\settings.json",
                r"path=C:\Users\[USER]\AppData\Roaming\EternalMonitor\settings.json"),
            (NAME_SMITH, r"home=C:\Users\Name Smith next=1", r"home=C:\Users\[USER] next=1"),
            (NAME_SMITH, r"home=C:\Users\Name Smith, next=1", r"home=C:\Users\[USER], next=1"),
            (NAME_SMITH, r"dir=c:/users/name smith/Desktop next=1", r"dir=c:/users/[USER]/Desktop next=1"),
            (NAME_SMITH, r#"path="C:\Users\Name Smith\x" next=1"#, r#"path="C:\Users\[USER]\x" next=1"#),
            (A_AND_A_SMITH, r"path=C:\Users\A Smith\Desktop\x", r"path=C:\Users\[USER]\Desktop\x"),
            (A_AND_A_SMITH, r"C:\Users\A Smith\x", r"C:\Users\[USER]\x"),
            (A_AND_A_SMITH, r"home=C:\Users\A Smith next=1", r"home=C:\Users\[USER] next=1"),
            (A_AND_A_SMITH, r"C:\Users\A\x", r"C:\Users\[USER]\x"),
            (A_AND_A_SMITH, r"C:\Users\Al\x", r"C:\Users\[USER]\x"),
            (A_AND_A_SMITH, r"home=C:\Users\A", r"home=C:\Users\[USER]"),
            (A_AND_A_SMITH, r#"path="C:\Users\A" next=1"#, r#"path="C:\Users\[USER]" next=1"#),
            (A_AND_A_SMITH, r"path='C:\Users\A'", r"path='C:\Users\[USER]'"),
            (NONE, r"path=C:\Users\[USER] access denied while opening /config",
                r"path=C:\Users\[USER] access denied while opening /config"),
            (NONE, r"path=C:\Users\[USER]\x", r"path=C:\Users\[USER]\x"),
            (NAME_SMITH, r"path=C:\Users\Public\Documents\x", r"path=C:\Users\Public\Documents\x"),
            (NAME_SMITH, r"path=C:\Users\Default\AppData\x", r"path=C:\Users\Default\AppData\x"),
            (NONE, r"path=C:\Users\Default User\x", r"path=C:\Users\Default User\x"),
            (NONE, r"path=C:\Users\All Users\x", r"path=C:\Users\All Users\x"),
            (NONE, r"member=BUILTIN\Users\group", r"member=BUILTIN\Users\group"),
            (NONE, r"path=C:\Program Files\EternalMonitor", r"path=C:\Program Files\EternalMonitor"),
            // Profiles not on this PC.
            (NONE, r"Host panic at C:\Users\bob\.cargo\registry\src\renderer.rs:984:17:",
                r"Host panic at C:\Users\[USER]\.cargo\registry\src\renderer.rs:984:17:"),
            (NONE, r"home=C:\Users\bob next=1", r"home=C:\Users\[USER] next=1"),
            (NONE, r"home=C:\Users\bob, next=1", r"home=C:\Users\[USER] next=1"),
            (NONE, r"home=C:\Users\Bob Smith\x", r"home=C:\Users\[USER]\x"),
            (NONE, r"path=C:\Users\bob was not found", r"path=C:\Users\[USER]"),
            // The residual: long free text after an unknown profile.
            (NONE, r"path=C:\Users\bob could not be opened because the disk is full",
                r"path=C:\Users\[USER] could not be opened because the disk is full"),
            (NONE, "panicked at /Users/bob/src/main.rs:1:1", "panicked at /Users/[USER]/src/main.rs:1:1"),
            (NONE, "dir=/home/bob/.config", "dir=/home/[USER]/.config"),
            (&["/home/Name Smith"], "dir=/home/Name Smith next=1", "dir=/home/[USER] next=1"),
            (&["/Users/Name Smith"], "dir=/Users/Name Smith/Library x", "dir=/Users/[USER]/Library x"),
        ];
        let wrong: Vec<_> = cases
            .iter()
            .filter_map(|(profiles, text, uploaded)| {
                let got = redact_upload(text, &Names::new(profiles, None));
                (got != *uploaded).then(|| format!("{text}\n  want {uploaded}\n  got  {got}"))
            })
            .collect();
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    /// What the registry's profile list feeds in: personal folders directly
    /// under `Users` (or `home`), longest first, and redirected profiles,
    /// never Windows' service profiles.
    #[test]
    fn the_profile_list_keeps_personal_folders_under_users() {
        let names = Names::new(
            &[
                r"C:\Users\A",
                r"%SystemDrive%\Users\A Smith",
                r"C:\Windows\ServiceProfiles\LocalService",
                r"C:\WINDOWS\system32\config\systemprofile",
                r"C:\Users\Public",
                r"C:\Users\Default",
                r"C:\users\a\",
                "/home/bob",
                r"D:\Profiles\Name Smith",
                r"d:\profiles\name smith\",
            ],
            None,
        );
        assert_eq!(names.profiles, ["A Smith", "bob", "A"]);
        assert_eq!(
            names.redirected,
            [(r"D:\Profiles".to_owned(), "Name Smith".to_owned())]
        );
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
        let started = std::time::Instant::now();
        drop(uploader);
        let waited = started.elapsed();
        ENABLED.store(true, Ordering::Relaxed);
        // Writing the window stays inside the quit's bound.
        assert!(
            waited < SHUTDOWN_WAIT + FINAL_WRITE_WAIT + Duration::from_millis(350),
            "quit waited {waited:?}"
        );
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
