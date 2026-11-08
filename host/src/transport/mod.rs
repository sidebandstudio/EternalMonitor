pub mod abr;
pub mod apple_usb;
mod audio;
#[cfg(test)]
pub(crate) use audio::tests::session as test_session;
pub mod beat;
pub mod congestion;
pub mod fault;
pub mod link;
pub mod pacer;
pub mod pointer;
pub mod retransmit;
pub mod session;
pub mod standby;
pub mod usb;
pub mod usbmuxd;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::ControlMessage;
use eternal_wire::v2::media::{
    media_payload_size, MediaHeader, MAX_FRAG_COUNT, MAX_MEDIA_PAYLOAD, MEDIA_HEADER_SIZE,
};
use eternal_wire::v2::{classify, Classified, MAX_DGRAM_SIZE};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::capture::second_screen;
use crate::control::{CaptureTarget, SharedControl, SupervisorCommand, VddStatus};
use crate::encoder::NALUnit;
use crate::stats::PIPELINE_STATS;
use abr::AbrController;
use beat::Phase;
use fault::FaultInjector;
use link::{LinkId, PeerId};
use pacer::{Deadline, FramePacer, PacingClock};
use retransmit::RetransmitRing;
use session::{Actions, ConfigSource, HEARTBEAT_INTERVAL};
use usb::{LinkEvent, TransportLinks};

/// Monotonic per-process counter stamped into each media header's `stream_epoch`. Every
/// `start_sender` (i.e. every pipeline run) gets a fresh value so the iPad can detect a stream
/// restart (seq reset to ~1) reliably instead of inferring it from a sequence-number gap.
static STREAM_EPOCH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The clock the transport gives the session and the second screen: the
/// system's, except in a test runtime with its clock paused, where it is
/// that runtime's, so a simulation can run through seconds of silence.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// How many UDP datagrams the transport sent to each address, and how many
/// from each it handed to the session, for the paused-clock simulations:
/// the loopback delivers when the kernel gets to it, so they wait for what
/// is in flight before moving the clock on.
#[cfg(feature = "test-hooks")]
pub mod probe {
    use std::collections::{BTreeMap, VecDeque};
    use std::net::SocketAddr;
    use std::time::Instant;

    use parking_lot::Mutex;

    static SENT: Mutex<BTreeMap<SocketAddr, u64>> = Mutex::new(BTreeMap::new());
    static TAKEN: Mutex<BTreeMap<SocketAddr, u64>> = Mutex::new(BTreeMap::new());

    pub(crate) fn sent(addr: SocketAddr) {
        *SENT.lock().entry(addr).or_default() += 1;
    }

    pub(crate) fn taken(addr: SocketAddr) {
        *TAKEN.lock().entry(addr).or_default() += 1;
    }

    pub fn sent_to(addr: SocketAddr) -> u64 {
        SENT.lock().get(&addr).copied().unwrap_or(0)
    }

    pub fn taken_from(addr: SocketAddr) -> u64 {
        TAKEN.lock().get(&addr).copied().unwrap_or(0)
    }

    /// The datagrams a refusal applies to.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Kind {
        Media,
        Control,
    }

    /// How a failed send fails.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Fail {
        /// As if the system had no buffer space (WSAENOBUFS, ENOBUFS).
        NoBufferSpace,
        /// Any other error: connection reset.
        Other,
    }

    struct Failing {
        kind: Kind,
        after: u64,
        fails: VecDeque<Fail>,
    }

    static FAIL: Mutex<BTreeMap<SocketAddr, Failing>> = Mutex::new(BTreeMap::new());

    /// Of the UDP sends of `kind` to `addr`, the next `after` go out and
    /// the ones after them fail as `fails` says, in order.
    pub fn fail_sends(addr: SocketAddr, kind: Kind, after: u64, fails: &[Fail]) {
        let fails = fails.iter().copied().collect();
        FAIL.lock().insert(addr, Failing { kind, after, fails });
    }

    /// As [`fail_sends`], with `count` sends refused for lack of buffer space.
    pub fn refuse_sends(addr: SocketAddr, kind: Kind, after: u64, count: usize) {
        fail_sends(addr, kind, after, &vec![Fail::NoBufferSpace; count]);
    }

    /// Failures [`fail_sends`] still has to give.
    pub fn refusals_left(addr: SocketAddr) -> usize {
        FAIL.lock()
            .get(&addr)
            .map_or(0, |failing| failing.fails.len())
    }

    /// How this send of `datagram` to `addr` fails, if it does.
    pub(crate) fn fail(addr: SocketAddr, datagram: &[u8]) -> Option<std::io::Error> {
        use eternal_wire::v2::{classify, Classified};
        let kind = match classify(datagram) {
            Classified::Media { .. } => Kind::Media,
            Classified::Control(_) => Kind::Control,
            _ => return None,
        };
        let fail = match FAIL.lock().get_mut(&addr).filter(|f| f.kind == kind) {
            Some(failing) if failing.after > 0 => {
                failing.after -= 1;
                None
            }
            Some(failing) => failing.fails.pop_front(),
            None => None,
        };
        attempted(addr, datagram, fail);
        fail.map(|fail| match fail {
            Fail::NoBufferSpace => super::congestion::no_buffer_space(),
            Fail::Other => std::io::Error::from(std::io::ErrorKind::ConnectionReset),
        })
    }

    /// One video datagram the transport tried to send, as the send saw it.
    #[derive(Clone, Copy, Debug)]
    pub struct Attempt {
        pub at: Instant,
        pub frame_seq: u32,
        pub frag_index: u16,
        pub retransmit: bool,
        pub failed: Option<Fail>,
    }

    static RECORDED: Mutex<Option<SocketAddr>> = Mutex::new(None);
    static ATTEMPTS: Mutex<Vec<Attempt>> = Mutex::new(Vec::new());
    /// When each pacing wait was asked for, and its deadline.
    static WAITS: Mutex<Vec<(Instant, Instant)>> = Mutex::new(Vec::new());

    /// Records every video send to `addr` from now on, and every pacing
    /// wait, clearing what was recorded.
    pub fn record(addr: SocketAddr) {
        *RECORDED.lock() = Some(addr);
        ATTEMPTS.lock().clear();
        WAITS.lock().clear();
    }

    /// The video sends recorded so far, in order.
    pub fn attempts() -> Vec<Attempt> {
        ATTEMPTS.lock().clone()
    }

    /// The pacing waits recorded so far: when each was asked for, and until when.
    pub fn waits() -> Vec<(Instant, Instant)> {
        WAITS.lock().clone()
    }

    fn attempted(addr: SocketAddr, datagram: &[u8], failed: Option<Fail>) {
        if *RECORDED.lock() != Some(addr) {
            return;
        }
        if let Ok((header, _)) = eternal_wire::v2::media::MediaHeader::decode(datagram) {
            ATTEMPTS.lock().push(Attempt {
                at: Instant::now(),
                frame_seq: header.frame_seq,
                frag_index: header.frag_index,
                retransmit: header.is_retransmit,
                failed,
            });
        }
    }

    pub(crate) fn waited(deadline: Instant) {
        if RECORDED.lock().is_some() {
            WAITS.lock().push((Instant::now(), deadline));
        }
    }
}

/// What an identity proof signs for a socket on `listen`: that address, or
/// for one on every address, `home()`. None, and no answer, without an
/// address to sign.
fn identity_endpoints(
    listen: SocketAddr,
    home: impl FnOnce() -> Vec<std::net::Ipv4Addr>,
) -> Option<(u16, Vec<[u8; 4]>)> {
    let std::net::IpAddr::V4(ip) = listen.ip() else {
        return None;
    };
    let addresses = if ip.is_unspecified() {
        home()
    } else {
        vec![ip]
    };
    (!addresses.is_empty()).then(|| {
        (
            listen.port(),
            addresses.iter().map(|ip| ip.octets()).collect(),
        )
    })
}

/// Stream parameters advertised in HELLO_ACK and heartbeats.
struct SharedConfigSource<'a> {
    shared: &'a SharedControl,
    stream_epoch: u32,
    pen_available: bool,
    /// Where the socket receives the stream.
    listen: SocketAddr,
}

