use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use super::*;

const SHA: &str = "b276602bd54a0eec9459b9f6d91f67669fa533600d47456f30cabb36449cf0a5";

fn manifest(version: &str, installer: &str, sha256: &str) -> Vec<u8> {
    serde_json::json!({
        "schema": 1,
        "windows": { "version": version, "installer": installer, "sha256": sha256 },
        "ipad": { "version": version, "build": 170, "testflight": "https://testflight.apple.com/join/ppc9QfXc" },
        "future_field": true
    })
    .to_string()
    .into_bytes()
}

fn production_installer(version: &str) -> String {
    format!("https://github.com/sidebandstudio/EternalMonitor/releases/download/v{version}/EternalMonitor-Setup.exe")
}

fn release(version: &str) -> Release {
    Release {
        version: version.into(),
        installer: production_installer(version),
        sha256: SHA.into(),
    }
}

#[test]
fn versions_compare_as_semver_and_ignore_previews_build_metadata_and_garbage() {
    assert_eq!(newer_stable("v1.10.0", "1.9.9"), Some("1.10.0".into()));
    assert_eq!(newer_stable("0.3.2", "0.3.1"), Some("0.3.2".into()));
    // A preview build is older than its stable release.
    assert_eq!(newer_stable("v0.3.0", "0.3.0-rc.1"), Some("0.3.0".into()));
    // Build metadata does not make a version newer.
    assert_eq!(newer_stable("0.3.1+ci.9", "0.3.1"), None);
    assert_eq!(
        newer_stable("0.3.2+ci.9", "0.3.1+local"),
        Some("0.3.2".into())
    );
    for candidate in [
        "v0.3.0",
        "v0.2.0",
        "v0.4.0-rc.1",
        "v0.4",
        "0.4.0.1",
        "",
        "https://example.com",
        "garbage",
    ] {
        assert_eq!(newer_stable(candidate, "0.3.0"), None, "{candidate}");
    }
    assert_eq!(newer_stable("0.4.0", "not a version"), None);
}

#[test]
fn manifest_must_name_a_stable_github_installer_with_a_sha256() {
    let endpoints = Endpoints::production();
    let parsed = parse_manifest(
        &manifest("0.3.2", &production_installer("0.3.2"), &SHA.to_uppercase()),
        &endpoints,
    )
    .unwrap();
    assert_eq!(parsed, release("0.3.2"), "hash is normalised to lowercase");

    let bad = [
        manifest("0.3.2-rc.1", &production_installer("0.3.2-rc.1"), SHA),
        manifest("0.3.2+build", &production_installer("0.3.2+build"), SHA),
        manifest("0.3", &production_installer("0.3"), SHA),
        manifest("0.3.2", &production_installer("0.3.1"), SHA),
        manifest("0.3.2", "https://evil.example/EternalMonitor-Setup.exe", SHA),
        manifest(
            "0.3.2",
            "http://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.2/EternalMonitor-Setup.exe",
            SHA,
        ),
        manifest("0.3.2", &production_installer("0.3.2"), ""),
        manifest("0.3.2", &production_installer("0.3.2"), &SHA[..63]),
        manifest("0.3.2", &production_installer("0.3.2"), &SHA.replace('b', "g")),
        br#"{"schema":2,"windows":{"version":"0.3.2","installer":"x","sha256":"y"}}"#.to_vec(),
        br#"{"schema":1,"ipad":{"version":"0.3.2","build":170}}"#.to_vec(),
        b"<html>not json</html>".to_vec(),
    ];
    for data in bad {
        assert!(
            matches!(
                parse_manifest(&data, &endpoints),
                Err(UpdateError::BadManifest(_))
            ),
            "{}",
            String::from_utf8_lossy(&data)
        );
    }
}

fn github(tag: &str, body: &str, url: &str, size: u64, extra: serde_json::Value) -> Vec<u8> {
    let mut asset = serde_json::json!({
        "name": "EternalMonitor-Setup.exe", "size": size, "browser_download_url": url
    });
    if let Some(digest) = extra.get("digest") {
        asset["digest"] = digest.clone();
    }
    serde_json::json!({
        "tag_name": tag,
        "draft": extra.get("draft").cloned().unwrap_or(false.into()),
        "prerelease": extra.get("prerelease").cloned().unwrap_or(false.into()),
        "body": body,
        "assets": [asset],
    })
    .to_string()
    .into_bytes()
}

