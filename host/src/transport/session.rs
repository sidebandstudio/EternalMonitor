//! Single-client protocol-v2 session state machine.
//!
//! Pure logic with an injected clock — the transport task feeds it inbound
//! control messages and time ticks; it returns actions (reply datagrams,
//! shared-state changes) for the transport to execute. This keeps every
//! rule (busy rejection, supersede-in-place, duplicate-ACK idempotency,
//! liveness hold and expiry, keyframe-request rate limiting) unit-testable
//! without sockets.

use std::net::SocketAddr;

use super::link::{LinkId, PeerId};
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{
    ByeReason, ControlHeader, ControlMessage, HelloAck, HelloStatus, InputEvent, KeyframeRequest,
    Nack, ReceiverReport, StreamConfig, FEATURE_SUPPORTS_NACK, FEATURE_WANTS_INPUT, HOSTCAP_NACK,
};
use tracing::{info, warn};

/// Host-dictated timing, advertised to the client in HELLO_ACK.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);
pub const REPORT_INTERVAL_MS: u16 = 500;
pub const LIVENESS_TIMEOUT: Duration = Duration::from_millis(3000);
/// How long after its last packet a client that had a picture keeps its
/// session. Host-only: iPads get [`LIVENESS_TIMEOUT`], which drives their
/// own check. Past that window the session is on hold: held input is
/// released and another iPad may take the slot, but video and heartbeats go
/// on, so an iPad whose uplink stalls while it still decodes never notices.
/// 10 s is longer than an iPad waits for a silent PC (3 s, then 5 s more)
/// and than the uplink stalls of a congested Wi-Fi channel.
pub const HOLD_LIMIT: Duration = Duration::from_secs(10);
const USB_TAKEOVER_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the late packets of a session ended for liveness get its
/// goodbye again, at most one copy per [`ENDED_REPLY_GAP`].
const ENDED_REPLY_FOR: Duration = Duration::from_secs(60);
const ENDED_REPLY_GAP: Duration = Duration::from_secs(1);
/// Input that reaches the host this much later than the quickest packet of
/// its session, by the iPad's own clock, waited in a stalled uplink. The
/// relay drops it: a click or key that lands seconds later can land in
/// another window. The Pencil's own staleness limit is the same.
pub const INPUT_LATE: Duration = Duration::from_secs(1);
/// How fast the session lets the iPad's clock run slow against the host's,
/// in microseconds per second: far more than any clock drifts, and far less
/// than [`INPUT_LATE`] over a stall.
const CLOCK_DRIFT_US_PER_S: u64 = 1000;
/// Refused nonces remembered, and for how long: far past an iPad's 2 s of
/// HELLO2 retransmits.
const REFUSED_KEPT: usize = 64;
const REFUSED_FOR: Duration = Duration::from_secs(60);
/// Refused attempts summarized per minute. Unauthenticated traffic can't
/// fill the support log: the rest are counted in one line a minute.
const REFUSED_SUMMARIES: u32 = 20;
const REFUSAL_MINUTE: Duration = Duration::from_secs(60);

/// The refused attempts of the current minute.
#[derive(Debug, Clone, Copy)]
struct RefusalMinute {
    since: Instant,
    summarized: u32,
    left_out: u32,
}
/// Honor at most one keyframe request per this window (PLI storm guard).
pub const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// What the transport must do after feeding the session an event.
#[derive(Debug, Default)]
pub struct Actions {
    /// Serialized control datagrams to send, with their destination.
    pub replies: Vec<(PeerId, Vec<u8>)>,
    /// The media target changed (new session or takeover): update
    /// `SharedControl::target_addr` and reset connection stats.
    pub new_target: Option<PeerId>,
    /// Ask the encoder for an IDR (new/superseded session, keyframe request,
    /// a client heard again after a hold).
    pub force_idr: bool,
    /// The client is gone (BYE or liveness expiry): stop sending media and, if
    /// the virtual display is in use, restart the pipeline so it tears down.
    pub client_lost: bool,
    /// The client went silent and its session is on hold: release held keys,
    /// mouse buttons and the pen, as for a lost client, but keep streaming.
    pub release_input: bool,
    /// A fresh receiver report arrived (feeds the ABR controller).
    pub report: Option<ReceiverReport>,
    /// A validated `(session_id, event)` from the connected client (only
    /// produced when that client's HELLO2 asked for input relay). The
    /// transport dedupes per session and injects.
    pub input: Option<(u32, InputEvent)>,
    /// That input arrived over [`INPUT_LATE`] late: the relay drops it,
    /// except a release of what the PC still holds.
    pub input_late: bool,
    /// Negotiated and session-gated request; the transport checks frag_count
    /// against its stored datagrams before sending anything.
    pub retransmit: Option<Nack>,
    /// The host ended this client's session (an update stopped it, or it
    /// went silent): tell it, making sure the message leaves (see
    /// [`Session::begin_update`]).
    pub goodbye: Option<(PeerId, Vec<u8>)>,
    /// A session, or a HELLO2 the host turned down, ended: its summary line
    /// is due (`crate::diagnostics::log_session_end`).
    pub ended: Option<SessionEnd>,
}

/// How far a session got and how it ended, for its support summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnd {
    /// `None` for a HELLO2 the host turned down.
    pub session_id: Option<u32>,
    /// The nonce of the client's HELLO2, which its own summary also names.
    pub client_nonce: u32,
    pub link: LinkId,
    pub reason: EndReason,
    /// From the session's start to the first video sent to it.
    pub first_video: Option<Duration>,
    /// The client's receiver reports counted complete frames.
    pub frames_received: bool,
    pub duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// The HELLO2 was answered with this refusal.
    Refused(HelloStatus),
    /// The client said goodbye, including a Wi-Fi goodbye whose cable
    /// takeover never came.
    Goodbye(ByeReason),
    Liveness,
    /// The same device connected again.
    Superseded,
    /// An update stopped the stream, or the app quit.
    Shutdown,
    /// The session's link (the USB tunnel) closed.
    LinkLost,
    /// The pairing credentials changed.
    Revoked,
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub device_name: String,
    pub device_id: u64,
    pub screen_px: (u16, u16),
    pub refresh_hz: u8,
    pub preferred_fps: u8,
    pub decoder_caps: u16,
    pub feature_caps: u16,
    pub connected_at: Instant,
}

struct ActiveSession {
    session_id: u32,
    accepted_hello: eternal_wire::v2::control::Hello2,
    ack: Vec<u8>,
    auth_token: [u8; 16],
    peer: PeerId,
    info: ClientInfo,
    liveness_deadline: Instant,
    /// When the client was last heard, and what it sent.
    heard: (Instant, Heard),
    /// When the session went on hold (see [`HOLD_LIMIT`]).
    held_since: Option<Instant>,
    /// The quickest packet's one-way offset, in microseconds of host time
    /// since the session started less the iPad's timestamp, and when it was
    /// last updated (see [`ActiveSession::lateness`]).
    client_clock: Option<(i64, Instant)>,
    late_input: LateInput,
    awaiting_takeover: bool,
    last_keyframe_grant: Option<Instant>,
    last_report: Option<ReceiverReport>,
    msg_seq_out: u32,
    /// When the first video went to this session.
    first_video_at: Option<Instant>,
    /// A receiver report counted complete frames.
    frames_received: bool,
    /// A Wi-Fi goodbye while the cable takes over.
    bye: Option<ByeReason>,
    /// The msg_seq of input packets taken in this session.
    input_seqs: SeqWindow,
}

/// What a client sent last, for the liveness log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    Hello,
    Report,
    Nack,
    Ping,
    Input,
    Keyframe,
}

impl Heard {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Report => "report",
            Self::Nack => "nack",
            Self::Ping => "ping",
            Self::Input => "input",
            Self::Keyframe => "keyframe",
        }
    }
}

/// Input that arrived over [`INPUT_LATE`] late, counted for the log.
#[derive(Debug, Default)]
struct LateInput {
    /// Since the last count logged.
    pending: u32,
    max_late: Duration,
    last_at: Option<Instant>,
    /// In the whole session.
    total: u32,
}

/// The last session the host ended for liveness, whose late packets get its
/// goodbye again: the iPad may have missed the first copies.
struct EndedSession {
    session_id: u32,
    peer: PeerId,
    goodbye: Vec<u8>,
    ended_at: Instant,
    last_sent: Instant,
}

/// How far behind the newest input msg_seq a late packet may arrive.
const SEQ_WINDOW: u32 = 1024;

/// The msg_seq values of the last [`SEQ_WINDOW`] input packets. The iPad
/// numbers every control packet, each copy of a press or release too, so
/// a value seen before is a repeat. Wrapping u32 order, as the iPad's
/// counter wraps.
#[derive(Default)]
struct SeqWindow {
    highest: Option<u32>,
    /// Bit `seq % SEQ_WINDOW` is set when `seq` was taken.
    seen: [u64; (SEQ_WINDOW / 64) as usize],
}

impl SeqWindow {
    /// True when `seq` is new and inside the window; it is then marked.
    fn accept(&mut self, seq: u32) -> bool {
        if let Some(highest) = self.highest {
            let ahead = seq.wrapping_sub(highest);
            if ahead == 0 || ahead > u32::MAX / 2 {
                if highest.wrapping_sub(seq) >= SEQ_WINDOW || self.taken(seq) {
                    return false;
                }
                self.mark(seq);
                return true;
            }
            if ahead >= SEQ_WINDOW {
                self.seen = Default::default();
            } else {
                for step in 1..=ahead {
                    let (word, bit) = Self::slot(highest.wrapping_add(step));
                    self.seen[word] &= !bit;
                }
            }
        }
        self.highest = Some(seq);
        self.mark(seq);
        true
    }

    fn slot(seq: u32) -> (usize, u64) {
        let index = seq % SEQ_WINDOW;
        ((index / 64) as usize, 1 << (index % 64))
    }
    fn taken(&self, seq: u32) -> bool {
        let (word, bit) = Self::slot(seq);
        self.seen[word] & bit != 0
    }
    fn mark(&mut self, seq: u32) {
        let (word, bit) = Self::slot(seq);
        self.seen[word] |= bit;
    }
}

/// Provides the current stream parameters for HELLO_ACK/heartbeats.
pub trait ConfigSource {
    fn pairing(&self) -> Option<&parking_lot::Mutex<crate::pairing::Pairing>> {
        None
    }

    fn stream_config(&self) -> StreamConfig;
    fn host_name(&self) -> String;
    fn host_caps(&self) -> u16 {
        0
    }
}

pub struct Session {
    active: Option<ActiveSession>,
    /// The last session that ended, and why.
    last_end: Option<(u32, EndReason)>,
    /// An update is taking over: new clients are turned away (`Busy`) until it
    /// either closes the host or gives up. Changed only under the session lock,
    /// together with the check that nobody is streaming.
    updating: bool,
    /// The goodbye for a stream an update stopped, sent by the next tick.
    goodbye: Option<(PeerId, Vec<u8>)>,
    /// That stream's end, reported by the next tick or [`Self::shut_down`].
    stopped: Option<SessionEnd>,
    /// The last session ended for liveness (see [`Self::answer_ended`]).
    ended_for_liveness: Option<EndedSession>,
    /// Recently refused HELLO2 nonces: a retransmit is the same attempt.
    refused: std::collections::VecDeque<(u32, Instant)>,
    refusal_minute: Option<RefusalMinute>,
    /// Injected so tests control randomness; production passes a seeded value
    /// derived from process entropy.
    next_session_id: u32,
    legacy_client_warned: bool,
}

impl Session {
    pub fn new(session_id_seed: u32) -> Self {
        Self {
            active: None,
            last_end: None,
            updating: false,
            goodbye: None,
            stopped: None,
            ended_for_liveness: None,
            refused: std::collections::VecDeque::new(),
            refusal_minute: None,
            next_session_id: session_id_seed.max(1),
            legacy_client_warned: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// The client went silent and its session is on hold (see
    /// [`HOLD_LIMIT`]).
    pub fn on_hold(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|session| session.held_since.is_some())
    }

    /// The id of the last session that ended, and why it ended.
    pub fn last_end(&self) -> Option<(u32, EndReason)> {
        self.last_end
    }

    /// Ends the active session for `reason`: remembers why, and returns
    /// its end for the summary line.
    fn end(&mut self, reason: EndReason, now: Instant) -> Option<SessionEnd> {
        let session = self.active.take()?;
        self.last_end = Some((session.session_id, reason));
        Some(session.end(reason, now))
    }

    /// Starts an update handoff. Atomic with admission: call with the session
    /// lock held; a client either got in before (and this returns `false`) or
    /// is refused until [`Self::end_update`]. With `stop_stream` (the user
    /// chose Stop and update) a client that got in loses its session here and
    /// is told the host is shutting down.
    pub fn begin_update(&mut self, stop_stream: bool) -> bool {
        if let Some(session) = self.active.as_ref() {
            if !stop_stream {
                return false;
            }
            let (peer, session_id) = (session.peer, session.session_id);
            info!(%peer, "Stopping the stream for an update");
            let msg_seq = self.next_msg_seq();
            let bye = ControlMessage::Bye(ByeReason::HostShuttingDown);
            self.goodbye = Some((
                peer,
                eternal_wire::v2::control::encode_control(session_id, msg_seq, &bye),
            ));
            self.stopped = self.end(EndReason::Shutdown, Instant::now());
        }
        self.updating = true;
        true
    }

    /// The goodbye [`Self::begin_update`] left for a stopped stream, if any.
    pub fn take_goodbye(&mut self) -> Option<(PeerId, Vec<u8>)> {
        self.goodbye.take()
    }

    /// The update handoff failed; clients may connect again.
    pub fn end_update(&mut self) {
        self.updating = false;
    }

    /// The app is quitting: the end of the stream it stops, or of one an
    /// update stopped that no tick reported yet.
    pub fn shut_down(&mut self, now: Instant) -> Option<SessionEnd> {
        let active = self.end(EndReason::Shutdown, now);
        self.stopped.take().or(active)
    }

    /// The first video went to `session_id`. Later calls change nothing.
    pub fn note_video_sent(&mut self, session_id: u32, now: Instant) {
        if let Some(session) = self.active.as_mut() {
            if session.session_id == session_id {
                session.first_video_at.get_or_insert(now);
            }
        }
    }

    pub fn session_id(&self) -> Option<u32> {
        self.active.as_ref().map(|s| s.session_id)
    }

    pub fn link_lost(&mut self, link: LinkId) -> Actions {
        let on_link = self
            .active
            .as_ref()
            .is_some_and(|session| session.peer.link == link);
        let lost = if on_link {
            self.end(EndReason::LinkLost, Instant::now())
        } else {
            None
        };
        Actions {
            client_lost: lost.is_some(),
            ended: lost,
            ..Actions::default()
        }
    }

    pub fn peer(&self) -> Option<PeerId> {
        self.active.as_ref().map(|s| s.peer)
    }

    pub fn client_info(&self) -> Option<ClientInfo> {
        self.active.as_ref().map(|s| s.info.clone())
    }

    /// Whether the connected client advertised HEVC decode in its HELLO2.
    /// False with no active session — no client, no reason to encode HEVC.
    pub fn client_supports_hevc(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|s| s.info.decoder_caps & eternal_wire::v2::control::CAP_DECODE_HEVC != 0)
    }

