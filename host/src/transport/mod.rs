pub mod abr;
pub mod fault;
pub mod link;
pub mod pacer;
pub mod retransmit;
pub mod session;
pub mod usb;
pub mod usbmuxd;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use eternal_wire::v2::media::{
    media_payload_size, MediaHeader, MAX_FRAG_COUNT, MAX_MEDIA_PAYLOAD, MEDIA_HEADER_SIZE,
};
use eternal_wire::v2::{classify, Classified, MAX_DGRAM_SIZE};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::control::{CaptureTarget, SharedControl, SupervisorCommand, VddStatus};
use crate::encoder::NALUnit;
use crate::stats::PIPELINE_STATS;
use abr::AbrController;
use fault::FaultInjector;
use link::{LinkId, PeerId};
use pacer::FramePacer;
use retransmit::RetransmitRing;
use session::{Actions, ConfigSource, HEARTBEAT_INTERVAL};
use usb::{LinkEvent, TransportLinks};

/// Monotonic per-process counter stamped into each media header's `stream_epoch`. Every
/// `start_sender` (i.e. every pipeline run) gets a fresh value so the iPad can detect a stream
/// restart (seq reset to ~1) reliably instead of inferring it from a sequence-number gap.
static STREAM_EPOCH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Stream parameters advertised in HELLO_ACK and heartbeats.
struct SharedConfigSource<'a> {
    shared: &'a SharedControl,
    stream_epoch: u32,
}

impl ConfigSource for SharedConfigSource<'_> {
    fn stream_config(&self) -> eternal_wire::v2::control::StreamConfig {
        let (width, height, software) = {
            let stats = PIPELINE_STATS.lock();
            let (w, h) = stats.capture_resolution;
            (w, h, stats.using_software_fallback)
        };
        eternal_wire::v2::control::StreamConfig {
            stream_epoch: self.stream_epoch,
            width: width.min(u32::from(u16::MAX)) as u16,
            height: height.min(u32::from(u16::MAX)) as u16,
            fps: self
                .shared
                .target_fps
                .load(Ordering::SeqCst)
                .min(u32::from(u16::MAX)) as u16,
            codec: self.shared.active_codec.load(Ordering::SeqCst),
            flags: if software {
                eternal_wire::v2::control::STREAM_FLAG_SOFTWARE_ENCODER
            } else {
                0
            },
            bitrate_bps: self.shared.abr_current_bps.load(Ordering::SeqCst),
        }
    }

    fn host_caps(&self) -> u16 {
        if PIPELINE_STATS.lock().usb_service_reachable {
            eternal_wire::v2::control::HOSTCAP_USB
        } else {
            0
        }
    }

    fn host_name(&self) -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "EternalMonitor".to_string())
    }
}

