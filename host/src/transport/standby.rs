//! Answers iPads while no pipeline generation is streaming.
//!
//! The control socket used to exist only inside a running pipeline, so a
//! HELLO2 sent while the stream restarted, after Stop, or after the
//! supervisor gave up went unanswered, and the iPad said the PC didn't
//! answer. The links now outlive generations: the UDP socket is bound once
//! and the USB hub subscription is kept, and whoever holds them answers. A
//! running generation claims them; between generations the standby
//! responder holds them and tells each HELLO2 why the PC isn't streaming.
//! The links pass between the two without closing, so a HELLO2 that arrives
//! during the hand-over waits in the socket's queue for the next owner.

use std::io;
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{parse_control, ControlMessage};
use eternal_wire::v2::{classify, Classified};
use tokio::net::UdpSocket;
use tokio::sync::{Notify, OwnedMutexGuard};
use tracing::{debug, info, warn};

use super::link::PeerId;
use super::session::Standby;
use super::usb::{LinkEvent, ParkedLinks, TransportLinks};
use crate::control::{SharedControl, StreamPhase};

/// How long a starting generation waits for the links. Only an earlier
/// generation that refused to exit holds them this long.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(5);
/// The standby's pause when a generation is about to claim the links.
const HANDOFF_PAUSE: Duration = Duration::from_millis(10);
/// How often the standby retries a port another program holds.
const BIND_RETRY: Duration = Duration::from_secs(2);

/// The links while nobody holds them, and who wants them next.
#[derive(Default)]
pub struct LinkSlot {
    links: Arc<tokio::sync::Mutex<Option<ParkedLinks>>>,
    /// Generations waiting for the links; the standby yields to them.
    claims: AtomicUsize,
    wake: Notify,
    shut_down: AtomicBool,
}

/// A generation's hold on the links. Dropping it parks them for the standby.
pub struct ClaimedLinks {
    slot: OwnedMutexGuard<Option<ParkedLinks>>,
    links: Option<TransportLinks>,
}

impl Deref for ClaimedLinks {
    type Target = TransportLinks;

    fn deref(&self) -> &TransportLinks {
        self.links
            .as_ref()
            .expect("claimed links are present until dropped")
    }
}

impl DerefMut for ClaimedLinks {
    fn deref_mut(&mut self) -> &mut TransportLinks {
        self.links
            .as_mut()
            .expect("claimed links are present until dropped")
    }
}

impl Drop for ClaimedLinks {
    fn drop(&mut self) {
        if let Some(links) = self.links.take() {
            *self.slot = park(links);
        }
    }
}

fn park(links: TransportLinks) -> Option<ParkedLinks> {
    links
        .park()
        .inspect_err(|error| warn!(%error, "Could not keep the control port open between streams"))
        .ok()
}

impl LinkSlot {
    /// Takes the links for a pipeline generation, from the standby or the
    /// previous generation, binding the port the first time.
    pub async fn claim(
        &self,
        listen_port: u16,
        shared: &SharedControl,
    ) -> io::Result<ClaimedLinks> {
        self.claims.fetch_add(1, Ordering::SeqCst);
        self.wake.notify_one();
        let held = tokio::time::timeout(CLAIM_TIMEOUT, Arc::clone(&self.links).lock_owned()).await;
        self.claims.fetch_sub(1, Ordering::SeqCst);
        let mut slot = held.map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "an earlier stream still holds the control port",
            )
        })?;
        let links = open(&mut slot, listen_port, shared)?;
        Ok(ClaimedLinks {
            slot,
            links: Some(links),
        })
    }

    /// Stops the standby responder and closes the port once it lets go.
    pub fn shut_down(&self) {
        self.shut_down.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Records what the supervisor is doing; the standby answers from it.
    pub fn set_phase(&self, shared: &SharedControl, phase: StreamPhase) {
        *shared.stream_phase.lock() = phase;
        self.wake.notify_one();
    }

    fn should_yield(&self) -> bool {
        self.claims.load(Ordering::SeqCst) > 0 || self.shut_down.load(Ordering::SeqCst)
    }
}

fn open(
    slot: &mut Option<ParkedLinks>,
    listen_port: u16,
    shared: &SharedControl,
) -> io::Result<TransportLinks> {
    match slot.take() {
        Some(parked) => TransportLinks::resume(parked),
        None => Ok(TransportLinks::new(
            UdpSocket::from_std(bind(listen_port)?)?,
            Arc::clone(&shared.force_next_idr),
        )),
    }
}

