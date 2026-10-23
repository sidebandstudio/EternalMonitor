//! A normal quit must not wait for slow support uploads. Batches it could not
//! send stay in the outbox for the next launch. Runs the real binary, like
//! quit_flush.rs, because the wait happens on main's exit path.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const QUEUED: usize = 8;
const ACK_DELAY: Duration = Duration::from_secs(4);

/// Acknowledges each upload after `ACK_DELAY`; returns the endpoint and the
/// number of requests that have started.
fn slow_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let count = started.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let count = count.clone();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let body = loop {
                    let Ok(read) = stream.read(&mut buffer) else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
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
                count.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(ACK_DELAY);
                let batch: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let reply = serde_json::json!({"accepted_batch": batch["batch_id"]}).to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            });
        }
    });
    (format!("http://{address}/v1/events"), started)
}

/// Batches left by an earlier launch that could not reach the server.
fn seed_outbox(outbox: &Path) {
    std::fs::create_dir_all(outbox).unwrap();
    std::fs::write(outbox.join("installation-id"), "ab".repeat(16)).unwrap();
    for index in 0..QUEUED {
        let batch_id = format!("{index:032x}");
        let batch = serde_json::json!({
            "schema": 1,
            "batch_id": batch_id,
            "installation_id": "ab".repeat(16),
            "launch_id": "cd".repeat(16),
            "platform": "test",
            "app_version": "0.0.0",
            "build": "test",
            "events": [{"sequence": index + 1, "at_ms": 1_000 + index, "level": "info",
                        "category": "host", "message": "queued"}],
        });
        let path = outbox.join(format!("{}-{batch_id}.json", 1_000 + index));
        std::fs::write(path, batch.to_string()).unwrap();
    }
}

fn outbox_messages(outbox: &Path) -> Vec<String> {
    std::fs::read_dir(outbox)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|x| x == "json"))
        .flat_map(|path| {
            let batch: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            batch["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["message"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn wait_for(log: &Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !std::fs::read_to_string(log).is_ok_and(|log| log.contains(text)) {
        assert!(Instant::now() < deadline, "{text:?} never reached {log:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn quitting_does_not_wait_for_slow_uploads_and_keeps_the_rest() {
    let dir: PathBuf = std::env::temp_dir().join(format!("em-quit-uploads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let app_dir = dir.join("EternalMonitor");
    let outbox = app_dir.join("diagnostics-outbox");
    seed_outbox(&outbox);
    let (endpoint, requests) = slow_server();
    std::fs::write(
        app_dir.join("diagnostics-upload.json"),
        serde_json::json!({"endpoint": endpoint, "ingest_key": "write-only"}).to_string(),
    )
    .unwrap();
    let log = app_dir.join("logs").join("eternal-host-session.log");
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut host = Command::new(env!("CARGO_BIN_EXE_eternal-host"))
        .arg(port.to_string())
        .env("APPDATA", &dir)
        .env("ETERNAL_HEADLESS", "1")
        .env("ETERNAL_CAPTURE", "synthetic")
        .env("ETERNAL_SYNTH_SIZE", "320x180")
        .env("ETERNAL_ENCODER", "libx264")
        .env("ETERNAL_DIAGNOSTICS_ALLOW_HTTP", "1")
        .env_remove("ETERNAL_DIAGNOSTICS_ENDPOINT")
        .env_remove("ETERNAL_DIAGNOSTICS_INGEST_KEY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&log, "UDP transport ready");
    // Quit while the first upload waits for its acknowledgement.
    let deadline = Instant::now() + Duration::from_secs(10);
    while requests.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "no upload started");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));
    let quit = Instant::now();
    let status = Command::new("kill")
        .args(["-TERM", &host.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(60);
    let exit = loop {
        if let Some(exit) = host.try_wait().unwrap() {
            break exit;
        }
        if Instant::now() > deadline {
            let _ = host.kill();
            panic!("the host did not quit");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let waited = quit.elapsed();

    let messages = outbox_messages(&outbox);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(exit.success(), "{exit:?}");
    assert!(waited < Duration::from_secs(6), "quit took {waited:?}");
    // At most the request in flight was sent; the rest wait for next launch,
    // together with this launch's last lines.
    let queued = messages.iter().filter(|m| *m == "queued").count();
    assert!(queued >= QUEUED - 1, "{queued} of {QUEUED} batches kept");
    assert!(
        messages
            .iter()
            .any(|m| m.ends_with("EternalMonitor shutting down")),
        "{messages:?}"
    );
}
