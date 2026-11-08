//! A transport loop stuck in one call is named by the supervisor's watch, with
//! the step it is stuck in, and again when it moves. Drives the real loop and
//! blocks it the way a stuck call would: the session lock it needs at its
//! next tick, or at the second screen's step after it, is held elsewhere.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eternal_host::control::SharedControl;
use eternal_host::transport::beat::{Phase, Verdict, Watch};
use eternal_host::transport::{self};
use tokio::sync::mpsc;

/// The transport binds every address, so the port must be free on all.
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap()
}

fn now_ms() -> u64 {
    eternal_host::clock::host_now_us() / 1000
}

/// Checks the beat as the supervisor does, every 100 ms, until `until`.
/// Fails at once if the transport ended (it could not bind, say), which
/// would otherwise read as a loop that never marks.
fn watch_until(
    watch: &mut Watch,
    shared: &SharedControl,
    transport: &std::thread::JoinHandle<()>,
    until: Instant,
) -> Vec<Verdict> {
    let mut verdicts = Vec::new();
    while Instant::now() < until {
        assert!(!transport.is_finished(), "the transport ended early");
        verdicts.extend(watch.check(shared.transport_beat.read(), now_ms()));
        std::thread::sleep(Duration::from_millis(100));
    }
    verdicts
}

#[test]
fn a_stuck_transport_loop_is_named_with_its_step() {
    eternal_host::clock::init();
    // Never reach a real iPad over USB.
    std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
    let port = free_udp_port();
    let shared = SharedControl::new(port, 10_000_000);
    shared.running.store(true, Ordering::SeqCst);
    let (nal_tx, nal_rx) = mpsc::channel(4);
    let (_audio_tx, audio_rx) = mpsc::channel(4);
    let control = shared.clone();
    let transport = std::thread::spawn(move || {
        let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(transport::start_sender(
                nal_rx,
                audio_rx,
                port,
                control,
                supervisor_tx,
                1,
            ))
            .unwrap();
    });
    let mut watch = Watch::default();
    let started = Instant::now();
    while shared.transport_beat.read().is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the loop starts"
        );
        assert!(!transport.is_finished(), "the transport ended early");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Running freely, it ticks every 250 ms.
    assert_eq!(
        watch_until(
            &mut watch,
            &shared,
            &transport,
            Instant::now() + Duration::from_millis(1500)
        ),
        []
    );

    let held = shared.session.lock();
    let stuck = watch_until(
        &mut watch,
        &shared,
        &transport,
        Instant::now() + Duration::from_secs(4),
    );
    drop(held);
    // The loop takes the session lock at its tick and again, after each
    // tick's replies, for the second screen's next step. Whichever it
    // reaches first while the lock is held is where it is stuck.
    let [Verdict::Stalled {
        phase: stuck_in,
        silent_ms,
    }] = stuck.as_slice()
    else {
        panic!("{stuck:?}");
    };
    assert!(
        matches!(stuck_in, Phase::Tick | Phase::SecondScreen),
        "{stuck:?}"
    );
    assert!(*silent_ms >= 2_000, "{stuck:?}");
    let moved = watch_until(
        &mut watch,
        &shared,
        &transport,
        Instant::now() + Duration::from_secs(1),
    );
    assert!(
        matches!(
            moved.as_slice(),
            [Verdict::Moving { phase, stalled_ms }] if phase == stuck_in && *stalled_ms >= 2_000
        ),
        "{moved:?}"
    );

    shared.stop();
    drop(nal_tx);
    transport.join().unwrap();
    assert_eq!(
        shared.transport_beat.read(),
        None,
        "an ended loop is not stuck"
    );
}