fn body(sha: &str) -> String {
    format!("EternalMonitor v0.3.2\n\n**EternalMonitor-Setup.exe SHA-256:**\n```\n{sha}\n```")
}

#[test]
fn github_release_must_publish_the_same_sha256_and_installer() {
    let release = release("0.3.2");
    let url = release.installer.clone();
    let none = serde_json::json!({});
    assert_eq!(
        check_github_release(
            &github("v0.3.2", &body(&SHA.to_uppercase()), &url, 9, none.clone()),
            &release
        ),
        Ok(9)
    );
    let digest = serde_json::json!({ "digest": format!("sha256:{SHA}") });
    assert_eq!(
        check_github_release(&github("v0.3.2", &body(SHA), &url, 9, digest), &release),
        Ok(9)
    );

    let other = "0".repeat(64);
    let rejected = [
        github("v0.3.2", &body(&other), &url, 9, none.clone()),
        github("v0.3.2", "No hash here", &url, 9, none.clone()),
        github("v0.3.2", &format!("Checksum {SHA}"), &url, 9, none.clone()),
        github("v0.3.1", &body(SHA), &url, 9, none.clone()),
        github(
            "v0.3.2",
            &body(SHA),
            &url,
            9,
            serde_json::json!({ "draft": true }),
        ),
        github(
            "v0.3.2",
            &body(SHA),
            &url,
            9,
            serde_json::json!({ "prerelease": true }),
        ),
        github(
            "v0.3.2",
            &body(SHA),
            "https://github.com/x/y/z.exe",
            9,
            none.clone(),
        ),
        github("v0.3.2", &body(SHA), &url, 0, none.clone()),
        github(
            "v0.3.2",
            &body(SHA),
            &url,
            MAX_INSTALLER_BYTES + 1,
            none.clone(),
        ),
        github(
            "v0.3.2",
            &body(SHA),
            &url,
            9,
            serde_json::json!({ "digest": format!("sha256:{other}") }),
        ),
    ];
    for data in rejected {
        assert!(
            matches!(
                check_github_release(&data, &release),
                Err(UpdateError::UnsafeDownload(_))
            ),
            "{}",
            String::from_utf8_lossy(&data)
        );
    }
}

#[test]
fn only_https_on_allowed_hosts_is_downloaded() {
    let endpoints = Endpoints::production();
    for url in [
        "https://github.com/sidebandstudio/EternalMonitor/releases/download/v1/EternalMonitor-Setup.exe",
        "https://objects.githubusercontent.com/x",
        "https://release-assets.githubusercontent.com/x?sig=1",
        "https://GitHub.com/x",
    ] {
        assert!(allowed_url(url, &endpoints), "{url}");
    }
    for url in [
        "http://github.com/x",
        "https://github.com.evil.example/x",
        "https://evil.example/github.com",
        "https://user@github.com/x",
        "ftp://github.com/x",
        "/relative",
        "",
    ] {
        assert!(!allowed_url(url, &endpoints), "{url}");
    }
    let local = Endpoints::local("http://127.0.0.1:8765").unwrap();
    assert!(allowed_url("http://127.0.0.1:8765/x", &local));
    assert!(!allowed_url("http://github.com/x", &local));
    assert_eq!(
        resolve_redirect("https://github.com/a/b", "/c?d=1").as_deref(),
        Some("https://github.com/c?d=1")
    );
    assert_eq!(
        resolve_redirect("https://github.com/a", "//evil.example/x"),
        None
    );
    assert_eq!(resolve_redirect("https://github.com/a", "c"), None);
}

// ── A fake release server ───────────────────────────────────────────────────

type Reply = (u16, Vec<(String, String)>, Vec<u8>);
type Route = Box<dyn Fn(&str, &str) -> Reply + Send>;

/// Serves `route(base, path)` on a loopback port until the test ends.
fn serve(route: Route) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let own = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) <= 2 {
                    break;
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, headers, body) = route(&own, &path);
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    base
}

fn sha_of(data: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
}

