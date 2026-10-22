//! Update checks and in-app updates. Nothing here runs on the GUI or stream
//! threads: checks and downloads use one worker thread.
//!
//! The website's `latest.json` names the newest Windows release and iPad
//! build. It is bumped only after Apple approves the matching TestFlight build,
//! so the PC never offers an update the iPad cannot match yet. Before
//! installing, the installer's SHA-256 must match both that manifest and the
//! GitHub release body, which are published by different steps.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Where people can always get the installer by hand.
pub const WEBSITE_DOWNLOAD: &str = "https://eternalmonitor.dev/download.html";
pub use eternal_updater::SETUP_FLAGS;
const INSTALLER_NAME: &str = "EternalMonitor-Setup.exe";
const CHECK_INTERVAL: u64 = 24 * 60 * 60;
/// A relaunch checks again only when the last check is this old, so a crash
/// loop or repeated restarts cannot hammer the website.
const STARTUP_INTERVAL: u64 = 60 * 60;
const LATER_INTERVAL: u64 = 24 * 60 * 60;
/// The stream must stay off this long before a waiting update starts, so a
/// quick reconnect does not close the app under the user.
const STREAM_IDLE: Duration = Duration::from_secs(3);
const MAX_JSON_BYTES: u64 = 256 * 1024;
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
/// No version, device or installation identifiers: a GET needs none.
const USER_AGENT: &str = "EternalMonitor";

// ── Endpoints ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Endpoints {
    pub manifest: String,
    /// `v{version}` is appended to read that release.
    pub release_api: String,
    /// The installer URL must be exactly `{prefix}v{version}/EternalMonitor-Setup.exe`.
    pub installer_prefix: String,
    /// Hosts the download may start at or be redirected to.
    pub download_hosts: Vec<String>,
    pub https_only: bool,
}

impl Endpoints {
    pub fn production() -> Self {
        Self {
            manifest: "https://eternalmonitor.dev/latest.json".into(),
            release_api:
                "https://api.github.com/repos/sidebandstudio/EternalMonitor/releases/tags/".into(),
            installer_prefix: "https://github.com/sidebandstudio/EternalMonitor/releases/download/"
                .into(),
            download_hosts: vec![
                "github.com".into(),
                "objects.githubusercontent.com".into(),
                "release-assets.githubusercontent.com".into(),
            ],
            https_only: true,
        }
    }

    /// Debug builds only: `ETERNAL_UPDATE_BASE=http://127.0.0.1:8765` serves a
    /// fake release for end-to-end tests. Release builds always use production.
    fn from_env() -> Self {
        if cfg!(debug_assertions) {
            if let Ok(base) = std::env::var("ETERNAL_UPDATE_BASE") {
                if let Some(endpoints) = Self::local(base.trim_end_matches('/')) {
                    tracing::warn!(%base, "Update source overridden for testing");
                    return endpoints;
                }
            }
        }
        Self::production()
    }

    fn local(base: &str) -> Option<Self> {
        let host = parse_url(base)?.1;
        Some(Self {
            manifest: format!("{base}/latest.json"),
            release_api: format!("{base}/repos/sidebandstudio/EternalMonitor/releases/tags/"),
            installer_prefix: format!("{base}/releases/download/"),
            download_hosts: vec![host],
            https_only: false,
        })
    }
}

// ── Versions and the published release ──────────────────────────────────────

/// `Some(version)` when `candidate` is a stable release newer than `current`.
/// Build metadata is ignored; previews and malformed versions never qualify.
pub fn newer_stable(candidate: &str, current: &str) -> Option<String> {
    let mut release =
        semver::Version::parse(candidate.strip_prefix('v').unwrap_or(candidate)).ok()?;
    let mut current = semver::Version::parse(current).ok()?;
    if !release.pre.is_empty() {
        return None;
    }
    release.build = semver::BuildMetadata::EMPTY;
    current.build = semver::BuildMetadata::EMPTY;
    (release > current).then(|| release.to_string())
}

/// The Windows release named by the manifest, already validated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub installer: String,
    pub sha256: String,
}

#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    windows: Option<ManifestWindows>,
}

#[derive(Deserialize)]
struct ManifestWindows {
    version: String,
    installer: String,
    sha256: String,
}

