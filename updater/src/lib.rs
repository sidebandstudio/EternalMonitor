//! EternalMonitor-updater.exe, the native helper that installs an update.
//!
//! The host downloads and verifies EternalMonitor-Setup.exe into a fresh
//! update folder, copies this helper there (Setup replaces the install folder),
//! starts it suspended, checks it is the copy it verified, and waits for
//! [`READY`]. Before saying it, the helper pins the update folder (no links
//! anywhere on its path), opens the installer so no other process can write,
//! rename or delete it, checks its SHA-256 from that open handle, and opens
//! the host process so it can wait for that exact process to exit. It keeps the
//! installer open until Setup has finished and starts Setup suspended, letting
//! it run only if Windows mapped that very file. After [`READY`] the host
//! closes; from then on every path, failures included, ends with the
//! installed app running again once that host has exited.
//!
//! Threat model (SECURITY.md): network attackers are fully covered by HTTPS,
//! the host allowlist and pinned hashes. Against malware already running as
//! the same user this is defense in depth, not a promise to win every race:
//! Microsoft does not treat UAC as a security boundary, and such malware can
//! ask for elevation itself.
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod sha256;
#[cfg(windows)]
pub mod win;

/// The line the helper prints once the installer is locked and verified.
pub const READY: &str = "ready";
/// Inno Setup flags for an in-place upgrade: a progress window and the UAC
/// prompt, no wizard pages and no message boxes. Setup does not relaunch the
/// app in silent mode (`skipifsilent`); the helper does that.
pub const SETUP_FLAGS: &[&str] = &[
    "/SP-",
    "/SILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/CLOSEAPPLICATIONS",
    "/NORESTARTAPPLICATIONS",
];
/// How long the helper waits for the host, and any other process running the
/// installed app, to close before running Setup.
const HOST_EXIT_WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperArgs {
    pub installer: PathBuf,
    pub sha256: String,
    /// The installed host, started again at the end.
    pub app: PathBuf,
    /// The host's own arguments (its listen port), passed on to that start.
    pub app_args: Vec<OsString>,
    pub host_pid: u32,
    pub log: PathBuf,
}

impl HelperArgs {
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "--installer".into(),
            self.installer.clone().into(),
            "--sha256".into(),
            self.sha256.clone().into(),
            "--app".into(),
            self.app.clone().into(),
            "--host-pid".into(),
            self.host_pid.to_string().into(),
            "--log".into(),
            self.log.clone().into(),
        ];
        for arg in &self.app_args {
            args.push("--app-arg".into());
            args.push(arg.clone());
        }
        args
    }

    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let (mut installer, mut sha256, mut app, mut host_pid, mut log) =
            (None, None, None, None, None);
        let mut app_args = Vec::new();
        while let Some(name) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("{name:?} needs a value"))?;
            match name.to_str() {
                Some("--installer") => installer = Some(PathBuf::from(value)),
                Some("--sha256") => sha256 = value.into_string().ok(),
                Some("--app") => app = Some(PathBuf::from(value)),
                Some("--app-arg") => app_args.push(value),
                Some("--host-pid") => host_pid = value.to_str().and_then(|v| v.parse().ok()),
                Some("--log") => log = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown argument {name:?}")),
            }
        }
        let sha256 = sha256
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or("--sha256 must be 64 hex digits")?
            .to_ascii_lowercase();
        Ok(Self {
            installer: installer.ok_or("--installer is required")?,
            sha256,
            app: app.ok_or("--app is required")?,
            app_args,
            host_pid: host_pid.ok_or("--host-pid is required")?,
            log: log.ok_or("--log is required")?,
        })
    }
}

/// Opens `path` for reading so that, while the handle lives, no other
/// process can open it for writing or rename or delete it (Windows sharing
/// rules). Reading and running it stay allowed.
pub fn open_locked(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        options.share_mode(FILE_SHARE_READ);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
    }
    Ok(file)
}

/// SHA-256 of the whole file, read through `file` itself.
pub fn sha256_hex(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    sha256::of_reader(&mut *file)
}