    pub fn client_supports_nack(&self) -> bool {
        self.active.as_ref().is_some_and(|s| {
            s.peer.link == LinkId::Udp && s.info.feature_caps & FEATURE_SUPPORTS_NACK != 0
        })
    }

    pub fn audio_session_id(&self) -> Option<u32> {
        self.active
            .as_ref()
            .filter(|session| {
                !session.awaiting_takeover
                    && session.info.feature_caps & eternal_wire::v2::control::FEATURE_WANTS_AUDIO
                        != 0
            })
            .map(|session| session.session_id)
    }

    pub fn last_report(&self) -> Option<ReceiverReport> {
        self.active.as_ref().and_then(|s| s.last_report)
    }

    fn allocate_session_id(&mut self) -> u32 {
        let id = self.next_session_id;
        self.next_session_id = self.next_session_id.wrapping_add(0x9E37_79B9).max(1);
        id
    }

    fn next_msg_seq(&mut self) -> u32 {
        match self.active.as_mut() {
            Some(session) => {
                session.msg_seq_out = session.msg_seq_out.wrapping_add(1).max(1);
                session.msg_seq_out
            }
            None => 1,
        }
    }

    /// Feed one control datagram off a link, with its header. Input whose
    /// msg_seq this session took already, or that is too old, is dropped.
    pub fn handle_datagram(
        &mut self,
        source: PeerId,
        header: &ControlHeader,
        message: ControlMessage,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        self.handle(
            source,
            header.session_id,
            Some(header.msg_seq),
            message,
            config,
            now,
        )
    }

    /// Feed one inbound control message. `header_session_id` is the id the
    /// datagram claims; every message except HELLO2 must match the live
    /// session's. `now` is injected for testability.
    ///
    /// Without a msg_seq, input skips the repeat check of
    /// [`Self::handle_datagram`].
    pub fn handle_control(
        &mut self,
        source: PeerId,
        header_session_id: u32,
        message: ControlMessage,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        self.handle(source, header_session_id, None, message, config, now)
    }

    fn handle(
        &mut self,
        source: PeerId,
        header_session_id: u32,
        msg_seq: Option<u32>,
        message: ControlMessage,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        // HELLO2 is the only message that legitimately carries no session id
        // (it is asking for one). Everything else is authenticated by the id
        // the handshake minted, not just by source IP: an IP is shared by
        // every process on the device and by anything behind the same NAT, and
        // is trivially spoofed on the local link. Without this, a stray
        // INPUT_EVENT moved the mouse and clicked, a stray RECEIVER_REPORT
        // steered the bitrate, and a late BYE from a superseded session tore
        // down the one that replaced it.
        if !matches!(message, ControlMessage::Hello2(_)) {
            match self.active.as_ref() {
                Some(session)
                    if session.session_id == header_session_id && !session.awaiting_takeover => {}
                _ if matches!(message, ControlMessage::Bye(_)) => return Actions::default(),
                _ => return self.answer_ended(source, header_session_id, now),
            }
        }
        match message {
            ControlMessage::Hello2(hello) => self.handle_hello(source, hello, config, now),
            ControlMessage::Bye(reason) => self.handle_bye(source, reason, now),
            ControlMessage::KeyframeRequest(request) => self.handle_keyframe(source, request, now),
            ControlMessage::ReceiverReport(report) => self.handle_report(source, report, now),
            ControlMessage::Ping(ping) => self.handle_ping(source, ping, now),
            ControlMessage::InputEvent(event) => self.handle_input(source, event, msg_seq, now),
            ControlMessage::Nack(nack) => self.handle_nack(source, nack, now),
            // Host-outbound types arriving inbound: ignore.
            _ => Actions::default(),
        }
    }

    /// Test helper: send as the currently-connected client.
    #[cfg(test)]
    fn handle_control_authed(
        &mut self,
        source: PeerId,
        message: ControlMessage,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let id = self.session_id().unwrap_or(0);
        self.handle_control(source, id, message, config, now)
    }