pub fn parse_manifest(data: &[u8], endpoints: &Endpoints) -> Result<Release, UpdateError> {
    let bad = |why: &str| UpdateError::BadManifest(why.into());
    let manifest: Manifest =
        serde_json::from_slice(data).map_err(|error| bad(&error.to_string()))?;
    if manifest.schema != 1 {
        return Err(bad("unknown schema"));
    }
    let windows = manifest.windows.ok_or_else(|| bad("no Windows release"))?;
    let version = semver::Version::parse(&windows.version).map_err(|_| bad("bad version"))?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(bad("not a stable version"));
    }
    let sha256 = windows.sha256.to_ascii_lowercase();
    if !is_sha256(&sha256) {
        return Err(bad("bad SHA-256"));
    }
    let expected = format!("{}v{version}/{INSTALLER_NAME}", endpoints.installer_prefix);
    if windows.installer != expected {
        return Err(bad("unexpected installer URL"));
    }
    Ok(Release {
        version: version.to_string(),
        installer: expected,
        sha256,
    })
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// Checks the GitHub release against the manifest and returns the installer's
/// size. Fails closed: the release body must publish the same SHA-256.
pub fn check_github_release(data: &[u8], release: &Release) -> Result<u64, UpdateError> {
    let unsafe_download = |why: &str| UpdateError::UnsafeDownload(why.into());
    let github: GithubRelease = serde_json::from_slice(data)
        .map_err(|error| UpdateError::BadManifest(error.to_string()))?;
    if github.tag_name != format!("v{}", release.version) || github.draft || github.prerelease {
        return Err(unsafe_download("release is not the published stable tag"));
    }
    let published = github
        .body
        .as_deref()
        .and_then(published_sha256)
        .ok_or_else(|| unsafe_download("release has no SHA-256"))?;
    if published != release.sha256 {
        return Err(unsafe_download("release SHA-256 differs from the website"));
    }
    let asset = github
        .assets
        .iter()
        .find(|asset| asset.name == INSTALLER_NAME)
        .ok_or_else(|| unsafe_download("release has no installer"))?;
    if asset.browser_download_url != release.installer {
        return Err(unsafe_download("installer URL differs from the website"));
    }
    if let Some(digest) = asset.digest.as_deref() {
        if digest.to_ascii_lowercase() != format!("sha256:{}", release.sha256) {
            return Err(unsafe_download("GitHub digest differs"));
        }
    }
    if asset.size == 0 || asset.size > MAX_INSTALLER_BYTES {
        return Err(unsafe_download("installer size out of range"));
    }
    Ok(asset.size)
}

/// The release workflow writes "**EternalMonitor-Setup.exe SHA-256:**" and
/// the hash in a code block.
fn published_sha256(body: &str) -> Option<String> {
    let pattern = regex::Regex::new(r"(?is)SHA-256.*?\b([0-9a-f]{64})\b").ok()?;
    Some(pattern.captures(body)?[1].to_ascii_lowercase())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// No answer, an HTTP error or a broken connection.
    Network(String),
    BadManifest(String),
    /// A hash, size or host check failed. Nothing is installed.
    UnsafeDownload(String),
    Launch(String),
}

impl UpdateError {
    /// What the user reads. The detail goes to the log.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Network(_) | Self::BadManifest(_) => {
                "The update could not be downloaded. Check your internet connection and try again."
            }
            Self::UnsafeDownload(_) => {
                "The downloaded update did not pass its safety check, so it was not installed."
            }
            Self::Launch(_) => "The update could not start.",
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(why) => write!(f, "network: {why}"),
            Self::BadManifest(why) => write!(f, "manifest: {why}"),
            Self::UnsafeDownload(why) => write!(f, "unsafe download: {why}"),
            Self::Launch(why) => write!(f, "launch: {why}"),
        }
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────────

/// `(scheme, host)`, or `None` for URLs with credentials or no host.
fn parse_url(url: &str) -> Option<(String, String)> {
    let uri: ureq::http::Uri = url.parse().ok()?;
    let authority = uri.authority()?;
    if authority.as_str().contains('@') {
        return None;
    }
    Some((
        uri.scheme_str()?.to_ascii_lowercase(),
        authority.host().to_ascii_lowercase(),
    ))
}

fn allowed_url(url: &str, endpoints: &Endpoints) -> bool {
    let Some((scheme, host)) = parse_url(url) else {
        return false;
    };
    let scheme_ok = scheme == "https" || (!endpoints.https_only && scheme == "http");
    scheme_ok && endpoints.download_hosts.contains(&host)
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(10)))
        // Redirects are followed by hand so every hop's host is checked.
        .max_redirects(0)
        .build()
        .new_agent()
}

fn get_json(url: &str, accept: &str) -> Result<Vec<u8>, UpdateError> {
    agent(Duration::from_secs(10))
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", accept)
        .call()
        .and_then(|mut response| {
            if !response.status().is_success() {
                return Err(ureq::Error::StatusCode(response.status().as_u16()));
            }
            response
                .body_mut()
                .with_config()
                .limit(MAX_JSON_BYTES)
                .read_to_vec()
        })
        .map_err(|error| UpdateError::Network(error.to_string()))
}

pub fn fetch_manifest(endpoints: &Endpoints) -> Result<Release, UpdateError> {
    let data = get_json(&endpoints.manifest, "application/json")?;
    parse_manifest(&data, endpoints)
}