/// The locked installer, or why it may not run.
pub fn open_verified(path: &Path, sha256: &str) -> Result<File, String> {
    let mut file = open_locked(path).map_err(|error| format!("cannot lock installer: {error}"))?;
    let actual =
        sha256_hex(&mut file).map_err(|error| format!("cannot read installer: {error}"))?;
    if actual != sha256 {
        return Err(format!("installer SHA-256 is {actual}, expected {sha256}"));
    }
    Ok(file)
}

/// What waiting for the host to close found.
#[derive(Debug, PartialEq, Eq)]
pub enum HostExit {
    Closed,
    StillRunning,
}

/// Runs the helper. Returns its exit code; `out` is the pipe to the host.
pub fn run(args: HelperArgs, out: &mut dyn Write) -> i32 {
    run_with(args, out, HOST_EXIT_WAIT)
}

/// [`run`], with how long Setup waits for the host to close.
fn run_with(args: HelperArgs, out: &mut dyn Write, host_exit_wait: Duration) -> i32 {
    let mut log = Log::open(&args.log);
    let prepared = prepare(&args);
    let (installer, host) = match prepared {
        Ok(ready) => ready,
        Err(error) => {
            // The host is still open and shows the failure; nothing to relaunch.
            log.note(&format!("refused: {error}"));
            let _ = writeln!(out, "error: {error}");
            return 3;
        }
    };
    log.note("installer locked and verified");
    if writeln!(out, "{READY}").and_then(|()| out.flush()).is_err() {
        log.note("host is gone before the update started; not installing");
        return 4;
    }
    let installed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let deadline = Instant::now() + host_exit_wait;
        match host.wait(Some(host_exit_wait)) {
            HostExit::Closed => log.note("EternalMonitor closed"),
            HostExit::StillRunning => {
                // Setup would close it mid-use or fail on its files. Leave
                // the running app alone; the start after it closes reports
                // the update did not finish.
                return Err("EternalMonitor did not close; not installing".to_string());
            }
        }
        // The host's failure reporter is a copy of the app without a window
        // and can outlive the host while it turns the virtual display off.
        // Setup's /CLOSEAPPLICATIONS does not close such a process and would
        // then fail on the locked executable. Wait for every copy, within
        // the same budget; one still running means not installing.
        match others_closed(&args.app, deadline) {
            Ok(()) => {}
            Err(OthersRunning::Unknown(error)) => log.note(&format!(
                "cannot list other EternalMonitor processes: {error}"
            )),
            Err(OthersRunning::Running) => {
                return Err(
                    "another EternalMonitor process did not close; not installing".to_string(),
                )
            }
        }
        run_setup(&args, &installer.file)
    }));
    match installed {
        Ok(Ok(code)) => log.note(&format!("Setup exit code {code}")),
        Ok(Err(error)) => log.note(&format!("Setup did not run: {error}")),
        Err(_) => log.note("the helper failed while installing"),
    }
    drop(installer);
    // Whatever happened, the user gets EternalMonitor back, but only once the
    // old host has exited. A start while it still runs would find it, show
    // its window and quit, and then nothing would be running when it closes.
    // While it runs the user has that app, so there is no deadline here.
    //
    // The single-instance mutex is a named object that lasts until its last
    // handle closes, not until its creator exits. The host's handle comes from
    // CreateMutexW with no security attributes, so it is not inheritable, and
    // it is never owned or duplicated: its children cannot keep the name
    // alive, and Windows closes it before the process counts as exited. The
    // only other handles are a second start's own check, which closes it at
    // once, so the start below finds the name free.
    if host.wait(None) == HostExit::StillRunning {
        log.note("cannot wait for EternalMonitor to close; not starting it again");
        return 6;
    }
    match relaunch(&args.app, &args.app_args) {
        Ok(()) => {
            log.note("EternalMonitor started");
            0
        }
        Err(error) => {
            log.note(&format!("could not start EternalMonitor: {error}"));
            5
        }
    }
}

/// The verified installer, kept open with the pinned folder that holds it.
struct Installer {
    file: File,
    #[cfg(windows)]
    _folder: std::os::windows::io::OwnedHandle,
}

