//! Runs the real helper binary against stand-in installers and apps.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use eternal_updater::{sha256_hex, HelperArgs, READY};

struct Case {
    dir: PathBuf,
    args: HelperArgs,
    host: Child,
}

impl Drop for Case {
    fn drop(&mut self) {
        close(&mut self.host);
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn case(name: &str, installer: &str) -> Case {
    let dir = std::env::temp_dir().join(format!("eternal-helper-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let setup = dir.join("setup.sh");
    script(&setup, installer);
    let app = dir.join("app.sh");
    script(
        &app,
        &format!("echo relaunched > '{}'", dir.join("relaunched").display()),
    );
    let sha256 = sha256_hex(&mut std::fs::File::open(&setup).unwrap()).unwrap();
    let host = open_host();
    Case {
        args: HelperArgs {
            installer: setup,
            sha256,
            app,
            app_args: Vec::new(),
            host_pid: host.id(),
            log: dir.join("update.log"),
        },
        dir,
        host,
    }
}

/// A stand-in host that stays open until the test closes it, as the real
/// host does once the helper says it is ready. A host that closed on a timer
/// could close before a slow machine had even started the helper.
fn open_host() -> Child {
    Command::new("sleep").arg("600").spawn().unwrap()
}

fn close(host: &mut Child) {
    let _ = host.kill();
    let _ = host.wait();
}

/// How long the helper may take to say whether it is ready, and then to
/// exit. Only a hung helper waits this long.
const HELPER_WAIT: Duration = Duration::from_secs(60);

/// Starts the helper, returns its first line and exit code. Once the helper
/// says it is ready, the host closes. Anything but ready or an error fails
/// the test at once, and so does a helper still running after
/// [`HELPER_WAIT`]. The helper is killed first and the case's drop closes
/// the host, so neither waits out the host's ten minutes.
fn run(case: &mut Case) -> (String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_eternal-updater"))
        .args(case.args.to_args())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (line_tx, line_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = line_tx.send(line);
    });
    let Ok(line) = line_rx.recv_timeout(HELPER_WAIT) else {
        close(&mut child);
        panic!("the helper said nothing within {HELPER_WAIT:?}");
    };
    let line = line.trim().to_string();
    if line == READY {
        close(&mut case.host);
    } else if !line.starts_with("error:") {
        close(&mut child);
        panic!("the helper said neither ready nor an error: {line:?}");
    }
    let deadline = Instant::now() + HELPER_WAIT;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return (line, status.code().unwrap());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    close(&mut child);
    panic!("the helper did not exit within {HELPER_WAIT:?} of saying {line:?}");
}

/// Whether `path` appears within `wait`. The relaunched app writes it on its
/// own after the helper exits.
fn appears(path: &Path, wait: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < wait {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn verified_setup_runs_silently_then_the_app_starts() {
    let mut c = case("ok", "echo \"$@\" > \"$(dirname \"$0\")/setup-args\"");
    assert_eq!(run(&mut c), (READY.to_string(), 0));
    let args = std::fs::read_to_string(c.dir.join("setup-args")).unwrap();
    for flag in eternal_updater::SETUP_FLAGS {
        assert!(args.contains(flag), "{flag} in {args}");
    }
    assert!(appears(&c.dir.join("relaunched"), Duration::from_secs(60)));
    let log = std::fs::read_to_string(&c.args.log).unwrap();
    assert!(log.contains("Setup exit code 0"), "{log}");
}

#[test]
fn a_changed_installer_is_refused_before_ready() {
    let mut c = case("swapped", "touch \"$(dirname \"$0\")/ran\"");
    std::fs::write(&c.args.installer, "#!/bin/sh\ntouch ran-evil\n").unwrap();
    let (line, code) = run(&mut c);
    assert!(
        line.starts_with("error:") && line.contains("SHA-256"),
        "{line}"
    );
    assert_eq!(code, 3);
    assert!(!c.dir.join("ran").exists());
    // The host is still open, so the helper must not start a second copy.
    assert!(!appears(&c.dir.join("relaunched"), Duration::from_secs(5)));
}

#[test]
fn a_setup_that_cannot_start_still_brings_the_app_back() {
    let mut c = case("unrunnable", "exit 0");
    // Verified bytes, but not executable: spawning Setup fails after ready.
    std::fs::set_permissions(&c.args.installer, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(run(&mut c), (READY.to_string(), 0));
    assert!(appears(&c.dir.join("relaunched"), Duration::from_secs(60)));
    let log = std::fs::read_to_string(&c.args.log).unwrap();
    assert!(log.contains("Setup did not run"), "{log}");
}

#[test]
fn a_host_that_is_already_gone_is_refused_before_ready() {
    let mut c = case("gone", "touch \"$(dirname \"$0\")/ran\"");
    c.args.host_pid = 999_999;
    let (line, code) = run(&mut c);
    assert!(line.starts_with("error:"), "{line}");
    assert_eq!(code, 3);
    assert!(!c.dir.join("ran").exists());
}

#[test]
fn a_failing_setup_still_brings_the_app_back() {
    let mut c = case("fails", "exit 7");
    assert_eq!(run(&mut c), (READY.to_string(), 0));
    assert!(appears(&c.dir.join("relaunched"), Duration::from_secs(60)));
    assert!(std::fs::read_to_string(&c.args.log)
        .unwrap()
        .contains("Setup exit code 7"));
}