/// Downloads into a new, randomly named folder under `root`, verifies, and
/// returns the installer path. A failed or unverified download leaves no
/// installer behind.
pub fn download(
    endpoints: &Endpoints,
    release: &Release,
    root: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, UpdateError> {
    let io = |error: std::io::Error| UpdateError::Network(error.to_string());
    let api = format!("{}v{}", endpoints.release_api, release.version);
    let github = get_json(&api, "application/vnd.github+json")?;
    let size = check_github_release(&github, release)?;
    // Earlier updates are finished; their folders can go.
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).map_err(io)?;
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|error| UpdateError::Network(error.to_string()))?;
    let dir = root.join(hex(&random));
    // A fresh folder: nothing another process prepared is reused.
    std::fs::create_dir(&dir).map_err(io)?;
    let target = dir.join(format!("EternalMonitor-Setup-{}.exe", release.version));
    let partial = target.with_extension("part");
    let result = fetch_verified(endpoints, release, size, &partial, progress)
        .and_then(|()| std::fs::rename(&partial, &target).map_err(io));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result.map(|()| target)
}

fn fetch_verified(
    endpoints: &Endpoints,
    release: &Release,
    size: u64,
    path: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), UpdateError> {
    let unsafe_download = |why: String| UpdateError::UnsafeDownload(why);
    let network = |error: &dyn std::fmt::Display| UpdateError::Network(error.to_string());
    let agent = agent(Duration::from_secs(30 * 60));
    let mut url = release.installer.clone();
    for _ in 0..=MAX_REDIRECTS {
        if !allowed_url(&url, endpoints) {
            return Err(unsafe_download(format!("host not allowed: {url}")));
        }
        let response = agent
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| network(&e))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| unsafe_download("redirect without a location".into()))?;
            url = resolve_redirect(&url, location)
                .ok_or_else(|| unsafe_download(format!("bad redirect: {location}")))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(UpdateError::Network(format!("HTTP {}", response.status())));
        }
        if response
            .body()
            .content_length()
            .is_some_and(|length| length != size)
        {
            return Err(unsafe_download("unexpected download size".into()));
        }
        let mut file = std::fs::File::create(path).map_err(|e| network(&e))?;
        let mut reader = response.into_body().into_reader().take(size + 1);
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buffer = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            let read = reader.read(&mut buffer).map_err(|e| network(&e))?;
            if read == 0 {
                break;
            }
            received += read as u64;
            if received > size {
                return Err(unsafe_download("download is larger than published".into()));
            }
            digest.update(&buffer[..read]);
            file.write_all(&buffer[..read]).map_err(|e| network(&e))?;
            progress(received, size);
        }
        file.sync_all().map_err(|e| network(&e))?;
        if received != size {
            return Err(UpdateError::Network("download ended early".into()));
        }
        if hex(digest.finish().as_ref()) != release.sha256 {
            return Err(unsafe_download("SHA-256 mismatch".into()));
        }
        return Ok(());
    }
    Err(unsafe_download("too many redirects".into()))
}

fn resolve_redirect(from: &str, location: &str) -> Option<String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Some(location.to_string());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        let uri: ureq::http::Uri = from.parse().ok()?;
        return Some(format!(
            "{}://{}{location}",
            uri.scheme_str()?,
            uri.authority()?
        ));
    }
    None
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Installing ──────────────────────────────────────────────────────────────

/// How long the helper may take to lock and verify the installer.
const HELPER_READY: Duration = Duration::from_secs(30);

/// The installed helper, next to the host (`eternal-updater` in a cargo build).
fn installed_helper(app: &Path) -> Option<PathBuf> {
    let dir = app.parent()?;
    [
        "EternalMonitor-updater.exe",
        "eternal-updater.exe",
        "eternal-updater",
    ]
    .iter()
    .map(|name| dir.join(name))
    .find(|path| path.is_file())
}

/// Copies the helper out of the install folder, which Setup replaces, and
/// returns the copy held open so it cannot change before it starts. The copy
/// must hash the same as the installed helper.
pub fn stage_helper(source: &Path, dir: &Path) -> Result<(PathBuf, std::fs::File), UpdateError> {
    let launch = |error: &dyn std::fmt::Display| UpdateError::Launch(error.to_string());
    let name = source
        .file_name()
        .ok_or_else(|| launch(&"no helper name"))?;
    let copy = dir.join(name);
    std::fs::copy(source, &copy).map_err(|e| launch(&e))?;
    let mut original = std::fs::File::open(source).map_err(|e| launch(&e))?;
    let mut held = eternal_updater::open_locked(&copy).map_err(|e| launch(&e))?;
    let expected = eternal_updater::sha256_hex(&mut original).map_err(|e| launch(&e))?;
    let actual = eternal_updater::sha256_hex(&mut held).map_err(|e| launch(&e))?;
    if expected != actual {
        return Err(UpdateError::UnsafeDownload(
            "the update helper changed".into(),
        ));
    }
    Ok((copy, held))
}