/// The UDP socket iPads send HELLO2 to and the pipeline sends video from.
fn bind(listen_port: u16) -> io::Result<std::net::UdpSocket> {
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], listen_port));
    let raw = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    // A 4 MiB send buffer lets the kernel absorb whatever the pacer's
    // hard budget doesn't smooth (best effort — some platforms clamp it).
    let _ = raw.set_send_buffer_size(4 * 1024 * 1024);
    let _ = raw.set_recv_buffer_size(4 * 1024 * 1024);
    let _ = raw.set_tos(0xB8);
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Networking::WinSock::{
            WSAGetLastError, WSAIoctl, SIO_UDP_CONNRESET, SOCKET,
        };
        let disabled = 0u32;
        let mut returned = 0;
        let result = unsafe {
            WSAIoctl(
                SOCKET(raw.as_raw_socket() as usize),
                SIO_UDP_CONNRESET,
                Some((&disabled as *const u32).cast()),
                std::mem::size_of_val(&disabled) as u32,
                None,
                0,
                &mut returned,
                None,
                None,
            )
        };
        if result != 0 {
            warn!(error = ?unsafe { WSAGetLastError() }, "Could not disable UDP connection-reset notifications");
        }
    }
    raw.set_nonblocking(true)?;
    raw.bind(&bind_addr.into())?;
    Ok(raw.into())
}

/// Starts the standby responder on its own thread. It holds the links
/// whenever no generation does, until [`LinkSlot::shut_down`].
pub fn spawn(listen_port: u16, shared: SharedControl) -> io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("standby".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(run(listen_port, shared)),
                Err(error) => warn!(%error, "Could not start answering iPads between streams"),
            }
        })
}

async fn run(listen_port: u16, shared: SharedControl) {
    let slot = Arc::clone(&shared.links);
    let mut bind_failed = false;
    while !slot.shut_down.load(Ordering::SeqCst) {
        let mut held = tokio::select! {
            held = Arc::clone(&slot.links).lock_owned() => held,
            _ = slot.wake.notified() => continue,
        };
        if slot.should_yield() {
            drop(held);
            tokio::time::sleep(HANDOFF_PAUSE).await;
            continue;
        }
        let mut links = match open(&mut held, listen_port, &shared) {
            Ok(links) => links,
            Err(error) => {
                drop(held);
                if !bind_failed {
                    warn!(%error, port = listen_port, "Could not listen for iPads while the stream is off");
                    bind_failed = true;
                }
                tokio::select! {
                    _ = tokio::time::sleep(BIND_RETRY) => {}
                    _ = slot.wake.notified() => {}
                }
                continue;
            }
        };
        bind_failed = false;
        debug!("Answering iPads while no stream runs");
        serve(&mut links, &slot, &shared).await;
        if slot.shut_down.load(Ordering::SeqCst) {
            break;
        }
        *held = park(links);
    }
    info!("Stopped answering iPads between streams");
}

async fn serve(links: &mut TransportLinks, slot: &LinkSlot, shared: &SharedControl) {
    while !slot.should_yield() {
        // A generation is starting and claims the links in a moment. Leave
        // what arrives queued for it, so the first HELLO2 after Start or
        // Restart connects.
        if *shared.stream_phase.lock() == StreamPhase::Running {
            slot.wake.notified().await;
            continue;
        }
        tokio::select! {
            biased;
            _ = slot.wake.notified() => {}
            event = links.receive() => match event {
                Ok(LinkEvent::Datagram(peer, datagram)) => {
                    answer(links, shared, peer, &datagram).await;
                }
                // A cable that closes now is gone for the next pipeline too;
                // its session ends on the liveness timeout there.
                Ok(LinkEvent::Closed(_)) => {}
                Err(error) => {
                    debug!(%error, "Standby receive failed");
                    tokio::time::sleep(HANDOFF_PAUSE).await;
                }
            },
        }
    }
}

/// What a HELLO2 hears while no pipeline runs.
fn standby(shared: &SharedControl) -> Standby {
    match *shared.stream_phase.lock() {
        StreamPhase::Running | StreamPhase::Restarting => Standby::Restarting,
        StreamPhase::Stopped => Standby::Stopped,
        StreamPhase::Failed => Standby::Failed(crate::gui::stream_failed_reason(
            *shared.capture_block.lock(),
        )),
    }
}

