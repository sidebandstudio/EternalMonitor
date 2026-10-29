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
        recommended: true,
        note: None,
        rollout: None,
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

#[test]
fn manifest_reads_the_optional_recommended_flag_and_note() {
    let endpoints = Endpoints::production();
    let with = |windows: serde_json::Value| {
        let mut entry = serde_json::json!({
            "version": "0.3.2",
            "installer": production_installer("0.3.2"),
            "sha256": SHA,
        });
        for (key, value) in windows.as_object().unwrap() {
            entry[key] = value.clone();
        }
        let data = serde_json::json!({ "schema": 1, "windows": entry, "ipad": { "later": 1 } });
        parse_manifest(data.to_string().as_bytes(), &endpoints).unwrap()
    };
    // Missing: recommended, no note. Unknown fields are ignored.
    let plain = with(serde_json::json!({ "channel": "stable", "size": 9 }));
    assert_eq!(plain, release("0.3.2"));
    let soft = with(serde_json::json!({
        "recommended": false,
        "note": "  Fixes the second screen on laptops.  ",
    }));
    assert!(!soft.recommended);
    assert_eq!(
        soft.note.as_deref(),
        Some("Fixes the second screen on laptops.")
    );
    // A note that isn't one short line, or a field of the wrong type, is
    // dropped instead of stopping the update.
    for bad in [
        serde_json::json!({ "note": "" }),
        serde_json::json!({ "note": "   " }),
        serde_json::json!({ "note": "Two\nlines" }),
        // Separators and invisible format characters, as the iPad and
        // check-versions.sh refuse them.
        serde_json::json!({ "note": "Two\u{2028}lines" }),
        serde_json::json!({ "note": "Two\u{2029}paragraphs" }),
        serde_json::json!({ "note": "Zero\u{200B}width" }),
        serde_json::json!({ "note": "Reversed \u{202E}text" }),
        serde_json::json!({ "note": "Soft\u{00AD}hyphen" }),
        serde_json::json!({ "note": "\u{FEFF}Fixes audio." }),
        serde_json::json!({ "note": "Bell\u{7}" }),
        serde_json::json!({ "note": "x".repeat(201) }),
        serde_json::json!({ "note": 7, "recommended": "yes" }),
        serde_json::json!({ "note": null, "recommended": null }),
    ] {
        assert_eq!(with(bad.clone()), release("0.3.2"), "{bad}");
    }
    assert_eq!(
        with(serde_json::json!({ "note": "x".repeat(200) }))
            .note
            .map(|n| n.len()),
        Some(200)
    );
}