/// Waits for the helper's `ready`: it holds the verified installer and owns
/// the relaunch. Anything else (it exits, reports an error or stays silent)
/// means this process must stay open; `stop` ends the helper then.
pub fn await_ready(
    stdout: impl std::io::Read + Send + 'static,
    timeout: Duration,
    stop: impl FnOnce(),
) -> Result<(), UpdateError> {
    use std::io::BufRead;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line.trim().to_string());
    });
    let line = rx.recv_timeout(timeout).unwrap_or_default();
    if line == eternal_updater::READY {
        return Ok(());
    }
    stop();
    tracing::warn!(%line, "The update helper did not start the update");
    if line.contains("SHA-256") {
        Err(UpdateError::UnsafeDownload(line))
    } else if line.is_empty() {
        Err(UpdateError::Launch("the update helper stopped".into()))
    } else {
        Err(UpdateError::Launch(line))
    }
}

/// Starts the helper for `setup` and returns once it has taken over.
fn start_install(setup: &Path, release: &Release) -> Result<(), UpdateError> {
    let launch = |error: &dyn std::fmt::Display| UpdateError::Launch(error.to_string());
    let app = std::env::current_exe().map_err(|e| launch(&e))?;
    let source = installed_helper(&app).ok_or_else(|| launch(&"the update helper is missing"))?;
    let dir = setup.parent().ok_or_else(|| launch(&"no update folder"))?;
    // The update folder and every folder above it must be real folders; the
    // folder stays pinned while the helper starts. (Threat model: SECURITY.md.)
    #[cfg(windows)]
    let _folder = eternal_updater::win::pin_dir(dir).map_err(UpdateError::UnsafeDownload)?;
    let (helper, held) = stage_helper(&source, dir)?;
    let args = eternal_updater::HelperArgs {
        installer: setup.to_path_buf(),
        sha256: release.sha256.clone(),
        app,
        host_pid: std::process::id(),
        log: dir.join("update.log"),
    };
    spawn_helper(&helper, &held, &args)
}

/// Starts the staged helper suspended and lets it run only if Windows mapped
/// the copy `held` is open on; then waits for its `ready`.
#[cfg(windows)]
fn spawn_helper(
    helper: &Path,
    held: &std::fs::File,
    args: &eternal_updater::HelperArgs,
) -> Result<(), UpdateError> {
    // The helper must outlive this process. Leave our job object when the
    // job allows it (CREATE_BREAKAWAY_FROM_JOB); otherwise stay in it.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let line = helper_command_line(helper, args);
    let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    let (process, stdout) = eternal_updater::win::spawn_verified_piped(
        helper,
        &line,
        held,
        flags | CREATE_BREAKAWAY_FROM_JOB,
    )
    .or_else(|_| eternal_updater::win::spawn_verified_piped(helper, &line, held, flags))
    .map_err(UpdateError::Launch)?;
    await_ready(stdout, HELPER_READY, move || {
        eternal_updater::win::terminate(&process);
    })
}

#[cfg(not(windows))]
fn spawn_helper(
    helper: &Path,
    _held: &std::fs::File,
    args: &eternal_updater::HelperArgs,
) -> Result<(), UpdateError> {
    let mut child = std::process::Command::new(helper)
        .args(args.to_args())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| UpdateError::Launch(e.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| UpdateError::Launch("no pipe".into()))?;
    await_ready(stdout, HELPER_READY, move || {
        let _ = child.kill();
        let _ = child.wait();
    })
}

/// The helper's Windows command line: its own name, then each argument quoted
/// the way the CRT splits them.
pub fn helper_command_line(helper: &Path, args: &eternal_updater::HelperArgs) -> String {
    let mut line = quote_arg(&helper.display().to_string());
    for arg in args.to_args() {
        line.push(' ');
        line.push_str(&quote_arg(&arg.to_string_lossy()));
    }
    line
}

/// Quotes one argument for the Windows CRT's command-line parser.
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

// ── State ───────────────────────────────────────────────────────────────────

/// Kept in `update-state.json` next to settings.json.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    checked_at: Option<u64>,
    latest: Option<Release>,
    /// "Later" for one version, with the time it was chosen.
    later: Option<(String, u64)>,
    /// The last version announced by a Windows notification.
    notified: Option<String>,
    /// Written just before Setup starts: `(from, to)`.
    pending: Option<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Waiting for the stream to stop. Holds the installer once downloaded.
    Queued(Option<PathBuf>),
    Downloading {
        received: u64,
        total: u64,
    },
    /// Verified and ready; the GUI starts it when nobody is streaming.
    Ready(PathBuf),
    Installing,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    NotChecked,
    Checking,
    UpToDate,
    Available,
    Failed,
}

/// What the banner shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Available(String),
    Queued(String),
    Downloading(String, Option<f32>),
    Installing(String),
    Failed(String),
    Updated(String),
}