/// A fake site and GitHub release for `published`. The installer URL
/// redirects to `redirect` (default: a blob on the same host) which serves
/// `served`.
fn fake_release(published: &[u8], served: Vec<u8>, redirect: Option<&str>) -> (Endpoints, Release) {
    let sha = sha_of(published);
    let size = published.len() as u64;
    let redirect = redirect.map(str::to_string);
    let api_sha = sha.clone();
    let base = serve(Box::new(move |base, path| {
        let installer = format!("{base}/releases/download/v0.3.2/EternalMonitor-Setup.exe");
        if path.ends_with("/releases/tags/v0.3.2") {
            let release = github(
                "v0.3.2",
                &body(&api_sha),
                &installer,
                size,
                serde_json::json!({}),
            );
            return (200, vec![], release);
        }
        if path.ends_with("/EternalMonitor-Setup.exe") {
            let target = redirect.clone().unwrap_or_else(|| "/assets/blob".into());
            return (302, vec![("Location".into(), target)], Vec::new());
        }
        if path == "/assets/blob" {
            return (200, vec![], served.clone());
        }
        (404, vec![], Vec::new())
    }));
    let endpoints = Endpoints::local(&base).unwrap();
    let release = Release {
        version: "0.3.2".into(),
        installer: format!("{base}/releases/download/v0.3.2/EternalMonitor-Setup.exe"),
        sha256: sha,
    };
    (endpoints, release)
}