    fn handle_input(
        &mut self,
        source: PeerId,
        event: InputEvent,
        msg_seq: Option<u32>,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source) {
            return actions;
        }
        // Only relay input the client declared it wants to send — a session
        // that connected view-only stays view-only until it re-handshakes.
        if session.info.feature_caps & FEATURE_WANTS_INPUT == 0 {
            return actions;
        }
        // Each copy of a press or release has its own msg_seq, so this drops
        // only repeats; the event_id deduper still sorts out the copies.
        if msg_seq.is_some_and(|seq| !session.input_seqs.accept(seq)) {
            return actions;
        }
        // A stream of touches is proof of life as good as any report.
        actions.force_idr = session.heard(Heard::Input, now);
        actions.input_late = session
            .lateness(event.client_time_us, now)
            .is_some_and(|late| session.late_input.note(late, now));
        actions.input = Some((session.session_id, event));
        actions
    }

    fn handle_nack(&mut self, source: PeerId, nack: Nack, now: Instant) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source)
            || session.peer.link != LinkId::Udp
            || session.info.feature_caps & FEATURE_SUPPORTS_NACK == 0
            || nack.validate().is_err()
        {
            return actions;
        }
        actions.force_idr = session.heard(Heard::Nack, now);
        actions.retransmit = Some(nack);
        actions
    }

    fn handle_hello(
        &mut self,
        source: PeerId,
        hello: eternal_wire::v2::control::Hello2,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();

        // Version gate first.
        if hello.proto_min > 2 || hello.proto_max < 2 {
            crate::updates::note_ipad_protocol(hello.proto_min, hello.proto_max);
            return self.refuse(source, HelloStatus::VersionUnsupported, &hello, config, now);
        }

        // An update is about to close the host: don't start a stream it would cut.
        // The iPad shows Busy ("already streaming"); a retry after the restart works.
        if self.updating {
            info!(peer = %source, "Client turned away while an update starts");
            return self.refuse(source, HelloStatus::Busy, &hello, config, now);
        }

        // Keep credentials locked through authorization, rotation and ACK creation:
        // a simultaneous GUI token reset must not issue a new secret to an old token.
        let mut pairing = config.pairing().map(|state| state.lock());
        let token = pairing.as_ref().map_or([0; 16], |state| state.ack_token());
        if let Some(session) = self.active.as_ref() {
            if session.peer == source
                && session.accepted_hello == hello
                && session.auth_token == token
            {
                // Exact replay of an accepted handshake. In particular, losing the
                // first code-based ACK must not require the now-rotated code.
                actions.replies.push((source, session.ack.clone()));
                return actions;
            }
        }
        let grant = match pairing
            .as_mut()
            .map(|state| state.authorize(source, &hello, now))
        {
            Some(Ok(grant)) => grant,
            Some(Err(status)) => {
                return self.refuse(source, status, &hello, config, now);
            }
            None => crate::pairing::Grant {
                token: [0; 16],
                used_code: false,
                new_pairing: false,
            },
        };

        // The media target: the client's advertised listen port at its source IP
        // (normally identical to the source port; 0 = malformed, use the source).
        let media_target = PeerId {
            link: source.link,
            addr: source.addr.map(|addr| {
                SocketAddr::new(
                    addr.ip(),
                    if hello.listen_port != 0 {
                        hello.listen_port
                    } else {
                        addr.port()
                    },
                )
            }),
        };

        if let Some(session) = self.active.as_ref() {
            if session.same_device(source, hello.device_id) {
                if matches!(session.peer.link, LinkId::Usb { .. }) && source.link == LinkId::Udp {
                    return self.refuse(source, HelloStatus::Busy, &hello, config, now);
                }
                // Same device, new connect attempt: supersede in place.
                info!(peer = %source, "Client reconnected — superseding session in place");
            } else if session.held_since.is_some() {
                // The session's own client went silent: the slot is free.
                info!(peer = %source, "Another client takes over a silent session");
            } else {
                // A different device while one is streaming: reject.
                info!(peer = %source, "Second client rejected while a session is active");
                return self.refuse(source, HelloStatus::Busy, &hello, config, now);
            }
        }

        if grant.used_code {
            if let Err(error) = pairing
                .as_mut()
                .expect("code grant has pairing state")
                .rotate_code()
            {
                warn!(%error, "Pairing code rotation failed");
                return self.refuse(source, HelloStatus::Error, &hello, config, now);
            }
        }

        let silent_taken_over = self
            .active
            .as_ref()
            .is_some_and(|session| !session.same_device(source, hello.device_id));
        if silent_taken_over {
            let (ended, goodbye) = self.expire(now);
            actions.ended = ended;
            actions.goodbye = goodbye;
        } else {
            actions.ended = self.end(EndReason::Superseded, now);
        }
        let session_id = self.allocate_session_id();
        crate::telemetry::note_client_device(&hello.device_name);
        info!(
            peer = %source,
            session_id,
            device = %hello.device_name,
            screen = format!("{}x{}", hello.screen_px_w, hello.screen_px_h),
            screen_points = format!("{}x{}", hello.screen_pt_w, hello.screen_pt_h),
            refresh_hz = hello.refresh_hz,
            preferred_fps = hello.preferred_fps,
            decoder_caps = hello.decoder_caps,
            feature_caps = hello.feature_caps,
            "Client session established"
        );
        self.active = Some(ActiveSession {
            session_id,
            accepted_hello: hello.clone(),
            ack: Vec::new(),
            auth_token: grant.token,
            peer: source,
            info: ClientInfo {
                device_name: hello.device_name.clone(),
                device_id: hello.device_id,
                screen_px: (hello.screen_px_w, hello.screen_px_h),
                refresh_hz: hello.refresh_hz,
                preferred_fps: hello.preferred_fps,
                decoder_caps: hello.decoder_caps,
                feature_caps: hello.feature_caps,
                connected_at: now,
            },
            liveness_deadline: now + LIVENESS_TIMEOUT,
            heard: (now, Heard::Hello),
            held_since: None,
            client_clock: None,
            late_input: LateInput::default(),
            awaiting_takeover: false,
            last_keyframe_grant: None,
            last_report: None,
            msg_seq_out: 0,
            first_video_at: None,
            frames_received: false,
            bye: None,
            input_seqs: SeqWindow::default(),
        });

        // Only a new pairing carries the token. iPads keep the token they
        // have when an ACK carries none.
        let ack = self.make_ack_with_token(
            HelloStatus::Ok,
            hello.client_nonce,
            session_id,
            config,
            if grant.new_pairing {
                grant.token
            } else {
                [0; 16]
            },
        );
        self.active.as_mut().unwrap().ack = ack.clone();
        actions.replies.push((source, ack));
        actions.new_target = Some(media_target);
        actions.force_idr = true;
        actions
    }

    fn handle_bye(&mut self, source: PeerId, reason: ByeReason, now: Instant) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source) {
            return actions;
        }
        info!(peer = %source, ?reason, "Client said goodbye");
        if reason == ByeReason::Superseded
            && source.link == LinkId::Udp
            && session.info.device_id != 0
        {
            // WiFi BYE and USB HELLO travel on different links and may arrive
            // in either order. Retain the display until the same device takes
            // over, with a short deadline if the new link never handshakes.
            session.awaiting_takeover = true;
            session.liveness_deadline = now + USB_TAKEOVER_TIMEOUT;
            session.bye = Some(reason);
            info!(peer = %source, "Awaiting USB takeover after WiFi goodbye");
            return actions;
        }
        actions.ended = self.end(EndReason::Goodbye(reason), now);
        actions.client_lost = true;
        actions
    }

    fn handle_keyframe(
        &mut self,
        source: PeerId,
        request: KeyframeRequest,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source) {
            return actions;
        }
        actions.force_idr = session.heard(Heard::Keyframe, now);
        info!(reason = ?request.reason, "Keyframe request received");

        if session.grant_keyframe(now) {
            info!(reason = ?request.reason, "Keyframe requested by client — forcing IDR");
            actions.force_idr = true;
        }
        actions
    }

    fn handle_report(&mut self, source: PeerId, report: ReceiverReport, now: Instant) -> Actions {
        let mut actions = Actions::default();
        if let Some(session) = self.active.as_mut() {
            if session_peer_matches(session, source) {
                actions.force_idr = session.heard(Heard::Report, now);
                session.last_report = Some(report);
                session.frames_received |= report.frames_complete > 0;
                actions.report = Some(report);
            }
        }
        actions
    }

    fn handle_ping(
        &mut self,
        source: PeerId,
        ping: eternal_wire::v2::control::Ping,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source) {
            return actions;
        }
        actions.force_idr = session.heard(Heard::Ping, now);
        session.lateness(ping.t1_us, now);

        let t2 = crate::clock::host_now_us();
        let session_id = session.session_id;
        let msg_seq = self.next_msg_seq();
        let pong = ControlMessage::Pong(eternal_wire::v2::control::Pong {
            t1_us: ping.t1_us,
            t2_us: t2,
            t3_us: crate::clock::host_now_us(),
        });
        actions.replies.push((
            source,
            eternal_wire::v2::control::encode_control(session_id, msg_seq, &pong),
        ));
        actions
    }

    /// A new pipeline generation is listening again. The client's heartbeats
    /// and reports during the restart gap went unheard, so the gap must not
    /// count against it: 0.3.1 expired live iPads the moment a slow restart
    /// finished and made them reconnect.
    pub fn transport_resumed(&mut self, now: Instant) {
        if let Some(session) = self.active.as_mut() {
            session.liveness_deadline = session.liveness_deadline.max(now + LIVENESS_TIMEOUT);
        }
    }

    /// Periodic tick: expire the session when the client has been silent past
    /// the liveness window. Returns the heartbeat to send (if a session is
    /// active and `send_heartbeat` is true).
    pub fn tick(
        &mut self,
        config: &impl ConfigSource,
        send_heartbeat: bool,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        self.close_refusal_minute(now);
        if let Some(goodbye) = self.take_goodbye() {
            actions.goodbye = Some(goodbye);
            actions.client_lost = self.active.is_none();
        }
        actions.ended = self.stopped.take();
        let Some(session) = self.active.as_ref() else {
            return actions;
        };

        let revoked = config.pairing().is_some_and(|state| {
            let state = state.lock();
            state.required && session.auth_token != state.token()
        });
        if revoked {
            info!(peer = %session.peer, "Pairing credentials changed — reconnect required");
            actions.ended = self.end(EndReason::Revoked, now);
            actions.client_lost = true;
            return actions;
        }

        if now >= session.liveness_deadline {
            // A client that had a picture may only have lost its uplink: it
            // keeps the session, on hold, until the hold limit. One that
            // never had a picture, or said goodbye, ends now.
            let holds = session.frames_received && !session.awaiting_takeover;
            if !holds || now >= session.liveness_deadline + (HOLD_LIMIT - LIVENESS_TIMEOUT) {
                if let Some(bye) = session.bye {
                    session.log_expiry(now);
                    actions.ended = self.end(EndReason::Goodbye(bye), now);
                } else {
                    (actions.ended, actions.goodbye) = self.expire(now);
                }
                actions.client_lost = true;
                return actions;
            }
            if session.held_since.is_none() {
                let (heard_at, heard) = session.heard;
                warn!(
                    session_id = session.session_id,
                    silent_ms = now.saturating_duration_since(heard_at).as_millis() as u64,
                    last_heard = heard.as_str(),
                    "Client silent — holding the session"
                );
                if let Some(session) = self.active.as_mut() {
                    session.held_since = Some(now);
                }
                actions.release_input = true;
            }
        }

        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        session.late_input.log(session.session_id, now);
        if send_heartbeat {
            let peer = session.peer;
            let session_id = session.session_id;
            let msg_seq = self.next_msg_seq();
            let heartbeat = ControlMessage::Heartbeat(eternal_wire::v2::control::Heartbeat {
                host_time_us: crate::clock::host_now_us(),
                stream_config: self.negotiated_config(config),
            });
            actions.replies.push((
                peer,
                eternal_wire::v2::control::encode_control(session_id, msg_seq, &heartbeat),
            ));
        }
        actions
    }

    /// Ends the active session for liveness, with the goodbye that tells its
    /// client to connect again: `error`, which every 0.3.x iPad answers with
    /// a reconnect. Late packets of the session get it again
    /// ([`Self::answer_ended`]).
    fn expire(&mut self, now: Instant) -> (Option<SessionEnd>, Option<(PeerId, Vec<u8>)>) {
        let Some(session) = self.active.as_ref() else {
            return (None, None);
        };
        session.log_expiry(now);
        let (peer, session_id) = (session.peer, session.session_id);
        let msg_seq = self.next_msg_seq();
        let goodbye = eternal_wire::v2::control::encode_control(
            session_id,
            msg_seq,
            &ControlMessage::Bye(ByeReason::Error),
        );
        self.ended_for_liveness = Some(EndedSession {
            session_id,
            peer,
            goodbye: goodbye.clone(),
            ended_at: now,
            last_sent: now,
        });
        (self.end(EndReason::Liveness, now), Some((peer, goodbye)))
    }

    /// A packet for no live session. One for the session last ended for
    /// liveness, from its client, means the goodbye was lost or overtaken:
    /// it goes again, once a second at most, for a minute.
    fn answer_ended(&mut self, source: PeerId, session_id: u32, now: Instant) -> Actions {
        let mut actions = Actions::default();
        let Some(ended) = self.ended_for_liveness.as_mut() else {
            return actions;
        };
        let from_client = ended.session_id == session_id
            && ended.peer.link == source.link
            && ended.peer.addr.map(|a| a.ip()) == source.addr.map(|a| a.ip());
        if !from_client
            || now.saturating_duration_since(ended.ended_at) >= ENDED_REPLY_FOR
            || now.saturating_duration_since(ended.last_sent) < ENDED_REPLY_GAP
        {
            return actions;
        }
        ended.last_sent = now;
        actions.replies.push((source, ended.goodbye.clone()));
        actions
    }

    /// One STREAM_CONFIG notify for the active client (bitrate/fps/resolution
    /// changed). The heartbeat's embedded config self-heals if this is lost.
    pub fn stream_config_notify(&mut self, config: &impl ConfigSource) -> Vec<(PeerId, Vec<u8>)> {
        let Some(session) = self.active.as_ref() else {
            return Vec::new();
        };
        let peer = session.peer;
        let session_id = session.session_id;
        let msg_seq = self.next_msg_seq();
        let message = ControlMessage::StreamConfig(self.negotiated_config(config));
        vec![(
            peer,
            eternal_wire::v2::control::encode_control(session_id, msg_seq, &message),
        )]
    }

    /// A legacy (v0.1.x) ETERNALHELLO arrived: no wire reply — the old app
    /// can't parse anything we'd send — but tell the user what happened.
    pub fn note_legacy_hello(&mut self, source: PeerId) {
        crate::updates::note_legacy_ipad();
        if !self.legacy_client_warned {
            self.legacy_client_warned = true;
            warn!(
                peer = %source,
                "A v0.1.x iPad app tried to connect. Protocol v2 is a clean break — \
                 update the iPad app to stream again."
            );
        }
    }

    /// Answers `hello` with a refusal. The first refusal of a nonce ends
    /// that attempt; a retransmit of it gets the answer again.
    fn refuse(
        &mut self,
        source: PeerId,
        status: HelloStatus,
        hello: &eternal_wire::v2::control::Hello2,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let ack = self.make_ack(status, hello.client_nonce, 0, config);
        self.refused
            .retain(|(_, at)| now.saturating_duration_since(*at) < REFUSED_FOR);
        let first = !self
            .refused
            .iter()
            .any(|(nonce, _)| *nonce == hello.client_nonce);
        if first {
            if self.refused.len() == REFUSED_KEPT {
                self.refused.pop_front();
            }
            self.refused.push_back((hello.client_nonce, now));
        }
        let summarized = first && self.summarize_refusal(now);
        Actions {
            replies: vec![(source, ack)],
            ended: summarized.then_some(SessionEnd {
                session_id: None,
                client_nonce: hello.client_nonce,
                link: source.link,
                reason: EndReason::Refused(status),
                first_video: None,
                frames_received: false,
                duration: Duration::ZERO,
            }),
            ..Actions::default()
        }
    }

    /// Whether a new refused attempt gets a summary in this minute's budget.
    fn summarize_refusal(&mut self, now: Instant) -> bool {
        self.close_refusal_minute(now);
        let minute = self.refusal_minute.get_or_insert(RefusalMinute {
            since: now,
            summarized: 0,
            left_out: 0,
        });
        if minute.summarized < REFUSED_SUMMARIES {
            minute.summarized += 1;
            true
        } else {
            minute.left_out += 1;
            false
        }
    }

    /// Ends a minute of refusals that is over, saying how many it left out.
    fn close_refusal_minute(&mut self, now: Instant) {
        let over = self
            .refusal_minute
            .take_if(|minute| now.saturating_duration_since(minute.since) >= REFUSAL_MINUTE);
        if let Some(minute) = over.filter(|minute| minute.left_out > 0) {
            warn!(
                left_out = minute.left_out,
                summarized = minute.summarized,
                "Turned down more connection attempts in a minute than the session summaries keep"
            );
        }
    }

    fn make_ack(
        &mut self,
        status: HelloStatus,
        client_nonce: u32,
        session_id: u32,
        config: &impl ConfigSource,
    ) -> Vec<u8> {
        self.make_ack_with_token(status, client_nonce, session_id, config, [0; 16])
    }

    fn make_ack_with_token(
        &mut self,
        status: HelloStatus,
        client_nonce: u32,
        session_id: u32,
        config: &impl ConfigSource,
        auth_token: [u8; 16],
    ) -> Vec<u8> {
        let msg_seq = self.next_msg_seq();
        let ack = ControlMessage::HelloAck(HelloAck {
            status,
            accepted_version: 2,
            client_nonce,
            session_id,
            heartbeat_interval_ms: HEARTBEAT_INTERVAL.as_millis() as u16,
            report_interval_ms: REPORT_INTERVAL_MS,
            liveness_timeout_ms: LIVENESS_TIMEOUT.as_millis() as u16,
            stream_config: self.negotiated_config(config),
            host_name: config.host_name(),
            auth_token,
            host_caps: HOSTCAP_NACK | config.host_caps(),
        });
        eternal_wire::v2::control::encode_control(session_id, msg_seq, &ack)
    }

    fn negotiated_config(&self, source: &impl ConfigSource) -> StreamConfig {
        let mut config = source.stream_config();
        let preference = self
            .active
            .as_ref()
            .map_or(0, |s| u32::from(s.info.preferred_fps));
        config.fps = crate::control::merge_fps(u32::from(config.fps), preference) as u16;
        config
    }
}

impl LateInput {
    /// Counts an input event `late` behind its session's quickest packet;
    /// returns whether it is past [`INPUT_LATE`].
    fn note(&mut self, late: Duration, now: Instant) -> bool {
        if late <= INPUT_LATE {
            return false;
        }
        self.pending += 1;
        self.total += 1;
        self.max_late = self.max_late.max(late);
        self.last_at = Some(now);
        true
    }

    /// One line for a burst of late input, once a second has passed
    /// without more.
    fn log(&mut self, session_id: u32, now: Instant) {
        let done = self
            .last_at
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(1));
        if self.pending == 0 || !done {
            return;
        }
        info!(
            session_id,
            late_input = self.pending,
            max_late_ms = self.max_late.as_millis() as u64,
            "Dropped input that arrived late"
        );
        self.pending = 0;
        self.max_late = Duration::ZERO;
    }
}

impl ActiveSession {
    /// How much later than the session's quickest packet one stamped
    /// `client_us` on the iPad's clock arrived; `None` without a stamp. The
    /// iPad stamps input and pings with one monotonic clock, so the smallest
    /// difference between host time and that stamp is the clocks' offset
    /// plus the quickest trip. The offset may rise by
    /// [`CLOCK_DRIFT_US_PER_S`], so an iPad clock that runs a little slow
    /// never makes input look late.
    fn lateness(&mut self, client_us: u64, now: Instant) -> Option<Duration> {
        if client_us == 0 {
            return None;
        }
        let host_us = now
            .saturating_duration_since(self.info.connected_at)
            .as_micros() as i64;
        let offset = host_us - client_us as i64;
        let quickest = self.client_clock.map(|(quickest, at)| {
            let drift =
                now.saturating_duration_since(at).as_millis() as u64 * CLOCK_DRIFT_US_PER_S / 1000;
            quickest.saturating_add(drift as i64)
        });
        self.client_clock = Some((
            quickest.map_or(offset, |quickest| quickest.min(offset)),
            now,
        ));
        let late = quickest.map_or(0, |quickest| offset.saturating_sub(quickest));
        Some(Duration::from_micros(late.max(0) as u64))
    }

    /// The client was heard: it gets a full liveness window again, and a
    /// session on hold carries on. Returns whether to force an IDR: reports
    /// and keyframe requests sent during the silence may not have arrived.
    fn heard(&mut self, kind: Heard, now: Instant) -> bool {
        let silent = now.saturating_duration_since(self.heard.0);
        self.heard = (now, kind);
        self.liveness_deadline = now + LIVENESS_TIMEOUT;
        let Some(since) = self.held_since.take() else {
            return false;
        };
        info!(
            session_id = self.session_id,
            silent_ms = silent.as_millis() as u64,
            held_ms = now.saturating_duration_since(since).as_millis() as u64,
            heard = kind.as_str(),
            "Client heard again — session kept"
        );
        self.grant_keyframe(now)
    }

    /// Whether an IDR may be forced now: one per
    /// [`KEYFRAME_REQUEST_MIN_INTERVAL`].
    fn grant_keyframe(&mut self, now: Instant) -> bool {
        let granted = !matches!(
            self.last_keyframe_grant,
            Some(last) if now.duration_since(last) < KEYFRAME_REQUEST_MIN_INTERVAL
        );
        if granted {
            self.last_keyframe_grant = Some(now);
        }
        granted
    }

    /// `device=` stays last: uploads cut the iPad's name at the line's end.
    fn log_expiry(&self, now: Instant) {
        let (heard_at, heard) = self.heard;
        warn!(
            peer = %self.peer,
            silent_ms = now.saturating_duration_since(heard_at).as_millis() as u64,
            held_ms = self
                .held_since
                .map_or(0, |since| now.saturating_duration_since(since).as_millis() as u64),
            last_heard = heard.as_str(),
            late_input = self.late_input.total,
            device = %self.info.device_name,
            "Client liveness expired — tearing session down"
        );
    }

    fn end(&self, reason: EndReason, now: Instant) -> SessionEnd {
        let start = self.info.connected_at;
        SessionEnd {
            session_id: Some(self.session_id),
            client_nonce: self.accepted_hello.client_nonce,
            link: self.peer.link,
            reason,
            first_video: self
                .first_video_at
                .map(|at| at.saturating_duration_since(start)),
            frames_received: self.frames_received,
            duration: now.saturating_duration_since(start),
        }
    }