pub enum Command {
    Check { manual: bool },
    Download(Release),
    Install(PathBuf, Release),
}

pub struct State {
    current: String,
    auto: bool,
    manual_check: bool,
    saved: Saved,
    phase: Phase,
    check: CheckStatus,
    updated: Option<String>,
    idle_since: Option<Instant>,
    force: bool,
    /// As of the last GUI frame.
    streaming: bool,
    /// The helper has taken over; the GUI should close.
    close: bool,
}

impl State {
    fn new(current: &str, auto: bool, saved: Saved) -> Self {
        let mut state = Self {
            current: current.into(),
            auto,
            manual_check: false,
            saved,
            phase: Phase::Idle,
            check: CheckStatus::NotChecked,
            updated: None,
            idle_since: None,
            force: false,
            streaming: false,
            close: false,
        };
        // A previous run started Setup. The relaunched version tells us how it went.
        if let Some((from, to)) = state.saved.pending.take() {
            if newer_stable(&to, &state.current).is_none() {
                state.updated = Some(state.current.clone());
            } else {
                tracing::warn!(%from, %to, "Update did not finish");
                state.phase = Phase::Failed(format!(
                    "The update did not finish, so you still have version {}. If Windows asks for permission, click Yes.",
                    state.current
                ));
            }
        }
        state
    }

    fn available(&self) -> Option<&Release> {
        self.saved
            .latest
            .as_ref()
            .filter(|latest| newer_stable(&latest.version, &self.current).is_some())
    }

    fn due(&self, now: u64, interval: u64) -> bool {
        self.auto
            && self
                .saved
                .checked_at
                .is_none_or(|last| now.saturating_sub(last) >= interval || last > now)
    }

    fn begin_check(&mut self, manual: bool) {
        self.manual_check |= manual;
        self.check = CheckStatus::Checking;
    }

    /// Returns a version to announce with a Windows notification, at most
    /// once per version and only for automatic checks.
    fn finish_check(
        &mut self,
        now: u64,
        manual: bool,
        result: Result<Release, UpdateError>,
    ) -> Option<String> {
        // Failures count too, so an offline PC is not asked again every launch.
        self.saved.checked_at = Some(now);
        match result {
            Ok(release) => {
                self.saved.latest = Some(release);
                self.check = if self.available().is_some() {
                    CheckStatus::Available
                } else {
                    CheckStatus::UpToDate
                };
            }
            Err(error) => {
                tracing::info!(%error, "Update check unavailable");
                self.check = CheckStatus::Failed;
            }
        }
        if manual {
            if let Some(version) = self.available().map(|r| r.version.clone()) {
                // Asking explicitly overrides an earlier "Later".
                self.saved.later = None;
                self.saved.notified = Some(version);
            }
            return None;
        }
        let version = self.available()?.version.clone();
        if !self.auto || self.snoozed(now) || self.saved.notified.as_deref() == Some(&version) {
            return None;
        }
        self.saved.notified = Some(version.clone());
        Some(version)
    }

    fn snoozed(&self, now: u64) -> bool {
        match (&self.saved.later, self.available()) {
            (Some((version, at)), Some(latest)) => {
                *version == latest.version && now.saturating_sub(*at) < LATER_INTERVAL && *at <= now
            }
            _ => false,
        }
    }

    pub fn notice(&self, now: u64) -> Option<Notice> {
        let version = || {
            self.saved
                .latest
                .as_ref()
                .map_or_else(String::new, |r| r.version.clone())
        };
        match &self.phase {
            Phase::Queued(_) => return Some(Notice::Queued(version())),
            Phase::Downloading { received, total } => {
                let fraction = (*total > 0).then(|| *received as f32 / *total as f32);
                return Some(Notice::Downloading(version(), fraction));
            }
            Phase::Ready(_) | Phase::Installing => return Some(Notice::Installing(version())),
            Phase::Failed(message) => return Some(Notice::Failed(message.clone())),
            Phase::Idle => {}
        }
        if let Some(version) = &self.updated {
            return Some(Notice::Updated(version.clone()));
        }
        let release = self.available()?;
        ((self.auto || self.manual_check) && !self.snoozed(now))
            .then(|| Notice::Available(release.version.clone()))
    }

    fn later(&mut self, now: u64) {
        if let Some(version) = self.available().map(|r| r.version.clone()) {
            self.saved.later = Some((version, now));
        }
        self.updated = None;
        if matches!(self.phase, Phase::Failed(_)) {
            self.phase = Phase::Idle;
        }
    }

    /// The user chose Update now (or Try again).
    fn update_now(&mut self, streaming: bool) -> Option<Command> {
        if !matches!(self.phase, Phase::Idle | Phase::Failed(_)) {
            return None;
        }
        let release = self.available()?.clone();
        self.force = false;
        if streaming {
            self.phase = Phase::Queued(None);
            self.idle_since = None;
            return None;
        }
        self.phase = Phase::Downloading {
            received: 0,
            total: 0,
        };
        Some(Command::Download(release))
    }

