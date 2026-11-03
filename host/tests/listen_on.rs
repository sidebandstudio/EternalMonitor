//! A stream that listens on one network shows that network's address once it
//! connects. Drives the real transport loop, and the restart the network check
//! asks for, with test adapters (debug builds only).
#![cfg(debug_assertions)]

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use eternal_host::control::{SharedControl, SupervisorCommand};
use eternal_host::network::{default_route, set_test_adapters, Adapter, Kind, ListenOn};
use eternal_host::stats::PIPELINE_STATS;

fn free_udp_port(ip: Ipv4Addr) -> u16 {
    UdpSocket::bind((ip, 0))
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap()
}

fn wifi(up: bool, ip: Ipv4Addr) -> Adapter {
    Adapter {
        name: "Wi-Fi".into(),
        kind: Kind::Wifi,
        up,
        gateway: true,
        ipv4: vec![ip],
    }
}

/// Runs the transport until it stops, on its own thread.
fn start(
    shared: &SharedControl,
    port: u16,
) -> (
    std::thread::JoinHandle<()>,
    std_mpsc::Receiver<SupervisorCommand>,
) {
    shared.running.store(true, Ordering::SeqCst);
    let (nal_tx, nal_rx) = tokio::sync::mpsc::channel(4);
    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
    let (supervisor_tx, supervisor_rx) = std_mpsc::channel();
    let shared = shared.clone();
    let transport = std::thread::spawn(move || {
        // Open channels: the transport stops only when told to.
        let _senders = (nal_tx, audio_tx);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(eternal_host::transport::start_sender(
                nal_rx,
                audio_rx,
                port,
                shared,
                supervisor_tx,
            ))
            .unwrap();
    });
    (transport, supervisor_rx)
}

fn shown() -> String {
    PIPELINE_STATS.lock().listen_addr.clone()
}

#[test]
fn the_address_row_shows_a_chosen_network_that_came_back() {
    // The test binds the chosen network's address, so it needs a real one.
    let Some(IpAddr::V4(ip)) = default_route().filter(|ip| !ip.is_loopback()) else {
        eprintln!("SKIP: no IPv4 default route to stand in for the chosen network");
        return;
    };
    let port = free_udp_port(ip);
    set_test_adapters(Some(vec![wifi(false, ip)]));
    let shared = SharedControl::new(port, 10_000_000);
    shared.pairing.lock().required = false;
    *shared.listen_on.lock() = ListenOn::Adapter("Wi-Fi".into());
    // As at startup, before the stream starts.
    let startup = eternal_host::gui::listen_address(&shared.listen_on.lock(), port);
    PIPELINE_STATS.lock().set_listen_addr(startup);
    assert_eq!(shown(), format!("unknown:{port}"));

    let (transport, supervisor_rx) = start(&shared, port);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(shown(), format!("unknown:{port}"));

    // The network connects: the next check, within 10 s, restarts the stream.
    set_test_adapters(Some(vec![wifi(true, ip)]));
    let command = supervisor_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the network check asks for a restart");
    assert!(matches!(command, SupervisorCommand::Restart));
    transport.join().unwrap();

    // The supervisor starts the stream again, now on the network's address.
    let (transport, _supervisor_rx) = start(&shared, port);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while shown() != format!("{ip}:{port}") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(shown(), format!("{ip}:{port}"));
    shared.stop();
    transport.join().unwrap();
    set_test_adapters(None);
}