fn download_case(endpoints: &Endpoints, release: &Release) -> Result<PathBuf, UpdateError> {
    let dir = std::env::temp_dir().join(format!(
        "eternal-update-test-{}-{}",
        std::process::id(),
        &sha_of(release.installer.as_bytes())[..12]
    ));
    let result = download(endpoints, release, &dir, |_, _| {});
    if result.is_err() {
        let leftovers = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "a failed download must leave nothing to run");
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

#[test]
fn download_follows_redirects_and_verifies_the_installer() {
    let payload = b"MZ fake installer".to_vec();
    let (endpoints, release) = fake_release(&payload, payload.clone(), None);
    let mut progress = Vec::new();
    let dir = std::env::temp_dir().join(format!("eternal-update-ok-{}", std::process::id()));
    let path = download(&endpoints, &release, &dir, |received, total| {
        progress.push((received, total))
    })
    .unwrap();
    assert_eq!(progress.last(), Some(&(17, 17)));
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    // Each update gets a fresh, randomly named folder.
    let folder = path.parent().unwrap();
    assert_eq!(folder.parent(), Some(dir.as_path()));
    assert_eq!(folder.file_name().unwrap().len(), 16);
    let again = download(&endpoints, &release, &dir, |_, _| {}).unwrap();
    assert_ne!(again.parent(), path.parent());
    assert!(!path.exists(), "the previous update's folder is removed");
    assert!(eternal_updater::open_verified(&again, &release.sha256).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hash_mismatch_is_refused() {
    let (endpoints, release) =
        fake_release(b"MZ fake installer", b"MZ evil installer".to_vec(), None);
    let error = download_case(&endpoints, &release).unwrap_err();
    assert!(
        matches!(&error, UpdateError::UnsafeDownload(why) if why.contains("SHA-256")),
        "{error}"
    );
}

#[test]
fn larger_than_published_is_refused() {
    let mut bigger = b"MZ fake installer".to_vec();
    bigger.extend_from_slice(&[0; 4096]);
    let (endpoints, release) = fake_release(b"MZ fake installer", bigger, None);
    assert!(matches!(
        download_case(&endpoints, &release),
        Err(UpdateError::UnsafeDownload(_))
    ));
}

#[test]
fn redirect_to_an_unexpected_host_is_refused() {
    let payload = b"MZ fake installer".to_vec();
    let (endpoints, release) = fake_release(
        &payload,
        payload.clone(),
        Some("http://localhost.evil.example/blob"),
    );
    let error = download_case(&endpoints, &release).unwrap_err();
    assert!(
        matches!(&error, UpdateError::UnsafeDownload(why) if why.contains("host not allowed")),
        "{error}"
    );
}

#[test]
fn website_and_github_hashes_must_agree() {
    let payload = b"MZ fake installer".to_vec();
    let (endpoints, mut release) = fake_release(&payload, payload.clone(), None);
    release.sha256 = "1".repeat(64);
    assert!(matches!(
        download_case(&endpoints, &release),
        Err(UpdateError::UnsafeDownload(_))
    ));
}

#[test]
fn offline_check_is_a_network_error() {
    let endpoints = Endpoints::local("http://127.0.0.1:9").unwrap();
    assert!(matches!(
        fetch_manifest(&endpoints),
        Err(UpdateError::Network(_))
    ));
}

#[test]
fn manifest_is_fetched_from_the_configured_site() {
    let base = serve(Box::new(|base, path| match path {
        "/latest.json" => (
            200,
            vec![],
            manifest(
                "0.3.2",
                &format!("{base}/releases/download/v0.3.2/EternalMonitor-Setup.exe"),
                SHA,
            ),
        ),
        _ => (404, vec![], Vec::new()),
    }));
    let endpoints = Endpoints::local(&base).unwrap();
    assert_eq!(fetch_manifest(&endpoints).unwrap().version, "0.3.2");
    let missing = Endpoints {
        manifest: format!("{base}/missing.json"),
        ..endpoints
    };
    assert!(matches!(
        fetch_manifest(&missing),
        Err(UpdateError::Network(_))
    ));
}

// ── State machine ───────────────────────────────────────────────────────────

const DAY: u64 = 24 * 60 * 60;

fn state(saved: Saved) -> State {
    State::new("0.3.1", true, saved)
}

#[test]
fn check_shows_banner_and_notifies_once_per_version() {
    let mut s = state(Saved::default());
    assert!(s.due(1000, STARTUP_INTERVAL));
    assert_eq!(s.notice(1000), None);
    s.begin_check(false);
    assert_eq!(s.check, CheckStatus::Checking);
    assert_eq!(
        s.finish_check(1000, false, Ok(release("0.3.2"))),
        Some("0.3.2".into())
    );
    assert_eq!(s.check, CheckStatus::Available);
    assert_eq!(s.notice(1000), Some(Notice::Available("0.3.2".into())));
    assert!(!s.due(1000 + STARTUP_INTERVAL - 1, STARTUP_INTERVAL));
    assert!(!s.due(1000 + DAY - 1, CHECK_INTERVAL));
    assert!(s.due(1000 + DAY, CHECK_INTERVAL));
    // The same version is announced only once, even after a restart.
    assert_eq!(
        s.finish_check(1000 + DAY, false, Ok(release("0.3.2"))),
        None
    );
    let mut restarted = state(s.saved.clone());
    assert_eq!(
        restarted.finish_check(1000 + 2 * DAY, false, Ok(release("0.3.2"))),
        None
    );
    assert_eq!(
        restarted.finish_check(1000 + 3 * DAY, false, Ok(release("0.3.3"))),
        Some("0.3.3".into())
    );
}

#[test]
fn failed_checks_are_throttled_and_keep_the_last_good_answer() {
    let mut s = state(Saved::default());
    s.finish_check(10, false, Ok(release("0.3.2")));
    s.finish_check(10 + DAY, false, Err(UpdateError::Network("offline".into())));
    assert_eq!(s.check, CheckStatus::Failed);
    assert_eq!(s.notice(10 + DAY), Some(Notice::Available("0.3.2".into())));
    assert!(!s.due(11 + DAY, STARTUP_INTERVAL));
    // The installed version is current: no banner, "up to date".
    s.finish_check(10 + 2 * DAY, false, Ok(release("0.3.1")));
    assert_eq!(s.check, CheckStatus::UpToDate);
    assert_eq!(s.notice(10 + 2 * DAY), None);
}

#[test]
fn automatic_setting_is_respected() {
    let mut s = State::new("0.3.1", false, Saved::default());
    assert!(
        !s.due(1000, STARTUP_INTERVAL),
        "no automatic checks when off"
    );
    assert_eq!(s.finish_check(1000, false, Ok(release("0.3.2"))), None);
    assert_eq!(
        s.notice(1000),
        None,
        "a cached result stays hidden when off"
    );
    // Check now shows the answer even with automatic checks off.
    s.begin_check(true);
    assert_eq!(s.finish_check(1001, true, Ok(release("0.3.2"))), None);
    assert_eq!(s.notice(1001), Some(Notice::Available("0.3.2".into())));
}

#[test]
fn later_is_remembered_per_version_for_a_day() {
    let path =
        std::env::temp_dir().join(format!("eternal-update-state-{}.json", std::process::id()));
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(release("0.3.2")));
    s.later(2000);
    assert_eq!(s.notice(2001), None);
    save(Some(&path), &s.saved);

    // Restarting within the day keeps it hidden and does not notify again.
    let mut restarted = state(load(Some(&path)));
    assert_eq!(restarted.notice(2000 + DAY - 1), None);
    assert_eq!(
        restarted.finish_check(2000 + DAY - 1, false, Ok(release("0.3.2"))),
        None
    );
    // A day later it comes back.
    assert_eq!(
        restarted.notice(2000 + DAY),
        Some(Notice::Available("0.3.2".into()))
    );
    // A newer version is not hidden by "Later" on the old one.
    let mut newer = state(load(Some(&path)));
    assert_eq!(
        newer.finish_check(2100, false, Ok(release("0.3.3"))),
        Some("0.3.3".into())
    );
    assert_eq!(newer.notice(2100), Some(Notice::Available("0.3.3".into())));
    // Check now overrides "Later".
    restarted.later(5000);
    restarted.finish_check(5001, true, Ok(release("0.3.2")));
    restarted.manual_check = true;
    assert_eq!(
        restarted.notice(5001),
        Some(Notice::Available("0.3.2".into()))
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn update_waits_for_the_stream_to_stop() {
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(release("0.3.2")));
    let start = Instant::now();

    assert!(s.update_now(true).is_none());
    assert_eq!(s.notice(1000), Some(Notice::Queued("0.3.2".into())));
    assert!(s.tick(true, start).is_none());
    assert!(s.tick(false, start).is_none());
    // A quick reconnect restarts the wait.
    assert!(s.tick(true, start + Duration::from_secs(2)).is_none());
    assert!(s.tick(false, start + Duration::from_secs(3)).is_none());
    assert!(matches!(
        s.tick(false, start + Duration::from_secs(6)),
        Some(Command::Download(r)) if r.version == "0.3.2"
    ));
    s.progress(50, 200);
    assert_eq!(
        s.notice(1000),
        Some(Notice::Downloading("0.3.2".into(), Some(0.25)))
    );
    s.download_finished(Ok(PathBuf::from("setup.exe")));
    // A stream started during the download: wait again, keep the file.
    assert!(s.take_ready(true).is_none());
    assert_eq!(s.phase, Phase::Queued(Some(PathBuf::from("setup.exe"))));
    assert!(s.tick(false, start + Duration::from_secs(10)).is_none());
    assert!(s.tick(false, start + Duration::from_secs(14)).is_none());
    assert_eq!(s.phase, Phase::Ready(PathBuf::from("setup.exe")));
    let (path, release) = s.take_ready(false).unwrap();
    assert_eq!(
        (path.to_str(), release.version.as_str()),
        (Some("setup.exe"), "0.3.2")
    );
    assert_eq!(s.phase, Phase::Installing);
    assert_eq!(s.saved.pending, Some(("0.3.1".into(), "0.3.2".into())));
}

#[test]
fn update_now_while_streaming_can_be_forced_or_cancelled() {
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(release("0.3.2")));
    s.update_now(true);
    s.cancel();
    assert_eq!(s.notice(1000), Some(Notice::Available("0.3.2".into())));
    s.update_now(true);
    assert!(matches!(s.force_now(), Some(Command::Download(_))));
    s.download_finished(Ok(PathBuf::from("setup.exe")));
    assert!(s.take_ready(true).is_some(), "forced: the stream may end");
}

#[test]
fn failures_show_a_plain_message_and_can_be_retried() {
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(release("0.3.2")));
    assert!(s.update_now(false).is_some());
    assert!(s.update_now(false).is_none(), "one download at a time");
    s.download_finished(Err(UpdateError::UnsafeDownload("SHA-256 mismatch".into())));
    assert_eq!(
        s.notice(1000),
        Some(Notice::Failed(
            "The downloaded update did not pass its safety check, so it was not installed.".into()
        ))
    );
    assert!(s.update_now(false).is_some(), "Try again");
    s.download_finished(Ok(PathBuf::from("setup.exe")));
    s.take_ready(false).unwrap();
    s.install_failed(&UpdateError::Launch("denied".into()));
    assert_eq!(s.saved.pending, None);
    assert_eq!(
        s.notice(1000),
        Some(Notice::Failed("The update could not start.".into()))
    );
    for error in [
        UpdateError::Network("x".into()),
        UpdateError::BadManifest("x".into()),
    ] {
        assert!(error.message().contains("internet connection"));
    }
}

#[test]
fn relaunch_reports_whether_the_update_finished() {
    let pending = |from: &str, to: &str| Saved {
        pending: Some((from.into(), to.into())),
        latest: Some(release(to)),
        ..Saved::default()
    };
    let updated = State::new("0.3.2", true, pending("0.3.1", "0.3.2"));
    assert_eq!(updated.notice(0), Some(Notice::Updated("0.3.2".into())));
    assert_eq!(updated.saved.pending, None);

    let failed = State::new("0.3.1", true, pending("0.3.1", "0.3.2"));
    match failed.notice(0) {
        Some(Notice::Failed(message)) => {
            assert!(
                message.contains("you still have version 0.3.1"),
                "{message}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(failed.saved.pending, None, "reported once");
}

#[test]
fn setup_runs_silently_with_a_progress_window() {
    for flag in [
        "/SILENT",
        "/SUPPRESSMSGBOXES",
        "/NORESTART",
        "/CLOSEAPPLICATIONS",
        "/SP-",
    ] {
        assert!(SETUP_FLAGS.contains(&flag), "{flag}");
    }
    assert!(
        !SETUP_FLAGS.contains(&"/VERYSILENT"),
        "people see a progress window"
    );
}

/// A stand-in helper: a shell script with the given body.
#[cfg(unix)]
fn fake_helper(body: &str) -> std::process::Child {
    std::process::Command::new("/bin/sh")
        .args(["-c", body])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

#[cfg(unix)]
fn wait_ready(mut child: std::process::Child, timeout: Duration) -> Result<(), UpdateError> {
    let stdout = child.stdout.take().unwrap();
    await_ready(stdout, timeout, move || {
        let _ = child.kill();
        let _ = child.wait();
    })
}

#[cfg(unix)]
#[test]
fn the_app_closes_only_after_the_helper_takes_over() {
    let quick = Duration::from_millis(500);
    assert_eq!(
        wait_ready(fake_helper("echo ready; sleep 1"), quick),
        Ok(())
    );
    // Exits without a word, as when a policy blocks the helper.
    assert_eq!(
        wait_ready(fake_helper("exit 1"), quick),
        Err(UpdateError::Launch("the update helper stopped".into()))
    );
    // The installer changed before the helper could lock it.
    assert!(matches!(
        wait_ready(
            fake_helper("echo 'error: installer SHA-256 is 00'; exit 3"),
            quick
        ),
        Err(UpdateError::UnsafeDownload(_))
    ));
    // Silent and still running: it is stopped, and the app stays.
    let stuck = fake_helper("sleep 30");
    let pid = stuck.id() as i32;
    assert!(wait_ready(stuck, quick).is_err());
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "the stuck helper is stopped"
    );
}

#[test]
fn helper_arguments_survive_windows_quoting() {
    let args = eternal_updater::HelperArgs {
        installer: PathBuf::from(
            r"C:\Users\A B\AppData\Local\EternalMonitor\updates\0a\EternalMonitor-Setup-0.3.2.exe",
        ),
        sha256: "0".repeat(64),
        app: PathBuf::from(r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe"),
        host_pid: 42,
        log: PathBuf::from(r"C:\Users\A B\x\"),
    };
    let line = helper_command_line(Path::new(r"C:\u\EternalMonitor-updater.exe"), &args);
    assert!(
        line.starts_with(r"C:\u\EternalMonitor-updater.exe --installer "),
        "{line}"
    );
    assert!(
        line.contains(r#" "C:\Program Files\EternalMonitor\EternalMonitor-host.exe""#),
        "{line}"
    );
    // A trailing backslash is doubled so it doesn't escape the closing quote.
    assert!(line.ends_with(r#""C:\Users\A B\x\\""#), "{line}");
    assert_eq!(quote_arg("plain"), "plain");
    assert_eq!(quote_arg(""), "\"\"");
    assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
}

#[test]
fn a_stream_that_starts_during_the_handoff_sends_the_update_back_to_waiting() {
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(release("0.3.2")));
    s.update_now(false);
    s.download_finished(Ok(PathBuf::from("setup.exe")));
    let (path, _) = s.take_ready(false).unwrap();
    assert_eq!(s.phase, Phase::Installing);
    // The worker found an iPad had connected; it gives the installer back.
    s.requeue(path.clone());
    assert_eq!(s.phase, Phase::Queued(Some(path)));
    assert_eq!(s.saved.pending, None);
    assert!(!s.close, "nothing closes");
    assert_eq!(s.notice(1000), Some(Notice::Queued("0.3.2".into())));
    // When the stream stops, the same verified download is used again.
    let start = Instant::now();
    assert!(s.tick(false, start).is_none());
    assert!(s.tick(false, start + Duration::from_secs(4)).is_none());
    assert!(matches!(s.phase, Phase::Ready(_)));
}

struct StreamSource;

impl crate::transport::session::ConfigSource for StreamSource {
    fn stream_config(&self) -> eternal_wire::v2::control::StreamConfig {
        eternal_wire::v2::control::StreamConfig {
            stream_epoch: 1,
            width: 1280,
            height: 720,
            fps: 60,
            codec: 0,
            flags: 0,
            bitrate_bps: 10_000_000,
        }
    }

    fn host_name(&self) -> String {
        "TEST-HOST".into()
    }
}

/// A session with one iPad streaming from `peer`.
fn streaming_session(peer: crate::transport::link::PeerId) -> SharedSession {
    use eternal_wire::v2::control::{ControlMessage, Hello2, CAP_DECODE_H264};
    let mut session = crate::transport::session::Session::new(1);
    let hello = ControlMessage::Hello2(Hello2 {
        proto_min: 2,
        proto_max: 2,
        client_nonce: 1,
        listen_port: 50000,
        decoder_caps: CAP_DECODE_H264,
        feature_caps: 0,
        screen_px_w: 2420,
        screen_px_h: 1668,
        screen_pt_w: 1210,
        screen_pt_h: 834,
        refresh_hz: 120,
        device_name: "Test iPad".into(),
        device_id: 0,
        preferred_fps: 0,
        auth_token: [0; 16],
        pairing_code: 0,
    });
    let admitted = session.handle_control(peer, 0, hello, &StreamSource, Instant::now());
    assert_eq!(admitted.new_target, Some(peer));
    Arc::new(Mutex::new(session))
}

fn worker(state: State, session: SharedSession) -> Worker {
    Worker {
        state: Arc::new(Mutex::new(state)),
        path: None,
        endpoints: Endpoints::production(),
        session,
        wake: Arc::new(|_| {}),
    }
}

/// Through the worker, as the GUI hands it over: without Stop and update a
/// streaming iPad keeps its stream and the update waits; with it, the stream
/// ends with a goodbye the iPad shows, and the handoff goes ahead.
#[test]
fn stop_and_update_ends_the_stream_and_hands_over() {
    use eternal_wire::v2::control::{parse_control, ByeReason, ControlMessage};
    let peer = crate::transport::link::PeerId::udp(([10, 0, 0, 5], 50000).into());
    let setup = PathBuf::from("setup.exe");
    let ready = |force: bool| {
        let mut s = state(Saved::default());
        s.finish_check(1000, false, Ok(release("0.3.2")));
        s.update_now(true);
        if force {
            s.force_now();
        }
        s.download_finished(Ok(setup.clone()));
        if !force {
            // The stream stopped for a moment, then an iPad connected again
            // just before the handoff.
            s.phase = Phase::Ready(setup.clone());
        }
        let (path, release) = s.take_ready(false).unwrap();
        (s, path, release)
    };

    let (s, path, release) = ready(false);
    let waiting = worker(s, streaming_session(peer));
    waiting.install(&path, &release);
    assert_eq!(
        waiting.state.lock().phase,
        Phase::Queued(Some(setup.clone()))
    );
    assert!(waiting.session.lock().is_active(), "the stream goes on");

    let (s, path, release) = ready(true);
    let forced = worker(s, streaming_session(peer));
    forced.install(&path, &release);
    let state = forced.state.lock();
    assert!(
        !matches!(state.phase, Phase::Queued(_)),
        "Stop and update was sent back to waiting: {:?}",
        state.phase
    );
    // There is no helper next to the test binary, so the handoff itself fails
    // after the stream has ended; the app shows that and stays open.
    assert_eq!(
        state.phase,
        Phase::Failed("The update could not start.".into())
    );
    let mut session = forced.session.lock();
    assert!(!session.is_active(), "the stream ended");
    let sent = session.tick(&StreamSource, false, Instant::now());
    assert!(sent.client_lost);
    assert!(sent.replies.is_empty());
    let (to, datagram) = sent.goodbye.expect("a goodbye");
    assert_eq!(to, peer);
    assert!(matches!(
        parse_control(&datagram),
        Ok((header, ControlMessage::Bye(ByeReason::HostShuttingDown))) if header.session_id != 0
    ));
    assert!(
        session
            .tick(&StreamSource, false, Instant::now())
            .goodbye
            .is_none(),
        "said once"
    );
}

#[test]
fn a_failed_helper_keeps_the_app_open_with_a_message() {
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(release("0.3.2")));
    s.update_now(false);
    s.download_finished(Ok(PathBuf::from("setup.exe")));
    s.take_ready(false).unwrap();
    assert!(!s.close, "nothing closes before the helper answers");
    assert_eq!(s.notice(1000), Some(Notice::Installing("0.3.2".into())));
    s.install_failed(&UpdateError::Launch("the update helper stopped".into()));
    assert!(!s.close);
    assert_eq!(s.saved.pending, None);
    assert_eq!(
        s.notice(1000),
        Some(Notice::Failed("The update could not start.".into()))
    );
    // Try again, and this time the helper answers.
    s.update_now(false);
    s.download_finished(Ok(PathBuf::from("setup.exe")));
    s.take_ready(false).unwrap();
    s.install_started();
    assert!(s.close);
}

#[test]
fn the_helper_is_copied_out_and_held_unchanged() {
    let dir = std::env::temp_dir().join(format!("eternal-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("install")).unwrap();
    std::fs::create_dir_all(dir.join("update")).unwrap();
    let source = dir.join("install/EternalMonitor-updater.exe");
    std::fs::write(&source, b"MZ helper").unwrap();
    let (copy, held) = stage_helper(&source, &dir.join("update")).unwrap();
    assert_eq!(copy, dir.join("update/EternalMonitor-updater.exe"));
    assert_eq!(std::fs::read(&copy).unwrap(), b"MZ helper");
    #[cfg(windows)]
    assert!(
        std::fs::write(&copy, b"MZ evil").is_err(),
        "held while it starts"
    );
    drop(held);
    assert!(stage_helper(&dir.join("install/missing.exe"), &dir.join("update")).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

/// The PC's mismatch banner says to click the button it shows: Check for
/// updates until an update is found, then Update now.
#[test]
fn the_mismatch_text_names_the_button_the_banner_shows() {
    for update_found in [false, true] {
        let (_, body) = mismatch_text(OlderSide::Pc, update_found);
        let button = mismatch_button(update_found);
        assert!(
            body.contains(&format!("Click {button}")),
            "{body:?} does not name {button:?}"
        );
    }
    assert_eq!(mismatch_button(false), "Check for updates");
    assert_eq!(mismatch_button(true), "Update now");
}

#[test]
fn mismatch_names_the_older_side() {
    assert_eq!(older_side(2, 2), None);
    assert_eq!(older_side(1, 3), None);
    assert_eq!(older_side(3, 4), Some(OlderSide::Pc));
    assert_eq!(older_side(1, 1), Some(OlderSide::Ipad));
    let (title, body) = mismatch_text(OlderSide::Pc, true);
    assert!(title.contains("this PC") && body.contains("Update now"));
    let (title, body) = mismatch_text(OlderSide::Ipad, false);
    assert!(title.contains("iPad") && body.contains("TestFlight"));

    let log = MismatchLog::default();
    log.note_protocol(2, 2);
    assert_eq!(log.recent(), None);
    log.note_protocol(3, 3);
    assert_eq!(log.recent(), Some(OlderSide::Pc));
    log.note_legacy();
    assert_eq!(log.recent(), Some(OlderSide::Ipad));
    log.clear();
    assert_eq!(log.recent(), None);
}