    /// "Update now" again while a stream runs: the user accepts the disconnect.
    fn force_now(&mut self) -> Option<Command> {
        self.force = true;
        self.idle_since = None;
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Queued(Some(path)) => {
                self.phase = Phase::Ready(path);
                None
            }
            Phase::Queued(None) => {
                self.phase = Phase::Downloading {
                    received: 0,
                    total: 0,
                };
                self.available().cloned().map(Command::Download)
            }
            other => {
                self.phase = other;
                None
            }
        }
    }

    fn cancel(&mut self) {
        if matches!(self.phase, Phase::Queued(_)) {
            self.phase = Phase::Idle;
        }
    }

    /// Called every frame. Starts a queued update once streaming has stopped.
    fn tick(&mut self, streaming: bool, now: Instant) -> Option<Command> {
        self.streaming = streaming;
        if !matches!(self.phase, Phase::Queued(_)) {
            return None;
        }
        if streaming {
            self.idle_since = None;
            return None;
        }
        let since = *self.idle_since.get_or_insert(now);
        if now.duration_since(since) < STREAM_IDLE {
            return None;
        }
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Queued(Some(path)) => {
                self.phase = Phase::Ready(path);
                None
            }
            _ => {
                self.phase = Phase::Downloading {
                    received: 0,
                    total: 0,
                };
                self.available().cloned().map(Command::Download)
            }
        }
    }

    fn progress(&mut self, received: u64, total: u64) {
        if let Phase::Downloading { .. } = self.phase {
            self.phase = Phase::Downloading { received, total };
        }
    }

    fn download_finished(&mut self, result: Result<PathBuf, UpdateError>) {
        self.phase = match result {
            Ok(path) => Phase::Ready(path),
            Err(error) => {
                tracing::warn!(%error, "Update download failed");
                Phase::Failed(error.message().into())
            }
        };
    }

    /// Hands the installer to the GUI unless a stream would be cut.
    fn take_ready(&mut self, streaming: bool) -> Option<(PathBuf, Release)> {
        let Phase::Ready(path) = &self.phase else {
            return None;
        };
        if streaming && !self.force {
            self.phase = Phase::Queued(Some(path.clone()));
            self.idle_since = None;
            return None;
        }
        let path = path.clone();
        let release = self.saved.latest.clone()?;
        self.phase = Phase::Installing;
        self.saved.pending = Some((self.current.clone(), release.version.clone()));
        Some((path, release))
    }

    /// A stream started before the handoff: wait for it again, keeping the
    /// verified download, and don't report an update as pending.
    fn requeue(&mut self, path: PathBuf) {
        if self.phase == Phase::Installing {
            self.phase = Phase::Queued(Some(path));
            self.idle_since = None;
            self.saved.pending = None;
        }
    }

    /// The helper said `ready`.
    fn install_started(&mut self) {
        if self.phase == Phase::Installing {
            self.close = true;
        }
    }

    fn install_failed(&mut self, error: &UpdateError) {
        tracing::warn!(%error, "Could not start the update");
        self.close = false;
        self.saved.pending = None;
        self.phase = Phase::Failed(error.message().into());
    }
}

// ── Version mismatch ────────────────────────────────────────────────────────

/// Which side must update when an iPad cannot connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OlderSide {
    Pc,
    Ipad,
}

/// The protocol versions this host speaks.
const PROTOCOL: std::ops::RangeInclusive<u8> = 2..=2;

/// Compares an iPad's advertised protocol range with ours.
pub fn older_side(ipad_min: u8, ipad_max: u8) -> Option<OlderSide> {
    if ipad_max < *PROTOCOL.start() {
        Some(OlderSide::Ipad)
    } else if ipad_min > *PROTOCOL.end() {
        Some(OlderSide::Pc)
    } else {
        None
    }
}

const MISMATCH_SHOWN: Duration = Duration::from_secs(10 * 60);

/// The last failed connection caused by a version difference.
#[derive(Default)]
pub struct MismatchLog(Mutex<Option<(OlderSide, Instant)>>);

impl MismatchLog {
    fn note_protocol(&self, ipad_min: u8, ipad_max: u8) {
        if let Some(side) = older_side(ipad_min, ipad_max) {
            *self.0.lock() = Some((side, Instant::now()));
        }
    }

    fn note_legacy(&self) {
        *self.0.lock() = Some((OlderSide::Ipad, Instant::now()));
    }

    fn recent(&self) -> Option<OlderSide> {
        let mut mismatch = self.0.lock();
        if mismatch.is_some_and(|(_, at)| at.elapsed() > MISMATCH_SHOWN) {
            *mismatch = None;
        }
        mismatch.map(|(side, _)| side)
    }

