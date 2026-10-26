//! Runs the real helper binary against stand-in installers and apps.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use eternal_updater::{sha256_hex, HelperArgs, READY};

struct Case {
    dir: PathBuf,
    args: HelperArgs,
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
    Case {
        args: HelperArgs {
            installer: setup,
            sha256,
            app,
            app_args: Vec::new(),
            // A host that closes on its own shortly after the helper starts.
            host_pid: closing_host(),
            log: dir.join("update.log"),
        },
        dir,
    }
}

/// A stand-in host that exits a moment after the helper opens it.
fn closing_host() -> u32 {
    let child = Command::new("sleep").arg("0.5").spawn().unwrap();
    let pid = child.id();
    // Reap it in the background so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    pid
}

/// Starts the helper, returns its first line and exit code.
fn run(case: &Case) -> (String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_eternal-updater"))
        .args(case.args.to_args())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let code = child.wait().unwrap().code().unwrap();
    (line.trim().to_string(), code)
}

fn wait_for(path: &Path) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn verified_setup_runs_silently_then_the_app_starts() {
    let c = case("ok", "echo \"$@\" > \"$(dirname \"$0\")/setup-args\"");
    assert_eq!(run(&c), (READY.to_string(), 0));
    let args = std::fs::read_to_string(c.dir.join("setup-args")).unwrap();
    for flag in eternal_updater::SETUP_FLAGS {
        assert!(args.contains(flag), "{flag} in {args}");
    }
    assert!(wait_for(&c.dir.join("relaunched")));
    let log = std::fs::read_to_string(&c.args.log).unwrap();
    assert!(log.contains("Setup exit code 0"), "{log}");
}

#[test]
fn a_changed_installer_is_refused_before_ready() {
    let c = case("swapped", "touch \"$(dirname \"$0\")/ran\"");
    std::fs::write(&c.args.installer, "#!/bin/sh\ntouch ran-evil\n").unwrap();
    let (line, code) = run(&c);
    assert!(
        line.starts_with("error:") && line.contains("SHA-256"),
        "{line}"
    );
    assert_eq!(code, 3);
    assert!(!c.dir.join("ran").exists());
    // The host is still open, so the helper must not start a second copy.
    assert!(!wait_for(&c.dir.join("relaunched")));
}

#[test]
fn a_setup_that_cannot_start_still_brings_the_app_back() {
    let c = case("unrunnable", "exit 0");
    // Verified bytes, but not executable: spawning Setup fails after ready.
    std::fs::set_permissions(&c.args.installer, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(run(&c), (READY.to_string(), 0));
    assert!(wait_for(&c.dir.join("relaunched")));
    let log = std::fs::read_to_string(&c.args.log).unwrap();
    assert!(log.contains("Setup did not run"), "{log}");
}

#[test]
fn a_host_that_is_already_gone_is_refused_before_ready() {
    let mut c = case("gone", "touch \"$(dirname \"$0\")/ran\"");
    c.args.host_pid = 999_999;
    let (line, code) = run(&c);
    assert!(line.starts_with("error:"), "{line}");
    assert_eq!(code, 3);
    assert!(!c.dir.join("ran").exists());
}

#[test]
fn a_failing_setup_still_brings_the_app_back() {
    let c = case("fails", "exit 7");
    assert_eq!(run(&c), (READY.to_string(), 0));
    assert!(wait_for(&c.dir.join("relaunched")));
    assert!(std::fs::read_to_string(&c.args.log)
        .unwrap()
        .contains("Setup exit code 7"));
}