/// Consumes NAL units from the encoder, fragments each access unit into v2 media
/// datagrams (raw Annex B — no FlatBuffer wrapper), and runs the protocol-v2
/// control plane (hello/ack, heartbeats, keyframe requests, liveness) on the
/// same socket.
pub async fn start_sender(
    mut nal_rx: mpsc::Receiver<NALUnit>,
    listen_port: u16,
    shared: SharedControl,
    supervisor_tx: std_mpsc::Sender<SupervisorCommand>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind_addr: SocketAddr = format!("0.0.0.0:{listen_port}").parse().unwrap();
    let socket = {
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
        UdpSocket::from_std(raw.into())?
    };

    // Never 0 — the receiver treats epoch 0 as invalid. (Wrap after 2^32 runs is
    // harmless: a fresh session id accompanies any host relaunch.)
    let stream_epoch = STREAM_EPOCH_COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
        .max(1);

    let local_addr = socket.local_addr()?;
    info!(%local_addr, stream_epoch, "UDP transport ready — waiting for a v2 client HELLO");
    PIPELINE_STATS
        .lock()
        .set_target_addr(shared.target_addr.lock().to_string());

    let config = SharedConfigSource {
        shared: &shared,
        stream_epoch,
    };

    let abr_enabled = !std::env::var("ETERNAL_ABR").is_ok_and(|v| v.trim() == "0");
    let mut abr = AbrController::new(shared.bitrate_bps.load(Ordering::SeqCst), abr_enabled);
    shared
        .abr_current_bps
        .store(abr.current_bps(), Ordering::SeqCst);

    let probability = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|r| (0.0..=1.0).contains(r))
            .unwrap_or(0.0)
    };
    let drop_rate = probability("ETERNAL_DROP");
    let reorder_rate = probability("ETERNAL_REORDER");
    let jitter_ms = std::env::var("ETERNAL_JITTER_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        .min(1000);
    let mut fault = FaultInjector::new(drop_rate, reorder_rate, Duration::from_millis(jitter_ms));
    if fault.enabled() {
        warn!(
            drop_rate,
            reorder_rate, jitter_ms, "Media fault injection active"
        );
    }

    // Frames already sitting in the encoder channel when a session starts are
    // pre-IDR leftovers; a new session must begin with a keyframe.
    let mut last_session_id: Option<u32> = None;
    let mut awaiting_keyframe = true;

    let mut input_relay = crate::input::InputRelay::default();
    let mut retransmit_ring = RetransmitRing::default();

    let mut links = TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
    let mut deferred_control = VecDeque::new();
    let mut dgram_scratch = [0u8; MAX_DGRAM_SIZE];
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut liveness_tick = tokio::time::interval(Duration::from_millis(250));
    liveness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // Service timers and queued control before another paced frame.
            // Random branch selection can delay a NACK by multiple frame
            // bursts on platforms with coarse sleep granularity.
            biased;
            _ = tokio::time::sleep_until(fault.next_deadline().unwrap_or_else(|| Instant::now() + Duration::from_secs(3600)).into()), if fault.enabled() => {
                let destination = *shared.target_addr.lock();
                for datagram in fault.drain_due(Instant::now()) {
                    let _ = links.send_to(&datagram, destination).await;
                }
            }
            _ = heartbeat.tick() => {
                let actions = shared.session.lock().tick(&config, true, Instant::now());
                if actions.client_lost { retransmit_ring.clear(); fault.clear(); }
                execute_actions(actions, &links, &shared, &supervisor_tx).await;
            }
            _ = liveness_tick.tick() => {
                links.update_stats();
                if !shared.running.load(Ordering::SeqCst) {
                    info!("Transport loop stopping on running=false");
                    break;
                }
                let actions = shared.session.lock().tick(&config, false, Instant::now());
                if actions.client_lost { retransmit_ring.clear(); fault.clear(); }
                execute_actions(actions, &links, &shared, &supervisor_tx).await;
            }

            result = receive_control(&mut links, &mut deferred_control) => {
                match result {
                    Ok(LinkEvent::Closed(id)) => {
                        let actions = shared.session.lock().link_lost(id);
                        execute_actions(actions, &links, &shared, &supervisor_tx).await;
                    }
                    Ok(LinkEvent::Datagram(src, datagram)) => {
                        let datagram = datagram.as_slice();
                        match classify(datagram) {
                            Classified::Control(_) => {
                                match eternal_wire::v2::control::parse_control(datagram) {
                                    Ok((header, message)) => {
                                        let nack_frag_count = match &message {
                                            eternal_wire::v2::control::ControlMessage::Nack(nack) => Some(nack.frag_count),
                                            _ => None,
                                        };
                                        let mut actions = shared.session.lock().handle_control(
                                            src,
                                            header.session_id,
                                            message,
                                            &config,
                                            Instant::now(),
                                        );
                                        if actions.new_target.is_some() || actions.client_lost {
                                            retransmit_ring.clear();
                                            fault.clear();
                                        }
                                        if let (Some((epoch, seq, missing)), Some(frag_count)) =
                                            (actions.retransmit.take(), nack_frag_count)
                                        {
                                            send_repairs(&links.udp.socket, &mut retransmit_ring, &shared,
                                                epoch, seq, frag_count, &missing).await;
                                        }
                                        if let Some((session_id, event)) = actions.input.take() {
                                            if let Some(geometry) = *shared.capture_geometry.lock()
                                            {
                                                input_relay.relay(session_id, &event, geometry);
                                            }
                                        }
                                        if let Some(report) = actions.report.take() {
                                            info!(epoch = report.stream_epoch, complete = report.frames_complete,
                                                dropped = report.frames_dropped, repaired = report.frags_repaired,
                                                nacks = report.nacks_sent, jitter_us = report.jitter_us,
                                                retransmits = PIPELINE_STATS.lock().transport_retransmits,
                                                "Receiver report");
                                            let ceiling =
                                                shared.bitrate_bps.load(Ordering::SeqCst);
                                            // A lowered ceiling clamps the rung
                                            // immediately; that decision must be
                                            // honored, or dragging the slider
                                            // down changes the label and nothing
                                            // else for the life of the pipeline.
                                            let clamped = abr.set_ceiling(ceiling);
                                            let reported =
                                                abr.on_report(&report, Instant::now());
                                            let decision = abr::AbrDecision {
                                                target_bps: reported.target_bps,
                                                changed: clamped.changed || reported.changed,
                                            };
                                            if decision.changed {
                                                shared
                                                    .abr_current_bps
                                                    .store(decision.target_bps, Ordering::SeqCst);
                                                PIPELINE_STATS
                                                    .lock()
                                                    .set_bitrate(decision.target_bps);
                                                // One immediate notify; the 1s heartbeat's
                                                // embedded config self-heals any loss.
                                                let notify = shared
                                                    .session
                                                    .lock()
                                                    .stream_config_notify(&config);
                                                for (destination, datagram) in notify {
                                                    let _ = links.send_to(&datagram, destination)
                                                        .await;
                                                }
                                            }
                                        }
                                        execute_actions(
                                            actions, &links, &shared, &supervisor_tx,
                                        ).await;
                                    }
                                    Err(error) => {
                                        debug!(peer = %src, %error, "Dropped malformed control datagram");
                                    }
                                }
                            }
                            Classified::LegacyHello => {
                                shared.session.lock().note_legacy_hello(src)
                            }
                            Classified::Media { .. } | Classified::Unknown => {
                                debug!(peer = %src, len = datagram.len(), "Ignored unexpected datagram");
                            }
                        }
                    }
                    Err(e) => warn!(error = %e, "recv_from error"),
                }
            }

            nal_opt = nal_rx.recv() => {
                let Some(nal) = nal_opt else { break; };
                let Some(session_id) = shared.session.lock().session_id() else { continue; };
                let target_addr = *shared.target_addr.lock();
                if target_addr.link == LinkId::Udp && target_addr.addr.is_none_or(|a| a.ip().is_unspecified() || a.port() == 0) {
                    continue;
                }
                if last_session_id != Some(session_id) {
                    last_session_id = Some(session_id);
                    awaiting_keyframe = true;
                }
                if awaiting_keyframe {
                    if !nal.is_keyframe {
                        continue; // stale pre-session frame — the forced IDR is coming
                    }
                    awaiting_keyframe = false;
                }

                let send_start = Instant::now();
                let payload = &nal.data;
                let payload_size = media_payload_size(shared.max_dgram.load(Ordering::SeqCst) as usize).unwrap_or(MAX_MEDIA_PAYLOAD);
                let frag_count_usize = payload.len().div_ceil(payload_size).max(1);
                if frag_count_usize > usize::from(MAX_FRAG_COUNT) {
                    warn!(
                        seq = nal.sequence,
                        total_bytes = payload.len(),
                        "Dropping oversized frame that exceeds the 4 MiB transport cap"
                    );
                    continue;
                }
                let frag_count = frag_count_usize as u16;
                let capture_ts_us = crate::clock::instant_to_us(nal.timestamp);

                let mut send_failed = false;
                let keep_for_repair = shared.session.lock().client_supports_nack();
                let mut stored = Vec::new();
                let mut frame_pacer = FramePacer::new(frag_count_usize, Instant::now());
                for (index, chunk) in payload.chunks(payload_size).enumerate() {
                    let header = MediaHeader {
                        session_id,
                        stream_epoch,
                        frame_seq: nal.sequence as u32,
                        frag_index: index as u16,
                        frag_count,
                        is_keyframe: nal.is_keyframe,
                        is_retransmit: false,
                        capture_ts_us,
                        payload_len: chunk.len() as u16,
                    };
                    header.encode_into(&mut dgram_scratch);
                    dgram_scratch[MEDIA_HEADER_SIZE..MEDIA_HEADER_SIZE + chunk.len()]
                        .copy_from_slice(chunk);
                    let datagram = &dgram_scratch[..MEDIA_HEADER_SIZE + chunk.len()];
                    if keep_for_repair {
                        stored.push(datagram.to_vec());
                    }

                    if target_addr.link == LinkId::Udp && fault.enabled() {
                        for due in fault.push(datagram, Instant::now()) {
                            if let Err(e) = links.send_to(&due, target_addr).await {
                                if !send_failed {
                                    warn!(seq = nal.sequence, error = %e, "UDP send failed");
                                    send_failed = true;
                                }
                            }
                        }
                    } else if let Err(e) = links.send_to(datagram, target_addr).await {
                        if !send_failed {
                            warn!(seq = nal.sequence, fragment = index, error = %e, "UDP send failed");
                            send_failed = true;
                        }
                    }

                    let pause = frame_pacer.after_send(Instant::now());
                    if target_addr.link == LinkId::Udp && pause > Duration::ZERO {
                        repair_while_pacing(&links.udp.socket, &mut retransmit_ring, &shared, &config,
                            nal.sequence as u32, pause, &mut deferred_control).await;
                    }
                }

                if keep_for_repair {
                    retransmit_ring.insert(stream_epoch, nal.sequence as u32, stored);
                }

                let total_bytes = payload.len();
                let latency_ms = send_start.elapsed().as_secs_f64() * 1000.0
                    + nal.encode_duration_us as f64 / 1000.0;
                PIPELINE_STATS.lock().record_transport(
                    total_bytes as u64,
                    u64::from(frag_count),
                    latency_ms,
                    target_addr.to_string(),
                );

                debug!(
                    seq = nal.sequence,
                    fragments = frag_count,
                    total_bytes,
                    keyframe = nal.is_keyframe,
                    target = %target_addr,
                    "Frame sent"
                );
            }
        }
    }

    info!("NAL channel closed, transport sender shutting down");
    Ok(())
}