    fn clear(&self) {
        *self.0.lock() = None;
    }
}

static MISMATCH: MismatchLog = MismatchLog(parking_lot::const_mutex(None));

/// Called by the session when a HELLO2 asks for protocols we don't speak.
pub fn note_ipad_protocol(ipad_min: u8, ipad_max: u8) {
    MISMATCH.note_protocol(ipad_min, ipad_max);
}

/// Called when a v0.1.x iPad app sends its legacy hello.
pub fn note_legacy_ipad() {
    MISMATCH.note_legacy();
}

pub fn recent_mismatch() -> Option<OlderSide> {
    MISMATCH.recent()
}

pub fn clear_mismatch() {
    MISMATCH.clear();
}

/// The button on the PC's mismatch banner: install the update once one is
/// found, look for it until then.
pub fn mismatch_button(update_found: bool) -> &'static str {
    if update_found {
        "Update now"
    } else {
        "Check for updates"
    }
}

/// Title and body for the PC's mismatch banner. The body names the button
/// [`mismatch_button`] shows.
pub fn mismatch_text(side: OlderSide, update_found: bool) -> (&'static str, &'static str) {
    match side {
        OlderSide::Pc if update_found => (
            "Update this PC to connect your iPad",
            "The iPad app is newer than EternalMonitor on this PC, so they can't connect. Click Update now.",
        ),
        OlderSide::Pc => (
            "Update this PC to connect your iPad",
            "The iPad app is newer than EternalMonitor on this PC, so they can't connect. Click Check for updates, then Update now.",
        ),
        OlderSide::Ipad => (
            "Update the iPad app to connect",
            "The iPad app is older than EternalMonitor on this PC, so they can't connect. On the iPad, open TestFlight and tap Update.",
        ),
    }
}

// ── The worker and the GUI handle ───────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn state_path() -> Option<PathBuf> {
    crate::settings::app_data_dir().map(|dir| dir.join("update-state.json"))
}

/// Installers go under local (not roaming) app data.
fn download_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("EternalMonitor")
        .join("updates")
}