impl ConfigSource for SharedConfigSource<'_> {
    fn pairing(&self) -> Option<&parking_lot::Mutex<crate::pairing::Pairing>> {
        Some(&self.shared.pairing)
    }

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

    fn identity_endpoints(&self) -> Option<(u16, Vec<[u8; 4]>)> {
        identity_endpoints(self.listen, crate::network::home_addresses)
    }

    fn notice(&self) -> u8 {
        use crate::control::{CaptureTarget, VddStatus};
        let block = *self.shared.capture_block.lock();
        let second_screen_failed = *self.shared.capture_target.lock()
            == CaptureTarget::VirtualExtended
            && matches!(
                *self.shared.vdd_status.lock(),
                VddStatus::Failed | VddStatus::TaskFailed
            );
        crate::capture::recovery::heartbeat_notice(
            block,
            second_screen_failed,
            &mut self.shared.gpu_refused_since.lock(),
            std::time::Instant::now(),
        )
    }

    fn needs_windows_11(&self) -> bool {
        second_screen::windows_notice(self.shared, Instant::now())
    }

    fn host_caps(&self) -> u16 {
        // `start_sender` numbers sent frames contiguously per epoch.
        let numbering = eternal_wire::v2::control::HOSTCAP_CONTIGUOUS_FRAME_SEQ;
        let usb = if PIPELINE_STATS.lock().usb_service_reachable {
            eternal_wire::v2::control::HOSTCAP_USB
        } else {
            0
        };
        numbering
            | eternal_wire::v2::control::HOSTCAP_KEYBOARD
            | eternal_wire::v2::control::HOSTCAP_POINTER
            | if self.pen_available {
                eternal_wire::v2::control::HOSTCAP_PEN
            } else {
                0
            }
            | usb
            | if crate::audio::codec_available() {
                eternal_wire::v2::control::HOSTCAP_AUDIO
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

/// How often a stream looks at its chosen network while nobody streams.
const NETWORK_CHECK: Duration = Duration::from_secs(10);

/// With Listen on set to one network: while nobody streams, a stream bound
/// to an address that network no longer gives restarts to listen on the
/// one it gives now. The supervisor does that only while this stream still
/// runs, never once the user stopped it.
fn look_at_network(
    shared: &SharedControl,
    listen_on: &crate::network::ListenOn,
    bind_ip: std::net::Ipv4Addr,
    supervisor: &StreamSupervisor,
) {
    if *listen_on == crate::network::ListenOn::All
        || shared.client_connected()
        || crate::network::listen_ip_now(listen_on) == bind_ip
    {
        return;
    }
    info!(
        listen_on = listen_on.label(),
        "The chosen network changed: restarting the stream to listen on it"
    );
    shared.stop();
    if let Err(error) = supervisor.restart() {
        warn!(error = %error, "Failed to request a pipeline restart for the chosen network");
    }
}

/// The supervisor a stream answers to, and the pipeline generation the
/// stream belongs to.
struct StreamSupervisor {
    tx: std_mpsc::Sender<SupervisorCommand>,
    generation: u64,
}

impl StreamSupervisor {
    /// Asks the supervisor to restart this stream. It does only while this
    /// stream's generation still runs: never one the user stopped, or one a
    /// newer stream replaced.
    fn restart(&self) -> Result<(), std_mpsc::SendError<SupervisorCommand>> {
        self.tx.send(SupervisorCommand::RestartStream {
            generation: self.generation,
        })
    }
}

/// Consumes NAL units from the encoder, fragments each access unit into v2 media
/// datagrams (raw Annex B — no FlatBuffer wrapper), and runs the protocol-v2
/// control plane (hello/ack, heartbeats, keyframe requests, liveness) on the
/// same socket.
pub async fn start_sender(
    mut nal_rx: mpsc::Receiver<NALUnit>,
    mut audio_rx: mpsc::Receiver<crate::audio::SessionPacket>,
    listen_port: u16,
    shared: SharedControl,
    supervisor_tx: std_mpsc::Sender<SupervisorCommand>,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let supervisor = StreamSupervisor {
        tx: supervisor_tx,
        generation,
    };
    let listen_on = shared.listen_on.lock().clone();
    let bind_ip = crate::network::listen_ip_now(&listen_on);
    if bind_ip.is_loopback() {
        warn!(
            listen_on = listen_on.label(),
            "The chosen network isn't connected: listening on this PC only until it is (USB still works)"
        );
    }
    let bind_addr = SocketAddr::from((bind_ip, listen_port));
    let socket = match take_stream_socket(&shared, bind_addr) {
        Some(socket) => socket,
        None => bind_stream_socket(bind_addr)?,
    };
    let socket = UdpSocket::from_std(socket)?;

    // Never 0 — the receiver treats epoch 0 as invalid. (Wrap after 2^32 runs is
    // harmless: a fresh session id accompanies any host relaunch.)
    let stream_epoch = STREAM_EPOCH_COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
        .max(1);

    let local_addr = socket.local_addr()?;
    if bind_ip.is_unspecified() {
        // Lists the home network addresses before anyone asks for them.
        crate::network::home_addresses();
    }
    info!(%local_addr, stream_epoch, "UDP transport ready — waiting for a v2 client HELLO");
    // The Address row and QR code follow every bind, so a chosen network
    // that came back shows its address. Resolved before locking: the stats
    // lock is shared with every pipeline thread.
    let shown = crate::gui::shown_address(bind_ip, listen_port);
    PIPELINE_STATS.lock().set_listen_addr(shown);
    PIPELINE_STATS
        .lock()
        .set_target_addr(shared.target_addr.lock().to_string());

    let input = shared
        .input
        .begin(std::sync::Arc::clone(&shared.capture_geometry));
    let config = SharedConfigSource {
        shared: &shared,
        stream_epoch,
        pen_available: input.pen_available(),
        listen: local_addr,
    };

    let abr_enabled = !std::env::var("ETERNAL_ABR").is_ok_and(|v| v.trim() == "0");
    let abr = AbrController::new(shared.bitrate_bps.load(Ordering::SeqCst), abr_enabled);
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
    let fault = FaultInjector::new(drop_rate, reorder_rate, Duration::from_millis(jitter_ms));
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
    // The wire numbers access units contiguously per epoch, as proto says.
    // Capture numbers skip the frames the encoder never took, and the iPad
    // held the frame after each skip waiting for one that never existed.
    let mut frame_seq: u32 = 0;
    // The encoder closes its channel when it exits, before it reports why.
    // The loop goes on until `running` goes false, so a goodbye the
    // supervisor leaves on hearing that report (the storm brake) still goes
    // out.
    let mut video_open = true;

    let mut state = LoopState {
        retransmit_ring: RetransmitRing::default(),
        fault,
        input,
        abr,
        pointer: Default::default(),
    };
    let mut audio_sender = audio::AudioSender::default();
    let pacing_clock = PacingClock::new()?;

    let mut links = TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
    // One lock, with `running` read under it: a stop
    // (`diagnostics::stop_session`) queues its goodbye and clears `running`
    // under the same lock, so this either sees that stop and keeps its
    // goodbye, or comes first and the stop's goodbye is kept anyway.
    let retired = {
        let mut session = shared.session.lock();
        let stopping = !shared.running.load(Ordering::SeqCst);
        session.transport_resumed(now(), stopping)
    };
    if retired {
        debug!("Dropped a goodbye left while the stream was stopped");
    }
    let beat = std::sync::Arc::clone(&shared.transport_beat);
    let _beat_ends = BeatEnds(&beat);
    let mut deferred_control = VecDeque::new();
    let mut dgram_scratch = [0u8; MAX_DGRAM_SIZE];
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut liveness_tick = tokio::time::interval(Duration::from_millis(250));
    liveness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A chosen network can connect, or get a new address, after the stream
    // bound. It is looked at now, before anything else, and again every
    // 10 s while nobody streams. The first look used to be the interval's
    // first tick, which could lose to the liveness tick: a stream stopped
    // meanwhile then ended without looking.
    look_at_network(&shared, &listen_on, bind_ip, &supervisor);
    let mut network_check =
        tokio::time::interval_at(tokio::time::Instant::now() + NETWORK_CHECK, NETWORK_CHECK);
    network_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pointer_budget = Deadline::new("pointer-budget")?;

    loop {
        // Tokio's timer can wake 15 ms late on Windows, longer than a frame
        // budget: the budget's end waits on a high-resolution timer.
        let pointer_repeat = {
            let (shape, position) = pointer_latest(&shared);
            let budget = crate::capture::frame_budget_for(shared.effective_fps());
            pointer_budget.arm(state.pointer.budget_end(
                budget,
                position,
                shape.map(|shape| shape.id),
            ));
            state.pointer.repeat_due(budget)
        };
        beat.mark(Phase::Waiting);
        tokio::select! {
            // Service timers and queued control before another paced frame.
            // Random branch selection can delay a NACK by multiple frame
            // bursts on platforms with coarse sleep granularity.
            biased;
            _ = tokio::time::sleep_until(state.fault.next_deadline().unwrap_or_else(|| Instant::now() + Duration::from_secs(3600)).into()), if state.fault.enabled() => {
                beat.mark(Phase::Video);
                let destination = *shared.target_addr.lock();
                for datagram in state.fault.drain_due(Instant::now()) {
                    let _ = links.send_to(&datagram, destination).await;
                }
            }
            _ = heartbeat.tick() => {
                state.pointer.refresh(now());
                beat.mark(Phase::Tick);
                let actions = shared.session.lock().tick(&config, true, now());
                act(actions, &mut state, &links, &shared, &config, &supervisor).await;
            }
            // The PC is going down and left a goodbye: send it now, not at
            // the next liveness tick, while Windows still waits.
            _ = shared.power.woken() => {
                beat.mark(Phase::Tick);
                let actions = shared.session.lock().tick(&config, false, now());
                act(actions, &mut state, &links, &shared, &config, &supervisor).await;
            }
            _ = network_check.tick() => {
                beat.mark(Phase::Tick);
                look_at_network(&shared, &listen_on, bind_ip, &supervisor);
            }
            _ = liveness_tick.tick() => {
                beat.mark(Phase::Tick);
                links.update_stats();
                links.congestion.lock().settle(now());
                ease_congestion(&links, &mut state, &shared, &config).await;
                if !shared.running.load(Ordering::SeqCst) {
                    info!("Transport loop stopping on running=false");
                    break;
                }
                let actions = shared.session.lock().tick(&config, false, now());
                act(actions, &mut state, &links, &shared, &config, &supervisor).await;
            }

            result = receive_control(&mut links, &mut deferred_control) => {
                beat.mark(Phase::Control);
                match result {
                    Ok(LinkEvent::Closed(id)) => {
                        let actions = shared.session.lock().link_lost(id);
                        act(actions, &mut state, &links, &shared, &config, &supervisor).await;
                    }
                    Ok(LinkEvent::Datagram(src, datagram)) => {
                        #[cfg(feature = "test-hooks")]
                        if let Some(addr) = src.addr.filter(|_| src.link == LinkId::Udp) {
                            probe::taken(addr);
                        }
                        let datagram = datagram.as_slice();
                        match classify(datagram) {
                            Classified::Control(_) => {
                                match eternal_wire::v2::control::parse_control(datagram) {
                                    Ok((header, message)) => {
                                        let actions = shared.session.lock().handle_datagram(
                                            src,
                                            &header,
                                            message,
                                            datagram.len(),
                                            &config,
                                            now(),
                                        );
                                        act(actions, &mut state, &links, &shared, &config, &supervisor).await;
                                    }
                                    Err(error) => {
                                        debug!(peer = %src, %error, "Dropped malformed control datagram");
                                    }
                                }
                            }
                            Classified::LegacyHello => {
                                shared.session.lock().note_legacy_hello(src)
                            }
                            Classified::Media { .. } | Classified::Audio { .. } | Classified::Unknown => {
                                debug!(peer = %src, len = datagram.len(), "Ignored unexpected datagram");
                            }
                        }
                    }
                    Err(e) => warn!(error = %e, "recv_from error"),
                }
            }

            // Before video: a position is small and the iPad draws it at once.
            _ = shared.pointer.changed() => {
                beat.mark(Phase::Pointer);
                send_pointer(&mut state.pointer, &links, &shared).await;
            }
            _ = pointer_budget.wait(), if pointer_budget.armed() => {
                beat.mark(Phase::Pointer);
                send_pointer(&mut state.pointer, &links, &shared).await;
            }
            _ = tokio::time::sleep_until(pointer_repeat.unwrap_or_else(Instant::now).into()), if pointer_repeat.is_some() => {
                beat.mark(Phase::Pointer);
                send_pointer(&mut state.pointer, &links, &shared).await;
            }
            Some(packet) = audio_rx.recv() => {
                beat.mark(Phase::Audio);
                audio_sender.send(packet, &links, &shared, stream_epoch).await;
            }
            nal_opt = nal_rx.recv(), if video_open => {
                beat.mark(Phase::Video);
                let Some(nal) = nal_opt else {
                    info!("The encoder closed its channel; the transport runs until the stream stops");
                    video_open = false;
                    continue;
                };
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
                    let started = shared.session.lock().client_info().map(|info| info.connected_at);
                    match first_frame_gate(nal.is_keyframe, nal.timestamp, started) {
                        FirstFrame::Send => {}
                        FirstFrame::Skip => continue, // the forced IDR is coming
                        FirstFrame::SkipAndRequestKeyframe => {
                            shared.force_next_idr.store(true, Ordering::SeqCst);
                            continue;
                        }
                    }
                }

                let payload = &nal.data;
                // The PC's packet size, or the smaller one this session's
                // client asked for when none of the larger reached it.
                let max_dgram = shared.session.lock().max_media_dgram(session_id).map_or(
                    shared.max_dgram.load(Ordering::SeqCst),
                    |asked| shared.max_dgram.load(Ordering::SeqCst).min(u32::from(asked)),
                );
                let payload_size = media_payload_size(max_dgram as usize).unwrap_or(MAX_MEDIA_PAYLOAD);
                let frag_count_usize = payload.len().div_ceil(payload_size).max(1);
                // Held back until the client answers; the frames after it
                // wait for the keyframe its answer asks for.
                let wire_bytes = payload.len() + frag_count_usize * MEDIA_HEADER_SIZE;
                if !shared.session.lock().may_send_media(session_id, wire_bytes, now()) {
                    awaiting_keyframe = true;
                    continue;
                }
                if awaiting_keyframe {
                    awaiting_keyframe = false;
                    shared.session.lock().note_video_sent(session_id, now());
                }

                frame_seq = frame_seq.wrapping_add(1);
                let send_start = Instant::now();
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
                let mut stored = Vec::with_capacity(frag_count_usize);
                for (index, chunk) in payload.chunks(payload_size).enumerate() {
                    let header = MediaHeader {
                        session_id,
                        stream_epoch,
                        frame_seq,
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
                    stored.push(datagram.to_vec());
                }
                // An early gap can be reported while the remaining originals
                // are still paced. Make this frame repairable before sending
                // its first batch, so a delayed pacing timer cannot hold NACKs.
                if keep_for_repair {
                    state.retransmit_ring.insert(stream_epoch, frame_seq, stored.clone());
                }

                let mut frame_pacer = FramePacer::new(frag_count_usize, Instant::now());
                let mut backed_off = false;
                for datagram in &stored {
                    // A busy machine can exhaust the pacing budget mid-frame.
                    // Keep serving audio even when there are no more pauses;
                    // otherwise a large IDR can fill its 60 ms capture queue.
                    audio_sender.send_queued(&mut audio_rx, &links, &shared, stream_epoch).await;
                    let mut refused = false;
                    let mut note = |sent: std::io::Result<()>| match sent {
                        Err(e) if congestion::is_no_buffer_space(&e) => refused = true,
                        Err(e) if !send_failed => {
                            log_send_failed(target_addr.link, &e);
                            send_failed = true;
                        }
                        _ => {}
                    };
                    if target_addr.link == LinkId::Udp && state.fault.enabled() {
                        for due in state.fault.push(datagram, Instant::now()) {
                            note(links.send_to(&due, target_addr).await);
                        }
                    } else {
                        note(links.send_to(datagram, target_addr).await);
                    }
                    // The system had no buffer space for it. A frame backs
                    // off once and sends it again; a refusal after that gives
                    // up the rest of the frame, which repair recovers.
                    if refused && backed_off {
                        break;
                    }
                    if refused {
                        backed_off = true;
                        ease_congestion(&links, &mut state, &shared, &config).await;
                    }

                    let paced = frame_pacer.after_send(Instant::now());
                    let pause = if refused { congestion::BACKOFF } else { paced };
                    if target_addr.link == LinkId::Udp && pause > Duration::ZERO {
                        let repairs = repair_while_pacing(&links, &mut state, &shared, &config, &supervisor,
                            pacing_clock.until(Instant::now() + pause), &mut deferred_control);
                        tokio::pin!(repairs);
                        loop {
                            tokio::select! {
                                biased;
                                _ = &mut repairs => break,
                                Some(packet) = audio_rx.recv() => {
                                    audio_sender.send(packet, &links, &shared, stream_epoch).await;
                                }
                            }
                        }
                    }
                    if refused {
                        match links.send_to(datagram, target_addr).await {
                            Err(e) if congestion::is_no_buffer_space(&e) => break,
                            Err(e) if !send_failed => {
                                log_send_failed(target_addr.link, &e);
                                send_failed = true;
                            }
                            _ => {}
                        }
                    }
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
                    seq = frame_seq,
                    capture_seq = nal.sequence,
                    fragments = frag_count,
                    total_bytes,
                    keyframe = nal.is_keyframe,
                    target = %target_addr,
                    "Frame sent"
                );
            }
        }
    }

    // Every way out of the loop comes here, before the links close: an update
    // or a Stop that just stopped the stream must still tell the iPad.
    beat.mark(Phase::Actions);
    let goodbye = shared.session.lock().take_goodbye();
    if let Some(goodbye) = goodbye {
        say_goodbye(&links, goodbye).await;
        shared.power.said();
    }
    park_stream_socket(&shared, links.into_udp_socket());
    info!("NAL channel closed, transport sender shutting down");
    Ok(())
}

/// The UDP socket iPads send HELLO2 to and the stream is sent from, bound
/// to `bind_addr`. The standby responder binds it the same way.
fn bind_stream_socket(bind_addr: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
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

/// The stream socket the last owner of the port left, if it is bound to
/// `bind_addr`. One bound elsewhere (the chosen network changed) closes.
fn take_stream_socket(
    shared: &SharedControl,
    bind_addr: SocketAddr,
) -> Option<std::net::UdpSocket> {
    shared
        .stream_socket
        .lock()
        .take()
        .filter(|socket| socket.local_addr().is_ok_and(|addr| addr == bind_addr))
}

/// Leaves the stream socket, still bound, for the next owner of the port.
fn park_stream_socket(shared: &SharedControl, socket: UdpSocket) {
    match socket.into_std() {
        Ok(socket) => *shared.stream_socket.lock() = Some(socket),
        Err(error) => debug!(%error, "Closed the stream port between owners"),
    }
}

/// Marks the transport ended however `start_sender` stops, so the watch
/// never reads a finished loop's last mark as a stuck one.
struct BeatEnds<'a>(&'a beat::Beat);

impl Drop for BeatEnds<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FirstFrame {
    Send,
    Skip,
    SkipAndRequestKeyframe,
}

/// Says which link failed but not which frame or fragment, so a failing link
/// repeats one identical line that the log collapser can fold.
fn log_send_failed(link: LinkId, error: &std::io::Error) {
    let link = match link {
        LinkId::Udp => "wifi",
        LinkId::Usb { .. } => "usb",
    };
    warn!(link = %link, error = %error, "UDP send failed");
}

/// A session's first frame must be a keyframe captured after it started. An
/// encoder can hold a frame for a long time when nothing is being encoded:
/// NVENC kept the host's startup frame while the PC sat idle, and the next
/// viewer received that old, blank keyframe as its first picture. A stale
/// keyframe is skipped and a fresh one requested.
pub(crate) fn first_frame_gate(
    is_keyframe: bool,
    captured: Instant,
    session_start: Option<Instant>,
) -> FirstFrame {
    if !is_keyframe {
        FirstFrame::Skip
    } else if session_start.is_some_and(|start| captured < start) {
        FirstFrame::SkipAndRequestKeyframe
    } else {
        FirstFrame::Send
    }
}

/// How long a goodbye over USB may hold up the exit while it is written.
const GOODBYE_FLUSH: Duration = Duration::from_millis(250);
/// Copies of a goodbye over UDP, where any one can be lost, and the pause
/// between them so a single burst of Wi-Fi loss can't take them all.
const GOODBYE_COPIES: usize = 3;
const GOODBYE_GAP: Duration = Duration::from_millis(5);

/// Tells the iPad that the host ended its stream (an update stopped it, or
/// it went silent), and returns once the message has left. Over UDP that is a short burst, all sent before this
/// returns and the socket closes; the iPad acts on the first. Over USB one
/// copy is queued for the link's writer, which dropping the link would abort,
/// so wait for it to be written.
async fn say_goodbye(links: &TransportLinks, (peer, datagram): (PeerId, Vec<u8>)) {
    let sent = if peer.link == LinkId::Udp {
        let mut sent = Ok(());
        for copy in 0..GOODBYE_COPIES {
            if copy > 0 {
                tokio::time::sleep(GOODBYE_GAP).await;
            }
            sent = links.send_to(&datagram, peer).await;
        }
        sent
    } else {
        match links.send_to(&datagram, peer).await {
            Ok(()) => links.flush(peer, GOODBYE_FLUSH).await,
            Err(error) => Err(error),
        }
    };
    match sent {
        Ok(()) => info!(%peer, "Told the iPad its stream ended"),
        Err(error) => warn!(%peer, %error, "Could not tell the iPad its stream ended"),
    }
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
    links: &TransportLinks,
    ring: &mut RetransmitRing,
    shared: &SharedControl,
    nack: &eternal_wire::v2::control::Nack,
) {
    let target = *shared.target_addr.lock();
    if target.link != LinkId::Udp || target.addr.is_none() {
        return;
    }
    let mut sent = 0;
    for datagram in ring.resend(
        nack.stream_epoch,
        nack.frame_seq,
        nack.frag_count,
        &nack.missing,
        Instant::now(),
    ) {
        // Through the links, so a refused repair counts in the episode.
        if links.send_to(&datagram, target).await.is_ok() {
            sent += 1;
        }
    }
    if sent > 0 {
        let mut stats = PIPELINE_STATS.lock();
        stats.transport_retransmits += sent;
        debug!(
            seq = nack.frame_seq,
            retransmits = stats.transport_retransmits,
            "NACK fragments resent"
        );
    }
}

/// A coarse platform timer may stretch a pacing gap beyond the receiver's
/// repair budget. Continue serving NACKs, including the current frame, throughout
/// that wait. Other control messages retain their order until the current
/// frame finishes; at most 64 packets (128 KiB) can wait here.
async fn repair_while_pacing(
    links: &TransportLinks,
    state: &mut LoopState,
    shared: &SharedControl,
    config: &impl ConfigSource,
    supervisor: &StreamSupervisor,
    timer: impl std::future::Future<Output = ()>,
    deferred: &mut DeferredControl,
) {
    use eternal_wire::v2::control::{parse_control, ControlMessage};
    tokio::pin!(timer);
    loop {
        let mut bytes = [0; 2048];
        tokio::select! {
            biased;
            _ = &mut timer => break,
            received = links.udp.socket.recv_from(&mut bytes) => {
                let Ok((len, peer)) = received else { break; };
                if let Ok((header, message @ ControlMessage::Nack(_))) = parse_control(&bytes[..len]) {
                    #[cfg(feature = "test-hooks")]
                    probe::taken(peer);
                    let actions = shared.session.lock().handle_datagram(PeerId::udp(peer), &header,
                        message, len, config, now());
                    act(actions, state, links, shared, config, supervisor).await;
                    continue;
                }
                if deferred.len() < 64 { deferred.push_back(LinkEvent::Datagram(PeerId::udp(peer), bytes[..len].to_vec())); }
            }
        }
    }
}

/// What the transport loop keeps that the session's decisions change.
struct LoopState {
    retransmit_ring: RetransmitRing,
    fault: FaultInjector,
    /// Hands input to Windows on its own thread: Windows' input calls can
    /// wait seconds while it reconfigures displays.
    input: crate::input::Injector,
    abr: AbrController,
    pointer: pointer::PointerSender,
}

/// Carries out everything the session decided. Every path that feeds the
/// session ends here, so none can drop a decision: a NACK served while a
/// frame was paced once sent only its repair, and lost the IDR of a session
/// heard again after a hold.
async fn act(
    mut actions: Actions,
    state: &mut LoopState,
    links: &TransportLinks,
    shared: &SharedControl,
    config: &impl ConfigSource,
    supervisor: &StreamSupervisor,
) {
    if actions.new_target.is_some() || actions.client_lost {
        state.retransmit_ring.clear();
        state.fault.clear();
    }
    if actions.new_target.is_some() || actions.client_lost || actions.release_input {
        state.input.reset();
    }
    if let Some(nack) = actions.retransmit.take() {
        send_repairs(links, &mut state.retransmit_ring, shared, &nack).await;
    }
    if let Some((session_id, event)) = actions.input.take() {
        shared.transport_beat.mark(Phase::Input);
        if let Some(geometry) = *shared.capture_geometry.lock() {
            state
                .input
                .relay(session_id, event, geometry, actions.input_late);
        }
    }
    if actions.force_idr || actions.new_target.is_some() {
        state.pointer.keyframe(now());
    }
    if let Some(report) = actions.report.as_ref() {
        state.pointer.report(report.pointer_shape_id, now());
        shared.note_pointer_report(report.pointer_shape_id);
    }
    if let Some(report) = actions.report.take() {
        // A slow log sink must not hold the capture/encode stats lock.
        let retransmits = PIPELINE_STATS.lock().transport_retransmits;
        info!(
            epoch = report.stream_epoch,
            complete = report.frames_complete,
            dropped = report.frames_dropped,
            repaired = report.frags_repaired,
            nacks = report.nacks_sent,
            jitter_us = report.jitter_us,
            decode_fps_x10 = report.decode_fps_x10,
            decode_depth = report.decode_depth,
            retransmits,
            frags_received = report.frags_received,
            frags_lost = report.frags_lost,
            rtt_ms_x10 = report.rtt_ms_x10,
            latency_ms_x10 = report.e2e_latency_ms_x10,
            audio_lost = report.audio_packets_lost,
            "Receiver report"
        );
        let ceiling = shared.bitrate_bps.load(Ordering::SeqCst);
        // A lowered ceiling clamps the rung immediately; that decision must
        // be honored, or dragging the slider down changes the label and
        // nothing else for the life of the pipeline.
        let clamped = state.abr.set_ceiling(ceiling);
        let tunnel_drops =
            (shared.target_addr.lock().link != LinkId::Udp).then(link::tunnel_congestion_drops);
        let reported = state.abr.on_report(&report, tunnel_drops, Instant::now());
        let decision = abr::AbrDecision {
            target_bps: reported.target_bps,
            changed: clamped.changed || reported.changed,
        };
        apply_bitrate(decision, links, shared, config).await;
    }
    execute_actions(actions, links, shared, supervisor).await;
    send_pointer(&mut state.pointer, links, shared).await;
}

/// The pointer to send: hidden until the iPad holds its image, while
/// capture still draws it into the video (`client_draws_pointer`). No image
/// and hidden while capture withholds it (`PointerFeed::set_withheld`).
fn pointer_latest(
    shared: &SharedControl,
) -> (
    Option<std::sync::Arc<crate::capture::pointer::Shape>>,
    Option<crate::capture::pointer::Position>,
) {
    let (shape, position) = shared.pointer.latest();
    let withheld = shared.pointer.withheld();
    let shown = shared.client_draws_pointer() && !withheld;
    let position = position.map(|position| crate::capture::pointer::Position {
        visible: position.visible && shown,
        ..position
    });
    (shape.filter(|_| !withheld), position)
}

/// Sends what [`pointer::PointerSender`] says is due, to a client that
/// draws the pointer.
async fn send_pointer(
    sender: &mut pointer::PointerSender,
    links: &TransportLinks,
    shared: &SharedControl,
) {
    let session_id = shared.session.lock().pointer_session_id();
    if session_id.is_some() && sender.session_id() != session_id {
        info!(
            ?session_id,
            "The iPad draws the pointer: sending it apart from the video"
        );
    }
    sender.serve(session_id);
    if session_id.is_none() || (shared.pointer.withheld() && !sender.withhold()) {
        return;
    }
    let budget = crate::capture::frame_budget_for(shared.effective_fps());
    for send in sender.poll(now(), budget, pointer_latest(shared)) {
        match send {
            pointer::Send::Shape(shape, from) => {
                let image = &shape.image;
                let chunks = eternal_wire::v2::control::pointer_shape_chunks(
                    shape.id,
                    image.source_type,
                    image.flags,
                    (image.width, image.height),
                    image.hot,
                    &image.bytes,
                );
                let total = chunks.len();
                let mut next = from;
                let mut refused = false;
                for chunk in chunks.into_iter().skip(from) {
                    let message = ControlMessage::PointerShape(chunk);
                    let Some((peer, datagram)) = shared.session.lock().encode_for_client(&message)
                    else {
                        return;
                    };
                    // A full USB control queue refuses it: resume here later.
                    if links.send_to(&datagram, peer).await.is_err() {
                        refused = true;
                        break;
                    }
                    next += 1;
                }
                if next >= total {
                    PIPELINE_STATS.lock().pointer_shapes_sent += 1;
                }
                sender.shape_progress(shape.id, next, total, refused, now());
            }
            pointer::Send::State(state) => {
                PIPELINE_STATS.lock().pointer_states_sent += 1;
                let message = ControlMessage::PointerState(state);
                let Some((peer, datagram)) = shared.session.lock().encode_for_client(&message)
                else {
                    return;
                };
                // A lost position is replaced by the next, or the repeat.
                let _ = links.send_to(&datagram, peer).await;
            }
        }
    }
}

/// Steps the bitrate down once for each episode of sends the system had no
/// buffer space for, without waiting for the iPad to report the loss.
async fn ease_congestion(
    links: &TransportLinks,
    state: &mut LoopState,
    shared: &SharedControl,
    config: &impl ConfigSource,
) {
    if links.congestion.lock().take_step_down() {
        let decision = state.abr.on_send_refused(Instant::now());
        apply_bitrate(decision, links, shared, config).await;
    }
}

async fn apply_bitrate(
    decision: abr::AbrDecision,
    links: &TransportLinks,
    shared: &SharedControl,
    config: &impl ConfigSource,
) {
    if decision.changed {
        shared
            .abr_current_bps
            .store(decision.target_bps, Ordering::SeqCst);
        PIPELINE_STATS.lock().set_bitrate(decision.target_bps);
        // One immediate notify; the 1s heartbeat's embedded config
        // self-heals any loss.
        let notify = shared.session.lock().stream_config_notify(config);
        for (destination, datagram) in notify {
            let _ = links.send_to(&datagram, destination).await;
        }
    }
}

async fn execute_actions(
    mut actions: Actions,
    links: &TransportLinks,
    shared: &SharedControl,
    supervisor: &StreamSupervisor,
) {
    shared.transport_beat.mark(Phase::Actions);
    // An ended session's summary waits for this pass's second screen step,
    // which can give the second screen up for it, unless a new session
    // starts now: second screen news from here on is the new one's.
    let mut ended = actions.ended.take();
    if actions.new_target.is_some() {
        if let Some(end) = ended.take() {
            crate::diagnostics::log_session_end(&end, shared);
        }
        if let Some(session_id) = shared.session.lock().session_id() {
            crate::diagnostics::note_session_start(session_id, shared);
        }
    }
    for (destination, datagram) in &actions.replies {
        let mut sent = links.send_to(datagram, *destination).await;
        // The system had no buffer space for it: one short retry. The
        // episode's own lines report it if that fails too.
        if sent.as_ref().is_err_and(congestion::is_no_buffer_space) {
            tokio::time::sleep(congestion::REPLY_RETRY).await;
            sent = links.send_to(datagram, *destination).await;
        }
        match sent {
            Err(e) if !congestion::is_no_buffer_space(&e) => {
                warn!(peer = %destination, error = %e, "Failed to send control reply");
            }
            _ => {}
        }
    }

    if let Some(target) = actions.new_target {
        let preference = shared
            .session
            .lock()
            .client_info()
            .map_or(0, |info| u32::from(info.preferred_fps));
        shared
            .client_preferred_fps
            .store(preference, Ordering::SeqCst);
        *shared.target_addr.lock() = target;
        {
            let mut stats = PIPELINE_STATS.lock();
            stats.set_target_addr(target.to_string());
            stats.reset_connection_stats();
        }
        if *shared.capture_target.lock() == CaptureTarget::VirtualExtended {
            second_screen::note_connected(shared, now());
        }
    }

    if let Some(goodbye) = actions.goodbye {
        say_goodbye(links, goodbye).await;
        shared.power.said();
    }

    if actions.force_idr {
        shared.force_next_idr.store(true, Ordering::SeqCst);
    }

    if actions.client_lost {
        shared.client_preferred_fps.store(0, Ordering::SeqCst);
        let unspecified = PeerId {
            link: LinkId::Udp,
            addr: None,
        };
        *shared.target_addr.lock() = unspecified;
        PIPELINE_STATS
            .lock()
            .set_target_addr("waiting for client".to_string());

        // The managed virtual display outlives its viewer by a grace period,
        // so a quick reconnect finds it, and a start in progress finishes.
        // `second_screen::next_step` turns it off once the grace period ends.
        if *shared.capture_target.lock() == CaptureTarget::VirtualExtended {
            match *shared.vdd_status.lock() {
                VddStatus::Active => info!(
                    grace_s = second_screen::RELEASE_GRACE.as_secs(),
                    "Client gone while streaming the virtual display — keeping it on in case \
                     the iPad reconnects"
                ),
                VddStatus::Attaching => {
                    info!("Client gone while the virtual display starts — letting the start finish")
                }
                _ => {}
            }
        }
    }

    shared.transport_beat.mark(Phase::SecondScreen);
    let restart = match second_screen::next_step(shared, now()) {
        second_screen::Step::Nothing => false,
        second_screen::Step::Attach => {
            info!(
                "Client connected with extended display selected — mirroring the primary \
                 display while the virtual display starts"
            );
            second_screen::send(shared, second_screen::Command::Attach);
            false
        }
        second_screen::Step::Switch => {
            info!("Virtual display ready — restarting pipeline to stream it");
            true
        }
        second_screen::Step::Release => {
            info!("No iPad came back to the virtual display — restarting pipeline to turn it off");
            true
        }
        second_screen::Step::Fallback(reason) => {
            warn!(
                second_screen_fallback = reason,
                prove_timeout_ms = second_screen::PROVE_TIMEOUT.as_millis() as u64,
                "The virtual display showed no picture — restarting pipeline to mirror the \
                 primary display instead"
            );
            second_screen::send(shared, second_screen::Command::Fallback(reason));
            true
        }
    };
    if let Some(end) = ended {
        crate::diagnostics::log_session_end(&end, shared);
    }
    if restart {
        shared.stop();
        if let Err(error) = supervisor.restart() {
            warn!(error = %error, "Failed to request pipeline restart for the virtual display");
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_new_session_starts_with_a_keyframe_captured_after_it_began() {
        let start = Instant::now();
        let before = start - Duration::from_secs(17);
        let after = start + Duration::from_millis(10);
        assert_eq!(
            first_frame_gate(false, after, Some(start)),
            FirstFrame::Skip
        );
        // The encoder's leftover startup IDR must not open the session.
        assert_eq!(
            first_frame_gate(true, before, Some(start)),
            FirstFrame::SkipAndRequestKeyframe
        );
        assert_eq!(first_frame_gate(true, after, Some(start)), FirstFrame::Send);
        assert_eq!(first_frame_gate(true, start, Some(start)), FirstFrame::Send);
    }

    use super::*;
    use eternal_wire::v2::control::{
        encode_control, parse_control, ControlMessage, Hello2, Nack, Ping, ReceiverReport,
        CAP_DECODE_H264, FEATURE_SUPPORTS_NACK,
    };

    /// No iPad came back to the virtual display within the grace period:
    /// the stream asks to restart itself, by its own generation, whichever
    /// generation the supervisor counts now.
    #[test]
    fn the_second_screen_release_restarts_only_this_stream() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let shared = SharedControl::new(socket.local_addr().unwrap().port(), 15_000_000);
            shared.running.store(true, Ordering::SeqCst);
            *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
            *shared.vdd_status.lock() = VddStatus::Active;
            // The viewer left a grace period ago.
            assert_eq!(
                second_screen::next_step(&shared, Instant::now() - second_screen::RELEASE_GRACE),
                second_screen::Step::Nothing
            );
            let links = TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
            let (supervisor_tx, supervisor_rx) = std_mpsc::channel();
            let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst) + 1000;
            let supervisor = StreamSupervisor {
                tx: supervisor_tx,
                generation,
            };
            execute_actions(Actions::default(), &links, &shared, &supervisor).await;
            assert_eq!(
                supervisor_rx.try_recv(),
                Ok(SupervisorCommand::RestartStream { generation })
            );
            assert!(!shared.running.load(Ordering::SeqCst));
            assert!(supervisor_rx.try_recv().is_err());
        });
    }

    /// The second screen gives up for a session in the same pass that ends
    /// it: the session's summary names that fallback.
    #[test]
    fn a_session_ended_with_a_fallback_in_one_pass_names_it() {
        use tracing_subscriber::prelude::*;
        let _slot = crate::diagnostics::tests::FALLBACK_SLOT.lock();
        let summaries = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        crate::logging::prepare_log_capture();
        let _log = tracing::subscriber::set_default(
            tracing_subscriber::registry()
                .with(crate::diagnostics::FallbackWatch)
                .with(crate::diagnostics::tests::SummaryCapture(summaries.clone())),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let shared = SharedControl::new(socket.local_addr().unwrap().port(), 15_000_000);
            shared.pairing.lock().required = false;
            *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
            // Streaming the virtual display already, so the session's pass has
            // nothing to start.
            *shared.vdd_status.lock() = VddStatus::Active;
            let links = TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
            let (supervisor_tx, _supervisor_rx) = std_mpsc::channel();
            let supervisor = StreamSupervisor {
                tx: supervisor_tx,
                generation: 1,
            };
            let config = SharedConfigSource {
                shared: &shared,
                stream_epoch: 1,
                pen_available: false,
                listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            };
            let peer = PeerId::udp("127.0.0.1:19893".parse().unwrap());
            let hello = ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 0xFA11,
                listen_port: 19893,
                decoder_caps: CAP_DECODE_H264,
                feature_caps: 0,
                screen_px_w: 1920,
                screen_px_h: 1080,
                screen_pt_w: 960,
                screen_pt_h: 540,
                refresh_hz: 60,
                device_name: "fallback test".into(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            });
            let now = Instant::now();
            let accepted = shared
                .session
                .lock()
                .handle_control(peer, 0, hello, &config, now);
            execute_actions(accepted, &links, &shared, &supervisor).await;
            // The virtual display attached and has shown no picture for too long.
            second_screen::note_attached(&shared, "virtual".into());
            second_screen::start_proving(&shared, 1, now - second_screen::PROVE_TIMEOUT);
            let session_id = shared.session.lock().session_id().unwrap();
            let bye = shared.session.lock().handle_control(
                peer,
                session_id,
                ControlMessage::Bye(eternal_wire::v2::control::ByeReason::UserDisconnect),
                &config,
                Instant::now(),
            );
            assert!(bye.ended.is_some());
            execute_actions(bye, &links, &shared, &supervisor).await;
        });
        let summaries = summaries.lock();
        assert_eq!(summaries.len(), 1, "{summaries:?}");
        assert!(
            summaries[0].contains(" nonce=000000000000fa11 ")
                && summaries[0].contains(" mode=second_screen ")
                && summaries[0].contains(" fallback=no_frames "),
            "{}",
            summaries[0]
        );
        crate::diagnostics::tests::assert_contract(&summaries[0]);
    }

    /// A socket on one address signs that address; one on every address
    /// signs the home network addresses. With none to sign, or IPv6, the PC
    /// answers nothing rather than an empty list.
    #[test]
    fn identity_endpoints_need_an_address_to_sign() {
        let all = SocketAddr::from(([0, 0, 0, 0], 9876));
        let home = || vec![std::net::Ipv4Addr::new(192, 0, 2, 4)];
        assert_eq!(
            identity_endpoints(all, home),
            Some((9876, vec![[192, 0, 2, 4]]))
        );
        assert_eq!(identity_endpoints(all, Vec::new), None);
        let one = SocketAddr::from(([198, 51, 100, 3], 9876));
        assert_eq!(
            identity_endpoints(one, || panic!("lists nothing")),
            Some((9876, vec![[198, 51, 100, 3]]))
        );
        let v6 = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 9876));
        assert_eq!(identity_endpoints(v6, home), None);
    }

    #[tokio::test]
    async fn repairs_current_and_previous_frames_before_the_pacing_wait_ends() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let host = socket.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer = client.local_addr().unwrap();
        let shared = SharedControl::new(host.port(), 15_000_000);
        shared.pairing.lock().required = false;
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
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
        let mut state = loop_state();
        for frame_seq in [7, 8] {
            let mut original = vec![0xAB; MEDIA_HEADER_SIZE + 1];
            MediaHeader {
                session_id,
                stream_epoch: 1,
                frame_seq,
                frag_index: 0,
                frag_count: 1,
                is_keyframe: false,
                is_retransmit: false,
                capture_ts_us: 1234,
                payload_len: 1,
            }
            .encode_into(&mut original);
            state.retransmit_ring.insert(1, frame_seq, vec![original]);
        }
        let pacing = tokio::spawn(async move {
            let config = SharedConfigSource {
                shared: &shared,
                stream_epoch: 1,
                pen_available: false,
                listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            };
            let links = TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
            let (supervisor_tx, _supervisor_rx) = std_mpsc::channel();
            let supervisor = StreamSupervisor {
                tx: supervisor_tx,
                generation: 1,
            };
            let mut deferred = DeferredControl::new();
            repair_while_pacing(
                &links,
                &mut state,
                &shared,
                &config,
                &supervisor,
                tokio::time::sleep(Duration::from_secs(1)),
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
        for seq in [7, 8] {
            client
                .send_to(&encode_control(session_id, 1, &nack(seq)), host)
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
            assert_eq!(header.frame_seq, seq);
            assert!(header.is_retransmit);
            assert_eq!(payload, &[0xAB]);
            assert!(
                !pacing.is_finished(),
                "repair was delayed until after pacing"
            );
        }

        let pending = [ControlMessage::Ping(Ping { t1_us: 42 })];
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

    fn loop_state() -> LoopState {
        LoopState {
            retransmit_ring: RetransmitRing::default(),
            fault: FaultInjector::new(0.0, 0.0, Duration::ZERO),
            input: crate::input::InputThread::default().begin(Default::default()),
            abr: AbrController::new(15_000_000, true),
            pointer: Default::default(),
        }
    }

    /// Review r2: the USB tunnel's control queue takes 64 datagrams and
    /// refuses more while its writer is blocked. A 256x256 image is 194
    /// chunks: they went out until the queue filled, the rest were dropped,
    /// and the image counted as sent, so the iPad never had it whole. The
    /// chunks now resume where the link refused them.
    #[tokio::test]
    async fn an_image_larger_than_the_usb_queue_arrives_whole_after_backpressure() {
        use crate::capture::pointer::{PointerImage, Position};
        use eternal_wire::v2::control::{parse_control, FEATURE_DRAWS_POINTER};
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let shared = SharedControl::new(socket.local_addr().unwrap().port(), 15_000_000);
        shared.pairing.lock().required = false;
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let mut links = TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
        let cable = PeerId::usb(5);
        let (host_end, mut ipad_end) = tokio::io::duplex(4096);
        let link = link::FramedLink::connect(
            host_end,
            cable,
            std::sync::Arc::clone(&shared.force_next_idr),
        )
        .await
        .unwrap();
        links.adopt_for_test(std::sync::Arc::new(link));
        link::read_preamble(&mut ipad_end).await.unwrap();
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 1,
            listen_port: 0,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_DRAWS_POINTER,
            screen_px_w: 1920,
            screen_px_h: 1080,
            screen_pt_w: 960,
            screen_pt_h: 540,
            refresh_hz: 60,
            device_name: "cable test".into(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        shared
            .session
            .lock()
            .handle_control(cable, 0, hello, &config, now());
        assert!(
            shared.session.lock().pointer_session_id().is_some(),
            "a cable counts as answered"
        );
        let (side, total_bytes) = (256u16, 256usize * 256 * 4);
        shared.pointer.set_shape(PointerImage {
            source_type: 2,
            flags: 0,
            width: side,
            height: side,
            hot: (0, 0),
            bytes: (0..total_bytes).map(|i| i as u8).collect(),
        });
        shared.pointer.set_position(Position {
            visible: true,
            x: 10,
            y: 10,
            host_time_us: 0,
        });
        let chunk_count = total_bytes.div_ceil(eternal_wire::v2::control::POINTER_CHUNK_MAX);
        assert!(chunk_count > 64, "{chunk_count} chunks");

        // Nobody reads the tunnel: the queue fills and refuses the rest.
        let mut sender = pointer::PointerSender::default();
        send_pointer(&mut sender, &links, &shared).await;
        let mut got = vec![false; chunk_count];
        let mut first = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut rounds = 0;
        while got.iter().any(|g| !g) {
            assert!(
                Instant::now() < deadline,
                "only {} of {chunk_count} chunks arrived",
                got.iter().filter(|g| **g).count()
            );
            // Drain what the tunnel holds, then let the sender go on.
            while let Ok(Ok(datagram)) = tokio::time::timeout(
                Duration::from_millis(20),
                link::read_datagram(&mut ipad_end),
            )
            .await
            {
                if let Ok((_, ControlMessage::PointerShape(chunk))) = parse_control(&datagram) {
                    got[chunk.offset as usize / eternal_wire::v2::control::POINTER_CHUNK_MAX] =
                        true;
                }
            }
            if rounds == 0 {
                first = got.iter().filter(|g| **g).count();
            }
            rounds += 1;
            tokio::time::sleep(pointer::SHAPE_RETRY).await;
            send_pointer(&mut sender, &links, &shared).await;
        }
        assert!(
            first < chunk_count,
            "the queue refused some at first ({first} of {chunk_count})"
        );
        assert!(rounds > 1);
    }

    /// A cable pulled while a key and a mouse button are down ends the USB
    /// session, and both go up, as at every other end of a session.
    #[cfg(not(windows))]
    #[tokio::test]
    // The slot only keeps other tests out; nothing on this test's runtime
    // takes it.
    #[allow(clippy::await_holding_lock)]
    async fn a_usb_session_ended_by_its_cable_releases_held_input() {
        use crate::input::{
            recorder, CaptureGeometry, Injection, KIND_KEY, KIND_MOUSE_ABS, PHASE_BEGAN,
        };
        use eternal_wire::v2::control::{InputEvent, FEATURE_WANTS_INPUT};
        // The session starts and ends through `act`, which uses the fallback slot.
        let _slot = crate::diagnostics::tests::FALLBACK_SLOT.lock();
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let shared = SharedControl::new(socket.local_addr().unwrap().port(), 15_000_000);
        shared.pairing.lock().required = false;
        *shared.capture_geometry.lock() = Some(CaptureGeometry {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        });
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let links = TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
        let (supervisor_tx, _supervisor_rx) = std_mpsc::channel();
        let supervisor = StreamSupervisor {
            tx: supervisor_tx,
            generation: 1,
        };
        let mut state = loop_state();
        let cable = PeerId::usb(5);
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 1,
            listen_port: 0,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_WANTS_INPUT,
            screen_px_w: 1920,
            screen_px_h: 1080,
            screen_pt_w: 960,
            screen_pt_h: 540,
            refresh_hz: 60,
            device_name: "cable test".into(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let accepted = shared
            .session
            .lock()
            .handle_control(cable, 0, hello, &config, now());
        act(accepted, &mut state, &links, &shared, &config, &supervisor).await;
        let session_id = shared.session.lock().session_id().unwrap();
        let press = |event_id, kind, keycode| {
            ControlMessage::InputEvent(InputEvent {
                input_ver: 1,
                kind,
                phase: PHASE_BEGAN,
                buttons: 0,
                event_id,
                x_norm: 32768,
                y_norm: 32768,
                pressure_x1000: 0,
                scroll_dx: 0,
                scroll_dy: 0,
                keycode,
                modifiers: 0,
                client_time_us: 0,
                tilt_x: 0,
                tilt_y: 0,
            })
        };
        for event in [press(1, KIND_KEY, 0x04), press(2, KIND_MOUSE_ABS, 0)] {
            let pressed =
                shared
                    .session
                    .lock()
                    .handle_control(cable, session_id, event, &config, now());
            act(pressed, &mut state, &links, &shared, &config, &supervisor).await;
        }
        // Windows takes input on the input thread.
        let injected = |count: usize| {
            let started = Instant::now();
            while recorder::peek().len() < count && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(1));
            }
            recorder::take()
        };
        assert!(matches!(
            injected(3).as_slice(),
            [
                Injection::KeyDown { .. },
                Injection::MoveAbs { .. },
                Injection::LeftDown { .. }
            ]
        ));

        let unplugged = shared.session.lock().link_lost(cable.link);
        assert!(unplugged.client_lost);
        act(unplugged, &mut state, &links, &shared, &config, &supervisor).await;
        assert!(shared.session.lock().session_id().is_none());
        assert!(matches!(
            injected(2).as_slice(),
            [Injection::KeyUp { .. }, Injection::LeftUp { .. }]
        ));
    }

    /// A UDP session that asked for repairs, started at `start` and with a
    /// picture a second later.
    fn repairable_session(shared: &SharedControl, peer: SocketAddr, start: Instant) -> u32 {
        let config = SharedConfigSource {
            shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
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
            device_name: "repair hold test".into(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let mut session = shared.session.lock();
        let actions = session.handle_control(PeerId::udp(peer), 0, hello, &config, start);
        *shared.target_addr.lock() = actions.new_target.unwrap();
        let session_id = session.session_id().unwrap();
        let report = ControlMessage::ReceiverReport(ReceiverReport {
            frames_complete: 60,
            ..ReceiverReport::default()
        });
        session.handle_control(
            PeerId::udp(peer),
            session_id,
            report,
            &config,
            start + Duration::from_secs(1),
        );
        session_id
    }

    /// Serves one NACK from `client` while a frame of three fragments is
    /// paced, and returns the shared state once pacing ends, with what
    /// reached the client.
    async fn nack_while_pacing(
        socket: UdpSocket,
        client: &UdpSocket,
        shared: &SharedControl,
        session_id: u32,
    ) -> Vec<Vec<u8>> {
        let host = socket.local_addr().unwrap();
        let mut state = loop_state();
        let fragments = (0..3)
            .map(|frag_index| {
                let mut original = vec![0xAB; MEDIA_HEADER_SIZE + 1];
                MediaHeader {
                    session_id,
                    stream_epoch: 1,
                    frame_seq: 9,
                    frag_index,
                    frag_count: 3,
                    is_keyframe: false,
                    is_retransmit: false,
                    capture_ts_us: 1234,
                    payload_len: 1,
                }
                .encode_into(&mut original);
                original
            })
            .collect();
        state.retransmit_ring.insert(1, 9, fragments);
        let (end_pacing, paced) = tokio::sync::oneshot::channel::<()>();
        let pacing = tokio::spawn({
            let shared = shared.clone();
            async move {
                let config = SharedConfigSource {
                    shared: &shared,
                    stream_epoch: 1,
                    pen_available: false,
                    listen: SocketAddr::from(([127, 0, 0, 1], 0)),
                };
                let links =
                    TransportLinks::new(socket, std::sync::Arc::clone(&shared.force_next_idr));
                let (supervisor_tx, _supervisor_rx) = std_mpsc::channel();
                let supervisor = StreamSupervisor {
                    tx: supervisor_tx,
                    generation: 1,
                };
                let mut deferred = DeferredControl::new();
                repair_while_pacing(
                    &links,
                    &mut state,
                    &shared,
                    &config,
                    &supervisor,
                    async {
                        let _ = paced.await;
                    },
                    &mut deferred,
                )
                .await;
                deferred
            }
        });
        let nack = ControlMessage::Nack(Nack {
            stream_epoch: 1,
            frame_seq: 9,
            frag_count: 3,
            missing: vec![1],
        });
        client
            .send_to(&encode_control(session_id, 7, &nack), host)
            .await
            .unwrap();
        let mut received = Vec::new();
        let mut buffer = [0; 2048];
        while let Ok(Ok((len, _))) =
            tokio::time::timeout(Duration::from_millis(300), client.recv_from(&mut buffer)).await
        {
            received.push(buffer[..len].to_vec());
        }
        assert!(!pacing.is_finished(), "pacing ended early");
        end_pacing.send(()).unwrap();
        assert!(pacing.await.unwrap().is_empty(), "the NACK was deferred");
        received
    }

    /// A held session's first packet back can be a NACK for a frame that
    /// is still being paced out. Being heard carries the session on with an
    /// IDR, for the picture the stall damaged: the encoder must be asked for
    /// it, not only the fragment resent.
    #[tokio::test]
    async fn a_nack_while_pacing_that_ends_a_hold_asks_the_encoder_for_an_idr() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let shared = SharedControl::new(socket.local_addr().unwrap().port(), 15_000_000);
        shared.pairing.lock().required = false;
        let now = Instant::now();
        let session_id = repairable_session(
            &shared,
            client.local_addr().unwrap(),
            now - Duration::from_secs(5),
        );
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let held = shared
            .session
            .lock()
            .tick(&config, false, now - Duration::from_millis(500));
        assert!(held.release_input && shared.session.lock().on_hold());
        shared.force_next_idr.store(false, Ordering::SeqCst);

        let received = nack_while_pacing(socket, &client, &shared, session_id).await;
        assert_eq!(received.len(), 1, "one repair");
        let (header, _) = MediaHeader::decode(&received[0]).unwrap();
        assert!(header.is_retransmit && (header.frame_seq, header.frag_index) == (9, 1));
        assert!(!shared.session.lock().on_hold());
        assert!(
            shared.force_next_idr.load(Ordering::SeqCst),
            "the resume IDR never reached the encoder"
        );
    }

    /// While the lock screen or a permission prompt keeps Windows from
    /// letting capture work, every heartbeat tells the iPad the PC is locked.
    /// Once capture works again, heartbeats say nothing more.
    #[test]
    fn heartbeats_say_the_pc_is_locked_while_a_secure_screen_blocks_capture() {
        use crate::capture::recovery::{
            note_capture_resumed, run_until_unblocked, CaptureBlock, CaptureUnavailable,
        };
        use eternal_wire::v2::control::{NOTICE_NONE, NOTICE_PC_LOCKED};
        let shared = SharedControl::new(9876, 15_000_000);
        shared.pairing.lock().required = false;
        let now = Instant::now();
        repairable_session(&shared, "127.0.0.1:50000".parse().unwrap(), now);
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let notices = || {
            let actions = shared.session.lock().tick(&config, true, now);
            actions
                .replies
                .iter()
                .filter_map(|(_, bytes)| match parse_control(bytes) {
                    Ok((_, ControlMessage::Heartbeat(heartbeat))) => Some(heartbeat.notice),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(notices(), [NOTICE_NONE]);
        // Windows answers duplication with E_ACCESSDENIED until the user is
        // back on the desktop.
        let mut attempts = 0;
        let mut while_blocked = Vec::new();
        let result = run_until_unblocked(
            &shared,
            |_| {
                attempts += 1;
                if attempts < 3 {
                    Err(CaptureUnavailable {
                        block: CaptureBlock::from_hresult(0x8007_0005_u32 as i32).unwrap(),
                        detail: "Access is denied".into(),
                    }
                    .into())
                } else {
                    Ok(())
                }
            },
            |_| {
                while_blocked.extend(notices());
                true
            },
        );
        assert!(result.is_ok());
        assert_eq!(while_blocked, [NOTICE_PC_LOCKED, NOTICE_PC_LOCKED]);
        note_capture_resumed(&shared);
        assert_eq!(notices(), [NOTICE_NONE]);
    }

    /// On Windows 10 a Second screen session mirrors, and its heartbeats
    /// tell the iPad why. Windows 11 starts the second screen with no
    /// such notice.
    #[test]
    fn heartbeats_say_second_screen_needs_windows_11_on_windows_10() {
        use eternal_wire::v2::control::{NOTICE_NONE, NOTICE_SECOND_SCREEN_NEEDS_WINDOWS_11};
        for (build, step, notice) in [
            (
                19045,
                second_screen::Step::Nothing,
                NOTICE_SECOND_SCREEN_NEEDS_WINDOWS_11,
            ),
            (26200, second_screen::Step::Attach, NOTICE_NONE),
        ] {
            let shared = SharedControl::new(9876, 15_000_000);
            shared.pairing.lock().required = false;
            *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
            *shared.vdd_status.lock() = VddStatus::WaitingForClient;
            second_screen::note_windows(&shared, Some(build), false);
            let now = Instant::now();
            repairable_session(&shared, "127.0.0.1:50000".parse().unwrap(), now);
            let config = SharedConfigSource {
                shared: &shared,
                stream_epoch: 1,
                pen_available: false,
                listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            };
            let notices = || {
                let actions = shared.session.lock().tick(&config, true, now);
                actions
                    .replies
                    .iter()
                    .filter_map(|(_, bytes)| match parse_control(bytes) {
                        Ok((_, ControlMessage::Heartbeat(heartbeat))) => Some(heartbeat.notice),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(notices(), [NOTICE_NONE], "{build}");
            assert_eq!(second_screen::next_step(&shared, now), step, "{build}");
            assert_eq!(notices(), [notice], "{build}");
        }
    }

    /// A screen Windows won't share from this graphics card: the heartbeats
    /// say so once it has lasted 2 s, and in Second screen mode, once the
    /// second screen failed, that the main screen it fell back to can't be
    /// shared either. While the second screen still starts, the mirror's
    /// notice stays.
    #[test]
    fn heartbeats_say_which_screen_this_graphics_card_cannot_share() {
        use crate::capture::recovery::CaptureBlock;
        use crate::control::{CaptureTarget, VddStatus};
        use eternal_wire::v2::control::{
            NOTICE_NONE, NOTICE_SECOND_SCREEN_UNSUPPORTED_GPU, NOTICE_UNSUPPORTED_GPU,
        };
        let shared = SharedControl::new(9876, 15_000_000);
        shared.pairing.lock().required = false;
        let now = Instant::now();
        repairable_session(&shared, "127.0.0.1:50000".parse().unwrap(), now);
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let notices = || {
            let actions = shared.session.lock().tick(&config, true, now);
            actions
                .replies
                .iter()
                .filter_map(|(_, bytes)| match parse_control(bytes) {
                    Ok((_, ControlMessage::Heartbeat(heartbeat))) => Some(heartbeat.notice),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        *shared.capture_block.lock() = Some(CaptureBlock::UnsupportedOnThisGpu);
        assert_eq!(notices(), [NOTICE_NONE], "a refusal that just began");
        *shared.gpu_refused_since.lock() = Some(Instant::now() - Duration::from_secs(3));
        assert_eq!(notices(), [NOTICE_UNSUPPORTED_GPU]);
        *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
        *shared.vdd_status.lock() = VddStatus::Attaching;
        assert_eq!(notices(), [NOTICE_UNSUPPORTED_GPU], "still starting");
        for failed in [VddStatus::Failed, VddStatus::TaskFailed] {
            *shared.vdd_status.lock() = failed;
            assert_eq!(
                notices(),
                [NOTICE_SECOND_SCREEN_UNSUPPORTED_GPU],
                "{failed:?}"
            );
        }
        *shared.capture_block.lock() = None;
        assert_eq!(notices(), [NOTICE_NONE]);
        assert_eq!(*shared.gpu_refused_since.lock(), None);
    }

    /// A NACK of a session that ended for liveness, while another frame is
    /// paced, is answered with the goodbye, as any of its late packets is.
    #[tokio::test]
    async fn a_nack_while_pacing_from_a_session_ended_for_liveness_gets_the_goodbye() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let shared = SharedControl::new(socket.local_addr().unwrap().port(), 15_000_000);
        shared.pairing.lock().required = false;
        let now = Instant::now();
        let session_id = repairable_session(
            &shared,
            client.local_addr().unwrap(),
            now - Duration::from_secs(15),
        );
        let config = SharedConfigSource {
            shared: &shared,
            stream_epoch: 1,
            pen_available: false,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let expired = shared
            .session
            .lock()
            .tick(&config, false, now - Duration::from_secs(2));
        assert!(expired.client_lost && expired.goodbye.is_some());

        let received = nack_while_pacing(socket, &client, &shared, session_id).await;
        let replies: Vec<_> = received
            .iter()
            .map(|bytes| parse_control(bytes).unwrap())
            .map(|(header, message)| (header.session_id, message))
            .collect();
        assert_eq!(
            replies,
            [(
                session_id,
                ControlMessage::Bye(eternal_wire::v2::control::ByeReason::Error)
            )]
        );
    }

    /// Windows holds the input thread inside a still Pencil's refresh. The
    /// loop takes the capture rect for every input datagram, so a refresh
    /// that kept it while Windows held the call stopped the loop at the
    /// iPad's next touch: no heartbeats, no HELLO answers, no goodbye.
    #[test]
    fn a_pen_refresh_windows_holds_never_stops_the_loop() {
        use crate::input::{CaptureGeometry, InputThread, Sink, KIND_KEY, PHASE_BEGAN};
        use eternal_wire::v2::control::{HelloStatus, InputEvent, FEATURE_WANTS_INPUT};
        // The loop's session starts through `act`, which uses the fallback slot.
        let _slot = crate::diagnostics::tests::FALLBACK_SLOT.lock();

        /// Holds the first refresh until told to go on.
        struct StuckRefresh {
            entered: Option<std_mpsc::Sender<()>>,
            hold: Option<std_mpsc::Receiver<()>>,
        }
        impl Sink for StuckRefresh {
            fn relay(&mut self, _: u32, _: &InputEvent, _: CaptureGeometry, _: bool) {}
            fn reset(&mut self) {}
            fn keep_alive(&mut self, _: Option<CaptureGeometry>) {
                if let Some(entered) = self.entered.take() {
                    let _ = entered.send(());
                }
                if let Some(hold) = self.hold.take() {
                    let _ = hold.recv();
                }
            }
            fn pen_available(&self) -> bool {
                true
            }
            fn spaced_until(&self) -> Option<Instant> {
                None
            }
            fn tick_needed(&self) -> bool {
                true
            }
        }

        let ipad = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        ipad.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|s| s.local_addr())
            .unwrap()
            .port();
        let host = SocketAddr::from(([127, 0, 0, 1], port));
        let (entered_tx, entered) = std_mpsc::channel();
        let (go, hold) = std_mpsc::channel();
        let mut shared = SharedControl::new(port, 15_000_000);
        shared.input = std::sync::Arc::new(InputThread::with_fake(move || StuckRefresh {
            entered: Some(entered_tx),
            hold: Some(hold),
        }));
        shared.pairing.lock().required = false;
        shared.running.store(true, Ordering::SeqCst);
        *shared.capture_geometry.lock() = Some(CaptureGeometry {
            left: 0,
            top: 0,
            width: 1280,
            height: 720,
        });
        let (nal_tx, nal_rx) = mpsc::channel(4);
        let control = shared.clone();
        let transport = std::thread::spawn(move || {
            let (_audio_tx, audio_rx) = mpsc::channel(4);
            let (supervisor_tx, _supervisor_rx) = std_mpsc::channel();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(start_sender(
                    nal_rx,
                    audio_rx,
                    port,
                    control,
                    supervisor_tx,
                    1,
                ))
                .unwrap();
        });
        // The refresh ticks from the stream's start; Windows holds it.
        entered
            .recv_timeout(Duration::from_secs(5))
            .expect("the input thread refreshes the pen");

        let mut msg_seq = 0;
        let mut send = |session_id, message: &ControlMessage| {
            msg_seq += 1;
            ipad.send_to(&encode_control(session_id, msg_seq, message), host)
                .unwrap();
        };
        let heard = |until: Instant| {
            let mut messages = Vec::new();
            let mut bytes = [0u8; 2048];
            while Instant::now() < until {
                if let Ok((len, _)) = ipad.recv_from(&mut bytes) {
                    if let Ok((_, message)) = parse_control(&bytes[..len]) {
                        messages.push(message);
                    }
                }
            }
            messages
        };
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 1,
            listen_port: 0,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_WANTS_INPUT,
            screen_px_w: 1280,
            screen_px_h: 720,
            screen_pt_w: 1280,
            screen_pt_h: 720,
            refresh_hz: 60,
            device_name: "Pen refresh test".into(),
            device_id: 1,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let started = Instant::now();
        let session_id = loop {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "no HELLO answer"
            );
            send(0, &hello);
            let accepted = heard(Instant::now() + Duration::from_millis(200))
                .into_iter()
                .find_map(|message| match message {
                    ControlMessage::HelloAck(ack) if ack.status == HelloStatus::Ok => {
                        Some(ack.session_id)
                    }
                    _ => None,
                });
            if let Some(id) = accepted {
                break id;
            }
        };

        // The iPad keeps typing while Windows holds the refresh; the loop
        // keeps its heartbeats going.
        let mut watch = beat::Watch::default();
        let mut verdicts = Vec::new();
        let mut heartbeats = 0;
        let until = Instant::now() + Duration::from_secs(3);
        let mut event_id = 0;
        while Instant::now() < until {
            event_id += 1;
            send(
                session_id,
                &ControlMessage::InputEvent(InputEvent {
                    input_ver: 1,
                    kind: KIND_KEY,
                    phase: PHASE_BEGAN,
                    buttons: 0,
                    event_id,
                    x_norm: 0,
                    y_norm: 0,
                    pressure_x1000: 0,
                    scroll_dx: 0,
                    scroll_dy: 0,
                    keycode: 0x01,
                    modifiers: 0,
                    client_time_us: 0,
                    tilt_x: 0,
                    tilt_y: 0,
                }),
            );
            heartbeats += heard(Instant::now() + Duration::from_millis(100))
                .iter()
                .filter(|message| matches!(message, ControlMessage::Heartbeat(_)))
                .count();
            verdicts.extend(watch.check(
                shared.transport_beat.read(),
                crate::clock::host_now_us() / 1000,
            ));
        }
        let stuck = heartbeats < 2 || !verdicts.is_empty();
        if stuck {
            // Let a stuck loop go, so the test ends.
            let _ = go.send(());
        }
        assert!(
            heartbeats >= 2,
            "heartbeats while Windows held the refresh: {heartbeats}"
        );
        assert_eq!(verdicts, []);
        shared.stop();
        drop(nal_tx);
        transport.join().unwrap();
        let _ = go.send(());
    }

    #[test]
    fn a_failed_send_names_the_link_and_repeats_identically() {
        let out = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
        struct Sink(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer = std::sync::Arc::clone(&out);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(move || Sink(std::sync::Arc::clone(&writer)))
            .finish();
        let error = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        crate::logging::prepare_log_capture();
        tracing::subscriber::with_default(subscriber, || {
            log_send_failed(LinkId::Usb { device_id: 3 }, &error);
            log_send_failed(LinkId::Usb { device_id: 3 }, &error);
            log_send_failed(LinkId::Udp, &error);
        });
        let text = String::from_utf8(out.lock().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(lines[0], lines[1], "the collapser needs identical lines");
        assert_eq!(
            lines[0].trim(),
            "WARN UDP send failed link=usb error=connection reset"
        );
        assert_eq!(
            lines[2].trim(),
            "WARN UDP send failed link=wifi error=connection reset"
        );
    }
}