    fn same_device(&self, source: PeerId, device_id: u64) -> bool {
        if self.info.device_id != 0 && device_id != 0 {
            self.info.device_id == device_id
        } else {
            session_peer_matches(self, source)
        }
    }
}

fn session_peer_matches(session: &ActiveSession, source: PeerId) -> bool {
    session.peer.link == source.link
        && session.peer.addr.map(|a| a.ip()) == source.addr.map(|a| a.ip())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::control::{Hello2, KeyframeReason, CAP_DECODE_H264};

    struct TestConfig;

    impl ConfigSource for TestConfig {
        fn stream_config(&self) -> StreamConfig {
            StreamConfig {
                stream_epoch: 7,
                width: 1280,
                height: 720,
                fps: 60,
                codec: 0,
                flags: 0,
                bitrate_bps: 15_000_000,
            }
        }

        fn host_name(&self) -> String {
            "TEST-HOST".to_string()
        }
    }

    fn hello(nonce: u32, port: u16) -> ControlMessage {
        ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: nonce,
            listen_port: port,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: 0,
            screen_px_w: 2420,
            screen_px_h: 1668,
            screen_pt_w: 1210,
            screen_pt_h: 834,
            refresh_hz: 120,
            device_name: "Test iPad".to_string(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        })
    }

    fn addr(ip: [u8; 4], port: u16) -> PeerId {
        PeerId::udp(SocketAddr::from((ip, port)))
    }

    fn identified_hello(nonce: u32, device_id: u64) -> ControlMessage {
        let ControlMessage::Hello2(mut hello) = hello(nonce, 50000) else {
            unreachable!()
        };
        hello.device_id = device_id;
        hello.feature_caps = FEATURE_SUPPORTS_NACK;
        ControlMessage::Hello2(hello)
    }

    /// Uploads leave the iPad's name out of every line, once its HELLO is
    /// accepted (`telemetry::note_client_device`).
    #[test]
    fn an_accepted_ipad_name_leaves_uploads() {
        let ControlMessage::Hello2(mut named) = hello(1, 50000) else {
            unreachable!()
        };
        named.device_name = "Zoë's iPad mini".to_string();
        let mut session = Session::new(100);
        let wifi = addr([10, 0, 0, 5], 50000);
        session.handle_control(
            wifi,
            0,
            ControlMessage::Hello2(named),
            &TestConfig,
            Instant::now(),
        );
        assert!(session.session_id().is_some());
        assert_eq!(
            crate::telemetry::uploaded("Stream for Zoë's iPad mini ended"),
            "Stream for <device-name> ended"
        );
    }

    /// An iPad named only with emoji, with an audio output already noted:
    /// the session starts, the name stays out of the registry, and uploads
    /// still leave out the output's name.
    #[test]
    fn an_emoji_only_ipad_name_panics_nothing() {
        crate::telemetry::note_audio_device("Headphones (Ines's AirPods Max)");
        let ControlMessage::Hello2(mut named) = hello(1, 50000) else {
            unreachable!()
        };
        named.device_name = "🎮🎧🎵".to_string();
        let mut session = Session::new(100);
        session.handle_control(
            addr([10, 0, 0, 5], 50000),
            0,
            ControlMessage::Hello2(named),
            &TestConfig,
            Instant::now(),
        );
        assert!(session.session_id().is_some());
        assert_eq!(
            crate::telemetry::uploaded("Stream for 🎮🎧🎵 ended"),
            "Stream for 🎮🎧🎵 ended"
        );
        assert_eq!(
            crate::telemetry::uploaded("Playing on Headphones (Ines's AirPods Max)"),
            "Playing on <headphones>"
        );
    }

    #[test]
    fn same_device_takes_over_usb_and_ignores_late_wifi_messages() {
        let mut session = Session::new(100);
        let now = Instant::now();
        let wifi = addr([10, 0, 0, 5], 50000);
        let usb = PeerId::usb(17);
        session.handle_control(wifi, 0, identified_hello(1, 77), &TestConfig, now);
        let wifi_id = session.session_id().unwrap();
        assert!(session.client_supports_nack());
        let takeover = session.handle_control(usb, 0, identified_hello(1, 77), &TestConfig, now);
        assert_eq!(takeover.new_target, Some(usb));
        assert!(takeover.force_idr);
        let usb_id = session.session_id().unwrap();
        assert_ne!(
            usb_id, wifi_id,
            "the same nonce on a new link creates a new session"
        );
        assert!(!session.client_supports_nack());
        for id in [wifi_id, usb_id] {
            let late = session.handle_control(
                wifi,
                id,
                ControlMessage::Bye(ByeReason::UserDisconnect),
                &TestConfig,
                now,
            );
            assert!(!late.client_lost);
            assert_eq!(session.peer(), Some(usb));
        }
        let duplicate = session.handle_control(usb, 0, identified_hello(1, 77), &TestConfig, now);
        assert!(duplicate.new_target.is_none());
        assert_eq!(parse_ack(&duplicate.replies[0].1).session_id, usb_id);
        let wifi_retry = session.handle_control(wifi, 0, identified_hello(2, 77), &TestConfig, now);
        assert_eq!(
            parse_ack(&wifi_retry.replies[0].1).status,
            HelloStatus::Busy
        );
        assert!(!session.link_lost(LinkId::Udp).client_lost);
        let usb_id = session.session_id().unwrap();
        assert!(session.link_lost(usb.link).client_lost);
        assert_eq!(session.last_end(), Some((usb_id, EndReason::LinkLost)));
        let fallback = session.handle_control(wifi, 0, identified_hello(3, 77), &TestConfig, now);
        assert_eq!(fallback.new_target, Some(wifi));
    }

    #[test]
    fn identified_foreign_device_is_busy_even_on_the_same_ip() {
        let mut session = Session::new(100);
        let now = Instant::now();
        let wifi = addr([10, 0, 0, 5], 50000);
        session.handle_control(wifi, 0, identified_hello(1, 77), &TestConfig, now);
        for source in [wifi, PeerId::usb(17)] {
            let foreign =
                session.handle_control(source, 0, identified_hello(2, 88), &TestConfig, now);
            assert_eq!(parse_ack(&foreign.replies[0].1).status, HelloStatus::Busy);
            assert!(foreign.new_target.is_none());
        }
        let new_ip = addr([10, 0, 0, 6], 50000);
        let same_device =
            session.handle_control(new_ip, 0, identified_hello(3, 77), &TestConfig, now);
        assert_eq!(same_device.new_target, Some(new_ip));
    }

    #[test]
    fn wifi_goodbye_preserves_the_display_until_usb_takes_over() {
        let mut session = Session::new(100);
        let now = Instant::now();
        let wifi = addr([10, 0, 0, 5], 50000);
        let usb = PeerId::usb(17);
        session.handle_control(wifi, 0, identified_hello(1, 77), &TestConfig, now);
        let wifi_id = session.session_id().unwrap();
        let bye = session.handle_control(
            wifi,
            wifi_id,
            ControlMessage::Bye(ByeReason::Superseded),
            &TestConfig,
            now,
        );
        assert!(!bye.client_lost);
        assert_eq!(session.peer(), Some(wifi));
        let later = now + USB_TAKEOVER_TIMEOUT / 2;
        let foreign = session.handle_control(usb, 0, identified_hello(2, 88), &TestConfig, later);
        assert_eq!(parse_ack(&foreign.replies[0].1).status, HelloStatus::Busy);
        let takeover = session.handle_control(usb, 0, identified_hello(2, 77), &TestConfig, later);
        assert_eq!(takeover.new_target, Some(usb));
        assert!(takeover.force_idr);
        assert!(!takeover.client_lost);
        assert_ne!(session.session_id(), Some(wifi_id));
        assert!(
            !session
                .tick(&TestConfig, false, now + USB_TAKEOVER_TIMEOUT)
                .client_lost
        );
    }

    #[test]
    fn missing_usb_takeover_expires_and_late_wifi_traffic_cannot_extend_it() {
        let mut session = Session::new(100);
        let now = Instant::now();
        let wifi = addr([10, 0, 0, 5], 50000);
        session.handle_control(wifi, 0, identified_hello(1, 77), &TestConfig, now);
        let id = session.session_id().unwrap();
        session.handle_control(
            wifi,
            id,
            ControlMessage::Bye(ByeReason::Superseded),
            &TestConfig,
            now,
        );
        let late = session.handle_control(
            wifi,
            id,
            ControlMessage::ReceiverReport(ReceiverReport::default()),
            &TestConfig,
            now + USB_TAKEOVER_TIMEOUT / 2,
        );
        assert!(late.report.is_none());
        let expired = session.tick(&TestConfig, false, now + USB_TAKEOVER_TIMEOUT);
        assert!(expired.client_lost);
        assert!(!session.is_active());
        for (peer, device_id, reason) in [
            (wifi, 0, ByeReason::Superseded),
            (wifi, 77, ByeReason::UserDisconnect),
            (PeerId::usb(17), 77, ByeReason::Superseded),
        ] {
            session.handle_control(peer, 0, identified_hello(2, device_id), &TestConfig, now);
            let bye =
                session.handle_control_authed(peer, ControlMessage::Bye(reason), &TestConfig, now);
            assert!(
                bye.client_lost,
                "grace only applies to an identified WiFi takeover"
            );
            assert!(!session.is_active());
        }
    }

    #[test]
    fn legacy_identity_stays_on_its_link_and_usb_never_requests_retransmits() {
        let mut session = Session::new(100);
        let now = Instant::now();
        session.handle_control(
            addr([10, 0, 0, 5], 50000),
            0,
            hello(1, 50000),
            &TestConfig,
            now,
        );
        let usb = PeerId::usb(17);
        let unknown = session.handle_control(usb, 0, hello(2, 50000), &TestConfig, now);
        assert_eq!(parse_ack(&unknown.replies[0].1).status, HelloStatus::Busy);
        session.link_lost(LinkId::Udp);
        session.handle_control(usb, 0, identified_hello(3, 77), &TestConfig, now);
        let id = session.session_id().unwrap();
        let nack = ControlMessage::Nack(Nack {
            stream_epoch: 7,
            frame_seq: 42,
            frag_count: 1,
            missing: vec![0],
        });
        assert!(session
            .handle_control(usb, id, nack, &TestConfig, now)
            .retransmit
            .is_none());
    }

    fn parse_ack(bytes: &[u8]) -> HelloAck {
        let (_, message) = eternal_wire::v2::control::parse_control(bytes).unwrap();
        match message {
            ControlMessage::HelloAck(ack) => ack,
            other => panic!("expected ack, got {other:?}"),
        }
    }

    #[test]
    fn nack_requires_capability_peer_and_session_and_extends_liveness() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let nack = ControlMessage::Nack(Nack {
            stream_epoch: 7,
            frame_seq: 42,
            frag_count: 9,
            missing: vec![0, 8],
        });
        session.handle_control(peer, 0, hello(1, 50000), &TestConfig, now);
        assert!(!session.client_supports_nack());
        assert!(session
            .handle_control_authed(peer, nack.clone(), &TestConfig, now)
            .retransmit
            .is_none());

        let ControlMessage::Hello2(mut wants) = hello(2, 50000) else {
            unreachable!()
        };
        wants.feature_caps = FEATURE_SUPPORTS_NACK;
        let actions =
            session.handle_control(peer, 0, ControlMessage::Hello2(wants), &TestConfig, now);
        assert_eq!(
            parse_ack(&actions.replies[0].1).host_caps & HOSTCAP_NACK,
            HOSTCAP_NACK
        );
        let id = session.session_id().unwrap();
        assert!(session.client_supports_nack());
        assert!(session
            .handle_control(peer, id + 1, nack.clone(), &TestConfig, now)
            .retransmit
            .is_none());
        assert!(session
            .handle_control(
                addr([10, 0, 0, 9], 50000),
                id,
                nack.clone(),
                &TestConfig,
                now
            )
            .retransmit
            .is_none());
        let later = now + Duration::from_secs(2);
        assert_eq!(
            session
                .handle_control(peer, id, nack, &TestConfig, later)
                .retransmit,
            Some(Nack {
                stream_epoch: 7,
                frame_seq: 42,
                frag_count: 9,
                missing: vec![0, 8],
            })
        );
        assert!(
            !session
                .tick(&TestConfig, false, now + Duration::from_secs(4))
                .client_lost
        );
    }

    #[test]
    fn first_hello_establishes_session_and_targets_media() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);

        let actions = session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);

        assert_eq!(actions.replies.len(), 1);
        let ack = parse_ack(&actions.replies[0].1);
        assert_eq!(ack.status, HelloStatus::Ok);
        assert_ne!(ack.session_id, 0);
        assert_eq!(ack.liveness_timeout_ms, 3000);
        assert_eq!(actions.new_target, Some(addr([10, 0, 0, 5], 50000)));
        assert!(actions.force_idr);
        assert!(session.is_active());

        let info = session.client_info().unwrap();
        assert_eq!(info.screen_px, (2420, 1668));
        assert_eq!(
            info.refresh_hz, 120,
            "panel refresh feeds the VDD mode list"
        );
    }

    #[test]
    fn duplicate_nonce_gets_identical_ack_without_new_session() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);

        let first = session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        let first_id = parse_ack(&first.replies[0].1).session_id;

        let dup = session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        assert_eq!(parse_ack(&dup.replies[0].1).session_id, first_id);
        assert!(dup.new_target.is_none(), "retransmit must not re-target");
        assert!(!dup.force_idr);
    }

    #[test]
    fn new_nonce_from_same_ip_supersedes_in_place() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let first = session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        let first_id = parse_ack(&first.replies[0].1).session_id;

        // App relaunched: new ephemeral source port, new nonce.
        let relaunched = addr([10, 0, 0, 5], 50101);
        let second = session.handle_control_authed(relaunched, hello(2, 50101), &TestConfig, now);
        let second_id = parse_ack(&second.replies[0].1).session_id;

        assert_ne!(
            second_id, first_id,
            "supersede must mint a fresh session id"
        );
        assert_eq!(second.new_target, Some(addr([10, 0, 0, 5], 50101)));
        assert!(second.force_idr);
    }

    #[test]
    fn different_ip_is_rejected_busy_while_active() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        session.handle_control_authed(
            addr([10, 0, 0, 5], 50000),
            hello(1, 50000),
            &TestConfig,
            now,
        );

        let intruder = addr([10, 0, 0, 9], 40000);
        let actions = session.handle_control_authed(intruder, hello(9, 40000), &TestConfig, now);
        let ack = parse_ack(&actions.replies[0].1);
        assert_eq!(ack.status, HelloStatus::Busy);
        assert_eq!(ack.session_id, 0);
        assert!(actions.new_target.is_none());
        assert_eq!(session.peer(), Some(addr([10, 0, 0, 5], 50000)));
    }

    #[test]
    fn an_update_handoff_is_atomic_with_admitting_clients() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);

        // Nobody streaming: the handoff starts and new clients are turned away.
        let mut session = Session::new(1);
        assert!(session.begin_update(false));
        let refused = session.handle_control(peer, 0, hello(1, 50000), &TestConfig, now);
        assert_eq!(parse_ack(&refused.replies[0].1).status, HelloStatus::Busy);
        assert!(refused.new_target.is_none());
        assert!(!session.is_active(), "no stream for the update to cut");

        // The handoff failed: the same client is admitted again.
        session.end_update();
        let admitted = session.handle_control(peer, 0, hello(2, 50000), &TestConfig, now);
        assert_eq!(admitted.new_target, Some(peer));

        // A client that got in first wins: the handoff does not start.
        assert!(!session.begin_update(false));
        let still = session.handle_control(peer, 0, hello(2, 50000), &TestConfig, now);
        assert_eq!(parse_ack(&still.replies[0].1).status, HelloStatus::Ok);
        assert!(session.tick(&TestConfig, false, now).replies.is_empty());

        // Unless the user chose Stop and update: that stream ends in the same
        // step, and the next client is turned away.
        let id = session.session_id().unwrap();
        assert!(session.begin_update(true));
        assert!(!session.is_active());
        let other = addr([10, 0, 0, 6], 50000);
        let refused = session.handle_control(other, 0, hello(3, 50000), &TestConfig, now);
        assert_eq!(parse_ack(&refused.replies[0].1).status, HelloStatus::Busy);
        // The next tick tells the iPad, once.
        let ticked = session.tick(&TestConfig, false, now);
        assert!(ticked.client_lost);
        assert!(ticked.replies.is_empty());
        let (to, goodbye) = ticked.goodbye.unwrap();
        assert_eq!(to, peer);
        let (header, bye) = eternal_wire::v2::control::parse_control(&goodbye).unwrap();
        assert_eq!(header.session_id, id);
        assert_eq!(bye, ControlMessage::Bye(ByeReason::HostShuttingDown));
        assert!(session.take_goodbye().is_none());
    }

    #[test]
    fn unsupported_version_is_refused() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let msg = ControlMessage::Hello2(Hello2 {
            proto_min: 3,
            proto_max: 4,
            client_nonce: 1,
            listen_port: 50000,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: 0,
            screen_px_w: 1,
            screen_px_h: 1,
            screen_pt_w: 1,
            screen_pt_h: 1,
            refresh_hz: 60,
            device_name: String::new(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let actions = session.handle_control_authed(peer, msg, &TestConfig, now);
        assert_eq!(
            parse_ack(&actions.replies[0].1).status,
            HelloStatus::VersionUnsupported
        );
        assert!(!session.is_active());
    }

    #[test]
    fn bye_and_liveness_expiry_tear_down() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        let first = session.session_id().unwrap();

        let bye = session.handle_control_authed(
            peer,
            ControlMessage::Bye(ByeReason::UserDisconnect),
            &TestConfig,
            now,
        );
        assert!(bye.client_lost);
        assert!(!session.is_active());
        assert_eq!(
            session.last_end(),
            Some((first, EndReason::Goodbye(ByeReason::UserDisconnect)))
        );

        assert!(bye.goodbye.is_none(), "the client said goodbye itself");

        // Re-establish, then let liveness lapse: the iPad is told.
        session.handle_control_authed(peer, hello(2, 50000), &TestConfig, now);
        let second = session.session_id().unwrap();
        let expired = session.tick(&TestConfig, false, now + LIVENESS_TIMEOUT);
        assert!(expired.client_lost);
        assert!(!session.is_active());
        assert_eq!(session.last_end(), Some((second, EndReason::Liveness)));
        assert_eq!(goodbye_of(&expired), (peer, second, ByeReason::Error));
    }

    #[test]
    fn a_pipeline_restart_gap_does_not_expire_the_client() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        // The transport was down for 11 s (a capture restart with backoff).
        let resumed = now + Duration::from_secs(11);
        session.transport_resumed(resumed);
        assert!(!session.tick(&TestConfig, false, resumed).client_lost);
        assert!(session.is_active());
        // A client that stays silent after the restart still expires.
        let expired = session.tick(&TestConfig, false, resumed + LIVENESS_TIMEOUT);
        assert!(expired.client_lost);
    }

    #[test]
    fn reports_extend_liveness_and_are_stored() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);

        let later = now + Duration::from_millis(2500);
        let report = ReceiverReport {
            frames_complete: 99,
            ..ReceiverReport::default()
        };
        session.handle_control_authed(
            peer,
            ControlMessage::ReceiverReport(report),
            &TestConfig,
            later,
        );

        // Would have been held at now+3s without the report.
        let ticked = session.tick(&TestConfig, false, now + Duration::from_millis(4000));
        assert!(!ticked.client_lost && !ticked.release_input);
        assert_eq!(session.last_report().unwrap().frames_complete, 99);

        // Held 3s after the report, and ended at the hold limit.
        let held = session.tick(&TestConfig, false, later + LIVENESS_TIMEOUT);
        assert!(held.release_input && !held.client_lost);
        let expired = session.tick(&TestConfig, false, later + HOLD_LIMIT);
        assert!(expired.client_lost);
    }

    #[test]
    fn keyframe_requests_are_rate_limited() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);

        let request = ControlMessage::KeyframeRequest(KeyframeRequest {
            stream_epoch: 7,
            last_complete_seq: 10,
            reason: KeyframeReason::GapLoss,
        });

        let first = session.handle_control_authed(peer, request.clone(), &TestConfig, now);
        assert!(first.force_idr);

        let spammed = session.handle_control_authed(
            peer,
            request.clone(),
            &TestConfig,
            now + Duration::from_millis(100),
        );
        assert!(
            !spammed.force_idr,
            "requests inside the window are coalesced"
        );

        let granted_again = session.handle_control_authed(
            peer,
            request,
            &TestConfig,
            now + Duration::from_millis(600),
        );
        assert!(granted_again.force_idr);
    }

    #[test]
    fn a_resume_is_an_ordinary_rate_limited_keyframe_request() {
        // A 0.3.4 iPad holding a picture asks for a keyframe every 600 ms and
        // sends Resume after 2 s. Resume asks for one IDR under the same
        // 500 ms limit as any other reason, and for nothing else: the
        // encoder reopens only on AMF, through its keyframe reopen.
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        let request = |reason| {
            ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: 7,
                last_complete_seq: 10,
                reason,
            })
        };

        let mut granted = Vec::new();
        for (millis, reason) in [
            (1_800, KeyframeReason::GapLoss),
            (2_000, KeyframeReason::Resume),
            (2_400, KeyframeReason::GapLoss),
            (2_600, KeyframeReason::Resume),
        ] {
            let actions = session.handle_control_authed(
                peer,
                request(reason),
                &TestConfig,
                now + Duration::from_millis(millis),
            );
            assert!(actions.replies.is_empty(), "{millis} ms");
            assert!(actions.new_target.is_none() && !actions.client_lost);
            if actions.force_idr {
                granted.push(millis);
            }
        }
        assert_eq!(granted, [1_800, 2_400]);

        let resume = session.handle_control_authed(
            peer,
            request(KeyframeReason::Resume),
            &TestConfig,
            now + Duration::from_millis(3_000),
        );
        assert!(resume.force_idr, "granted once the window has passed");
    }

    fn input_event(event_id: u32) -> ControlMessage {
        ControlMessage::InputEvent(InputEvent {
            input_ver: 1,
            kind: 0,
            phase: 0,
            buttons: 1,
            event_id,
            x_norm: 100,
            y_norm: 100,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: 0,
            modifiers: 0,
            client_time_us: 0,
            tilt_x: 0,
            tilt_y: 0,
        })
    }

    #[test]
    fn input_relayed_only_for_sessions_that_asked() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);

        // View-only session: input is dropped.
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        let dropped = session.handle_control_authed(peer, input_event(1), &TestConfig, now);
        assert!(
            dropped.input.is_none(),
            "view-only sessions must not inject"
        );

        // Re-handshake asking for input relay.
        let mut wants = match hello(2, 50000) {
            ControlMessage::Hello2(h) => h,
            _ => unreachable!(),
        };
        wants.feature_caps = FEATURE_WANTS_INPUT;
        session.handle_control_authed(peer, ControlMessage::Hello2(wants), &TestConfig, now);

        let relayed = session.handle_control_authed(peer, input_event(2), &TestConfig, now);
        assert!(relayed.input.is_some());

        // A different IP can't inject into this session.
        let intruder = addr([10, 0, 0, 9], 40000);
        let foreign = session.handle_control_authed(intruder, input_event(3), &TestConfig, now);
        assert!(foreign.input.is_none());
    }

    #[test]
    fn the_seq_window_takes_each_number_once_within_1024() {
        let mut window = SeqWindow::default();
        assert!(window.accept(5000));
        assert!(!window.accept(5000));
        // A gap, then a late arrival inside the window, once.
        assert!(window.accept(5003));
        assert!(window.accept(5001));
        assert!(!window.accept(5001));
        assert!(window.accept(5002));
        // 1,024 wide: highest - 1023 is the oldest number still taken.
        assert!(window.accept(6026));
        assert!(!window.accept(5003));
        assert!(window.accept(5004));
        assert!(!window.accept(5002));
        // A jump past the whole window forgets everything older.
        assert!(window.accept(9000));
        assert!(!window.accept(6026));
        assert!(window.accept(8000));
        assert!(!window.accept(8000));
    }

    #[test]
    fn the_seq_window_follows_the_counter_through_its_wrap() {
        let mut window = SeqWindow::default();
        for seq in (u32::MAX - 10..=u32::MAX).chain(0..10) {
            assert!(window.accept(seq), "{seq}");
        }
        assert!(!window.accept(u32::MAX));
        assert!(!window.accept(3));
        assert!(window.accept(u32::MAX - 20));
        assert!(!window.accept(u32::MAX - 1100));
    }

    /// Taps as the iPad sends them over Wi-Fi: each press and release twice
    /// with one event_id, every packet with its own msg_seq. Reordered,
    /// with packets repeated, the first copy of one release lost and its
    /// second copy late, every press and release lands exactly once. A press
    /// that arrives again after the event_id deduper forgot it doesn't land
    /// again.
    #[test]
    fn reordered_and_repeated_input_lands_each_press_and_release_once() {
        use crate::input::{EventDeduper, PHASE_BEGAN, PHASE_ENDED, PHASE_MOVED};
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let ControlMessage::Hello2(mut wants) = hello(1, 50000) else {
            unreachable!()
        };
        wants.feature_caps = FEATURE_WANTS_INPUT;
        session.handle_control(peer, 0, ControlMessage::Hello2(wants), &TestConfig, now);
        let session_id = session.session_id().unwrap();

        let event = |event_id, phase| {
            let ControlMessage::InputEvent(mut event) = input_event(event_id) else {
                unreachable!()
            };
            event.phase = phase;
            event
        };
        let mut sent = Vec::new();
        for tap in 0..40 {
            let (press, release) = (tap * 2 + 1, tap * 2 + 2);
            for (event_id, phase) in [
                (press, PHASE_BEGAN),
                (press, PHASE_BEGAN),
                (0, PHASE_MOVED),
                (release, PHASE_ENDED),
                (release, PHASE_ENDED),
            ] {
                // The iPad's counter starts at 1 and also numbers its
                // reports and pings, so input numbers have gaps.
                let msg_seq = sent.len() as u32 * 2 + 1;
                sent.push((msg_seq, event(event_id, phase)));
            }
        }
        // Tap 3's release: the first copy is lost and the second is late.
        let late = sent.remove(19);
        let lost = sent.remove(18);
        for (_, copy) in [&late, &lost] {
            assert_eq!((copy.event_id, copy.phase), (8, PHASE_ENDED));
        }
        let mut delivered = Vec::new();
        for (n, chunk) in sent.chunks(7).enumerate() {
            let mut chunk = chunk.to_vec();
            if n % 2 == 1 {
                chunk.reverse();
            }
            // Every third chunk arrives twice over.
            if n % 3 == 0 {
                chunk.extend(chunk.clone());
            }
            delivered.extend(chunk);
        }
        delivered.push(late);

        let mut deduper = EventDeduper::default();
        let mut deliver = |session: &mut Session, msg_seq, event| {
            let header = ControlHeader {
                packet_type: eternal_wire::v2::PacketType::InputEvent,
                session_id,
                msg_seq,
            };
            let message = ControlMessage::InputEvent(event);
            let actions = session.handle_datagram(peer, &header, message, &TestConfig, now);
            actions
                .input
                .map(|(_, event)| event)
                .filter(|event| deduper.accept(event))
        };
        let mut landed = std::collections::HashMap::<(u32, u8), u32>::new();
        for (msg_seq, event) in delivered {
            if let Some(event) = deliver(&mut session, msg_seq, event) {
                if event.phase != PHASE_MOVED {
                    *landed.entry((event.event_id, event.phase)).or_default() += 1;
                }
            }
        }
        for tap in 0..40 {
            assert_eq!(landed.get(&(tap * 2 + 1, PHASE_BEGAN)), Some(&1), "{tap}");
            assert_eq!(landed.get(&(tap * 2 + 2, PHASE_ENDED)), Some(&1), "{tap}");
        }
        assert_eq!(landed.len(), 80);

        // The first press, once more: its event_id left the deduper long ago.
        let first = event(1, PHASE_BEGAN);
        assert!(EventDeduper::default().accept(&first));
        assert!(deliver(&mut session, 1, first).is_none());
    }

    #[test]
    fn control_messages_need_the_negotiated_session_id() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut wants = match hello(1, 50000) {
            ControlMessage::Hello2(h) => h,
            _ => unreachable!(),
        };
        wants.feature_caps = FEATURE_WANTS_INPUT;
        session.handle_control(peer, 0, ControlMessage::Hello2(wants), &TestConfig, now);
        let live = session.session_id().unwrap();

        // Same source address, wrong session id: another process on that
        // device, or a spoofed packet on the local link. It must not move the
        // mouse, steer the bitrate, or end the session.
        let wrong = live.wrapping_add(1);
        assert!(session
            .handle_control(peer, wrong, input_event(1), &TestConfig, now)
            .input
            .is_none());
        assert!(session
            .handle_control(
                peer,
                wrong,
                ControlMessage::ReceiverReport(ReceiverReport::default()),
                &TestConfig,
                now
            )
            .report
            .is_none());
        let bye = session.handle_control(
            peer,
            wrong,
            ControlMessage::Bye(ByeReason::UserDisconnect),
            &TestConfig,
            now,
        );
        assert!(
            !bye.client_lost,
            "a foreign BYE must not tear the session down"
        );
        assert!(session.is_active());

        // The real client still works.
        assert!(session
            .handle_control(peer, live, input_event(2), &TestConfig, now)
            .input
            .is_some());
    }

    #[test]
    fn input_extends_liveness() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut wants = match hello(1, 50000) {
            ControlMessage::Hello2(h) => h,
            _ => unreachable!(),
        };
        wants.feature_caps = FEATURE_WANTS_INPUT;
        session.handle_control_authed(peer, ControlMessage::Hello2(wants), &TestConfig, now);

        // A drag mid-window keeps the session alive past the original deadline.
        let later = now + Duration::from_millis(2500);
        session.handle_control_authed(peer, input_event(1), &TestConfig, later);
        let ticked = session.tick(&TestConfig, false, now + Duration::from_millis(4000));
        assert!(!ticked.client_lost);
    }

    #[test]
    fn heartbeats_flow_only_while_active() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        assert!(session.tick(&TestConfig, true, now).replies.is_empty());

        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        let ticked = session.tick(&TestConfig, true, now + Duration::from_millis(100));
        assert_eq!(ticked.replies.len(), 1);
        let (_, message) = eternal_wire::v2::control::parse_control(&ticked.replies[0].1).unwrap();
        assert!(matches!(message, ControlMessage::Heartbeat(_)));
    }

    struct PairConfig(parking_lot::Mutex<crate::pairing::Pairing>);
    impl ConfigSource for PairConfig {
        fn stream_config(&self) -> StreamConfig {
            TestConfig.stream_config()
        }
        fn host_name(&self) -> String {
            TestConfig.host_name()
        }
        fn pairing(&self) -> Option<&parking_lot::Mutex<crate::pairing::Pairing>> {
            Some(&self.0)
        }
    }
    fn pair_config(required: bool) -> PairConfig {
        PairConfig(parking_lot::Mutex::new(
            crate::pairing::Pairing::new(required, [7; 16]).unwrap(),
        ))
    }
    fn pair_hello(nonce: u32) -> Hello2 {
        let ControlMessage::Hello2(mut hello) = hello(nonce, 50000) else {
            unreachable!()
        };
        hello.device_id = 17;
        hello
    }
    fn pair_ack(actions: &Actions) -> HelloAck {
        let (_, ControlMessage::HelloAck(ack)) =
            eternal_wire::v2::control::parse_control(&actions.replies[0].1).unwrap()
        else {
            panic!("expected ACK")
        };
        ack
    }
    fn pair_send(
        session: &mut Session,
        config: &PairConfig,
        peer: PeerId,
        hello: Hello2,
        now: Instant,
    ) -> Actions {
        session.handle_control(peer, 0, ControlMessage::Hello2(hello), config, now)
    }

    #[test]
    fn retransmitted_rejected_handshake_is_one_pairing_attempt() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let peer = addr([192, 0, 2, 1], 50000);
        let now = Instant::now();
        let mut hello = pair_hello(1);
        for retry in 0..8 {
            let ack = pair_ack(&pair_send(
                &mut session,
                &config,
                peer,
                hello.clone(),
                now + Duration::from_millis(retry * 300),
            ));
            assert_eq!(ack.status, HelloStatus::Unauthorized);
            assert_eq!(ack.auth_token, [0; 16]);
        }
        hello.client_nonce = 2;
        hello.pairing_code = config.0.lock().code();
        assert_eq!(
            pair_ack(&pair_send(
                &mut session,
                &config,
                peer,
                hello,
                now + Duration::from_secs(3)
            ))
            .status,
            HelloStatus::Ok
        );
    }

    #[test]
    fn changing_credentials_with_the_same_nonce_still_counts_as_guesses() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let peer = addr([192, 0, 2, 1], 50000);
        let now = Instant::now();
        let mut hello = pair_hello(1);
        for guess in 1..=5 {
            hello.pairing_code = 1_000_000 + guess;
            let ack = pair_ack(&pair_send(&mut session, &config, peer, hello.clone(), now));
            assert_eq!(
                ack.status,
                if guess == 5 {
                    HelloStatus::RateLimited
                } else {
                    HelloStatus::Unauthorized
                }
            );
        }
    }

    #[test]
    fn pairing_required_code_rotation_retry_token_and_regeneration() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let peer = addr([192, 0, 2, 1], 50000);
        let now = Instant::now();
        let mut hello = pair_hello(1);
        let rejected = pair_send(&mut session, &config, peer, hello.clone(), now);
        assert_eq!(pair_ack(&rejected).status, HelloStatus::Unauthorized);
        assert_eq!(pair_ack(&rejected).auth_token, [0; 16]);
        assert!(rejected.new_target.is_none());
        assert!(!session.is_active());
        hello.pairing_code = config.0.lock().code();
        let accepted = pair_send(&mut session, &config, peer, hello.clone(), now);
        let ack = pair_ack(&accepted);
        assert_eq!(ack.status, HelloStatus::Ok);
        assert_eq!(ack.auth_token, [7; 16]);
        assert_ne!(config.0.lock().code(), hello.pairing_code);
        let duplicate = pair_send(&mut session, &config, peer, hello.clone(), now);
        assert_eq!(duplicate.replies, accepted.replies);
        assert!(duplicate.new_target.is_none());
        let mut modified = hello.clone();
        modified.listen_port += 1;
        assert_eq!(
            pair_ack(&pair_send(&mut session, &config, peer, modified, now)).status,
            HelloStatus::Unauthorized
        );
        hello.client_nonce += 1;
        assert_eq!(
            pair_ack(&pair_send(&mut session, &config, peer, hello.clone(), now)).status,
            HelloStatus::Unauthorized
        );
        hello.auth_token = ack.auth_token;
        hello.pairing_code = 0;
        let reconnected = pair_send(&mut session, &config, peer, hello.clone(), now);
        assert_eq!(pair_ack(&reconnected).status, HelloStatus::Ok);
        assert_eq!(pair_ack(&reconnected).auth_token, [0; 16]);
        assert_ne!(pair_ack(&reconnected).session_id, ack.session_id);
        config.0.lock().regenerate_token().unwrap();
        let stale = pair_send(&mut session, &config, peer, hello, now);
        assert_eq!(pair_ack(&stale).status, HelloStatus::Unauthorized);
        assert_eq!(pair_ack(&stale).auth_token, [0; 16]);
        assert!(session.tick(&config, false, now).client_lost);
        assert!(!session.is_active());
        assert_eq!(
            session.last_end(),
            Some((pair_ack(&reconnected).session_id, EndReason::Revoked))
        );
    }

    #[test]
    fn pairing_optional_usb_and_busy_do_not_consume_a_code() {
        let peer = addr([192, 0, 2, 1], 50000);
        let now = Instant::now();
        let config = pair_config(false);
        let mut session = Session::new(10);
        let accepted = pair_send(&mut session, &config, peer, pair_hello(1), now);
        assert_eq!(pair_ack(&accepted).status, HelloStatus::Ok);
        assert_eq!(pair_ack(&accepted).auth_token, [0; 16]);
        config.0.lock().required = true;
        // Requiring pairing now must invalidate the unpaired duplicate too.
        assert_eq!(
            pair_ack(&pair_send(&mut session, &config, peer, pair_hello(1), now)).status,
            HelloStatus::Unauthorized
        );
        assert!(session.tick(&config, false, now).client_lost);
        let usb = PeerId {
            link: LinkId::Usb { device_id: 1 },
            addr: None,
        };
        let code = config.0.lock().code();
        let accepted = pair_send(&mut session, &config, usb, pair_hello(2), now);
        assert_eq!(pair_ack(&accepted).status, HelloStatus::Ok);
        assert_eq!(pair_ack(&accepted).auth_token, [7; 16]);
        assert_eq!(config.0.lock().code(), code);
        let mut foreign = pair_hello(3);
        foreign.device_id += 1;
        foreign.pairing_code = code;
        let busy = pair_send(&mut session, &config, peer, foreign, now);
        assert_eq!(pair_ack(&busy).status, HelloStatus::Busy);
        assert_eq!(pair_ack(&busy).auth_token, [0; 16]);
        assert_eq!(config.0.lock().code(), code);
    }

    #[test]
    fn pairing_rate_limit_blocks_code_guesses_but_preserves_paired_and_usb_access() {
        let config = pair_config(true);
        let now = Instant::now();
        let peer = addr([192, 0, 2, 1], 50000);
        let mut session = Session::new(10);
        for nonce in 1..=5 {
            let ack = pair_ack(&pair_send(
                &mut session,
                &config,
                peer,
                pair_hello(nonce),
                now,
            ));
            assert_eq!(
                ack.status,
                if nonce == 5 {
                    HelloStatus::RateLimited
                } else {
                    HelloStatus::Unauthorized
                }
            );
            assert_eq!(ack.auth_token, [0; 16]);
        }
        let mut hello = pair_hello(6);
        hello.pairing_code = config.0.lock().code();
        assert_eq!(
            pair_ack(&pair_send(&mut session, &config, peer, hello.clone(), now)).status,
            HelloStatus::RateLimited
        );
        hello.auth_token = [7; 16];
        let paired = pair_ack(&pair_send(&mut session, &config, peer, hello.clone(), now));
        assert_eq!(paired.status, HelloStatus::Ok);
        assert_eq!(paired.auth_token, [0; 16]);
        hello.auth_token = [0; 16];
        hello.client_nonce += 1;
        assert_eq!(
            pair_ack(&pair_send(
                &mut session,
                &config,
                peer,
                hello,
                now + Duration::from_secs(60)
            ))
            .status,
            HelloStatus::Ok
        );
    }

    #[test]
    fn only_a_new_pairing_gets_the_token() {
        let config = pair_config(true);
        let now = Instant::now();
        let wifi = addr([192, 0, 2, 1], 50000);
        let usb = PeerId {
            link: LinkId::Usb { device_id: 1 },
            addr: None,
        };
        let mut session = Session::new(10);
        let mut send = |peer, nonce, token: [u8; 16], code| {
            let mut hello = pair_hello(nonce);
            hello.auth_token = token;
            hello.pairing_code = code;
            pair_send(&mut session, &config, peer, hello, now)
        };

        // The code pairs, and a retransmit of that HELLO, after a lost ACK,
        // gets the same ACK with the token.
        let code = config.0.lock().code();
        let paired = send(wifi, 1, [0; 16], code);
        assert_eq!(pair_ack(&paired).status, HelloStatus::Ok);
        assert_eq!(pair_ack(&paired).auth_token, [7; 16]);
        assert_eq!(send(wifi, 1, [0; 16], code).replies, paired.replies);

        // Reconnects with the token, over Wi-Fi or USB, don't get it again.
        for (peer, nonce) in [(wifi, 2), (usb, 3)] {
            let ack = pair_ack(&send(peer, nonce, [7; 16], 0));
            assert_eq!(ack.status, HelloStatus::Ok);
            assert_eq!(ack.auth_token, [0; 16]);
        }

        // USB pairs an iPad without the token, or with an old one.
        for (nonce, token) in [(4, [0; 16]), (5, [9; 16])] {
            let ack = pair_ack(&send(usb, nonce, token, 0));
            assert_eq!(ack.status, HelloStatus::Ok);
            assert_eq!(ack.auth_token, [7; 16]);
        }

        // With pairing off there is no token to give.
        config.0.lock().required = false;
        let ack = pair_ack(&send(usb, 6, [0; 16], 0));
        assert_eq!(ack.status, HelloStatus::Ok);
        assert_eq!(ack.auth_token, [0; 16]);
    }

    fn ended(actions: &Actions) -> SessionEnd {
        actions.ended.clone().expect("a session end")
    }

    #[test]
    fn a_goodbye_ends_the_session_with_its_nonce_first_video_and_length() {
        let mut session = Session::new(500);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let accepted = session.handle_control(peer, 0, hello(0xA1, 50000), &TestConfig, now);
        assert_eq!(accepted.ended, None);
        let id = session.session_id().unwrap();
        session.note_video_sent(id + 1, now + Duration::from_millis(10));
        session.note_video_sent(id, now + Duration::from_millis(40));
        session.note_video_sent(id, now + Duration::from_millis(90));
        // Frames count once a report shows some; a later empty one keeps them.
        for frames_complete in [0, 3, 0] {
            let report = ControlMessage::ReceiverReport(ReceiverReport {
                frames_complete,
                ..Default::default()
            });
            session.handle_control_authed(peer, report, &TestConfig, now);
        }
        let bye = session.handle_control_authed(
            peer,
            ControlMessage::Bye(ByeReason::UserDisconnect),
            &TestConfig,
            now + Duration::from_secs(2),
        );
        assert_eq!(
            ended(&bye),
            SessionEnd {
                session_id: Some(id),
                client_nonce: 0xA1,
                link: LinkId::Udp,
                reason: EndReason::Goodbye(ByeReason::UserDisconnect),
                first_video: Some(Duration::from_millis(40)),
                frames_received: true,
                duration: Duration::from_secs(2),
            }
        );
    }

    #[test]
    fn liveness_expiry_and_a_new_nonce_end_sessions() {
        let mut session = Session::new(500);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control(peer, 0, hello(1, 50000), &TestConfig, now);
        let first = session.session_id().unwrap();
        let again = session.handle_control(peer, 0, hello(2, 50000), &TestConfig, now);
        let old = ended(&again);
        assert_eq!(
            (old.session_id, old.client_nonce, old.reason),
            (Some(first), 1, EndReason::Superseded)
        );
        let second = session.session_id().unwrap();
        let expired = session.tick(&TestConfig, false, now + LIVENESS_TIMEOUT);
        let lapsed = ended(&expired);
        assert_eq!(
            (
                lapsed.session_id,
                lapsed.client_nonce,
                lapsed.reason,
                lapsed.first_video
            ),
            (Some(second), 2, EndReason::Liveness, None)
        );
        assert_eq!(lapsed.duration, LIVENESS_TIMEOUT);
        assert!(!lapsed.frames_received);
        assert_eq!(goodbye_of(&expired).1, second);
        assert_eq!(
            session
                .tick(&TestConfig, false, now + LIVENESS_TIMEOUT)
                .ended,
            None
        );
    }

    #[test]
    fn a_wifi_goodbye_ends_at_the_cable_takeover_or_when_none_comes() {
        let now = Instant::now();
        let wifi = addr([10, 0, 0, 5], 50000);
        let usb = PeerId::usb(17);
        for takeover in [true, false] {
            let mut session = Session::new(500);
            session.handle_control(wifi, 0, identified_hello(1, 77), &TestConfig, now);
            let bye = session.handle_control_authed(
                wifi,
                ControlMessage::Bye(ByeReason::Superseded),
                &TestConfig,
                now,
            );
            assert_eq!(bye.ended, None, "the display waits for the cable");
            let end = if takeover {
                ended(&session.handle_control(usb, 0, identified_hello(2, 77), &TestConfig, now))
            } else {
                ended(&session.tick(&TestConfig, false, now + USB_TAKEOVER_TIMEOUT))
            };
            let reason = if takeover {
                EndReason::Superseded
            } else {
                EndReason::Goodbye(ByeReason::Superseded)
            };
            assert_eq!(
                (end.client_nonce, end.link, end.reason),
                (1, LinkId::Udp, reason)
            );
        }
    }

    #[test]
    fn a_closed_cable_an_update_and_quitting_end_the_session() {
        let now = Instant::now();
        let usb = PeerId::usb(17);
        let mut session = Session::new(500);
        session.handle_control(usb, 0, identified_hello(3, 77), &TestConfig, now);
        assert_eq!(session.link_lost(LinkId::Udp).ended, None);
        let lost = ended(&session.link_lost(usb.link));
        assert_eq!(
            (lost.client_nonce, lost.link, lost.reason),
            (3, usb.link, EndReason::LinkLost)
        );

        session.handle_control(usb, 0, identified_hello(4, 77), &TestConfig, now);
        assert!(session.begin_update(true));
        let stopped = session.tick(&TestConfig, false, now);
        assert!(stopped.goodbye.is_some());
        assert_eq!(
            (ended(&stopped).client_nonce, ended(&stopped).reason),
            (4, EndReason::Shutdown)
        );
        assert_eq!(session.shut_down(now), None, "reported once");

        session.end_update();
        session.handle_control(usb, 0, identified_hello(5, 77), &TestConfig, now);
        let quit = session.shut_down(now + Duration::from_secs(3)).unwrap();
        assert_eq!((quit.client_nonce, quit.reason), (5, EndReason::Shutdown));
        assert_eq!(quit.duration, Duration::from_secs(3));
        assert!(!session.is_active());
    }

    #[test]
    fn a_refused_hello_is_one_attempt_without_a_session() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut session = Session::new(500);
        let ControlMessage::Hello2(mut old) = hello(9, 50000) else {
            unreachable!()
        };
        old.proto_min = 3;
        old.proto_max = 3;
        let refused = session.handle_control(
            peer,
            0,
            ControlMessage::Hello2(old.clone()),
            &TestConfig,
            now,
        );
        assert_eq!(
            ended(&refused),
            SessionEnd {
                session_id: None,
                client_nonce: 9,
                link: LinkId::Udp,
                reason: EndReason::Refused(HelloStatus::VersionUnsupported),
                first_video: None,
                frames_received: false,
                duration: Duration::ZERO,
            }
        );
        let retransmit =
            session.handle_control(peer, 0, ControlMessage::Hello2(old), &TestConfig, now);
        assert_eq!(retransmit.replies.len(), 1);
        assert_eq!(retransmit.ended, None);

        session.handle_control(peer, 0, hello(10, 50000), &TestConfig, now);
        let other = addr([10, 0, 0, 6], 50000);
        let busy = session.handle_control(other, 0, hello(11, 50000), &TestConfig, now);
        assert_eq!(ended(&busy).reason, EndReason::Refused(HelloStatus::Busy));
        assert!(session.is_active(), "the streaming session goes on");

        let config = pair_config(true);
        let mut paired = Session::new(500);
        let unpaired = pair_send(&mut paired, &config, peer, pair_hello(12), now);
        assert_eq!(
            ended(&unpaired).reason,
            EndReason::Refused(HelloStatus::Unauthorized)
        );
        // Two attempts' retransmits interleaved are still two attempts.
        let ends: Vec<u32> = [12, 13, 12, 13, 13]
            .into_iter()
            .filter_map(|nonce| pair_send(&mut paired, &config, peer, pair_hello(nonce), now).ended)
            .map(|end| end.client_nonce)
            .collect();
        assert_eq!(ends, [13]);
        // A minute later, a nonce counts again.
        let later = now + REFUSED_FOR;
        assert!(pair_send(&mut paired, &config, peer, pair_hello(13), later)
            .ended
            .is_some());
        assert!(pair_send(&mut paired, &config, peer, pair_hello(13), later)
            .ended
            .is_none());
    }

    /// Seventeen nonces cycled a hundred times are seventeen attempts, and a
    /// flood of new ones gets 20 summaries a minute and one line with the rest.
    #[test]
    fn refused_attempts_cannot_flood_the_summaries() {
        let config = pair_config(true);
        let peer = addr([10, 0, 0, 9], 50000);
        let now = Instant::now();
        let mut session = Session::new(500);
        let cycled = (0..100)
            .flat_map(|_| 0..17u32)
            .filter(|&nonce| {
                pair_send(&mut session, &config, peer, pair_hello(nonce), now)
                    .ended
                    .is_some()
            })
            .count();
        assert_eq!(cycled, 17);

        let mut session = Session::new(500);
        let flood = (1000..1100u32)
            .filter(|&nonce| {
                pair_send(&mut session, &config, peer, pair_hello(nonce), now)
                    .ended
                    .is_some()
            })
            .count();
        assert_eq!(flood, REFUSED_SUMMARIES as usize);
        assert_eq!(session.refusal_minute.unwrap().left_out, 80);

        #[derive(Default)]
        struct LeftOut(std::sync::Arc<parking_lot::Mutex<Vec<u64>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LeftOut {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                struct Count<'a>(&'a parking_lot::Mutex<Vec<u64>>);
                impl tracing::field::Visit for Count<'_> {
                    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                        if field.name() == "left_out" {
                            self.0.lock().push(value);
                        }
                    }
                    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {
                    }
                }
                event.record(&mut Count(&self.0));
            }
        }
        use tracing_subscriber::prelude::*;
        let counts = LeftOut::default();
        let seen = counts.0.clone();
        tracing::subscriber::with_default(tracing_subscriber::registry().with(counts), || {
            session.tick(&config, false, now + Duration::from_secs(30));
            session.tick(&config, false, now + REFUSAL_MINUTE);
            session.tick(&config, false, now + REFUSAL_MINUTE * 2);
        });
        assert_eq!(*seen.lock(), [80], "one line for the minute");
        assert!(
            pair_send(
                &mut session,
                &config,
                peer,
                pair_hello(5000),
                now + REFUSAL_MINUTE * 2
            )
            .ended
            .is_some(),
            "a new minute summarizes again"
        );
    }

    #[test]
    fn changed_pairing_credentials_end_the_session() {
        let config = pair_config(true);
        let mut session = Session::new(500);
        let peer = addr([192, 0, 2, 1], 50000);
        let now = Instant::now();
        let mut hello = pair_hello(20);
        hello.pairing_code = config.0.lock().code();
        assert!(pair_send(&mut session, &config, peer, hello, now)
            .new_target
            .is_some());
        config.0.lock().regenerate_token().unwrap();
        let revoked = ended(&session.tick(&config, false, now));
        assert_eq!(
            (revoked.client_nonce, revoked.reason),
            (20, EndReason::Revoked)
        );
    }

    /// The goodbye's destination, session id and reason.
    fn goodbye_of(actions: &Actions) -> (PeerId, u32, ByeReason) {
        let (to, bytes) = actions.goodbye.clone().expect("a goodbye");
        let (header, message) = eternal_wire::v2::control::parse_control(&bytes).unwrap();
        let ControlMessage::Bye(reason) = message else {
            panic!("expected a goodbye, got {message:?}")
        };
        (to, header.session_id, reason)
    }

    fn report(frames_complete: u32) -> ControlMessage {
        ControlMessage::ReceiverReport(ReceiverReport {
            frames_complete,
            ..ReceiverReport::default()
        })
    }

    /// A streaming session whose client counted frames at `now`: it has a
    /// picture, so a silence holds it.
    fn picture_session(peer: PeerId, device_id: u64, now: Instant) -> (Session, u32) {
        let mut session = Session::new(700);
        let ControlMessage::Hello2(mut hello) = identified_hello(1, device_id) else {
            unreachable!()
        };
        hello.feature_caps |= FEATURE_WANTS_INPUT;
        session.handle_control(peer, 0, ControlMessage::Hello2(hello), &TestConfig, now);
        let id = session.session_id().unwrap();
        session.handle_control(peer, id, report(30), &TestConfig, now);
        (session, id)
    }

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    /// A client that had a picture and goes quiet keeps its session for the
    /// hold limit: at the liveness timeout its input is released once and
    /// another iPad may take the slot, but heartbeats go on under the same
    /// session id. At the hold limit it ends, and the iPad is told.
    #[test]
    fn a_silent_client_with_a_picture_is_held_then_told_goodbye() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let (mut session, id) = picture_session(peer, 77, start);

        let quiet = session.tick(&TestConfig, true, at(start, 2_900));
        assert!(!quiet.release_input && !session.on_hold());

        let held = session.tick(&TestConfig, true, at(start, 3_000));
        assert!(held.release_input);
        assert!(!held.client_lost && held.ended.is_none() && held.goodbye.is_none());
        assert!(session.on_hold());
        assert_eq!(session.session_id(), Some(id));
        assert_eq!(held.replies.len(), 1, "heartbeats go on");
        let (header, heartbeat) =
            eternal_wire::v2::control::parse_control(&held.replies[0].1).unwrap();
        assert!(matches!(heartbeat, ControlMessage::Heartbeat(_)));
        assert_eq!(header.session_id, id);

        let still = session.tick(&TestConfig, true, at(start, 9_900));
        assert!(!still.release_input, "input is released once");
        assert!(!still.client_lost && still.ended.is_none());
        assert_eq!(still.replies.len(), 1);

        let expired = session.tick(&TestConfig, true, at(start, 10_000));
        assert!(expired.client_lost && expired.replies.is_empty());
        let end = ended(&expired);
        assert_eq!(
            (
                end.session_id,
                end.reason,
                end.duration,
                end.frames_received
            ),
            (Some(id), EndReason::Liveness, HOLD_LIMIT, true)
        );
        assert_eq!(goodbye_of(&expired), (peer, id, ByeReason::Error));
        assert!(!session.is_active());
    }

    /// Anything from the client during the hold carries the session on: no
    /// new session, one IDR under the usual limit, and a full liveness
    /// window from that packet. A later silence holds it again.
    #[test]
    fn a_client_heard_during_the_hold_carries_on() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let request = ControlMessage::KeyframeRequest(KeyframeRequest {
            stream_epoch: 7,
            last_complete_seq: 10,
            reason: KeyframeReason::GapLoss,
        });
        let ping = ControlMessage::Ping(eternal_wire::v2::control::Ping { t1_us: 1 });
        let nack = ControlMessage::Nack(Nack {
            stream_epoch: 7,
            frame_seq: 42,
            frag_count: 2,
            missing: vec![1],
        });
        for first in [report(31), request.clone(), ping, nack, input_event(9)] {
            let (mut session, id) = picture_session(peer, 77, start);
            assert!(
                session
                    .tick(&TestConfig, false, at(start, 3_000))
                    .release_input
            );
            let resumed =
                session.handle_control(peer, id, first.clone(), &TestConfig, at(start, 7_200));
            assert!(resumed.force_idr, "{first:?}");
            assert!(resumed.new_target.is_none() && resumed.ended.is_none());
            assert!(!session.on_hold());
            // The client's own keyframe request in the same burst is coalesced.
            let burst =
                session.handle_control(peer, id, request.clone(), &TestConfig, at(start, 7_250));
            assert!(!burst.force_idr);
            for millis in [9_900, 10_000, 10_200] {
                let ticked = session.tick(&TestConfig, false, at(start, millis));
                assert!(!ticked.release_input && !ticked.client_lost, "{millis}");
            }
            assert!(
                session
                    .tick(&TestConfig, false, at(start, 10_250))
                    .release_input,
                "held again one window after the last packet"
            );
            assert_eq!(session.session_id(), Some(id));
            assert_eq!(session.last_end(), None);
        }
    }

    /// A client that never had a picture has nothing to keep: its session
    /// ends at the liveness timeout, as before, and the iPad is told.
    #[test]
    fn a_client_without_a_picture_ends_at_the_liveness_timeout_with_a_goodbye() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut session = Session::new(700);
        session.handle_control(peer, 0, identified_hello(1, 77), &TestConfig, start);
        let id = session.session_id().unwrap();
        session.handle_control(peer, id, report(0), &TestConfig, start);
        let expired = session.tick(&TestConfig, false, start + LIVENESS_TIMEOUT);
        assert!(expired.client_lost && !expired.release_input);
        assert_eq!(ended(&expired).reason, EndReason::Liveness);
        assert!(!ended(&expired).frames_received);
        assert_eq!(goodbye_of(&expired), (peer, id, ByeReason::Error));
    }

    /// Late packets of a session ended for liveness, from its client, get its
    /// goodbye again, once a second at most, for a minute. Nobody else gets
    /// it, and nothing answers a client's own goodbye.
    #[test]
    fn late_packets_of_a_session_ended_for_liveness_get_its_goodbye() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut session = Session::new(700);
        session.handle_control(peer, 0, identified_hello(1, 77), &TestConfig, start);
        let id = session.session_id().unwrap();
        let expired = session.tick(&TestConfig, false, start + LIVENESS_TIMEOUT);
        let (_, goodbye) = expired.goodbye.clone().unwrap();
        let ended_at = start + LIVENESS_TIMEOUT;
        let late = |session: &mut Session, source, session_id, message, millis| {
            session
                .handle_control(
                    source,
                    session_id,
                    message,
                    &TestConfig,
                    at(ended_at, millis),
                )
                .replies
        };
        // The burst just went.
        assert!(late(&mut session, peer, id, report(1), 500).is_empty());
        let answer = late(&mut session, peer, id, report(1), 1_000);
        assert_eq!(answer, vec![(peer, goodbye.clone())]);
        assert!(late(&mut session, peer, id, report(1), 1_900).is_empty());
        // Another port on the same iPad is the same client.
        let other_port = addr([10, 0, 0, 5], 50001);
        let ping = ControlMessage::Ping(eternal_wire::v2::control::Ping { t1_us: 1 });
        assert_eq!(
            late(&mut session, other_port, id, ping.clone(), 2_000),
            vec![(other_port, goodbye.clone())]
        );
        // Another address, another session id, or the client's own goodbye.
        assert!(late(
            &mut session,
            addr([10, 0, 0, 6], 50000),
            id,
            report(1),
            4_000
        )
        .is_empty());
        assert!(late(&mut session, peer, id + 1, report(1), 4_000).is_empty());
        assert!(late(&mut session, PeerId::usb(17), id, report(1), 4_000).is_empty());
        assert!(late(
            &mut session,
            peer,
            id,
            ControlMessage::Bye(ByeReason::UserDisconnect),
            4_000
        )
        .is_empty());
        assert_eq!(late(&mut session, peer, id, ping.clone(), 4_000).len(), 1);
        // A new session doesn't change what the old one's packets get; the
        // iPad drops a goodbye for a session id that isn't its own.
        session.handle_control(
            peer,
            0,
            identified_hello(2, 77),
            &TestConfig,
            at(ended_at, 5_000),
        );
        assert!(session.is_active());
        assert_eq!(late(&mut session, peer, id, report(1), 5_000).len(), 1);
        assert!(late(&mut session, peer, id, report(1), 59_999).len() == 1);
        assert!(late(&mut session, peer, id, report(1), 61_000).is_empty());
    }

    /// Ends the host didn't decide on silence send no goodbye of their own:
    /// the client's goodbye, a cable pulled, a Wi-Fi goodbye whose cable
    /// takeover never came, and an update (which has its own).
    #[test]
    fn only_a_silence_sends_the_liveness_goodbye() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let (mut session, id) = picture_session(peer, 77, start);
        let bye = session.handle_control(
            peer,
            id,
            ControlMessage::Bye(ByeReason::UserDisconnect),
            &TestConfig,
            start,
        );
        assert!(bye.client_lost && bye.goodbye.is_none());
        assert!(session
            .handle_control(peer, id, report(1), &TestConfig, at(start, 2_000))
            .replies
            .is_empty());

        let usb = PeerId::usb(17);
        let (mut session, _) = picture_session(usb, 77, start);
        let pulled = session.link_lost(usb.link);
        assert!(pulled.client_lost && pulled.goodbye.is_none());

        let (mut session, id) = picture_session(peer, 77, start);
        session.handle_control(
            peer,
            id,
            ControlMessage::Bye(ByeReason::Superseded),
            &TestConfig,
            start,
        );
        let no_cable = session.tick(&TestConfig, false, start + USB_TAKEOVER_TIMEOUT);
        assert!(no_cable.client_lost && no_cable.goodbye.is_none());
        assert_eq!(
            ended(&no_cable).reason,
            EndReason::Goodbye(ByeReason::Superseded)
        );

        let (mut session, id) = picture_session(peer, 77, start);
        assert!(session.begin_update(true));
        let stopped = session.tick(&TestConfig, false, start);
        assert_eq!(
            goodbye_of(&stopped),
            (peer, id, ByeReason::HostShuttingDown)
        );
        assert!(session
            .tick(&TestConfig, false, start + HOLD_LIMIT)
            .goodbye
            .is_none());
    }

    /// Another iPad gets Busy while the session's client is heard, and the
    /// slot once its session is on hold, as it did once the session expired.
    /// The silent session ends for liveness, with its goodbye.
    #[test]
    fn another_ipad_takes_the_slot_of_a_held_session() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let other = addr([10, 0, 0, 6], 50000);
        let (mut session, id) = picture_session(peer, 77, start);
        let busy = session.handle_control(
            other,
            0,
            identified_hello(5, 88),
            &TestConfig,
            at(start, 2_900),
        );
        assert_eq!(parse_ack(&busy.replies[0].1).status, HelloStatus::Busy);
        assert!(
            session
                .tick(&TestConfig, false, at(start, 3_000))
                .release_input
        );
        let taken = session.handle_control(
            other,
            0,
            identified_hello(6, 88),
            &TestConfig,
            at(start, 3_500),
        );
        assert_eq!(parse_ack(&taken.replies[0].1).status, HelloStatus::Ok);
        assert_eq!(taken.new_target, Some(other));
        assert!(taken.force_idr && !taken.client_lost);
        let end = ended(&taken);
        assert_eq!(
            (end.session_id, end.reason),
            (Some(id), EndReason::Liveness)
        );
        assert_eq!(goodbye_of(&taken), (peer, id, ByeReason::Error));
        assert_ne!(session.session_id(), Some(id));
        assert!(!session.on_hold());
        // The silent iPad's late packets learn the same.
        assert_eq!(
            session
                .handle_control(peer, id, report(1), &TestConfig, at(start, 4_600))
                .replies
                .len(),
            1
        );
    }

    /// The same iPad connecting again during a hold supersedes its session,
    /// as at any other time: no goodbye, a new session at once.
    #[test]
    fn the_same_ipad_reconnecting_during_a_hold_supersedes_it() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let (mut session, id) = picture_session(peer, 77, start);
        session.tick(&TestConfig, false, at(start, 3_000));
        assert!(session.on_hold());
        let again = session.handle_control(
            peer,
            0,
            identified_hello(2, 77),
            &TestConfig,
            at(start, 4_000),
        );
        assert_eq!(again.new_target, Some(peer));
        assert!(again.goodbye.is_none());
        let end = ended(&again);
        assert_eq!(
            (end.session_id, end.reason),
            (Some(id), EndReason::Superseded)
        );
        assert!(!session.on_hold());
    }

    /// A cable pulled during a hold ends the session at once, as at any
    /// other time.
    #[test]
    fn a_cable_pulled_during_a_hold_ends_the_session_at_once() {
        let start = Instant::now();
        let usb = PeerId::usb(17);
        let (mut session, id) = picture_session(usb, 77, start);
        session.tick(&TestConfig, false, at(start, 3_000));
        assert!(session.on_hold());
        let pulled = session.link_lost(usb.link);
        assert!(pulled.client_lost && pulled.goodbye.is_none());
        assert_eq!(session.last_end(), Some((id, EndReason::LinkLost)));
    }

    /// The handshake keeps advertising 3000 ms: every iPad's own check of
    /// the PC runs on it.
    #[test]
    fn the_hold_limit_is_host_only() {
        let mut session = Session::new(700);
        let ack = session.handle_control(
            addr([10, 0, 0, 5], 50000),
            0,
            identified_hello(1, 77),
            &TestConfig,
            Instant::now(),
        );
        assert_eq!(parse_ack(&ack.replies[0].1).liveness_timeout_ms, 3000);
        assert!(HOLD_LIMIT > LIVENESS_TIMEOUT + Duration::from_millis(5_200));
    }

    /// The goodbye has the layout of the `bye_background` golden vector,
    /// which the 0.3.2 and 0.3.3 iPad test suites parse from this same
    /// file, with reason 3: `.error` in every shipped iPad, which reconnects
    /// at once on it.
    #[test]
    fn the_liveness_goodbye_is_the_bye_shipped_ipads_parse() {
        let vectors = include_str!("../../../proto/testdata/v2_vectors.txt");
        let golden = vectors
            .lines()
            .find_map(|line| line.strip_prefix("bye_background "))
            .expect("the golden goodbye");
        let golden: Vec<u8> = (0..golden.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&golden[i..i + 2], 16).unwrap())
            .collect();
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let (mut session, id) = picture_session(peer, 77, start);
        let expired = session.tick(&TestConfig, false, start + HOLD_LIMIT);
        let (_, goodbye) = expired.goodbye.unwrap();
        assert_eq!(goodbye.len(), golden.len());
        // Magic, version, type, flags and length; then the session id.
        assert_eq!(goodbye[..8], golden[..8]);
        assert_eq!(goodbye[8..12], id.to_le_bytes());
        assert_eq!(*golden.last().unwrap(), ByeReason::AppBackground as u8);
        assert_eq!(*goodbye.last().unwrap(), 3);
    }

    /// An input event stamped `client_us` on the iPad's clock.
    fn stamped_key(event_id: u32, client_us: u64) -> ControlMessage {
        let ControlMessage::InputEvent(mut event) = input_event(event_id) else {
            unreachable!()
        };
        event.client_time_us = client_us;
        ControlMessage::InputEvent(event)
    }

    /// The iPad's clock: its own epoch, far from the host's.
    const IPAD_EPOCH_US: u64 = 987_654_321_000;

    /// Input the iPad sent during a 5.2 s uplink stall reaches the PC when
    /// the stall ends, seconds late by the iPad's own clock against its
    /// pings: it is marked late, and counted. Input sent once the uplink
    /// works is not, nor input without a stamp.
    #[test]
    fn input_that_waited_in_a_stalled_uplink_is_late() {
        let start = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let (mut session, id) = picture_session(peer, 77, start);
        let ipad = |millis: u64| IPAD_EPOCH_US + millis * 1000;
        let mut send = |message, arrives: u64| {
            session.handle_control(peer, id, message, &TestConfig, at(start, arrives))
        };
        // Pings every 2 s and a key, each 3 ms on the way.
        for millis in [0, 2_000] {
            let ping = ControlMessage::Ping(eternal_wire::v2::control::Ping {
                t1_us: ipad(millis),
            });
            assert!(!send(ping, millis + 3).input_late);
        }
        assert!(!send(stamped_key(1, ipad(3_000)), 3_003).input_late);
        // The stall: sent from 4.0 s, all delivered at 9.2 s.
        let stalled = [(2, 4_100), (3, 5_000), (4, 8_150), (5, 9_150)];
        let late: Vec<bool> = stalled
            .iter()
            .map(|&(event_id, sent)| send(stamped_key(event_id, ipad(sent)), 9_200).input_late)
            .collect();
        assert_eq!(late, [true, true, true, false]);
        assert!(!send(stamped_key(6, ipad(9_300)), 9_304).input_late);
        assert!(!send(stamped_key(7, 0), 9_400).input_late, "no stamp");
        let late_input = &session.active.as_ref().unwrap().late_input;
        assert_eq!((late_input.total, late_input.pending), (3, 3));
        // 5.1 s on the way less the quickest trip, 3 ms, and the 6.2 ms the
        // iPad's clock may have drifted since that trip.
        assert_eq!(
            late_input.max_late,
            Duration::from_micros(5_097_000 - 6_197)
        );
        session.tick(&TestConfig, false, at(start, 10_150));
        assert_eq!(session.active.as_ref().unwrap().late_input.pending, 3);
        session.tick(&TestConfig, false, at(start, 10_200));
        let late_input = &session.active.as_ref().unwrap().late_input;
        assert_eq!((late_input.total, late_input.pending), (3, 0), "logged");
    }

    /// Two hours of input every 100 ms and pings every 2 s, each up to
    /// 600 ms on the way but delivered in order, with the iPad's clock
    /// running 200 ppm fast or slow: nothing is late.
    #[test]
    fn input_with_ordinary_jitter_and_drift_is_never_late() {
        for ppm in [-200i64, 0, 200] {
            let start = Instant::now();
            let peer = addr([10, 0, 0, 5], 50000);
            let (mut session, id) = picture_session(peer, 77, start);
            let mut seed = 0x9E37_79B9_7F4A_7C15u64;
            let mut jitter_ms = move || {
                seed ^= seed >> 12;
                seed ^= seed << 25;
                seed ^= seed >> 27;
                seed.wrapping_mul(0x2545_F491_4F6C_DD1D) % 600
            };
            let mut arrived = 0;
            let mut late = 0;
            for step in 0..72_000u64 {
                let sent = step * 100;
                arrived = (sent + jitter_ms()).max(arrived);
                let client_us =
                    (IPAD_EPOCH_US as i64 + sent as i64 * 1000 + sent as i64 * ppm / 1000) as u64;
                let message = if step % 20 == 0 {
                    ControlMessage::Ping(eternal_wire::v2::control::Ping { t1_us: client_us })
                } else {
                    stamped_key(step as u32, client_us)
                };
                let actions =
                    session.handle_control(peer, id, message, &TestConfig, at(start, arrived));
                late += usize::from(actions.input_late);
            }
            assert_eq!(late, 0, "{ppm} ppm");
        }
    }
}