/// `update-state.json` written by a build without the new fields still loads.
#[test]
fn saved_state_from_before_the_popup_still_loads() {
    let old = serde_json::json!({
        "checked_at": 10,
        "latest": {
            "version": "0.3.3",
            "installer": production_installer("0.3.3"),
            "sha256": SHA,
        },
        "later": null,
        "notified": "0.3.3",
        "pending": null,
    });
    let saved: Saved = serde_json::from_value(old).unwrap();
    assert_eq!(saved.latest, Some(release("0.3.3")));
    assert_eq!(saved.popup, None);
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

/// The release workflow publishes `.github/release-body.md` with the hash in
/// it. People arriving from 0.3.0 and 0.3.1's "View release" read it, and
/// this updater must still find the hash.
#[test]
fn the_published_release_body_carries_the_installer_hash() {
    // A Windows checkout (core.autocrlf) has CRLF line endings. release.yml
    // turns them back into LF before publishing; compare the same text here.
    let template = include_str!("../../../.github/release-body.md").replace("\r\n", "\n");
    assert_eq!(template.matches("{{SHA256}}").count(), 1);
    // Get-FileHash prints uppercase hex. The parser must also take CRLF.
    let body = template.replace("{{SHA256}}", &SHA.to_uppercase());
    let release = release("0.3.2");
    for body in [body.clone(), body.replace('\n', "\r\n")] {
        assert_eq!(published_sha256(&body).as_deref(), Some(SHA));
        let data = github(
            "v0.3.2",
            &body,
            &release.installer,
            9,
            serde_json::json!({}),
        );
        assert_eq!(check_github_release(&data, &release), Ok(9));
    }
    // The label comes before any other 64-digit hex the page might show.
    assert_eq!(template.matches("SHA-256").count(), 1);
    assert!(template.contains("**EternalMonitor-Setup.exe SHA-256:**\n```\n{{SHA256}}\n```"));
    for words in [
        "**Recommended update.** Updating is highly recommended for everyone.",
        "download **EternalMonitor-Setup.exe** below and run it",
        "It installs over your current version and keeps your settings.",
        "open TestFlight and tap **Update**",
        "(https://eternalmonitor.dev/download.html)",
        "RELEASE_NOTES.md",
    ] {
        assert!(template.contains(words), "{words}");
    }
    assert!(!template.contains("Protocol v2"), "stale v0.1.x note");
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
        ..release("0.3.2")
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
fn popup_shows_once_per_version_then_at_most_every_three_days() {
    let mut s = state(Saved::default());
    assert_eq!(s.popup(1000), None, "nothing found yet");
    s.finish_check(1000, false, Ok(release("0.3.2")));
    let shown = s.popup(1000).unwrap();
    assert_eq!(shown.title(), "EternalMonitor 0.3.2 is ready");
    assert_eq!(shown.message(), "Updating is highly recommended.");
    assert_eq!(shown.note, None);
    // Later (or Update now, or Escape) closes it for three days, across
    // restarts, while the banner stays.
    s.popup_closed(2000);
    assert_eq!(s.popup(2000), None);
    assert_eq!(s.notice(2000), Some(Notice::Available("0.3.2".into())));
    let mut restarted = state(s.saved.clone());
    assert_eq!(restarted.popup(2000 + 3 * DAY - 1), None);
    assert!(restarted.popup(2000 + 3 * DAY).is_some());
    restarted.popup_closed(2000 + 3 * DAY);
    assert_eq!(restarted.popup(2000 + 5 * DAY), None);
    assert!(restarted.popup(2000 + 6 * DAY).is_some());
    // A newer version shows at once.
    restarted.finish_check(2000 + 6 * DAY, false, Ok(release("0.3.3")));
    assert_eq!(
        restarted.popup(2000 + 6 * DAY).map(|p| p.version),
        Some("0.3.3".into())
    );
}

/// A close time ahead of the clock counts as closed now: the next popup is
/// three days from the current clock, never at once and never "not for years".
#[test]
fn a_clock_set_back_or_far_ahead_waits_three_days_from_now() {
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(release("0.3.2")));
    s.popup_closed(10 * DAY);
    // The clock goes back one second.
    assert_eq!(s.popup(10 * DAY - 1), None);
    assert_eq!(s.saved.popup, Some(("0.3.2".into(), 10 * DAY - 1)));
    assert_eq!(s.popup(13 * DAY - 2), None);
    assert!(s.popup(13 * DAY - 1).is_some());
    // Closed while the clock was years ahead, then corrected.
    s.saved.popup = Some(("0.3.2".into(), u64::MAX / 2));
    assert_eq!(s.popup(20 * DAY), None);
    // Saved that way, so a restart keeps the same three days.
    let mut restarted = state(s.saved.clone());
    assert_eq!(restarted.saved.popup, Some(("0.3.2".into(), 20 * DAY)));
    assert_eq!(restarted.popup(23 * DAY - 1), None);
    assert!(restarted.popup(23 * DAY).is_some());
}

#[test]
fn popup_follows_the_banner_and_the_manifest_wording() {
    let mut s = state(Saved::default());
    let soft = Release {
        recommended: false,
        note: Some("Fixes audio on some PCs.".into()),
        ..release("0.3.2")
    };
    s.finish_check(1000, false, Ok(soft));
    let popup = s.popup(1000).unwrap();
    assert_eq!(popup.message(), "An update is available.");
    assert_eq!(popup.note.as_deref(), Some("Fixes audio on some PCs."));
    // The banner's Later hides the popup with it for that day.
    s.later(1000);
    assert_eq!(s.popup(1000 + DAY - 1), None);
    assert!(s.popup(1000 + DAY).is_some());
    // Not while an update is already under way.
    s.update_now(true);
    assert_eq!(s.popup(1000 + DAY), None);
    s.cancel();
    assert!(s.popup(1000 + DAY).is_some());
    // Up to date: nothing.
    s.finish_check(1000 + DAY, false, Ok(release("0.3.1")));
    assert_eq!(s.popup(1000 + DAY), None);

    // With automatic checks off, Check now shows the banner but no popup.
    let mut off = State::new("0.3.1", false, Saved::default());
    off.begin_check(true);
    off.finish_check(1000, true, Ok(release("0.3.2")));
    assert!(off.notice(1000).is_some());
    assert_eq!(off.popup(1000), None);
}

/// The manifest's `windows.rollout` with the example's start,
/// 2026-11-02T00:00:00Z, over a week.
const ROLLOUT_START: u64 = 1_793_577_600;

fn staged(version: &str) -> Release {
    Release {
        rollout: Some(Rollout {
            start: ROLLOUT_START,
            days: 7,
        }),
        ..release(version)
    }
}

/// An install placed `share` of the way through the week's ramp.
fn install_at(share: f64) -> Saved {
    let id = (0u32..)
        .map(|i| format!("{i:032x}"))
        .find(|id| {
            let at = crate::remote_config::bucket(id, "0.3.2") as f64 / u64::MAX as f64;
            (at - share).abs() < 0.01
        })
        .unwrap();
    Saved {
        rollout_id: Some(id),
        ..Saved::default()
    }
}

/// A staged release reaches each install's banner, popup and notification
/// on its day. Settings, Check now, Update now and a version mismatch never
/// wait for it.
#[test]
fn a_staged_release_waits_only_for_the_automatic_prompt() {
    let turn = ROLLOUT_START + 7 * DAY / 2;
    let mut s = state(install_at(0.5));
    assert_eq!(s.finish_check(turn - DAY, false, Ok(staged("0.3.2"))), None);
    assert_eq!(s.check, CheckStatus::Available);
    assert_eq!(s.notice(turn - DAY), None);
    assert_eq!(s.popup(turn - DAY), None);
    assert!(s.available().is_some(), "Settings says it is ready");
    // Its turn comes between checks: the banner and popup show at once,
    // the notification at the next check.
    assert_eq!(s.notice(turn + 60), Some(Notice::Available("0.3.2".into())));
    assert!(s.popup(turn + 60).is_some());
    assert_eq!(
        s.finish_check(turn + 60, false, Ok(staged("0.3.2"))),
        Some("0.3.2".into())
    );

    // Check now shows it whatever the turn, and Update now works.
    let mut asked = state(install_at(0.9));
    asked.begin_check(true);
    asked.finish_check(ROLLOUT_START, true, Ok(staged("0.3.2")));
    assert_eq!(
        asked.notice(ROLLOUT_START),
        Some(Notice::Available("0.3.2".into()))
    );
    let mut waiting = state(install_at(0.9));
    waiting.finish_check(ROLLOUT_START, false, Ok(staged("0.3.2")));
    assert_eq!(waiting.notice(ROLLOUT_START), None);
    assert!(matches!(
        waiting.update_now(false),
        Some(Command::Download(r)) if r.version == "0.3.2"
    ));

    // The last day: everyone.
    let mut last = state(install_at(0.99));
    let end = ROLLOUT_START + 7 * DAY;
    assert_eq!(
        last.finish_check(end, false, Ok(staged("0.3.2"))),
        Some("0.3.2".into())
    );
    // Without a rollout, at once, as before.
    let mut plain = state(install_at(0.99));
    assert_eq!(
        plain.finish_check(ROLLOUT_START, false, Ok(release("0.3.2"))),
        Some("0.3.2".into())
    );
}

/// Review round 2: a start 15 days out counted as a typo (shown today) and
/// became a valid start the next day (hidden). Once an install's prompt
/// shows for a release it stays, through later days and a clock set back.
#[test]
fn a_prompt_that_showed_keeps_showing_for_that_release() {
    let today = ROLLOUT_START - 15 * DAY;
    let mut s = state(install_at(0.5));
    s.finish_check(today, false, Ok(staged("0.3.2")));
    assert_eq!(s.notice(today), Some(Notice::Available("0.3.2".into())));
    assert!(s.popup(today).is_some());
    assert_eq!(s.saved.rollout_reached.as_deref(), Some("0.3.2"));
    for later in [today + DAY, today + 5 * DAY, ROLLOUT_START, today - DAY] {
        assert_eq!(
            s.notice(later),
            Some(Notice::Available("0.3.2".into())),
            "{later}"
        );
    }
    // A restart keeps it.
    let restarted = state(s.saved.clone());
    assert!(restarted.notice(today + DAY).is_some());
    // The next staged release waits for its own turn.
    let mut next = state(s.saved.clone());
    let before = ROLLOUT_START - DAY;
    assert_eq!(next.finish_check(before, false, Ok(staged("0.3.3"))), None);
    assert_eq!(next.notice(before), None);
    assert_eq!(next.popup(before), None);
    assert_eq!(next.saved.rollout_reached.as_deref(), Some("0.3.2"));
}

/// Review round 2: a cached rollout skipped the manifest's limits, so
/// `days: 365` hid the prompt for a year, and extreme values overflowed.
#[test]
fn a_saved_rollout_out_of_range_is_no_rollout() {
    let saved = |rollout: serde_json::Value| {
        let mut latest = serde_json::to_value(release("0.3.2")).unwrap();
        latest["rollout"] = rollout;
        serde_json::from_value::<Saved>(serde_json::json!({
            "checked_at": 10,
            "latest": latest,
            "notified": "0.3.2",
        }))
        .unwrap()
    };
    for bad in [
        serde_json::json!({ "start": ROLLOUT_START, "days": 15 }),
        serde_json::json!({ "start": ROLLOUT_START, "days": 365 }),
        serde_json::json!({ "start": ROLLOUT_START, "days": u64::MAX }),
        serde_json::json!({ "start": "2026-11-02T00:00:00Z", "days": 7 }),
        serde_json::json!({ "start": -1, "days": 7 }),
        serde_json::json!("garbage"),
        serde_json::json!(null),
    ] {
        let loaded = saved(bad.clone());
        assert_eq!(loaded.latest.as_ref().unwrap().rollout, None, "{bad}");
        assert_eq!(loaded.notified.as_deref(), Some("0.3.2"), "the rest loads");
        let mut s = state(Saved {
            rollout_id: Some("0".repeat(32)),
            ..loaded
        });
        s.saved.notified = None;
        assert!(s.notice(ROLLOUT_START).is_some(), "{bad}");
    }
    // In range, and extreme starts, load and never overflow.
    let ok = saved(serde_json::json!({ "start": ROLLOUT_START, "days": 14 }));
    assert_eq!(ok.latest.unwrap().rollout.map(|r| r.days), Some(14));
    let far = saved(serde_json::json!({ "start": u64::MAX, "days": 14 }));
    let s = state(Saved {
        rollout_id: Some("0".repeat(32)),
        ..far
    });
    assert!(s.notice(u64::MAX).is_some() && s.notice(0).is_some());
}

/// Each install gets its id once, keeps it, and an older state file without
/// one gets a new one.
#[test]
fn the_rollout_id_is_made_once_and_kept() {
    let s = state(Saved::default());
    let id = s.saved.rollout_id.clone().unwrap();
    assert_eq!(id.len(), 32);
    assert_eq!(state(s.saved.clone()).saved.rollout_id, Some(id.clone()));
    assert_ne!(state(Saved::default()).saved.rollout_id, Some(id));
}

/// The example manifest with every new field, as 0.3.3's parser (copied
/// here) and this one read it: old apps see the same release.
#[test]
fn todays_parser_still_reads_a_manifest_with_the_new_fields() {
    #[derive(Deserialize)]
    struct Manifest033 {
        schema: u32,
        windows: Option<Windows033>,
    }
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Windows033 {
        version: String,
        installer: String,
        sha256: String,
        #[serde(default)]
        recommended: serde_json::Value,
        #[serde(default)]
        note: serde_json::Value,
    }
    let example = include_str!("../../../proto/testdata/latest_remote_config.json");
    let old: Manifest033 = serde_json::from_str(example).unwrap();
    let windows = old.windows.unwrap();
    assert_eq!((old.schema, windows.version.as_str()), (1, "0.3.4"));
    assert_eq!(windows.installer, production_installer("0.3.4"));

    let parsed = parse_manifest(example.as_bytes(), &Endpoints::production()).unwrap();
    assert_eq!(parsed.version, "0.3.4");
    assert_eq!(
        parsed.rollout,
        Some(Rollout {
            start: ROLLOUT_START,
            days: 7
        })
    );
    // A bad rollout is no rollout: the release itself still parses.
    let mut bad: serde_json::Value = serde_json::from_str(example).unwrap();
    bad["windows"]["rollout"] = serde_json::json!({ "start": "soon", "days": 99 });
    let parsed = parse_manifest(bad.to_string().as_bytes(), &Endpoints::production()).unwrap();
    assert_eq!(parsed.rollout, None);
    // And today's live manifest has none.
    let live = include_str!("../../../docs/latest.json");
    let parsed = parse_manifest(live.as_bytes(), &Endpoints::production()).unwrap();
    assert_eq!(parsed.rollout, None);
}

#[test]
fn an_ipad_app_older_than_0_3_2_is_told_to_update() {
    use eternal_wire::v2::control::{
        FEATURE_CHECKS_UPDATES, FEATURE_SUPPORTS_NACK, FEATURE_WANTS_AUDIO, FEATURE_WANTS_INPUT,
    };
    // Every bit an 0.3.1 iPad sends, and none.
    let old = FEATURE_WANTS_INPUT | FEATURE_WANTS_AUDIO | FEATURE_SUPPORTS_NACK;
    assert!(ipad_app_out_of_date(old));
    assert!(ipad_app_out_of_date(0));
    assert!(!ipad_app_out_of_date(old | FEATURE_CHECKS_UPDATES));
    assert!(!ipad_app_out_of_date(FEATURE_CHECKS_UPDATES));
    assert_eq!(
        IPAD_OUT_OF_DATE,
        (
            "Your iPad app is out of date",
            "Update it in TestFlight. It's highly recommended."
        )
    );
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

/// A check can replace the offered release while one is downloading or
/// waiting for a stream to end. The file never goes to the helper with
/// another release's hash: the update stops at its handoff and the banner
/// offers the new answer.
#[test]
fn a_release_found_mid_update_stops_it_and_is_offered() {
    let a = release("0.3.2");
    let b = Release {
        sha256: "1".repeat(64),
        ..release("0.3.3")
    };
    let path_a = PathBuf::from("setup-0.3.2.exe");

    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(a.clone()));
    s.begin_check(false);
    assert!(matches!(s.update_now(false), Some(Command::Download(r)) if r == a));
    s.finish_check(1001, false, Ok(b.clone()));
    assert_eq!(
        s.notice(1001),
        Some(Notice::Downloading("0.3.2".into(), None))
    );
    s.download_finished(Ok(path_a.clone()));
    assert_eq!(s.take_ready(false), None);
    assert_eq!((&s.phase, &s.chosen), (&Phase::Idle, &None));
    assert_eq!(s.saved.pending, None);
    assert_eq!(s.gate.holder(), None);
    assert_eq!(s.notice(1001), Some(Notice::Available("0.3.3".into())));
    assert!(matches!(s.update_now(false), Some(Command::Download(r)) if r == b));

    // The same answer again changes nothing.
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(a.clone()));
    s.update_now(false);
    s.download_finished(Ok(path_a.clone()));
    assert!(s.take_ready(true).is_none(), "waits for the stream");
    s.finish_check(1001, false, Ok(a.clone()));
    let start = Instant::now();
    assert!(s.tick(false, start).is_none());
    assert!(s.tick(false, start + Duration::from_secs(4)).is_none());
    assert_eq!(s.take_ready(false), Some((path_a.clone(), a.clone())));

    // Taken back while the download waited for the stream: nothing is
    // offered and nothing downloads.
    let mut s = state(Saved::default());
    s.finish_check(1000, false, Ok(a.clone()));
    s.update_now(true);
    s.finish_check(1001, false, Ok(release("0.3.1")));
    let start = Instant::now();
    assert!(s.tick(false, start).is_none());
    assert!(s.tick(false, start + Duration::from_secs(4)).is_none());
    assert_eq!(s.phase, Phase::Idle);
    assert_eq!(s.notice(1001), None);
}