type DeferredControl = VecDeque<LinkEvent>;

async fn receive_control(
    links: &mut TransportLinks,
    deferred: &mut DeferredControl,
) -> std::io::Result<LinkEvent> {
    if let Some(event) = deferred.pop_front() {
        Ok(event)
    } else {
        links.receive().await
    }
}

async fn send_repairs(
    socket: &UdpSocket,
    ring: &mut RetransmitRing,
    shared: &SharedControl,
    epoch: u32,
    seq: u32,
    frag_count: u16,
    missing: &[u16],
) {
    let target = *shared.target_addr.lock();
    let Some(destination) = target.addr.filter(|_| target.link == LinkId::Udp) else {
        return;
    };
    let mut sent = 0;
    for datagram in ring.resend(epoch, seq, frag_count, missing, Instant::now()) {
        if socket.send_to(&datagram, destination).await.is_ok() {
            sent += 1;
        }
    }
    if sent > 0 {
        let mut stats = PIPELINE_STATS.lock();
        stats.transport_retransmits += sent;
        debug!(
            seq,
            retransmits = stats.transport_retransmits,
            "NACK fragments resent"
        );
    }
}

/// A coarse platform timer may stretch a pacing gap beyond the receiver's
/// repair budget. Continue serving NACKs for already-sent frames throughout
/// that wait. Other control messages retain their order until the current
/// frame finishes; at most 64 packets (128 KiB) can wait here.
async fn repair_while_pacing(
    socket: &UdpSocket,
    ring: &mut RetransmitRing,
    shared: &SharedControl,
    config: &impl ConfigSource,
    current_seq: u32,
    pause: Duration,
    deferred: &mut DeferredControl,
) {
    use eternal_wire::v2::control::{parse_control, ControlMessage};
    let timer = tokio::time::sleep(pause);
    tokio::pin!(timer);
    loop {
        let mut bytes = [0; 2048];
        tokio::select! {
            biased;
            _ = &mut timer => break,
            received = socket.recv_from(&mut bytes) => {
                let Ok((len, peer)) = received else { break; };
                if let Ok((header, ControlMessage::Nack(nack))) = parse_control(&bytes[..len]) {
                    // The current frame enters the ring after its last original
                    // datagram; defer those NACKs rather than losing them.
                    if nack.frame_seq != current_seq {
                        let count = nack.frag_count;
                        let actions = shared.session.lock().handle_control(PeerId::udp(peer), header.session_id,
                            ControlMessage::Nack(nack), config, Instant::now());
                        if let Some((epoch, seq, missing)) = actions.retransmit {
                            send_repairs(socket, ring, shared, epoch, seq, count, &missing).await;
                        }
                        continue;
                    }
                }
                if deferred.len() < 64 { deferred.push_back(LinkEvent::Datagram(PeerId::udp(peer), bytes[..len].to_vec())); }
            }
        }
    }
}