fn load(path: Option<&Path>) -> Saved {
    path.and_then(|path| std::fs::read(path).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn save(path: Option<&Path>, saved: &Saved) {
    let Some(path) = path else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_vec_pretty(saved) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

/// Applies `change` and writes the file only if the saved part changed: the
/// GUI calls in every frame.
fn changing<T>(
    state: &Mutex<State>,
    path: Option<&Path>,
    change: impl FnOnce(&mut State) -> T,
) -> T {
    let mut state = state.lock();
    let before = state.saved.clone();
    let result = change(&mut state);
    if state.saved != before {
        save(path, &state.saved);
    }
    result
}

/// Debug builds only: `ETERNAL_UPDATE_HANDOFF_PAUSE_MS=before,after` widens
/// the handoff so end-to-end tests can connect an iPad just before or just
/// after it starts. Release builds never pause.
fn handoff_pause() -> (Duration, Duration) {
    if !cfg!(debug_assertions) {
        return (Duration::ZERO, Duration::ZERO);
    }
    let value = std::env::var("ETERNAL_UPDATE_HANDOFF_PAUSE_MS").unwrap_or_default();
    let mut parts = value
        .split(',')
        .map(|part| part.trim().parse::<u64>().unwrap_or(0));
    let before = parts.next().unwrap_or(0).min(60_000);
    let after = parts.next().unwrap_or(0).min(60_000);
    (Duration::from_millis(before), Duration::from_millis(after))
}

/// Called when something changed; `Some(version)` asks for a notification.
pub type Wake = Arc<dyn Fn(Option<&str>) + Send + Sync>;

/// The streaming session, shared with transport. The update handoff is
/// decided under its lock so a client can't start streaming in between.
pub type SharedSession = Arc<Mutex<crate::transport::session::Session>>;

pub struct Updater {
    state: Arc<Mutex<State>>,
    path: Option<PathBuf>,
    tx: mpsc::Sender<Command>,
}

impl Updater {
    pub fn start(auto: bool, session: SharedSession, wake: Wake) -> Self {
        let path = state_path();
        let saved = load(path.as_deref());
        let state = Arc::new(Mutex::new(State::new(
            env!("CARGO_PKG_VERSION"),
            auto,
            saved,
        )));
        save(path.as_deref(), &state.lock().saved);
        let (tx, rx) = mpsc::channel();
        if state.lock().due(now_secs(), STARTUP_INTERVAL) {
            let _ = tx.send(Command::Check { manual: false });
        }
        let worker = Worker {
            state: state.clone(),
            path: path.clone(),
            endpoints: Endpoints::from_env(),
            session,
            wake,
        };
        if let Err(error) = std::thread::Builder::new()
            .name("updates".into())
            .spawn(move || worker.run(rx))
        {
            tracing::warn!(%error, "Could not start the update worker");
        }
        Self { state, path, tx }
    }

    fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        changing(&self.state, self.path.as_deref(), change)
    }

    fn send(&self, command: Option<Command>) {
        if let Some(command) = command {
            let _ = self.tx.send(command);
        }
    }

    pub fn notice(&self) -> Option<Notice> {
        self.state.lock().notice(now_secs())
    }

    pub fn check_status(&self) -> CheckStatus {
        self.state.lock().check
    }

    /// A newer version, unless automatic checks are off and nobody asked.
    pub fn available_version(&self) -> Option<String> {
        let state = self.state.lock();
        state
            .available()
            .filter(|_| state.auto || state.manual_check)
            .map(|r| r.version.clone())
    }

    pub fn set_auto(&self, auto: bool) {
        let due = self.with(|state| {
            state.auto = auto;
            state.due(now_secs(), CHECK_INTERVAL)
        });
        if due {
            self.send(Some(Command::Check { manual: false }));
        }
    }

    pub fn check_now(&self) {
        let checking = self.with(|state| {
            let busy = state.check == CheckStatus::Checking;
            state.begin_check(true);
            busy
        });
        if !checking {
            self.send(Some(Command::Check { manual: true }));
        }
    }

    pub fn update_now(&self) {
        let command = self.with(|state| state.update_now(state.streaming));
        self.send(command);
    }

    pub fn force_now(&self) {
        let command = self.with(State::force_now);
        self.send(command);
    }

    pub fn cancel(&self) {
        self.with(State::cancel);
    }

    pub fn later(&self) {
        self.with(|state| state.later(now_secs()));
    }

    /// Advances a queued update and hands a ready installer to the worker,
    /// which starts the helper. `true` means the helper has taken over: close
    /// the app now. Until then the app stays open, whatever the helper does.
    pub fn tick(&self, streaming: bool) -> bool {
        let command = self.with(|state| state.tick(streaming, Instant::now()));
        self.send(command);
        let ready = self.with(|state| state.take_ready(streaming));
        self.send(ready.map(|(path, release)| Command::Install(path, release)));
        self.with(|state| std::mem::take(&mut state.close))
    }
}

struct Worker {
    state: Arc<Mutex<State>>,
    path: Option<PathBuf>,
    endpoints: Endpoints,
    session: SharedSession,
    wake: Wake,
}

impl Worker {
    fn run(self, rx: mpsc::Receiver<Command>) {
        loop {
            let command = match rx.recv_timeout(Duration::from_secs(30 * 60)) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !self.state.lock().due(now_secs(), CHECK_INTERVAL) {
                        continue;
                    }
                    Command::Check { manual: false }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            match command {
                Command::Check { manual } => {
                    self.state.lock().begin_check(manual);
                    (self.wake)(None);
                    let result = fetch_manifest(&self.endpoints);
                    let notify =
                        self.update(|state| state.finish_check(now_secs(), manual, result));
                    (self.wake)(notify.as_deref());
                }
                Command::Download(release) => {
                    let state = self.state.clone();
                    let wake = self.wake.clone();
                    let mut last = Instant::now();
                    let result = download(
                        &self.endpoints,
                        &release,
                        &download_dir(),
                        |received, total| {
                            state.lock().progress(received, total);
                            if last.elapsed() > Duration::from_millis(100) {
                                last = Instant::now();
                                wake(None);
                            }
                        },
                    );
                    self.update(|state| state.download_finished(result));
                    (self.wake)(None);
                }
                Command::Install(path, release) => {
                    self.install(&path, &release);
                    (self.wake)(None);
                }
            }
        }
    }

    fn update<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        changing(&self.state, self.path.as_deref(), change)
    }

    /// Hands over to the helper. Under the session lock: either a client is
    /// already streaming (go back to waiting, unless the user chose Stop and
    /// update, which ends that stream), or new clients are refused until the
    /// host closes or the handoff fails.
    fn install(&self, path: &Path, release: &Release) {
        let (before, after) = handoff_pause();
        std::thread::sleep(before);
        // Read on its own: the update state is never locked inside the session.
        let stop_stream = self.state.lock().force;
        let started = {
            let mut session = self.session.lock();
            session.begin_update(stop_stream)
        };
        std::thread::sleep(if started { after } else { Duration::ZERO });
        if !started {
            tracing::info!(
                "An iPad connected before the update started; waiting for the stream to stop"
            );
            self.update(|state| state.requeue(path.to_path_buf()));
            return;
        }
        match start_install(path, release) {
            Ok(()) => {
                tracing::info!(version = %release.version, "Update helper took over; closing");
                self.update(State::install_started);
            }
            Err(error) => {
                self.session.lock().end_update();
                self.update(|state| state.install_failed(&error));
            }
        }
    }
}

#[cfg(test)]
mod tests;