/// After a failed update the banner offers Try again while the launch check
/// of the saved release runs. Until it answers, Try again starts nothing; a
/// download already asked for checks the release again before it starts.
#[test]
fn try_again_waits_for_the_launch_check_of_a_saved_release() {
    let now = 100_000;
    let failed = || {
        let (mut saved, dir) = pending_saved("0.3.1", "0.3.2", Some("setup_exit=5\n"));
        saved.checked_at = Some(now - RECHECK_OFFER);
        let mut s = state(saved);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(s.launch_check(now));
        s
    };
    // Withdrawn on the website.
    let mut s = failed();
    assert!(failed_message(&s).starts_with("The update was cancelled"));
    assert!(s.rechecking());
    assert!(s.update_now(false).is_none(), "Try again waits");
    assert!(matches!(s.phase, Phase::Failed(_)));
    s.finish_check(now + 1, false, Ok(release("0.3.1")));
    assert!(!s.rechecking());
    assert!(s.update_now(false).is_none(), "nothing newer to install");
    // Replaced by a newer release: that one is installed.
    let mut s = failed();
    s.finish_check(now + 1, false, Ok(release("0.3.3")));
    assert!(matches!(s.update_now(false), Some(Command::Download(r)) if r.version == "0.3.3"));
    // Offline: the saved release, as before.
    let mut s = failed();
    s.finish_check(now + 1, false, Err(UpdateError::Network("offline".into())));
    assert!(matches!(s.update_now(false), Some(Command::Download(r)) if r.version == "0.3.2"));

    // Through the worker: a download asked for before a check replaced the
    // release stops without a request and the new answer is offered. The
    // fake site answers nothing, so a download that went ahead would fail.
    let site = serve(Box::new(|_, _| (404, vec![], Vec::new())));
    let mut s = failed();
    s.finish_check(now + 1, false, Err(UpdateError::Network("offline".into())));
    let Some(Command::Download(chosen)) = s.update_now(false) else {
        panic!("no download");
    };
    s.finish_check(now + 2, false, Ok(release("0.3.3")));
    let mut worker = worker(
        s,
        Arc::new(Mutex::new(crate::transport::session::Session::new(1))),
    );
    worker.endpoints = Endpoints::local(&site).unwrap();
    let state = worker.state.clone();
    let (tx, rx) = mpsc::channel();
    tx.send(Command::Download(chosen)).unwrap();
    drop(tx);
    worker.run(rx);
    let s = state.lock();
    assert_eq!((&s.phase, &s.chosen), (&Phase::Idle, &None));
    assert_eq!(s.notice(now + 2), Some(Notice::Available("0.3.3".into())));
}