/// Everything the helper checks before it may say [`READY`].
fn prepare(args: &HelperArgs) -> Result<(Installer, Host), String> {
    #[cfg(windows)]
    let folder = {
        let dir = args
            .installer
            .parent()
            .ok_or("the installer has no folder")?;
        win::pin_dir(dir)?
    };
    let file = open_verified(&args.installer, &args.sha256)?;
    let host = Host::open(args.host_pid)
        .map_err(|error| format!("cannot watch EternalMonitor: {error}"))?;
    Ok((
        Installer {
            file,
            #[cfg(windows)]
            _folder: folder,
        },
        host,
    ))
}

/// The host process, opened while it is still waiting for [`READY`].
struct Host {
    #[cfg(windows)]
    process: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    pid: u32,
}

impl Host {
    #[cfg(windows)]
    fn open(pid: u32) -> io::Result<Self> {
        Ok(Self {
            process: win::open_process(pid)?,
        })
    }

    #[cfg(not(windows))]
    fn open(pid: u32) -> io::Result<Self> {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { pid })
    }

    /// `None` waits for as long as it takes.
    #[cfg(windows)]
    fn wait(&self, timeout: Option<Duration>) -> HostExit {
        match win::wait(&self.process, timeout) {
            Some(_) => HostExit::Closed,
            None => HostExit::StillRunning,
        }
    }

    #[cfg(not(windows))]
    fn wait(&self, timeout: Option<Duration>) -> HostExit {
        let start = std::time::Instant::now();
        while timeout.is_none_or(|timeout| start.elapsed() < timeout) {
            // A zombie child of ours also counts as closed.
            let gone = unsafe { libc::kill(self.pid as libc::pid_t, 0) } != 0
                || unsafe {
                    libc::waitpid(self.pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG)
                } == self.pid as libc::pid_t;
            if gone {
                return HostExit::Closed;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        HostExit::StillRunning
    }
}

/// Why [`others_closed`] could not report that every copy closed.
#[derive(Debug)]
enum OthersRunning {
    /// A process running the app was still there at the deadline.
    Running,
    /// The processes could not be listed; Setup runs as it did before.
    Unknown(String),
}

/// Waits until `deadline` for every process running the installed `app` to
/// exit. This helper is never one of them: it runs from the update folder.
fn others_closed(app: &Path, deadline: Instant) -> Result<(), OthersRunning> {
    #[cfg(windows)]
    {
        let running =
            win::processes_running(app).map_err(|e| OthersRunning::Unknown(e.to_string()))?;
        for process in &running {
            let left = deadline.saturating_duration_since(Instant::now());
            if win::wait(process, Some(left)).is_none() {
                return Err(OthersRunning::Running);
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        loop {
            let running = unix_processes_running(app).map_err(OthersRunning::Unknown)?;
            if running.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(OthersRunning::Running);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Processes whose command line runs `app`, for the Unix test builds: as the
/// program, or as the script an interpreter runs.
#[cfg(not(windows))]
fn unix_processes_running(app: &Path) -> Result<Vec<u32>, String> {
    let listing = Command::new("ps")
        .args(["-axo", "pid=,stat=,args="])
        .output()
        .map_err(|e| e.to_string())?;
    let app = app.display().to_string();
    let me = std::process::id();
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: u32 = fields.next()?.parse().ok()?;
            let stat = fields.next()?;
            let args: Vec<&str> = fields.take(2).collect();
            let runs = args.first() == Some(&app.as_str()) || args.get(1) == Some(&app.as_str());
            (runs && pid != me && !stat.starts_with('Z')).then_some(pid)
        })
        .collect())
}

/// Setup's command line: the program, the silent flags and its log.
pub fn setup_command_line(installer: &Path, log: &Path) -> String {
    let mut line = format!("\"{}\"", installer.display());
    for flag in SETUP_FLAGS {
        line.push(' ');
        line.push_str(flag);
    }
    line.push_str(&format!(
        " /LOG=\"{}\"",
        log.with_file_name("setup.log").display()
    ));
    line
}

#[cfg(windows)]
fn run_setup(args: &HelperArgs, installer: &File) -> Result<i32, String> {
    let command = setup_command_line(&args.installer, &args.log);
    let process = win::start_verified(&args.installer, &command, installer, 0, None)?;
    win::wait(&process, None)
        .map(|code| code as i32)
        .ok_or_else(|| "cannot wait for Setup".to_string())
}

#[cfg(not(windows))]
fn run_setup(args: &HelperArgs, _installer: &File) -> Result<i32, String> {
    let status = Command::new(&args.installer)
        .args(SETUP_FLAGS)
        .arg(format!(
            "/LOG={}",
            args.log.with_file_name("setup.log").display()
        ))
        .status()
        .map_err(|error| format!("cannot start Setup: {error}"))?;
    Ok(status.code().unwrap_or(-1))
}

/// The app's command line: its path, then `args` quoted the way the CRT
/// splits them.
pub fn app_command_line(app: &Path, args: &[OsString]) -> String {
    let mut line = format!("\"{}\"", app.display());
    for arg in args {
        line.push(' ');
        line.push_str(&quote_arg(&arg.to_string_lossy()));
    }
    line
}

/// Quotes one argument for the Windows CRT's command-line parser.
pub fn quote_arg(arg: &str) -> String {
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

/// Starts the installed app again with the host's arguments, passing on none
/// of this helper's handles.
#[cfg(windows)]
pub fn relaunch(app: &Path, args: &[OsString]) -> io::Result<()> {
    win::start(app, &app_command_line(app, args))
}

/// Starts the installed app again with the host's arguments.
#[cfg(not(windows))]
pub fn relaunch(app: &Path, args: &[OsString]) -> io::Result<()> {
    let mut command = Command::new(app);
    command.args(args);
    if let Some(dir) = app.parent() {
        command.current_dir(dir);
    }
    command.spawn().map(drop)
}

/// Lines in update.log next to the installer, for Copy logs and support.
struct Log(Option<File>);

impl Log {
    fn open(path: &Path) -> Self {
        Self(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok(),
        )
    }

    fn note(&mut self, text: &str) {
        if let Some(file) = &mut self.0 {
            let at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = writeln!(file, "{at} {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("eternal-updater-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn arguments_round_trip_and_are_checked() {
        let args = HelperArgs {
            installer: PathBuf::from(r"C:\Users\A B\EternalMonitor-Setup-0.3.2.exe"),
            sha256: HELLO_SHA.into(),
            app: PathBuf::from(r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe"),
            app_args: vec!["9877".into(), "--log".into(), "".into()],
            host_pid: 42,
            log: PathBuf::from("update.log"),
        };
        assert_eq!(HelperArgs::parse(args.to_args()), Ok(args.clone()));
        let mut bad = args.to_args();
        bad[3] = "abc".into();
        assert!(HelperArgs::parse(bad).is_err());
        let mut missing = args.to_args();
        missing.truncate(8);
        assert!(HelperArgs::parse(missing).is_err());
        assert!(HelperArgs::parse(vec!["--other".into(), "x".into()]).is_err());
    }

    /// The host takes its listen port as an argument; the restarted app must
    /// get it back, each argument intact.
    #[test]
    fn the_app_restarts_with_the_hosts_arguments() {
        let app = Path::new(r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe");
        assert_eq!(
            app_command_line(app, &[]),
            r#""C:\Program Files\EternalMonitor\EternalMonitor-host.exe""#
        );
        let args = [
            "9877".into(),
            r"C:\Users\A B\port file.txt".into(),
            r#"say "hi""#.into(),
            r"C:\A B\".into(),
            "".into(),
        ];
        assert_eq!(
            app_command_line(app, &args),
            r#""C:\Program Files\EternalMonitor\EternalMonitor-host.exe" 9877 "C:\Users\A B\port file.txt" "say \"hi\"" "C:\A B\\" """#
        );
        assert_eq!(quote_arg(r"C:\no\spaces\"), r"C:\no\spaces\");
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn setup_runs_quoted_with_silent_flags_and_a_log() {
        let dir = Path::new("A B").join("updates").join("0a1b");
        let installer = dir.join("EternalMonitor-Setup-0.3.2.exe");
        let line = setup_command_line(&installer, &dir.join("update.log"));
        let expected = format!(
            "\"{}\" /SP- /SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /NORESTARTAPPLICATIONS /LOG=\"{}\"",
            installer.display(),
            dir.join("setup.log").display()
        );
        assert_eq!(line, expected);
        assert!(
            !line.contains("/VERYSILENT"),
            "people see a progress window"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_host_that_does_not_close_is_reported_not_ignored() {
        let mut stuck = Command::new("sleep").arg("30").spawn().unwrap();
        let host = Host::open(stuck.id()).unwrap();
        assert_eq!(
            host.wait(Some(Duration::from_millis(300))),
            HostExit::StillRunning
        );
        stuck.kill().unwrap();
        stuck.wait().unwrap();
        assert_eq!(
            host.wait(Some(Duration::from_millis(300))),
            HostExit::Closed
        );
        assert!(
            Host::open(999_999).is_err(),
            "a missing host is an error, not 'closed'"
        );
    }

    /// A stand-in host with the single-instance rule: a second start while
    /// one runs only "shows its window" and quits. Each start that gets the
    /// lock appends its PID to `hosts`, each turned-away one to `duplicates`.
    #[cfg(unix)]
    fn instance_script(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("host.sh");
        let d = dir.display();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif mkdir '{d}/instance' 2>/dev/null; then\n  echo $$ >> '{d}/hosts'\n  sleep ${{1:-30}}\n  rmdir '{d}/instance'\nelse\n  echo $$ >> '{d}/duplicates'\nfi\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The old host is still closing when Setup's wait ends. Starting the app
    /// then would find it, hand over and quit, leaving nothing running once it
    /// finishes closing. The helper must start the app only after that.
    #[cfg(unix)]
    #[test]
    fn a_host_that_closes_after_the_wait_leaves_exactly_one_running() {
        let dir = temp("late-host");
        let app = instance_script(&dir);
        let installer = dir.join("setup.sh");
        std::fs::write(
            &installer,
            "#!/bin/sh\ntouch \"$(dirname \"$0\")/setup-ran\"\n",
        )
        .unwrap();
        let sha256 = sha256_hex(&mut File::open(&installer).unwrap()).unwrap();
        let mut old = Command::new(&app).arg("2").spawn().unwrap();
        let hosts = dir.join("hosts");
        let start = std::time::Instant::now();
        while !hosts.exists() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let args = HelperArgs {
            installer,
            sha256,
            app,
            app_args: Vec::new(),
            host_pid: old.id(),
            log: dir.join("update.log"),
        };
        let mut out = Vec::new();
        run_with(args.clone(), &mut out, Duration::from_millis(300));
        assert_eq!(String::from_utf8_lossy(&out).trim(), READY);
        let _ = old.wait();
        // Give a relaunch time to run its script either way.
        std::thread::sleep(Duration::from_millis(500));
        let started = std::fs::read_to_string(&hosts).unwrap_or_default();
        let pids: Vec<i32> = started.lines().map(|l| l.parse().unwrap()).collect();
        let running: Vec<i32> = pids
            .iter()
            .copied()
            .filter(|&pid| unsafe { libc::kill(pid, 0) } == 0)
            .collect();
        for &pid in &running {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        let log = std::fs::read_to_string(&args.log).unwrap();
        assert!(
            !dir.join("duplicates").exists(),
            "the app started while the old host still ran: {log}"
        );
        assert_eq!(
            running.len(),
            1,
            "hosts {pids:?}, running {running:?}: {log}"
        );
        assert!(!dir.join("setup-ran").exists(), "Setup never runs late");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A copy of the app that outlives the host (the host's failure reporter)
    /// and a Setup that records whether that copy was still running.
    #[cfg(unix)]
    fn lingering_case(name: &str) -> (PathBuf, HelperArgs) {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp(name);
        let d = dir.display();
        let app = dir.join("app.sh");
        std::fs::write(
            &app,
            format!(
                "#!/bin/sh
if [ \"$1\" = linger ]; then
  touch '{d}/lingering'
  sleep \"$2\"
  rm '{d}/lingering'
  exit 0
fi
touch '{d}/relaunched'
"
            ),
        )
        .unwrap();
        let installer = dir.join("setup.sh");
        std::fs::write(
            &installer,
            format!(
                "#!/bin/sh
[ -e '{d}/lingering' ] && touch '{d}/setup-saw-copy'
touch '{d}/setup-ran'
"
            ),
        )
        .unwrap();
        for script in [&app, &installer] {
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let sha256 = sha256_hex(&mut File::open(&installer).unwrap()).unwrap();
        let args = HelperArgs {
            installer,
            sha256,
            app,
            app_args: Vec::new(),
            host_pid: 0,
            log: dir.join("update.log"),
        };
        (dir, args)
    }

    /// A stand-in host that closes a second after it starts.
    #[cfg(unix)]
    fn closing_host() -> u32 {
        let host = Command::new("sleep").arg("1").spawn().unwrap();
        let pid = host.id();
        std::thread::spawn(move || {
            let mut host = host;
            let _ = host.wait();
        });
        pid
    }

    #[cfg(unix)]
    fn linger(app: &Path, seconds: &str, dir: &Path) -> std::process::Child {
        let child = Command::new(app).args(["linger", seconds]).spawn().unwrap();
        let start = std::time::Instant::now();
        while !dir.join("lingering").exists() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    #[cfg(unix)]
    #[test]
    fn setup_waits_for_a_copy_of_the_app_that_outlives_the_host() {
        let (dir, mut args) = lingering_case("lingering-copy");
        let mut copy = linger(&args.app, "2.5", &dir);
        args.host_pid = closing_host();
        let mut out = Vec::new();
        run_with(args.clone(), &mut out, Duration::from_secs(10));
        let _ = copy.wait();
        let log = std::fs::read_to_string(&args.log).unwrap();
        assert!(dir.join("setup-ran").exists(), "{log}");
        assert!(
            !dir.join("setup-saw-copy").exists(),
            "Setup ran while a copy of the app still held it: {log}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_of_the_app_that_does_not_close_skips_setup_and_relaunches() {
        let (dir, mut args) = lingering_case("stuck-copy");
        let mut copy = linger(&args.app, "30", &dir);
        args.host_pid = closing_host();
        let mut out = Vec::new();
        let code = run_with(args.clone(), &mut out, Duration::from_millis(2000));
        copy.kill().unwrap();
        let _ = copy.wait();
        let log = std::fs::read_to_string(&args.log).unwrap();
        assert_eq!(code, 0, "{log}");
        assert!(!dir.join("setup-ran").exists(), "{log}");
        assert!(
            log.contains("another EternalMonitor process did not close"),
            "{log}"
        );
        // The user still gets the app back.
        let start = std::time::Instant::now();
        while !dir.join("relaunched").exists() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(dir.join("relaunched").exists(), "{log}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn verification_reads_the_locked_file() {
        let dir = temp("verify");
        let path = dir.join("setup.exe");
        std::fs::write(&path, b"hello").unwrap();
        assert!(open_verified(&path, HELLO_SHA).is_ok());
        assert!(open_verified(&path, &"0".repeat(64))
            .unwrap_err()
            .contains("SHA-256"));
        assert!(open_verified(&dir.join("missing.exe"), HELLO_SHA).is_err());
        assert!(
            open_verified(&dir, HELLO_SHA).is_err(),
            "a directory is not an installer"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The fix for a swap between the last hash check and Setup starting:
    /// while the helper holds the installer, nobody can change it.
    #[cfg(windows)]
    #[test]
    fn a_locked_installer_cannot_be_replaced() {
        let dir = temp("lock");
        let path = dir.join("setup.exe");
        std::fs::write(&path, b"hello").unwrap();
        let locked = open_verified(&path, HELLO_SHA).unwrap();
        assert!(std::fs::write(&path, b"evil!").is_err(), "overwrite");
        assert!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .is_err(),
            "append"
        );
        assert!(
            std::fs::rename(&path, dir.join("moved.exe")).is_err(),
            "rename"
        );
        assert!(std::fs::remove_file(&path).is_err(), "delete");
        assert!(
            std::fs::rename(&dir, dir.with_extension("moved")).is_err(),
            "rename the folder"
        );
        // Reading, which is what Windows needs to run it, still works.
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        drop(locked);
        std::fs::write(&path, b"later").unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