async fn execute_actions(
    actions: Actions,
    links: &TransportLinks,
    shared: &SharedControl,
    supervisor_tx: &std_mpsc::Sender<SupervisorCommand>,
) {
    for (destination, datagram) in &actions.replies {
        if let Err(e) = links.send_to(datagram, *destination).await {
            warn!(peer = %destination, error = %e, "Failed to send control reply");
        }
    }

    if let Some(target) = actions.new_target {
        *shared.target_addr.lock() = target;
        {
            let mut stats = PIPELINE_STATS.lock();
            stats.set_target_addr(target.to_string());
            stats.reset_connection_stats();
        }
        // The virtual extended display is enabled lazily by the capture loop's
        // reconcile step, which only acts while a client is connected. If the
        // user selected it before any client existed, this first registration
        // is the moment to bring it up — which needs a pipeline restart so the
        // capture loop re-runs reconciliation.
        let needs_vdd_restart = *shared.capture_target.lock() == CaptureTarget::VirtualExtended
            && *shared.vdd_status.lock() == VddStatus::WaitingForClient;
        if needs_vdd_restart {
            info!("Client connected with extended display selected — restarting pipeline to enable it");
            shared.stop();
            if let Err(error) = supervisor_tx.send(SupervisorCommand::Restart) {
                warn!(error = %error, "Failed to request pipeline restart for the virtual display");
            }
        }
    }

    if actions.force_idr {
        shared.force_next_idr.store(true, Ordering::SeqCst);
    }

    if actions.client_lost {
        let unspecified = PeerId {
            link: LinkId::Udp,
            addr: None,
        };
        *shared.target_addr.lock() = unspecified;
        PIPELINE_STATS
            .lock()
            .set_target_addr("waiting for client".to_string());

        // Tear the managed virtual display down when its viewer disappears —
        // the capture reconcile disables it on restart once no client is
        // connected. (This closes the DECISIONS.md "idle-disconnect teardown"
        // item, which was blocked on exactly this liveness signal.)
        let vdd_in_use = *shared.capture_target.lock() == CaptureTarget::VirtualExtended
            && *shared.vdd_status.lock() == VddStatus::Active;
        if vdd_in_use {
            info!("Client gone while streaming the virtual display — restarting pipeline to tear it down");
            shared.stop();
            if let Err(error) = supervisor_tx.send(SupervisorCommand::Restart) {
                warn!(error = %error, "Failed to request pipeline restart after client loss");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::control::{
        encode_control, parse_control, ControlMessage, Hello2, Nack, Ping, CAP_DECODE_H264,
        FEATURE_SUPPORTS_NACK,
    };

    #[tokio::test]
    async fn repairs_previous_frames_before_the_pacing_wait_ends() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let host = socket.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer = client.local_addr().unwrap();
        let shared = SharedControl::new(host.port(), 15_000_000);
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
        };
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 1,
            listen_port: peer.port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_SUPPORTS_NACK,
            screen_px_w: 1920,
            screen_px_h: 1080,
            screen_pt_w: 960,
            screen_pt_h: 540,
            refresh_hz: 60,
            device_name: "repair timing test".into(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let actions = shared.session.lock().handle_control(
            PeerId::udp(peer),
            0,
            hello,
            &config,
            Instant::now(),
        );
        *shared.target_addr.lock() = actions.new_target.unwrap();
        let session_id = shared.session.lock().session_id().unwrap();
        let mut original = vec![0xAB; MEDIA_HEADER_SIZE + 1];
        MediaHeader {
            session_id,
            stream_epoch: 1,
            frame_seq: 7,
            frag_index: 0,
            frag_count: 1,
            is_keyframe: false,
            is_retransmit: false,
            capture_ts_us: 1234,
            payload_len: 1,
        }
        .encode_into(&mut original);
        let mut ring = RetransmitRing::default();
        ring.insert(1, 7, vec![original]);
        let pacing = tokio::spawn(async move {
            let config = SharedConfigSource {
                shared: &shared,
                stream_epoch: 1,
            };
            let mut deferred = DeferredControl::new();
            repair_while_pacing(
                &socket,
                &mut ring,
                &shared,
                &config,
                8,
                Duration::from_secs(1),
                &mut deferred,
            )
            .await;
            deferred
        });
        let nack = |seq| {
            ControlMessage::Nack(Nack {
                stream_epoch: 1,
                frame_seq: seq,
                frag_count: 1,
                missing: vec![0],
            })
        };
        client
            .send_to(&encode_control(session_id, 1, &nack(7)), host)
            .await
            .unwrap();
        let mut buffer = [0; 2048];
        let (len, source) =
            tokio::time::timeout(Duration::from_millis(500), client.recv_from(&mut buffer))
                .await
                .expect("repair must bypass the pacing wait")
                .unwrap();
        assert_eq!(source, host);
        let (header, payload) = MediaHeader::decode(&buffer[..len]).unwrap();
        assert_eq!(header.frame_seq, 7);
        assert!(header.is_retransmit);
        assert_eq!(payload, &[0xAB]);
        assert!(
            !pacing.is_finished(),
            "repair was delayed until after pacing"
        );

        let pending = [nack(8), ControlMessage::Ping(Ping { t1_us: 42 })];
        for (index, message) in pending.iter().enumerate() {
            client
                .send_to(&encode_control(session_id, index as u32 + 2, message), host)
                .await
                .unwrap();
        }
        let deferred = pacing.await.unwrap();
        assert_eq!(deferred.len(), pending.len());
        for (event, expected) in deferred.into_iter().zip(pending) {
            let LinkEvent::Datagram(source, bytes) = event else {
                panic!("unexpected disconnect");
            };
            assert_eq!(source, PeerId::udp(peer));
            assert_eq!(parse_control(&bytes).unwrap().1, expected);
        }
    }
}