#[test]
fn a_worker_that_cannot_start_does_not_hold_the_saved_release() {
    let now = 100_000;
    let mut s = state(Saved {
        latest: Some(release("0.3.2")),
        checked_at: Some(now - RECHECK_OFFER),
        ..Saved::default()
    });
    assert!(s.launch_check(now));
    assert!(s.rechecking());
    let s = Mutex::new(s);
    start_worker(&s, || Err(std::io::Error::other("no threads left")));
    let s = s.lock();
    assert!(!s.rechecking());
    assert_eq!(s.notice(now), Some(Notice::Available("0.3.2".into())));
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

/// Setup installs into its own folder and the helper starts the exe that
/// asked, so a copy anywhere else would come back as the old version.
#[test]
fn the_install_lookup_uses_the_app_id_setup_records() {
    // Inno turns `{{` into `{` and keeps the rest of AppId as written.
    let iss = include_str!("../../../installer/EternalMonitor.iss");
    let app_id = iss
        .lines()
        .find_map(|line| line.strip_prefix("AppId="))
        .expect("EternalMonitor.iss sets AppId")
        .trim()
        .replace("{{", "{");
    assert!(
        UNINSTALL_KEY.ends_with(&format!("\\{app_id}_is1")),
        "{UNINSTALL_KEY} is not Setup's entry for AppId {app_id}"
    );
}

#[test]
fn the_installed_host_is_the_exe_setup_installs() {
    let iss = include_str!("../../../installer/EternalMonitor.iss");
    let source = format!(r#"Source: "{{#StagingDir}}\{INSTALLED_EXE}"; DestDir: "{{app}}";"#);
    assert!(
        iss.lines().any(|line| line.starts_with(&source)),
        "EternalMonitor.iss does not install {INSTALLED_EXE} into {{app}}"
    );
}

#[test]
fn only_the_installed_copy_installs_updates_in_place() {
    let installed = Path::new("C:\\")
        .join("Program Files")
        .join("EternalMonitor");
    let exe = installed.join("EternalMonitor-host.exe");
    assert!(installs_in_place(&exe, Some(&installed)));
    let trailing = PathBuf::from(format!(
        "{}{}",
        installed.display(),
        std::path::MAIN_SEPARATOR
    ));
    assert!(installs_in_place(&exe, Some(&trailing)));
    let other_case = PathBuf::from(installed.to_string_lossy().to_uppercase());
    assert!(installs_in_place(&exe, Some(&other_case)));

    // A renamed copy beside the installed host: Setup replaces
    // EternalMonitor-host.exe, and the helper would start this one again.
    let renamed = installed.join("EternalMonitor-host-old.exe");
    assert!(!installs_in_place(&renamed, Some(&installed)));
    // A build from source copied into the install folder.
    let other_name = installed.join("eternal-host.exe");
    assert!(!installs_in_place(&other_name, Some(&installed)));
    // Setup's own exe, spelled differently.
    assert!(installs_in_place(
        &installed.join("ETERNALMONITOR-HOST.EXE"),
        Some(&installed)
    ));
    let verbatim = PathBuf::from(format!(r"\\?\{}", exe.display()));
    assert!(installs_in_place(&verbatim, Some(&installed)));

    // The zip from the release page, unpacked in Downloads.
    let portable = Path::new("C:\\")
        .join("Users")
        .join("A B")
        .join("Downloads")
        .join("EternalMonitor")
        .join("EternalMonitor-host.exe");
    assert!(!installs_in_place(&portable, Some(&installed)));
    // A build from source.
    let built = Path::new("C:\\")
        .join("src")
        .join("target")
        .join("release")
        .join("eternal-host.exe");
    assert!(!installs_in_place(&built, Some(&installed)));
    // Never installed: Setup would install into Program Files.
    assert!(!installs_in_place(&portable, None));
    assert!(!installs_in_place(&exe, None));
}

/// The registry can record the install folder by its short (8.3) name.
#[cfg(windows)]
#[test]
fn a_short_install_folder_name_matches_the_installed_host() {
    use std::os::windows::process::CommandExt;
    let installed =
        std::env::temp_dir().join(format!("EternalMonitor install {}", std::process::id()));
    std::fs::create_dir_all(&installed).unwrap();
    let exe = installed.join("EternalMonitor-host.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(installed.join("EternalMonitor-host-old.exe"), b"").unwrap();
    let output = std::process::Command::new("cmd")
        .raw_arg(format!(
            "/c for %I in (\"{}\") do @echo %~sI",
            installed.display()
        ))
        .output()
        .unwrap();
    let short = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let results = [
        installs_in_place(&exe, Some(&short)),
        installs_in_place(&short.join("EternalMonitor-host.exe"), Some(&installed)),
        installs_in_place(&short.join("EternalMonitor-host-old.exe"), Some(&installed)),
    ];
    std::fs::remove_dir_all(&installed).unwrap();
    if short == installed {
        eprintln!("8.3 names are off for {}; skipped", installed.display());
        return;
    }
    assert_eq!(results, [true, true, false], "{}", short.display());
}

#[test]
fn helper_arguments_survive_windows_quoting() {
    let args = eternal_updater::HelperArgs {
        installer: PathBuf::from(
            r"C:\Users\A B\AppData\Local\EternalMonitor\updates\0a\EternalMonitor-Setup-0.3.2.exe",
        ),
        sha256: "0".repeat(64),
        app: PathBuf::from(r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe"),
        app_args: Vec::new(),
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

/// The update and "Restart EternalMonitor" share one handoff gate: a ready
/// update waits while a restart hands over, and holds the gate until it goes
/// back to waiting or fails.
#[test]
fn an_update_and_a_restart_never_hand_over_together() {
    use crate::handoff::{Gate, Handoff};
    let gate = Gate::default();
    let ready = || {
        let mut s = state(Saved::default());
        s.gate = gate.clone();
        s.finish_check(1000, false, Ok(release("0.3.2")));
        s.update_now(false);
        s.download_finished(Ok(PathBuf::from("setup.exe")));
        s
    };

    // A restart first: the update stays ready, saying it installs.
    gate.claim(Handoff::Restart).unwrap();
    let mut s = ready();
    assert_eq!(s.take_ready(false), None);
    assert_eq!(s.phase, Phase::Ready(PathBuf::from("setup.exe")));
    assert_eq!(s.saved.pending, None);
    assert_eq!(s.notice(1000), Some(Notice::Installing("0.3.2".into())));
    // The restart failed and this copy stays: the update goes ahead.
    gate.release(Handoff::Restart);
    assert!(s.take_ready(false).is_some());

    // The update first: a restart is refused until it fails.
    assert_eq!(gate.claim(Handoff::Restart), Err(Handoff::Update));
    s.install_failed(&UpdateError::Launch("the update helper stopped".into()));
    assert_eq!(gate.holder(), None);

    // An iPad connected just before the handoff: back to waiting, and a
    // restart may go meanwhile.
    let mut s = ready();
    let (path, _) = s.take_ready(false).unwrap();
    assert_eq!(gate.holder(), Some(Handoff::Update));
    s.requeue(path);
    assert_eq!(gate.holder(), None);

    // A helper that took over keeps it while this copy closes.
    let mut s = ready();
    s.take_ready(false).unwrap();
    s.install_started();
    assert!(s.close);
    assert_eq!(gate.claim(Handoff::Restart), Err(Handoff::Update));
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

#[test]
fn mismatch_names_the_older_side() {
    assert_eq!(older_side(2, 2), None);
    assert_eq!(older_side(1, 3), None);
    assert_eq!(older_side(3, 4), Some(OlderSide::Pc));
    assert_eq!(older_side(1, 1), Some(OlderSide::Ipad));
    let (title, body) = mismatch_text(OlderSide::Pc);
    assert!(title.contains("this PC") && body.contains("Update now"));
    let (title, body) = mismatch_text(OlderSide::Ipad);
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

// ── Recovery from a failed start or update ──────────────────────────────────

/// The lines `run` logs, without ANSI colours.
fn logged(run: impl FnOnce()) -> String {
    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = Sink(out.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, run);
    let text = String::from_utf8(out.lock().clone()).unwrap();
    text
}

#[test]
fn the_failure_message_offers_only_a_newer_stable_version() {
    let offline = || Some(Err(UpdateError::Network("offline".into())));
    // The website names a newer version.
    let live = failure_offer("0.3.4", None, Some(Ok(release("0.3.5"))));
    assert_eq!(live.version.as_deref(), Some("0.3.5"));
    assert_eq!((live.source, live.fetch), ("live", "ok"));
    assert_eq!(
        live.describe(),
        "Update offer on the failure message: offered version=0.3.5 source=live fetch=ok"
    );
    // The same version is not offered.
    let equal = failure_offer("0.3.5", Some(&release("0.3.5")), Some(Ok(release("0.3.5"))));
    assert_eq!(equal.version, None);
    assert_eq!(
        equal.describe(),
        "Update offer on the failure message: not offered source=live fetch=ok"
    );
    // A preview is never offered, saved or fetched (the manifest refuses it).
    let preview = failure_offer("0.3.4", Some(&release("0.3.5-beta.1")), None);
    assert_eq!(preview.version, None);
    let bad = parse_manifest(
        &manifest("0.3.5-beta.1", &production_installer("0.3.5-beta.1"), SHA),
        &Endpoints::production(),
    );
    assert!(bad.is_err());
    assert_eq!(failure_offer("0.3.4", None, Some(bad)).version, None);
    // Update checks off: no request, the saved release only.
    let off = failure_offer("0.3.4", Some(&release("0.3.5")), None);
    assert_eq!(off.version.as_deref(), Some("0.3.5"));
    assert_eq!((off.source, off.fetch), ("saved", "off"));
    // The website can't be reached: the saved release.
    let failed = failure_offer("0.3.4", Some(&release("0.3.5")), offline());
    assert_eq!(failed.version.as_deref(), Some("0.3.5"));
    assert_eq!(
        failed.describe(),
        "Update offer on the failure message: offered version=0.3.5 source=saved fetch=failed"
    );
    let nothing = failure_offer("0.3.4", None, offline());
    assert_eq!((nothing.version, nothing.source), (None, "none"));
    // A release taken back on the website is not offered from the saved one.
    let taken_back = failure_offer("0.3.4", Some(&release("0.3.5")), Some(Ok(release("0.3.4"))));
    assert_eq!(taken_back.version, None);
}

#[test]
fn the_failure_message_check_is_validated_and_bounded() {
    let base = serve(Box::new(|base, path| match path {
        "/latest.json" => (
            200,
            vec![],
            manifest(
                "0.3.5",
                &format!("{base}/releases/download/v0.3.5/EternalMonitor-Setup.exe"),
                SHA,
            ),
        ),
        _ => (404, vec![], Vec::new()),
    }));
    let endpoints = Endpoints::local(&base).unwrap();
    let soon = || Instant::now() + Duration::from_secs(3);
    let fetched = fetch_by(endpoints.clone(), soon()).unwrap();
    assert_eq!(fetched.version, "0.3.5");
    // The installer must be where the manifest checks say.
    let elsewhere = Endpoints {
        installer_prefix: "https://example.invalid/".into(),
        ..endpoints
    };
    assert!(matches!(
        fetch_by(elsewhere, soon()),
        Err(UpdateError::BadManifest(_))
    ));
    // A site that accepts the connection and never answers: the saved
    // release by the deadline, which counts from before the request.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let stuck = Endpoints::local(&format!("http://127.0.0.1:{port}")).unwrap();
    let lookup = FailureLookup {
        saved: Some(release("0.3.9")),
        checks: true,
    };
    assert_eq!(
        lookup.saved_offer().describe(),
        "Update offer on the failure message: offered version=0.3.9 source=saved fetch=failed"
    );
    let started = Instant::now();
    let deadline = started + Duration::from_millis(300);
    let offer = lookup.finish_from(stuck.clone(), deadline);
    let took = started.elapsed();
    assert_eq!(
        offer.describe(),
        "Update offer on the failure message: offered version=0.3.9 source=saved fetch=failed"
    );
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_millis(450),
        "{took:?}"
    );
    // A deadline already past makes no request.
    assert!(fetch_by(stuck, Instant::now()).is_err());
    drop(silent);
}

fn pending_saved(from: &str, to: &str, result: Option<&str>) -> (Saved, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "eternal-update-result-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(eternal_updater::RESULT_FILE);
    if let Some(text) = result {
        std::fs::write(&path, text).unwrap();
    }
    let saved = Saved {
        pending: Some((from.into(), to.into())),
        pending_result: Some(path),
        latest: Some(release(to)),
        ..Saved::default()
    };
    (saved, dir)
}

fn failed_message(state: &State) -> String {
    match state.notice(0) {
        Some(Notice::Failed(message)) => message,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_finished_update_is_logged_with_setup_s_exit_code() {
    let (saved, dir) = pending_saved("0.3.4", "0.3.5", Some("setup_exit=0\n"));
    let mut state = None;
    let log = logged(|| state = Some(State::new("0.3.5", true, saved)));
    let state = state.unwrap();
    assert!(
        log.contains("INFO") && log.contains("Update finished from=0.3.4 to=0.3.5 setup_exit=0"),
        "{log}"
    );
    assert_eq!(state.notice(0), Some(Notice::Updated("0.3.5".into())));
    assert_eq!(
        (&state.saved.pending, &state.saved.pending_result),
        (&None, &None)
    );
    // From a helper that leaves no result, the line has no exit code.
    let (saved, other) = pending_saved("0.3.3", "0.3.4", None);
    let log = logged(|| drop(State::new("0.3.4", true, saved)));
    assert!(log.contains("Update finished from=0.3.3 to=0.3.4"), "{log}");
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(other).unwrap();
}

/// Setup replaces the app before it repairs the virtual display driver, so
/// the new version runs even when Setup then fails. Only Setup's 0, or no
/// result from an older helper, counts as a finished update.
#[test]
fn a_setup_that_failed_after_installing_the_new_version_is_reported() {
    let case = |current: &str, result: Option<&str>| {
        let (saved, dir) = pending_saved("0.3.4", "0.3.5", result);
        let mut state = None;
        let log = logged(|| state = Some(State::new(current, true, saved)));
        std::fs::remove_dir_all(dir).unwrap();
        let state = state.unwrap();
        assert_eq!(
            (&state.saved.pending, &state.saved.pending_result),
            (&None, &None),
            "reported once"
        );
        (log, state.notice(0))
    };
    // 0 and the new version: finished.
    let (log, notice) = case("0.3.5", Some("setup_exit=0\n"));
    assert!(
        log.contains("INFO") && log.contains("Update finished from=0.3.4 to=0.3.5 setup_exit=0"),
        "{log}"
    );
    assert_eq!(notice, Some(Notice::Updated("0.3.5".into())));
    // 20, the driver repair, after the new version was installed.
    let (log, notice) = case("0.3.5", Some("setup_exit=20\n"));
    assert!(
        log.contains("WARN")
            && log.contains("Update finished with problems from=0.3.4 to=0.3.5 setup_exit=20"),
        "{log}"
    );
    assert!(!log.contains("Update finished from"), "{log}");
    assert_eq!(
        notice,
        Some(Notice::Incomplete(
            "EternalMonitor updated, but the virtual display driver couldn't be repaired. Second screen may not work. Reinstall from the website to fix it.".into()
        ))
    );
    // Any other failure after the new version was installed.
    let (log, notice) = case("0.3.5", Some("setup_exit=4\n"));
    assert!(
        log.contains("Update finished with problems from=0.3.4 to=0.3.5 setup_exit=4"),
        "{log}"
    );
    assert_eq!(
        notice,
        Some(Notice::Incomplete(
            "Some parts of the update didn't finish. Reinstall from the website.".into()
        ))
    );
    // 5 and still the old version: did not finish, as before.
    let (log, notice) = case("0.3.4", Some("setup_exit=5\n"));
    assert!(
        log.contains("WARN")
            && log.contains("Update did not finish from=0.3.4 to=0.3.5 setup_exit=5"),
        "{log}"
    );
    assert!(matches!(notice, Some(Notice::Failed(m)) if m.starts_with("The update was cancelled")));
    // No result from an older helper and the new version: finished.
    let (log, notice) = case("0.3.5", None);
    assert!(
        log.contains("INFO") && log.contains("Update finished from=0.3.4 to=0.3.5"),
        "{log}"
    );
    assert_eq!(notice, Some(Notice::Updated("0.3.5".into())));
    // The banner shows once: OK clears it.
    let (saved, dir) = pending_saved("0.3.4", "0.3.5", Some("setup_exit=20\n"));
    let mut state = State::new("0.3.5", true, saved);
    std::fs::remove_dir_all(dir).unwrap();
    state.later(0);
    assert_eq!(state.notice(0), None);
}

#[test]
fn a_failed_update_is_logged_and_explained_from_the_helper_s_result() {
    let case = |result: Option<&str>| {
        let (saved, dir) = pending_saved("0.3.4", "0.3.5", result);
        let mut state = None;
        let log = logged(|| state = Some(State::new("0.3.4", true, saved)));
        std::fs::remove_dir_all(dir).unwrap();
        let state = state.unwrap();
        assert_eq!(state.saved.pending, None, "reported once");
        (log, failed_message(&state))
    };
    // No result file: an older helper, as before.
    let (log, message) = case(None);
    assert!(
        log.contains("WARN")
            && log.contains("Update did not finish from=0.3.4 to=0.3.5 reason=\"no-result\""),
        "{log}"
    );
    assert!(
        message.contains("you still have version 0.3.4") && message.contains("click Yes"),
        "{message}"
    );
    // Declined permission prompt or Cancel before installing.
    let (log, message) = case(Some("setup_exit=2\n"));
    assert!(log.contains("setup_exit=2 files_in_use=false"), "{log}");
    assert!(
        message.starts_with("The update was cancelled, so you still have version 0.3.4."),
        "{message}"
    );
    // Cancel while installing.
    let (_, message) = case(Some("setup_exit=5\n"));
    assert!(message.starts_with("The update was cancelled"), "{message}");
    // The same code after a file in use.
    let (log, message) = case(Some("setup_exit=5\nfiles_in_use=1\n"));
    assert!(log.contains("setup_exit=5 files_in_use=true"), "{log}");
    assert!(
        message.starts_with("Another program was using EternalMonitor's files")
            && message.contains("Restart the PC"),
        "{message}"
    );
    // Another copy did not close, so Setup never ran.
    let (log, message) = case(Some("reason=another-copy-open\n"));
    assert!(log.contains("reason=another-copy-open"), "{log}");
    assert!(
        message.starts_with("Another copy of EternalMonitor was still open"),
        "{message}"
    );
    let (_, message) = case(Some("reason=host-still-open\n"));
    assert!(
        message.starts_with("Another copy of EternalMonitor was still open"),
        "{message}"
    );
    // Setup failed.
    let (log, message) = case(Some("setup_exit=4\n"));
    assert!(log.contains("setup_exit=4"), "{log}");
    assert!(
        message.starts_with("The installer could not finish"),
        "{message}"
    );
    let (_, message) = case(Some("reason=setup-not-started\n"));
    assert!(
        message.starts_with("The installer could not finish"),
        "{message}"
    );
}

#[test]
fn the_launch_reports_a_previous_update_once_before_the_window() {
    let (mut saved, dir) = pending_saved("0.3.4", "0.3.5", Some("setup_exit=5\n"));
    let previous = take_pending("0.3.4", &mut saved);
    assert!(matches!(&previous, Some(Previous::Failed(m)) if m.contains("cancelled")));
    assert_eq!(
        (saved.pending.clone(), saved.pending_result.clone()),
        (None, None)
    );
    // The window's state shows what the launch took, and logs nothing again.
    let mut state = None;
    let log = logged(|| state = Some(State::resume("0.3.4", true, saved, previous)));
    assert!(!log.contains("Update did not finish"), "{log}");
    assert!(failed_message(&state.unwrap()).contains("cancelled"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_handoff_records_where_the_helper_writes_its_result() {
    let mut s = state(Saved::default());
    s.finish_check(10, false, Ok(release("0.3.2")));
    s.update_now(false);
    let setup = PathBuf::from("updates")
        .join("0a1b")
        .join("EternalMonitor-Setup-0.3.2.exe");
    s.download_finished(Ok(setup.clone()));
    s.take_ready(false).unwrap();
    assert_eq!(
        s.saved.pending_result,
        Some(setup.with_file_name(eternal_updater::RESULT_FILE))
    );
    s.install_failed(&UpdateError::Launch("x".into()));
    assert_eq!(
        (s.saved.pending.clone(), s.saved.pending_result.clone()),
        (None, None)
    );
}

#[test]
fn a_saved_release_is_checked_again_before_it_is_offered() {
    let saved = |checked_at| Saved {
        checked_at: Some(checked_at),
        latest: Some(release("0.3.2")),
        ..Saved::default()
    };
    let now = 100_000;
    // Checked minutes ago: offered at once, no extra request.
    let mut fresh = state(saved(now - 60));
    assert!(!fresh.launch_check(now));
    assert_eq!(fresh.notice(now), Some(Notice::Available("0.3.2".into())));
    // Older: checked again, and nothing is offered until it answers.
    let mut stale = state(saved(now - RECHECK_OFFER));
    assert!(stale.launch_check(now));
    assert_eq!(stale.notice(now), None);
    assert_eq!(stale.popup(now), None);
    // The website took the release back: it is not offered.
    stale.finish_check(now + 1, false, Ok(release("0.3.1")));
    assert_eq!(stale.notice(now + 1), None);
    // Offline: the saved release is offered as before.
    let mut offline = state(saved(now - 2 * RECHECK_OFFER));
    assert!(offline.launch_check(now));
    offline.finish_check(now + 1, false, Err(UpdateError::Network("offline".into())));
    assert_eq!(
        offline.notice(now + 1),
        Some(Notice::Available("0.3.2".into()))
    );
    // Nothing to offer, or checks off: only the hourly rule applies.
    let mut current = state(Saved {
        checked_at: Some(now - RECHECK_OFFER),
        latest: Some(release("0.3.1")),
        ..Saved::default()
    });
    assert!(!current.launch_check(now));
    let mut off = State::new("0.3.1", false, saved(now - DAY));
    assert!(!off.launch_check(now));
}

#[test]
fn the_update_funnel_is_logged_without_identifiers() {
    let mut s = state(Saved::default());
    let log = logged(|| {
        s.finish_check(10, false, Ok(release("0.3.2")));
        s.popup_shown("0.3.2");
        s.popup_shown("0.3.2");
        s.update_now(false);
    });
    assert!(
        log.contains("Update available version=0.3.2 manual=false"),
        "{log}"
    );
    assert_eq!(
        log.matches("Update popup shown version=0.3.2").count(),
        1,
        "{log}"
    );
    assert!(
        log.contains("Update now clicked version=0.3.2 streaming=false"),
        "{log}"
    );
    let log = logged(|| {
        state(Saved::default()).finish_check(10, true, Ok(release("0.3.1")));
    });
    assert!(
        log.contains("Update check found no newer version manual=true"),
        "{log}"
    );
}