async fn answer(links: &TransportLinks, shared: &SharedControl, peer: PeerId, datagram: &[u8]) {
    match classify(datagram) {
        Classified::Control(_) => {}
        Classified::LegacyHello => {
            shared.session.lock().note_legacy_hello(peer);
            return;
        }
        // Reports, pings and video from a session waiting out a restart:
        // the next generation picks the session up again.
        _ => return,
    }
    let Ok((_, ControlMessage::Hello2(hello))) = parse_control(datagram) else {
        return;
    };
    let config = super::SharedConfigSource {
        shared,
        stream_epoch: 0,
        pen_available: false,
    };
    let actions =
        shared
            .session
            .lock()
            .standby_hello(peer, hello, &config, standby(shared), Instant::now());
    if let Some(status) = actions.hello {
        super::note_hello(peer, status);
    }
    for (destination, reply) in &actions.replies {
        if let Err(error) = links.send_to(reply, *destination).await {
            debug!(peer = %destination, %error, "Could not answer a HELLO between streams");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::control::{
        encode_control, Hello2, HelloAck, HelloStatus, CAP_DECODE_H264, FEATURE_STANDBY_STATUS,
    };

    /// The USB hub has one subscriber per process; these tests take turns.
    static HUB: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Host {
        shared: SharedControl,
        port: u16,
        standby: Option<std::thread::JoinHandle<()>>,
        _hub: std::sync::MutexGuard<'static, ()>,
    }

    impl Host {
        fn start(phase: StreamPhase) -> Self {
            let hub = HUB.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let port = std::net::UdpSocket::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let shared = SharedControl::new(port, 10_000_000);
            shared.pairing.lock().required = false;
            shared.links.set_phase(&shared, phase);
            let standby = spawn(port, shared.clone()).unwrap();
            Self {
                shared,
                port,
                standby: Some(standby),
                _hub: hub,
            }
        }
    }

    impl Drop for Host {
        fn drop(&mut self) {
            self.shared.links.shut_down();
            if let Some(standby) = self.standby.take() {
                standby.join().unwrap();
            }
        }
    }

    fn hello(nonce: u32, feature_caps: u16) -> Vec<u8> {
        encode_control(
            0,
            nonce,
            &ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: nonce,
                listen_port: 0,
                decoder_caps: CAP_DECODE_H264,
                feature_caps,
                screen_px_w: 2420,
                screen_px_h: 1668,
                screen_pt_w: 1210,
                screen_pt_h: 834,
                refresh_hz: 120,
                device_name: "standby test".into(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
        )
    }

    fn ipad() -> std::net::UdpSocket {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        socket
    }

    /// Sends one HELLO2 a few times until an ACK comes back.
    fn ask(ipad: &std::net::UdpSocket, port: u16, nonce: u32, caps: u16) -> Option<HelloAck> {
        let mut buffer = [0; 2048];
        for _ in 0..20 {
            ipad.send_to(&hello(nonce, caps), ("127.0.0.1", port))
                .unwrap();
            if let Ok(len) = ipad.recv(&mut buffer) {
                if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) {
                    return Some(ack);
                }
            }
        }
        None
    }

    #[test]
    fn hellos_are_answered_while_stopped_restarting_and_failed() {
        let host = Host::start(StreamPhase::Stopped);
        let ipad = ipad();
        let ack = ask(&ipad, host.port, 1, FEATURE_STANDBY_STATUS).expect("answered after Stop");
        assert_eq!(ack.status, HelloStatus::Stopped);

        host.shared
            .links
            .set_phase(&host.shared, StreamPhase::Restarting);
        let ack = ask(&ipad, host.port, 2, FEATURE_STANDBY_STATUS).expect("answered mid-restart");
        assert_eq!(ack.status, HelloStatus::Restarting);

        host.shared
            .links
            .set_phase(&host.shared, StreamPhase::Failed);
        let ack = ask(&ipad, host.port, 3, FEATURE_STANDBY_STATUS).expect("answered once failed");
        assert_eq!(ack.status, HelloStatus::Failed);
        assert_eq!(ack.reason, crate::gui::stream_failed_reason(None));
        *host.shared.capture_block.lock() =
            Some(crate::capture::recovery::CaptureBlock::DisplayChanging);
        let ack = ask(&ipad, host.port, 4, FEATURE_STANDBY_STATUS).unwrap();
        assert_eq!(
            ack.reason,
            crate::gui::stream_failed_reason(Some(
                crate::capture::recovery::CaptureBlock::DisplayChanging
            ))
        );
        assert!(!host.shared.session.lock().is_active());
    }

    #[test]
    fn a_0_3_1_ipad_is_turned_away_when_stopped_and_left_to_retry_mid_restart() {
        let host = Host::start(StreamPhase::Stopped);
        let ipad = ipad();
        let ack = ask(&ipad, host.port, 1, 0).expect("a stopped PC answers old iPads");
        assert_eq!(ack.status, HelloStatus::Error);
        host.shared
            .links
            .set_phase(&host.shared, StreamPhase::Restarting);
        assert!(ask(&ipad, host.port, 2, 0).is_none());
    }

    /// Every HELLO2 sent while the links change hands is answered once: by
    /// the standby before the claim and by the claimant after it. The socket
    /// never closes, so none falls into a gap.
    #[test]
    fn no_hello_is_lost_when_a_generation_claims_and_releases_the_links() {
        let host = Host::start(StreamPhase::Stopped);
        let ipad = ipad();
        assert!(ask(&ipad, host.port, 1, FEATURE_STANDBY_STATUS).is_some());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        const SENT: u32 = 300;
        let sender = {
            let ipad = ipad.try_clone().unwrap();
            let port = host.port;
            std::thread::spawn(move || {
                for nonce in 100..100 + SENT {
                    ipad.send_to(&hello(nonce, FEATURE_STANDBY_STATUS), ("127.0.0.1", port))
                        .unwrap();
                    std::thread::sleep(Duration::from_micros(500));
                }
            })
        };
        // A stand-in for a generation: claim, answer what arrives with OK,
        // and hand the links back, twice while the burst runs.
        let mut claimed_answers = 0;
        for _ in 0..2 {
            std::thread::sleep(Duration::from_millis(40));
            claimed_answers += runtime.block_on(async {
                let mut links = host
                    .shared
                    .links
                    .claim(host.port, &host.shared)
                    .await
                    .unwrap();
                let mut answered = 0;
                let until = tokio::time::Instant::now() + Duration::from_millis(40);
                while let Ok(Ok(LinkEvent::Datagram(peer, bytes))) =
                    tokio::time::timeout_at(until, links.receive()).await
                {
                    let (_, ControlMessage::Hello2(hello)) = parse_control(&bytes).unwrap() else {
                        continue;
                    };
                    let ack = HelloAck {
                        status: HelloStatus::Ok,
                        accepted_version: 2,
                        client_nonce: hello.client_nonce,
                        session_id: 1,
                        heartbeat_interval_ms: 1000,
                        report_interval_ms: 500,
                        liveness_timeout_ms: 3000,
                        stream_config: Default::default(),
                        host_name: "claimant".into(),
                        auth_token: [0; 16],
                        host_caps: 0,
                        reason: String::new(),
                    };
                    let reply = encode_control(1, 1, &ControlMessage::HelloAck(ack));
                    links.send_to(&reply, peer).await.unwrap();
                    answered += 1;
                }
                answered
            });
        }
        sender.join().unwrap();

        let mut nonces = std::collections::BTreeSet::new();
        let mut buffer = [0; 2048];
        let mut stopped = 0;
        while let Ok(len) = ipad.recv(&mut buffer) {
            let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) else {
                continue;
            };
            if ack.client_nonce >= 100 {
                assert!(nonces.insert(ack.client_nonce), "answered twice");
                stopped += usize::from(ack.status == HelloStatus::Stopped);
            }
        }
        assert_eq!(nonces.len(), SENT as usize, "every HELLO2 got one answer");
        assert!(claimed_answers > 0 && stopped > 0, "both owners answered");
    }

    /// While a generation starts, the standby leaves HELLO2s queued, so the
    /// first one after Start reaches the pipeline instead of hearing
    /// "restarting".
    #[test]
    fn a_starting_generation_gets_the_first_hello() {
        let host = Host::start(StreamPhase::Stopped);
        let ipad = ipad();
        assert!(ask(&ipad, host.port, 1, FEATURE_STANDBY_STATUS).is_some());
        host.shared
            .links
            .set_phase(&host.shared, StreamPhase::Running);
        std::thread::sleep(Duration::from_millis(50));
        ipad.send_to(&hello(2, FEATURE_STANDBY_STATUS), ("127.0.0.1", host.port))
            .unwrap();
        let mut buffer = [0; 2048];
        assert!(
            ipad.recv(&mut buffer).is_err(),
            "the standby left it queued"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let first = runtime.block_on(async {
            let mut links = host
                .shared
                .links
                .claim(host.port, &host.shared)
                .await
                .unwrap();
            match links.receive().await.unwrap() {
                LinkEvent::Datagram(_, bytes) => parse_control(&bytes).unwrap().1,
                LinkEvent::Closed(_) => panic!("unexpected close"),
            }
        });
        let ControlMessage::Hello2(first) = first else {
            panic!("expected the queued HELLO2");
        };
        assert_eq!(first.client_nonce, 2);
    }
}
