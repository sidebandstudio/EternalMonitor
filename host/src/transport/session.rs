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
    ByeReason, ControlHeader, ControlMessage, FailedReason, HelloAck, HelloStatus, InputEvent,
    KeyframeRequest, Nack, ReceiverReport, StreamConfig, FEATURE_MEDIA_SIZE, FEATURE_POWER_BYE,
    FEATURE_STANDBY_STATUS, FEATURE_SUPPORTS_NACK, FEATURE_WANTS_INPUT, HOSTCAP_MEDIA_SIZE,
    HOSTCAP_NACK, MIN_MEDIA_DGRAM,
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

/// How long the host stays closed to new clients after Windows said it is
/// going to sleep or shutting down ([`Session::power_down`]), unless it says
/// first that it woke or that the shutdown was cancelled. A suspend that
/// fails without a resume message, or a missed one, can't leave the PC
/// unreachable for longer.
pub const POWER_DOWN_HOLD: Duration = Duration::from_secs(30);
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

/// IDENTIFY answers per window: for one asking address, and in all. An
/// iPad's check sends at most 8 and stops at the first answer.
const IDENTIFY_WINDOW: Duration = Duration::from_secs(10);
const IDENTIFY_PER_SOURCE: u32 = 8;
const IDENTIFY_OVERALL: u32 = 32;
/// Asking addresses tracked in a window; more get no answer until it ends.
const IDENTIFY_SOURCES: usize = 16;

/// Bounds what IDENTIFY costs: each answer is an HMAC and a datagram, and
/// anyone on the network may ask. Only answers count: a request left
/// unanswered for want of addresses to sign uses up nothing.
#[derive(Debug, Default)]
struct IdentifyLimiter {
    overall: Option<(Instant, u32)>,
    sources: Vec<(std::net::IpAddr, Instant, u32)>,
}

impl IdentifyLimiter {
    /// Whether `ip` may be answered now, counting the answer if so.
    fn allow(&mut self, ip: std::net::IpAddr, now: Instant) -> bool {
        let over = |since: Instant| now.saturating_duration_since(since) >= IDENTIFY_WINDOW;
        self.overall.take_if(|(since, _)| over(*since));
        self.sources.retain(|(_, since, _)| !over(*since));
        let overall = self.overall.get_or_insert((now, 0));
        if overall.1 >= IDENTIFY_OVERALL {
            return false;
        }
        let index = match self.sources.iter().position(|(source, ..)| *source == ip) {
            Some(index) => index,
            None if self.sources.len() < IDENTIFY_SOURCES => {
                self.sources.push((ip, now, 0));
                self.sources.len() - 1
            }
            None => return false,
        };
        let source = &mut self.sources[index];
        if source.2 >= IDENTIFY_PER_SOURCE {
            return false;
        }
        source.2 += 1;
        overall.1 += 1;
        true
    }
}

/// The refused attempts of the current minute.
#[derive(Debug, Clone, Copy)]
struct RefusalMinute {
    since: Instant,
    summarized: u32,
    left_out: u32,
}
/// Honor at most one keyframe request per this window (PLI storm guard).
pub const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(500);
/// Smaller media sizes per session whose keyframe skips that window: the
/// iPad asks for two (1,200 then 576 bytes). Any later one is an ordinary,
/// rate-limited keyframe request.
const MEDIA_SIZE_IDRS: u8 = 2;

/// Media bytes, headers included, a Wi-Fi client's address gets before
/// anything of its session comes back from it: room for the first picture,
/// even a large keyframe of a high-resolution stream. Heartbeats go on. An
/// iPad pings the moment its HELLO_ACK lands, so it answers long before
/// this runs out.
pub const UNANSWERED_MEDIA_BUDGET: usize = 512 * 1024;
/// Media to every client not yet heard counts against one host-wide budget
/// of the same size, which comes back over [`UNANSWERED_FOR`]: a client
/// that moves to another address, or many addresses in turn, get no more.
///
/// Addresses whose session ended before they answered, and for how long a
/// new session there starts with its budget spent.
const UNANSWERED_KEPT: usize = 64;
const UNANSWERED_FOR: Duration = Duration::from_secs(60);

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

/// The stage whose failures made the PC give up, as far as the summary's
/// `failure=` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageFailure {
    Capture,
    Encoder,
    Other,
}

/// Why the stream stops for good ([`Session::shut_down`]): what the iPad is
/// told, and how the session's summary ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStop {
    /// The app quits. The iPad hears that the PC ended the session.
    Quit,
    /// The app quits for a restart or an update that hands over to a new
    /// copy: the iPad is told to connect again.
    Handoff,
    /// Stop on the PC.
    User,
    /// The PC gave up after repeated failures; the iPad may try again.
    GaveUp(StageFailure),
}

impl StreamStop {
    fn bye(self) -> ByeReason {
        match self {
            Self::Quit | Self::User => ByeReason::UserDisconnect,
            Self::Handoff | Self::GaveUp(_) => ByeReason::Error,
        }
    }

    fn reason(self) -> EndReason {
        match self {
            Self::Quit | Self::Handoff => EndReason::Shutdown,
            Self::User => EndReason::Stopped,
            Self::GaveUp(stage) => EndReason::Failed(stage),
        }
    }
}

/// What Windows is doing to the PC ([`Session::power_down`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    /// Sleep or hibernate.
    Sleep,
    /// Shut down or restart.
    Shutdown,
    /// The user signs out.
    Logoff,
}

impl PowerEvent {
    /// Stable log value.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sleep => "sleep",
            Self::Shutdown => "shutdown",
            Self::Logoff => "logoff",
        }
    }

    /// The goodbye for an iPad that understands it ([`FEATURE_POWER_BYE`]).
    fn bye(self) -> ByeReason {
        match self {
            Self::Sleep => ByeReason::HostSleeping,
            Self::Shutdown | Self::Logoff => ByeReason::HostPoweringOff,
        }
    }
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
    /// The client's receiver reports counted any video fragment.
    pub frags_received: bool,
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
    /// Stop on the PC.
    Stopped,
    /// The PC gave up restarting a stage that kept failing.
    Failed(StageFailure),
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
    /// A receiver report counted any video fragment.
    frags_received: bool,
    /// Video frames let through to this session ([`Session::may_send_media`]).
    frames_sent: u32,
    /// The largest media datagram the client asked for in this session
    /// ([`KeyframeRequest::max_media_dgram`]), and how many times it went
    /// down.
    max_media_dgram: Option<u16>,
    media_size_lowered: u8,
    /// A Wi-Fi goodbye while the cable takes over.
    bye: Option<ByeReason>,
    /// The msg_seq of input packets taken in this session.
    input_seqs: SeqWindow,
    /// Something of this session came back from the client after its
    /// HELLO2. Always for USB, where the PC dials the iPad on the cable.
    answered: bool,
    /// Media bytes sent before it answered (see [`UNANSWERED_MEDIA_BUDGET`]).
    unanswered_sent: usize,
    /// A frame was held back for the budget: the answer asks for a keyframe.
    withheld: bool,
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

/// Why no stream runs, for an iPad that asks meanwhile. The supervisor sets
/// it whenever no pipeline generation streams (see
/// [`Session::set_not_streaming`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStreaming {
    /// Someone clicked Stop: only Start on the PC streams again.
    Stopped,
    /// The supervisor is waiting out the pause before a restart.
    Restarting,
    /// The storm brake gave up after repeated errors, until Start.
    Failed(FailedReason),
}

/// Why a HELLO2 can't start a session ([`Session::closed_to_clients`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closed {
    /// Turned away with Busy, logged with this line.
    Busy(&'static str),
    /// Not answered at all, logged with this line.
    Silent(&'static str),
    /// Told why, once it passes the pairing check.
    NotStreaming(NotStreaming),
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
    /// The port this host receives the stream on and its home network
    /// addresses, which an identity proof signs. None answers no IDENTIFY.
    fn identity_endpoints(&self) -> Option<(u16, Vec<[u8; 4]>)> {
        None
    }

    /// The heartbeat's notice for the iPad (`NOTICE_*` in the wire crate).
    fn notice(&self) -> u8 {
        eternal_wire::v2::control::NOTICE_NONE
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
    /// The stream stopped for good ([`Self::shut_down`]) and no transport
    /// has resumed since: new clients are turned away, as nothing would
    /// serve them.
    stream_stopped: bool,
    /// Windows said the PC is going down, and when ([`Self::power_down`]).
    powering_down: Option<(PowerEvent, Instant)>,
    /// A session [`Self::power_down`] ended without a goodbye: the next tick
    /// reports the client lost, as a goodbye would.
    lost_unsaid: bool,
    /// The queued goodbye is [`Self::power_down`]'s.
    power_bye: bool,
    /// No stream runs: HELLO2s are told why instead of starting a session.
    not_streaming: Option<NotStreaming>,
    /// The goodbye for a stream an update or a stop ended, sent by the next
    /// tick or by the transport on its way out.
    goodbye: Option<(PeerId, Vec<u8>)>,
    /// That stream's end, reported by the next tick or [`Self::shut_down`].
    stopped: Option<SessionEnd>,
    /// The last session ended for liveness (see [`Self::answer_ended`]).
    ended_for_liveness: Option<EndedSession>,
    /// Recently refused HELLO2 nonces: a retransmit is the same attempt.
    refused: std::collections::VecDeque<(u32, Instant)>,
    refusal_minute: Option<RefusalMinute>,
    /// Wi-Fi addresses whose session ended before they answered (see
    /// [`UNANSWERED_FOR`]).
    unanswered: std::collections::VecDeque<(std::net::IpAddr, Instant)>,
    /// The host-wide media sent to clients before they answered, as of when
    /// it was last counted (see [`UNANSWERED_MEDIA_BUDGET`]).
    unanswered_media: (usize, Instant),
    identify_limiter: IdentifyLimiter,
    /// Counts the sessions. Session ids come from a per-launch keyed hash
    /// of this count ([`Self::session_id_keys`]).
    next_session_id: u32,
    session_id_keys: std::collections::hash_map::RandomState,
    legacy_client_warned: bool,
}

impl Session {
    pub fn new(session_id_seed: u32) -> Self {
        Self {
            active: None,
            last_end: None,
            updating: false,
            stream_stopped: false,
            powering_down: None,
            lost_unsaid: false,
            power_bye: false,
            not_streaming: None,
            goodbye: None,
            stopped: None,
            ended_for_liveness: None,
            refused: std::collections::VecDeque::new(),
            refusal_minute: None,
            unanswered: std::collections::VecDeque::new(),
            unanswered_media: (0, Instant::now()),
            identify_limiter: IdentifyLimiter::default(),
            next_session_id: session_id_seed,
            session_id_keys: std::collections::hash_map::RandomState::new(),
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
        // One entry per address, its time refreshed. Once the memory is
        // full the oldest goes; the host-wide budget still counts it.
        if let Some(addr) = session.peer.addr.filter(|_| !session.answered) {
            self.unanswered.retain(|(ip, at)| {
                *ip != addr.ip() && now.saturating_duration_since(*at) < UNANSWERED_FOR
            });
            if self.unanswered.len() == UNANSWERED_KEPT {
                self.unanswered.pop_front();
            }
            self.unanswered.push_back((addr.ip(), now));
        }
        Some(session.end(reason, now))
    }

    /// Whether a frame of `bytes` may go to `session_id`'s client now. Until
    /// it answers, frames go while they fit in its budget; the first that
    /// doesn't is held back, and so is every later one until it answers,
    /// which asks for a keyframe to start the picture again.
    pub fn may_send_media(&mut self, session_id: u32, bytes: usize, now: Instant) -> bool {
        let host_spent = self.unanswered_media_at(now);
        let Some(session) = self
            .active
            .as_mut()
            .filter(|session| session.session_id == session_id)
        else {
            return true;
        };
        if session.answered {
            session.frames_sent = session.frames_sent.saturating_add(1);
            return true;
        }
        if session.withheld
            || session.unanswered_sent + bytes > UNANSWERED_MEDIA_BUDGET
            || host_spent + bytes > UNANSWERED_MEDIA_BUDGET
        {
            session.withheld = true;
            return false;
        }
        session.unanswered_sent += bytes;
        session.frames_sent = session.frames_sent.saturating_add(1);
        self.unanswered_media = (host_spent + bytes, now);
        true
    }

    /// The largest media datagram `session_id`'s client asked for, if it
    /// asked for one.
    pub fn max_media_dgram(&self, session_id: u32) -> Option<u16> {
        self.active
            .as_ref()
            .filter(|session| session.session_id == session_id)
            .and_then(|session| session.max_media_dgram)
    }

    /// The host-wide media to clients not yet heard that still counts at
    /// `now`.
    fn unanswered_media_at(&self, now: Instant) -> usize {
        let (spent, at) = self.unanswered_media;
        let back = UNANSWERED_MEDIA_BUDGET as u128 * now.saturating_duration_since(at).as_millis()
            / UNANSWERED_FOR.as_millis();
        spent.saturating_sub(usize::try_from(back).unwrap_or(usize::MAX))
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
            info!(peer = %session.peer, "Stopping the stream for an update");
            self.stopped = self.end_saying(
                ByeReason::HostShuttingDown,
                EndReason::Shutdown,
                Instant::now(),
            );
        }
        self.updating = true;
        true
    }

    /// Ends the active session for `reason` and leaves `bye` for the
    /// transport to send before its links close.
    fn end_saying(
        &mut self,
        bye: ByeReason,
        reason: EndReason,
        now: Instant,
    ) -> Option<SessionEnd> {
        let session = self.active.as_ref()?;
        let (peer, session_id) = (session.peer, session.session_id);
        let msg_seq = self.next_msg_seq();
        self.power_bye = false;
        self.goodbye = Some((
            peer,
            eternal_wire::v2::control::encode_control(
                session_id,
                msg_seq,
                &ControlMessage::Bye(bye),
            ),
        ));
        self.end(reason, now)
    }

    /// The goodbye left for a stopped stream ([`Self::begin_update`],
    /// [`Self::shut_down`]), if any.
    pub fn take_goodbye(&mut self) -> Option<(PeerId, Vec<u8>)> {
        self.power_bye = false;
        self.goodbye.take()
    }

    /// Whether a stream runs, and why not. While it is set, a HELLO2 that
    /// passes the version, update and pairing checks is told why, and no
    /// session starts. A session the stream keeps across a restart stays.
    pub fn set_not_streaming(&mut self, state: Option<NotStreaming>) {
        self.not_streaming = state;
    }

    pub fn not_streaming(&self) -> Option<NotStreaming> {
        self.not_streaming
    }

    /// The update handoff failed; clients may connect again.
    pub fn end_update(&mut self) {
        self.updating = false;
    }

    /// The stream stops for good, for `why`: the end of the session it
    /// stops, or of one an update stopped that no tick reported yet. A
    /// session it stops is told why by the transport on its way out; the
    /// iPad used to hear nothing, and showed a lost signal or an unplugged
    /// cable. Until a transport resumes, new clients are turned away.
    pub fn shut_down(&mut self, now: Instant, why: StreamStop) -> Option<SessionEnd> {
        self.stream_stopped = true;
        let active = self.end_saying(why.bye(), why.reason(), now);
        self.stopped.take().or(active)
    }

    /// Windows said the PC is going to sleep, shutting down or signing out.
    /// The session ends now, and its summary is returned for the caller to
    /// write while the PC is still up. An iPad that understands it
    /// ([`FEATURE_POWER_BYE`]) is told why by the transport's next pass; an
    /// older one is told nothing, as before, and notices the silence. Until
    /// [`Self::power_up`] or [`POWER_DOWN_HOLD`], HELLO2s go unanswered, as
    /// they would a moment later from a PC that is down. The stream keeps
    /// running, so the PC streams again when it wakes.
    pub fn power_down(&mut self, now: Instant, event: PowerEvent) -> Option<SessionEnd> {
        self.powering_down = Some((event, now));
        let understands =
            self.active.as_ref()?.accepted_hello.feature_caps & FEATURE_POWER_BYE != 0;
        if understands {
            let end = self.end_saying(event.bye(), EndReason::Shutdown, now);
            self.power_bye = true;
            end
        } else {
            self.lost_unsaid = true;
            self.end(EndReason::Shutdown, now)
        }
    }

    /// A goodbye [`Self::power_down`] left waits for the transport.
    pub fn power_goodbye_pending(&self) -> bool {
        self.power_bye
    }

    /// The PC woke, or the shutdown was called off: clients may connect
    /// again. A goodbye no transport sent before the PC went down is for a
    /// session long over, and is dropped.
    pub fn power_up(&mut self) {
        self.powering_down = None;
        if std::mem::take(&mut self.power_bye) {
            self.goodbye = None;
        }
    }

    /// Why a HELLO2 can't start a session now, if it can't: an update is
    /// about to close the host, no stream runs (the supervisor says why), or
    /// the stream stopped and its transport is on its way out (the app
    /// quits). The first and last are turned away with Busy before anything
    /// else is looked at; a retry once the stream runs again works.
    fn closed_to_clients(&self, now: Instant) -> Option<Closed> {
        if self
            .powering_down
            .is_some_and(|(_, since)| now.saturating_duration_since(since) < POWER_DOWN_HOLD)
        {
            Some(Closed::Silent("Client unanswered while the PC goes down"))
        } else if self.updating {
            Some(Closed::Busy("Client turned away while an update starts"))
        } else if let Some(state) = self.not_streaming {
            Some(Closed::NotStreaming(state))
        } else if self.stream_stopped {
            Some(Closed::Busy("Client turned away while the stream stops"))
        } else {
            None
        }
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
                    && session.answered
                    && session.info.feature_caps & eternal_wire::v2::control::FEATURE_WANTS_AUDIO
                        != 0
            })
            .map(|session| session.session_id)
    }

    /// The session's id while its client draws the pointer itself, from
    /// when it has answered: until then nothing confirms it received its
    /// HELLO_ACK, so it gets no pointer messages (an image can be hundreds
    /// of kilobytes), and capture keeps the pointer in the video for it, as
    /// for any other client. Pointer messages start once the iPad answers;
    /// capture switches then, once, and never again in the session.
    pub fn pointer_session_id(&self) -> Option<u32> {
        self.active
            .as_ref()
            .filter(|session| {
                session.answered
                    && session.info.feature_caps & eternal_wire::v2::control::FEATURE_DRAWS_POINTER
                        != 0
            })
            .map(|session| session.session_id)
    }

    /// `message` for the connected client, numbered like every other
    /// control message the host sends it.
    pub fn encode_for_client(&mut self, message: &ControlMessage) -> Option<(PeerId, Vec<u8>)> {
        let (peer, session_id) = self
            .active
            .as_ref()
            .map(|session| (session.peer, session.session_id))?;
        let msg_seq = self.next_msg_seq();
        Some((
            peer,
            eternal_wire::v2::control::encode_control(session_id, msg_seq, message),
        ))
    }

    pub fn last_report(&self) -> Option<ReceiverReport> {
        self.active.as_ref().and_then(|s| s.last_report)
    }

    fn allocate_session_id(&mut self) -> u32 {
        use std::hash::BuildHasher;
        let taken = [
            self.session_id(),
            self.last_end.map(|(id, _)| id),
            self.ended_for_liveness
                .as_ref()
                .map(|ended| ended.session_id),
        ];
        loop {
            self.next_session_id = self.next_session_id.wrapping_add(1);
            let id = self.session_id_keys.hash_one(self.next_session_id) as u32;
            if id != 0 && !taken.contains(&Some(id)) {
                return id;
            }
        }
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
    ///
    /// `datagram_len` is the datagram's size: over Wi-Fi, a refused HELLO2,
    /// or one the PC answers while it isn't streaming, gets no answer larger
    /// than itself.
    pub fn handle_datagram(
        &mut self,
        source: PeerId,
        header: &ControlHeader,
        message: ControlMessage,
        datagram_len: usize,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        self.handle(
            source,
            header.session_id,
            Some((header.msg_seq, datagram_len)),
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
        // The datagram's msg_seq and size, when it came off a link.
        datagram: Option<(u32, usize)>,
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
        if !matches!(
            message,
            ControlMessage::Hello2(_) | ControlMessage::Identify(_)
        ) {
            match self.active.as_ref() {
                Some(session)
                    if session.session_id == header_session_id && !session.awaiting_takeover => {}
                _ if matches!(message, ControlMessage::Bye(_)) => return Actions::default(),
                _ => return self.answer_ended(source, header_session_id, now),
            }
        }
        // Answers over USB stay whole.
        let hello_len = datagram
            .map(|(_, len)| len)
            .filter(|_| source.link == LinkId::Udp);
        match message {
            ControlMessage::Hello2(hello) => {
                self.handle_hello(source, hello, hello_len, config, now)
            }
            ControlMessage::Identify(request) => self.answer_identify(source, request, config, now),
            ControlMessage::Bye(reason) => self.handle_bye(source, reason, now),
            ControlMessage::KeyframeRequest(request) => self.handle_keyframe(source, request, now),
            ControlMessage::ReceiverReport(report) => self.handle_report(source, report, now),
            ControlMessage::Ping(ping) => self.handle_ping(source, ping, now),
            ControlMessage::InputEvent(event) => {
                self.handle_input(source, event, datagram.map(|(seq, _)| seq), now)
            }
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
        hello_len: Option<usize>,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();

        // Version gate first.
        if hello.proto_min > 2 || hello.proto_max < 2 {
            crate::updates::note_ipad_protocol(hello.proto_min, hello.proto_max);
            return self.refuse(
                source,
                HelloStatus::VersionUnsupported,
                &hello,
                hello_len,
                config,
                now,
            );
        }

        // Don't start a stream nothing will serve.
        let closed = self.closed_to_clients(now);
        if let Some(Closed::Silent(why)) = closed {
            info!(peer = %source, "{why}");
            return actions;
        }
        if let Some(Closed::Busy(why)) = closed {
            info!(peer = %source, "{why}");
            return self.refuse(source, HelloStatus::Busy, &hello, hello_len, config, now);
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
                if let Some(Closed::NotStreaming(state)) = closed {
                    drop(pairing);
                    return self
                        .answer_not_streaming(source, state, &hello, hello_len, config, now);
                }
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
                return self.refuse(source, status, &hello, hello_len, config, now);
            }
            None => crate::pairing::Grant {
                token: [0; 16],
                used_code: false,
                new_pairing: false,
            },
        };
        // Only an iPad that may connect learns why the PC isn't streaming.
        // A right pairing code isn't spent: it still works after Start.
        if let Some(Closed::NotStreaming(state)) = closed {
            drop(pairing);
            return self.answer_not_streaming(source, state, &hello, hello_len, config, now);
        }

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
                    return self.refuse(source, HelloStatus::Busy, &hello, hello_len, config, now);
                }
                // Same device, new connect attempt: supersede in place.
                info!(peer = %source, "Client reconnected — superseding session in place");
            } else if session.held_since.is_some() {
                // The session's own client went silent: the slot is free.
                info!(peer = %source, "Another client takes over a silent session");
            } else {
                // A different device while one is streaming: reject.
                info!(peer = %source, "Second client rejected while a session is active");
                return self.refuse(source, HelloStatus::Busy, &hello, hello_len, config, now);
            }
        }

        if grant.used_code {
            if let Err(error) = pairing
                .as_mut()
                .expect("code grant has pairing state")
                .rotate_code()
            {
                warn!(%error, "Pairing code rotation failed");
                return self.refuse(source, HelloStatus::Error, &hello, hello_len, config, now);
            }
        }

        // A session taken over before it was heard passes on what it spent.
        let inherited = self
            .active
            .as_ref()
            .filter(|session| !session.answered)
            .map_or(0, |session| session.unanswered_sent);
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
        // An address whose last session ended before it answered (this
        // HELLO2 may have just superseded it) gets no media until it does.
        self.unanswered
            .retain(|(_, at)| now.saturating_duration_since(*at) < UNANSWERED_FOR);
        let answered = source.link != LinkId::Udp;
        let unanswered_sent = match source.addr {
            Some(addr) if self.unanswered.iter().any(|(ip, _)| *ip == addr.ip()) => {
                UNANSWERED_MEDIA_BUDGET
            }
            _ => inherited,
        };
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
            frags_received: false,
            frames_sent: 0,
            max_media_dgram: None,
            media_size_lowered: 0,
            bye: None,
            input_seqs: SeqWindow::default(),
            answered,
            unanswered_sent,
            withheld: false,
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
        // Only a client that was told the host takes it asks; anything else
        // is ignored, as before. The first smaller sizes restart the picture
        // at once: the frames sent so far gave the client no picture.
        let asked = request.max_media_dgram;
        if asked != 0 && session.info.feature_caps & FEATURE_MEDIA_SIZE != 0 {
            let size = asked.clamp(MIN_MEDIA_DGRAM, eternal_wire::v2::MAX_DGRAM_SIZE as u16);
            if session.max_media_dgram.is_none_or(|current| size < current) {
                session.max_media_dgram = Some(size);
                session.media_size_lowered = session.media_size_lowered.saturating_add(1);
                info!(
                    max_dgram = size,
                    frames_sent = session.frames_sent,
                    "Media packet size lowered for this session"
                );
                if session.media_size_lowered <= MEDIA_SIZE_IDRS {
                    session.last_keyframe_grant = Some(now);
                    actions.force_idr = true;
                    return actions;
                }
            }
        }

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
                session.frags_received |= report.frags_received > 0;
                actions.report = Some(report);
            }
        }
        actions
    }

    /// An iPad checking that this is the PC it paired with, at the address
    /// it asked, before it reconnects there. It starts no session and works
    /// while another device streams, and through the standby responder while
    /// nothing streams. Only Wi-Fi asks, within
    /// [`IdentifyLimiter`]'s bounds.
    fn answer_identify(
        &mut self,
        source: PeerId,
        request: eternal_wire::v2::control::Identify,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let (LinkId::Udp, Some(asker)) = (source.link, source.addr) else {
            return actions;
        };
        let Some(pairing) = config.pairing() else {
            return actions;
        };
        if request.version != eternal_wire::v2::control::IDENTIFY_VERSION {
            return actions;
        }
        let Some((port, addresses)) = config.identity_endpoints() else {
            return actions;
        };
        if !self.identify_limiter.allow(asker.ip(), now) {
            return actions;
        }
        let addresses: Vec<[u8; 4]> = addresses
            .into_iter()
            .take(eternal_wire::v2::control::IDENTITY_MAX_ADDRESSES)
            .collect();
        let identity = ControlMessage::Identity(eternal_wire::v2::control::Identity {
            version: eternal_wire::v2::control::IDENTIFY_VERSION,
            nonce: request.nonce,
            port,
            proof: pairing
                .lock()
                .identity_proof(&request.nonce, port, &addresses),
            addresses,
        });
        actions.replies.push((
            source,
            eternal_wire::v2::control::encode_control(0, 1, &identity),
        ));
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
    ///
    /// `stopping`: the stream was stopped for good (`running` false, read
    /// under the session lock, which [`Self::shut_down`]'s caller holds while
    /// it clears it) before this transport got going. Its goodbye is
    /// current, and this transport sends it on its way out; clients stay
    /// turned away. Otherwise the stream runs again: clients are let in, and
    /// a goodbye a stop left while no transport ran is retired, as its
    /// session is long over. Both happen in this one call, so a stop that
    /// comes after it keeps its goodbye. Returns whether one was retired.
    pub fn transport_resumed(&mut self, now: Instant, stopping: bool) -> bool {
        if let Some(session) = self.active.as_mut() {
            session.liveness_deadline = session.liveness_deadline.max(now + LIVENESS_TIMEOUT);
        }
        if stopping {
            return false;
        }
        let stale = self.stream_stopped && self.goodbye.take().is_some();
        self.stream_stopped = false;
        stale
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
        if std::mem::take(&mut self.lost_unsaid) {
            actions.client_lost |= self.active.is_none();
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
            let frames_sent = (session.info.feature_caps & FEATURE_MEDIA_SIZE != 0)
                .then_some(session.frames_sent);
            let msg_seq = self.next_msg_seq();
            let heartbeat = ControlMessage::Heartbeat(eternal_wire::v2::control::Heartbeat {
                host_time_us: crate::clock::host_now_us(),
                stream_config: self.negotiated_config(config),
                notice: config.notice(),
                frames_sent,
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
    /// Tells an iPad the PC isn't streaming, in the words it understands.
    /// 0.3.4 and older drop an ACK whose status they don't know. A restart
    /// is short and their HELLO2 retries outlast it, so they hear nothing,
    /// as before. A stopped or failed PC waits for someone at the PC, so it
    /// turns them down with Error: "Your PC turned down the connection",
    /// which also ends their reconnect cycle.
    ///
    /// Over Wi-Fi the answer is never larger than the HELLO2: iPads that
    /// understand these answers pad theirs so every answer fits whole. A
    /// stopping stream answers here too, until its transport ends, so the
    /// bound is here as well as in the standby responder.
    fn answer_not_streaming(
        &mut self,
        source: PeerId,
        state: NotStreaming,
        hello: &eternal_wire::v2::control::Hello2,
        hello_len: Option<usize>,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let (status, reason) = if hello.feature_caps & FEATURE_STANDBY_STATUS == 0 {
            match state {
                NotStreaming::Restarting => return Actions::default(),
                NotStreaming::Stopped | NotStreaming::Failed(_) => (HelloStatus::Error, ""),
            }
        } else {
            match state {
                NotStreaming::Stopped => (HelloStatus::Stopped, ""),
                NotStreaming::Restarting => (HelloStatus::Restarting, ""),
                NotStreaming::Failed(reason) => (HelloStatus::Failed, reason.token()),
            }
        };
        let actions =
            self.refuse_with_reason(source, status, reason, hello, hello_len, config, now);
        if actions.ended.is_some() && !actions.replies.is_empty() {
            info!(peer = %source, ?status, reason, "Told an iPad the PC isn't streaming");
        }
        actions
    }

    /// Answers `hello` with a refusal. The first refusal of a nonce ends
    /// that attempt; a retransmit of it gets the answer again.
    ///
    /// With `hello_len`, the answer is no larger than the HELLO2: without
    /// the PC name and host caps if need be, which iPads read only from an
    /// accepted HELLO2, or not sent at all. A not-streaming reason stays.
    /// Every iPad's HELLO2 fits the bare answer.
    fn refuse(
        &mut self,
        source: PeerId,
        status: HelloStatus,
        hello: &eternal_wire::v2::control::Hello2,
        hello_len: Option<usize>,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        self.refuse_with_reason(source, status, "", hello, hello_len, config, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn refuse_with_reason(
        &mut self,
        source: PeerId,
        status: HelloStatus,
        reason: &str,
        hello: &eternal_wire::v2::control::Hello2,
        hello_len: Option<usize>,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let msg_seq = self.next_msg_seq();
        let mut answer = self.hello_ack(status, hello.client_nonce, 0, config, [0; 16], reason);
        let encode = |answer: &HelloAck| {
            eternal_wire::v2::control::encode_control(
                0,
                msg_seq,
                &ControlMessage::HelloAck(answer.clone()),
            )
        };
        let mut ack = encode(&answer);
        let fits = |ack: &[u8]| hello_len.is_none_or(|len| ack.len() <= len);
        if !fits(&ack) {
            answer.host_name.clear();
            answer.host_caps = 0;
            ack = encode(&answer);
        }
        let replies = if fits(&ack) {
            vec![(source, ack)]
        } else {
            Vec::new()
        };
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
            replies,
            ended: summarized.then_some(SessionEnd {
                session_id: None,
                client_nonce: hello.client_nonce,
                link: source.link,
                reason: EndReason::Refused(status),
                first_video: None,
                frames_received: false,
                frags_received: false,
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

    fn make_ack_with_token(
        &mut self,
        status: HelloStatus,
        client_nonce: u32,
        session_id: u32,
        config: &impl ConfigSource,
        auth_token: [u8; 16],
    ) -> Vec<u8> {
        self.make_reply(status, client_nonce, session_id, config, auth_token, "")
    }

    fn make_reply(
        &mut self,
        status: HelloStatus,
        client_nonce: u32,
        session_id: u32,
        config: &impl ConfigSource,
        auth_token: [u8; 16],
        reason: &str,
    ) -> Vec<u8> {
        let msg_seq = self.next_msg_seq();
        let ack = self.hello_ack(status, client_nonce, session_id, config, auth_token, reason);
        eternal_wire::v2::control::encode_control(
            session_id,
            msg_seq,
            &ControlMessage::HelloAck(ack),
        )
    }

    fn hello_ack(
        &self,
        status: HelloStatus,
        client_nonce: u32,
        session_id: u32,
        config: &impl ConfigSource,
        auth_token: [u8; 16],
        reason: &str,
    ) -> HelloAck {
        // Only the client that asks hears that the host takes smaller media
        // datagrams; older iPads get the reply they always did.
        let media_size = status == HelloStatus::Ok
            && self.active.as_ref().is_some_and(|session| {
                session.session_id == session_id
                    && session.info.feature_caps & FEATURE_MEDIA_SIZE != 0
            });
        HelloAck {
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
            host_caps: HOSTCAP_NACK
                | config.host_caps()
                | if media_size { HOSTCAP_MEDIA_SIZE } else { 0 },
            reason: reason.to_string(),
        }
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
    /// and keyframe requests sent during the silence may not have arrived,
    /// and a client that answers after frames were held back from it needs
    /// a keyframe to start its picture.
    fn heard(&mut self, kind: Heard, now: Instant) -> bool {
        let silent = now.saturating_duration_since(self.heard.0);
        self.heard = (now, kind);
        self.liveness_deadline = now + LIVENESS_TIMEOUT;
        if !self.answered {
            self.answered = true;
            info!(
                session_id = self.session_id,
                answered_ms = now
                    .saturating_duration_since(self.info.connected_at)
                    .as_millis() as u64,
                heard = kind.as_str(),
                withheld = self.withheld,
                "Client answered"
            );
            if self.withheld {
                return self.grant_keyframe(now);
            }
        }
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
            frags_received: self.frags_received,
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

    /// Only an iPad that said it draws the pointer gets it apart from the
    /// video, numbered with the session's other control messages. An older
    /// iPad keeps the pointer in the video.
    #[test]
    fn only_an_ipad_that_draws_the_pointer_gets_it_apart() {
        use eternal_wire::v2::control::{parse_control, PointerState, FEATURE_DRAWS_POINTER};
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        assert_eq!(session.pointer_session_id(), None, "nobody connected");
        session.handle_control_authed(peer, hello(1, 50000), &TestConfig, now);
        assert!(session.is_active());
        assert_eq!(session.pointer_session_id(), None, "an iPad before 0.3.5");

        let ControlMessage::Hello2(mut draws) = hello(2, 50000) else {
            unreachable!()
        };
        draws.feature_caps = FEATURE_DRAWS_POINTER;
        session.handle_control_authed(peer, ControlMessage::Hello2(draws), &TestConfig, now);
        // Review r2: nothing has confirmed it got its HELLO_ACK, so no
        // pointer messages yet, and capture keeps drawing the pointer.
        assert_eq!(
            session.pointer_session_id(),
            None,
            "a HELLO that only claims the bit gets no pointer messages"
        );
        let id = session.session_id().unwrap();
        let answer = session.handle_control(
            peer,
            id,
            ControlMessage::ReceiverReport(ReceiverReport::default()),
            &TestConfig,
            now,
        );
        assert!(answer.replies.is_empty());
        assert_eq!(session.pointer_session_id(), Some(id), "it answered");
        let state = ControlMessage::PointerState(PointerState::default());
        let (to, datagram) = session.encode_for_client(&state).unwrap();
        assert_eq!(to, peer);
        let (header, parsed) = parse_control(&datagram).unwrap();
        assert_eq!(parsed, state);
        assert_eq!(header.session_id, id);

        session.handle_control_authed(
            peer,
            ControlMessage::Bye(ByeReason::UserDisconnect),
            &TestConfig,
            now,
        );
        assert_eq!(session.pointer_session_id(), None);
        assert!(session.encode_for_client(&state).is_none());
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
        session.transport_resumed(resumed, false);
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
            max_media_dgram: 0,
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
                max_media_dgram: 0,
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
            let len =
                eternal_wire::v2::control::encode_control(session_id, msg_seq, &message).len();
            let actions = session.handle_datagram(peer, &header, message, len, &TestConfig, now);
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

    fn media_size_hello(nonce: u32, feature_caps: u16) -> ControlMessage {
        let ControlMessage::Hello2(mut hello) = hello(nonce, 50000) else {
            unreachable!()
        };
        hello.feature_caps = feature_caps;
        ControlMessage::Hello2(hello)
    }

    fn sized_request(max_media_dgram: u16) -> ControlMessage {
        ControlMessage::KeyframeRequest(KeyframeRequest {
            stream_epoch: 7,
            last_complete_seq: 0,
            reason: KeyframeReason::Startup,
            max_media_dgram,
        })
    }

    fn heartbeat_of(actions: &Actions) -> eternal_wire::v2::control::Heartbeat {
        match eternal_wire::v2::control::parse_control(&actions.replies[0].1)
            .unwrap()
            .1
        {
            ControlMessage::Heartbeat(heartbeat) => heartbeat,
            other => panic!("{other:?}"),
        }
    }

    /// Only an iPad that says it may ask for smaller media datagrams hears
    /// that the host takes them, and gets the frame count in heartbeats.
    /// Any other iPad gets the reply and heartbeats it always did.
    #[test]
    fn media_size_is_offered_only_to_an_ipad_that_asks() {
        let peer = addr([10, 0, 0, 5], 50000);
        let now = Instant::now();
        for (feature_caps, offered) in [(FEATURE_MEDIA_SIZE, true), (0, false)] {
            let mut session = Session::new(1234);
            let accepted = session.handle_control(
                peer,
                0,
                media_size_hello(1, feature_caps),
                &TestConfig,
                now,
            );
            let ack = parse_ack(&accepted.replies[0].1);
            assert_eq!(ack.host_caps & HOSTCAP_MEDIA_SIZE != 0, offered);
            assert_eq!(ack.host_caps & !HOSTCAP_MEDIA_SIZE, HOSTCAP_NACK);
            let id = session.session_id().unwrap();
            for _ in 0..3 {
                assert!(session.may_send_media(id, 1000, now));
            }
            let ticked = session.tick(&TestConfig, true, now + Duration::from_millis(100));
            assert_eq!(
                heartbeat_of(&ticked).frames_sent,
                offered.then_some(3),
                "{feature_caps}"
            );
            // The heartbeat an older iPad gets is the plain one.
            if !offered {
                assert_eq!(
                    ticked.replies[0].1.len(),
                    eternal_wire::v2::control::CONTROL_HEADER_SIZE
                        + 8
                        + eternal_wire::v2::control::STREAM_CONFIG_SIZE
                );
            }
        }
        // A refusal never offers it.
        let mut busy = Session::new(1234);
        busy.handle_control(
            peer,
            0,
            media_size_hello(1, FEATURE_MEDIA_SIZE),
            &TestConfig,
            now,
        );
        let refused = busy.handle_control(
            addr([10, 0, 0, 6], 50000),
            0,
            media_size_hello(2, FEATURE_MEDIA_SIZE),
            &TestConfig,
            now,
        );
        let ack = parse_ack(&refused.replies[0].1);
        assert_eq!(ack.status, HelloStatus::Busy);
        assert_eq!(ack.host_caps & HOSTCAP_MEDIA_SIZE, 0);
    }

    /// A smaller size restarts the picture at once, inside the keyframe
    /// rate limit, and holds for the rest of the session: never larger,
    /// never under the minimum. A new session starts at the PC's size.
    #[test]
    fn a_smaller_media_size_lowers_only_this_session_and_forces_an_idr() {
        let peer = addr([10, 0, 0, 5], 50000);
        let now = Instant::now();
        let mut session = Session::new(1234);
        session.handle_control(
            peer,
            0,
            media_size_hello(1, FEATURE_MEDIA_SIZE),
            &TestConfig,
            now,
        );
        let id = session.session_id().unwrap();
        assert_eq!(session.max_media_dgram(id), None);

        let plain = session.handle_control_authed(peer, sized_request(0), &TestConfig, now);
        assert!(plain.force_idr);
        assert_eq!(session.max_media_dgram(id), None);

        let at = |millis| now + Duration::from_millis(millis);
        for (millis, asked, size, idr) in [
            (100, 1200, 1200, true),
            (200, 1200, 1200, false),
            (300, 1300, 1200, false),
            (400, 576, 576, true),
            (450, 100, 576, false),
        ] {
            let actions =
                session.handle_control_authed(peer, sized_request(asked), &TestConfig, at(millis));
            assert_eq!(session.max_media_dgram(id), Some(size), "{asked}");
            assert_eq!(actions.force_idr, idr, "{asked}");
            assert!(actions.replies.is_empty() && actions.new_target.is_none());
        }
        assert_eq!(session.max_media_dgram(id.wrapping_add(1)), None);

        // A client that keeps asking for less, a byte at a time, gets two
        // keyframes past the rate limit, then no more than any other client.
        session.handle_control(
            peer,
            0,
            media_size_hello(3, FEATURE_MEDIA_SIZE),
            &TestConfig,
            at(1_000),
        );
        let id = session.session_id().unwrap();
        let mut forced = Vec::new();
        for step in 0..825u64 {
            let actions = session.handle_control_authed(
                peer,
                sized_request(1400 - step as u16),
                &TestConfig,
                at(1_000 + step * 10),
            );
            if actions.force_idr {
                forced.push(step * 10);
            }
        }
        assert_eq!(session.max_media_dgram(id), Some(MIN_MEDIA_DGRAM));
        // Two at once, then one per rate-limit window from the second.
        assert_eq!(forced[..2], [0, 10]);
        assert!(forced[2..]
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= KEYFRAME_REQUEST_MIN_INTERVAL.as_millis() as u64));
        assert_eq!(forced[2], 510);
        assert!(forced.len() <= 2 + 8240 / 500, "{forced:?}");

        session.handle_control(
            peer,
            0,
            media_size_hello(2, FEATURE_MEDIA_SIZE),
            &TestConfig,
            at(500),
        );
        let next = session.session_id().unwrap();
        assert_ne!(next, id);
        assert_eq!(session.max_media_dgram(next), None);
    }

    /// An iPad that didn't say it may ask has its request read as the
    /// keyframe request it always was.
    #[test]
    fn a_media_size_from_an_ipad_that_did_not_offer_is_ignored() {
        let peer = addr([10, 0, 0, 5], 50000);
        let now = Instant::now();
        let mut session = Session::new(1234);
        session.handle_control(peer, 0, media_size_hello(1, 0), &TestConfig, now);
        let id = session.session_id().unwrap();
        let first = session.handle_control_authed(peer, sized_request(576), &TestConfig, now);
        assert!(first.force_idr);
        let second = session.handle_control_authed(
            peer,
            sized_request(576),
            &TestConfig,
            now + Duration::from_millis(100),
        );
        assert!(!second.force_idr, "rate limited as any keyframe request");
        assert_eq!(session.max_media_dgram(id), None);
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
    fn identity_reply(actions: &Actions) -> eternal_wire::v2::control::Identity {
        assert_eq!(actions.replies.len(), 1);
        let (header, ControlMessage::Identity(identity)) =
            eternal_wire::v2::control::parse_control(&actions.replies[0].1).unwrap()
        else {
            panic!("expected IDENTITY")
        };
        assert_eq!(header.session_id, 0);
        identity
    }

    fn identify(version: u8, nonce: [u8; 16]) -> ControlMessage {
        ControlMessage::Identify(eternal_wire::v2::control::Identify { version, nonce })
    }

    /// A paired host on port 9876 with the home network addresses given.
    struct IdentifyConfig(PairConfig, Vec<[u8; 4]>);
    impl ConfigSource for IdentifyConfig {
        fn stream_config(&self) -> StreamConfig {
            TestConfig.stream_config()
        }
        fn host_name(&self) -> String {
            TestConfig.host_name()
        }
        fn pairing(&self) -> Option<&parking_lot::Mutex<crate::pairing::Pairing>> {
            self.0.pairing()
        }
        fn identity_endpoints(&self) -> Option<(u16, Vec<[u8; 4]>)> {
            Some((9876, self.1.clone()))
        }
    }

    /// A paired host on every address whose home network addresses come
    /// from `HomeCache`, listed by `held_home`.
    struct ListedConfig(PairConfig, crate::network::HomeCache);
    impl ConfigSource for ListedConfig {
        fn stream_config(&self) -> StreamConfig {
            TestConfig.stream_config()
        }
        fn host_name(&self) -> String {
            TestConfig.host_name()
        }
        fn pairing(&self) -> Option<&parking_lot::Mutex<crate::pairing::Pairing>> {
            self.0.pairing()
        }
        fn identity_endpoints(&self) -> Option<(u16, Vec<[u8; 4]>)> {
            super::super::identity_endpoints(SocketAddr::from(([0, 0, 0, 0], 9876)), || {
                self.1.get()
            })
        }
    }

    /// Held while the test keeps the PC's adapters from being listed.
    static HOME_HELD: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    fn held_home() -> Option<Vec<crate::network::Adapter>> {
        let _open = HOME_HELD.lock();
        Some(vec![crate::network::Adapter {
            name: String::new(),
            kind: crate::network::Kind::Wifi,
            up: true,
            gateway: true,
            ipv4: vec![std::net::Ipv4Addr::new(192, 168, 1, 20)],
        }])
    }

    /// Once the last listing of the PC's addresses has expired, IDENTIFY gets
    /// no answer until a new listing comes, however long that takes: the PC
    /// never signs an address it may no longer have.
    #[test]
    fn identify_signs_no_expired_listing() {
        let lifetime = Duration::from_secs(1);
        let config = ListedConfig(
            pair_config(true),
            crate::network::HomeCache::with(held_home, lifetime),
        );
        let mut session = Session::new(100);
        let start = Instant::now();
        // A new asker and a new limiter window each time, so only the
        // listing decides.
        let mut asked = 0u32;
        let mut ask = |session: &mut Session| {
            asked += 1;
            let asker = addr([192, 0, 2, (asked % 250) as u8 + 1], 50100);
            answered(session, asker, &config, start + IDENTIFY_WINDOW * asked)
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ask(&mut session) {
            assert!(Instant::now() < deadline, "the addresses were never listed");
            std::thread::sleep(Duration::from_millis(5));
        }

        let held = HOME_HELD.lock();
        std::thread::sleep(lifetime + Duration::from_millis(100));
        for _ in 0..30 {
            assert!(!ask(&mut session), "signed an expired listing");
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(held);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ask(&mut session) {
            assert!(Instant::now() < deadline, "a fresh listing never came");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn identify_config(required: bool) -> IdentifyConfig {
        IdentifyConfig(
            pair_config(required),
            vec![[192, 0, 2, 1], [198, 51, 100, 1]],
        )
    }

    fn answered(
        session: &mut Session,
        asker: PeerId,
        config: &impl ConfigSource,
        now: Instant,
    ) -> bool {
        !session
            .handle_control(asker, 0, identify(1, [3; 16]), config, now)
            .replies
            .is_empty()
    }

    /// The PC proves it holds its pairing token at its own port and
    /// addresses, with pairing on or off, without starting a session.
    #[test]
    fn identify_is_answered_with_the_token_proof_and_starts_nothing() {
        let config = identify_config(true);
        let mut session = Session::new(100);
        let now = Instant::now();
        let asker = addr([192, 0, 2, 9], 50100);
        let actions = session.handle_control(asker, 0, identify(1, [3; 16]), &config, now);
        let identity = identity_reply(&actions);
        assert_eq!(actions.replies[0].0, asker);
        assert_eq!(identity.nonce, [3; 16]);
        assert_eq!(identity.port, 9876);
        assert_eq!(identity.addresses, config.1);
        let proof =
            |pairing: &crate::pairing::Pairing| pairing.identity_proof(&[3; 16], 9876, &config.1);
        assert_eq!(identity.proof, proof(&config.0 .0.lock()));
        assert_ne!(
            identity.proof,
            proof(&crate::pairing::Pairing::new(true, [8; 16]).unwrap())
        );
        assert!(session.session_id().is_none());
        assert!(actions.new_target.is_none());

        // At most four addresses go out, and the proof signs those four.
        let many = IdentifyConfig(
            pair_config(false),
            (1..=6).map(|last| [192, 0, 2, last]).collect(),
        );
        let identity =
            identity_reply(&session.handle_control(asker, 0, identify(1, [3; 16]), &many, now));
        assert_eq!(identity.addresses, many.1[..4]);
        assert_eq!(
            identity.proof,
            many.0
                 .0
                .lock()
                .identity_proof(&[3; 16], 9876, &many.1[..4])
        );
    }

    /// A layout this PC doesn't know gets no answer, as from a PC before
    /// 0.3.5; so does a host without pairing state or addresses to sign,
    /// and a request over a cable.
    #[test]
    fn identify_of_another_version_is_not_answered() {
        let mut session = Session::new(100);
        let now = Instant::now();
        let asker = addr([192, 0, 2, 9], 50100);
        let actions =
            session.handle_control(asker, 0, identify(2, [3; 16]), &identify_config(true), now);
        assert!(actions.replies.is_empty());
        assert!(!answered(&mut session, asker, &TestConfig, now));
        assert!(!answered(&mut session, asker, &pair_config(true), now));
        assert!(!answered(
            &mut session,
            PeerId::usb(1),
            &identify_config(true),
            now
        ));
        assert!(answered(&mut session, asker, &identify_config(true), now));
    }

    /// While no stream runs, the standby responder hands an IDENTIFY to the
    /// session: it is answered over Wi-Fi as while streaming, never over a
    /// cable, with an answer no larger than the request, and it starts
    /// nothing.
    #[test]
    fn identify_while_not_streaming_is_answered_as_while_streaming() {
        let config = identify_config(true);
        let request = eternal_wire::v2::control::encode_control(0, 1, &identify(1, [3; 16]));
        for state in [
            NotStreaming::Stopped,
            NotStreaming::Restarting,
            NotStreaming::Failed(FailedReason::Encoder),
        ] {
            let mut session = Session::new(100);
            session.set_not_streaming(Some(state));
            let mut ask = |peer: PeerId| {
                let (header, message) = eternal_wire::v2::control::parse_control(&request).unwrap();
                session.handle_datagram(
                    peer,
                    &header,
                    message,
                    request.len(),
                    &config,
                    Instant::now(),
                )
            };
            assert!(ask(PeerId::usb(1)).replies.is_empty(), "{state:?}");
            let actions = ask(addr([192, 0, 2, 9], 50100));
            let identity = identity_reply(&actions);
            assert_eq!(identity.nonce, [3; 16]);
            assert_eq!(actions.replies[0].1.len(), request.len());
            assert_eq!(request.len(), 84);
            assert!(actions.new_target.is_none());
            assert!(session.session_id().is_none());
            assert_eq!(session.not_streaming(), Some(state));
        }
    }

    /// Requests that get no answer, while the PC has no addresses to sign,
    /// use up none of the asker's answers.
    #[test]
    fn unanswered_identify_uses_up_no_answers() {
        let mut session = Session::new(100);
        let now = Instant::now();
        let asker = addr([192, 0, 2, 9], 50100);
        for _ in 0..IDENTIFY_OVERALL {
            assert!(!answered(&mut session, asker, &pair_config(true), now));
        }
        let config = identify_config(true);
        let answers = (0..IDENTIFY_PER_SOURCE * 2)
            .filter(|_| answered(&mut session, asker, &config, now))
            .count();
        assert_eq!(answers, IDENTIFY_PER_SOURCE as usize);
    }

    /// A burst from one address gets 8 answers in 10 s, and the PC 32 in
    /// all from at most 16 addresses; each count starts over when its
    /// window ends.
    #[test]
    fn identify_answers_are_bounded_per_asker_and_in_all() {
        let config = identify_config(true);
        let mut session = Session::new(100);
        let start = Instant::now();
        let asker = addr([192, 0, 2, 9], 50100);
        let burst = (0..20)
            .filter(|_| answered(&mut session, asker, &config, start))
            .count();
        assert_eq!(burst, IDENTIFY_PER_SOURCE as usize);
        // Another port on the same address shares its count.
        assert!(!answered(
            &mut session,
            addr([192, 0, 2, 9], 50101),
            &config,
            start
        ));
        // Others still get theirs, up to the PC's 32.
        let others: usize = (10..13)
            .map(|last| {
                (0..20)
                    .filter(|_| {
                        answered(&mut session, addr([192, 0, 2, last], 50100), &config, start)
                    })
                    .count()
            })
            .sum();
        assert_eq!(others, (IDENTIFY_OVERALL - IDENTIFY_PER_SOURCE) as usize);
        assert!(!answered(
            &mut session,
            addr([192, 0, 2, 40], 50100),
            &config,
            start
        ));

        let later = start + IDENTIFY_WINDOW - Duration::from_millis(1);
        assert!(!answered(&mut session, asker, &config, later));
        let next = start + IDENTIFY_WINDOW;
        assert!(answered(&mut session, asker, &config, next));
        assert!(answered(
            &mut session,
            addr([192, 0, 2, 40], 50100),
            &config,
            next
        ));

        // Past 16 addresses in a window, a new one waits for the next.
        let mut session = Session::new(100);
        for last in 0..IDENTIFY_SOURCES as u8 {
            assert!(answered(
                &mut session,
                addr([198, 51, 100, last], 50100),
                &config,
                start
            ));
        }
        assert!(!answered(&mut session, asker, &config, start));
        assert!(answered(&mut session, asker, &config, next));
    }

    /// While another iPad streams, IDENTIFY is answered within the same
    /// bounds, and the stream goes on whatever arrives.
    #[test]
    fn identify_while_streaming_is_bounded_and_leaves_the_stream() {
        let mut session = Session::new(100);
        let now = Instant::now();
        let streaming = addr([192, 0, 2, 5], 50000);
        session.handle_control(
            streaming,
            0,
            ControlMessage::Hello2(pair_hello(1)),
            &pair_config(false),
            now,
        );
        let live = session.session_id();
        assert!(live.is_some());

        let config = identify_config(false);
        let asker = addr([192, 0, 2, 9], 50100);
        let mut answers = 0;
        for index in 0..40u8 {
            let actions = session.handle_control(asker, 0, identify(1, [index; 16]), &config, now);
            assert!(
                actions.new_target.is_none() && !actions.client_lost && actions.goodbye.is_none()
            );
            if !actions.replies.is_empty() {
                let identity = identity_reply(&actions);
                assert_eq!(identity.nonce, [index; 16]);
                assert_eq!(actions.replies[0].0, asker, "never to the streaming iPad");
                answers += 1;
            }
        }
        assert_eq!(answers, IDENTIFY_PER_SOURCE);
        // The streaming iPad's own IDENTIFY counts on its own address.
        assert!(answered(&mut session, streaming, &config, now));
        assert_eq!(session.session_id(), live, "the stream goes on");
        let actions = session.tick(&config, true, now + Duration::from_millis(100));
        assert!(!actions.client_lost);
        assert_eq!(session.session_id(), live);
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
                frags_received: false,
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
        assert_eq!(
            session.shut_down(now, StreamStop::Quit),
            None,
            "reported once"
        );
        assert_eq!(session.take_goodbye(), None, "nobody left to tell");

        session.end_update();
        // The stream runs again: clients are let in.
        session.transport_resumed(now, false);
        session.handle_control(usb, 0, identified_hello(5, 77), &TestConfig, now);
        let quit = session
            .shut_down(now + Duration::from_secs(3), StreamStop::Quit)
            .unwrap();
        assert_eq!((quit.client_nonce, quit.reason), (5, EndReason::Shutdown));
        assert_eq!(quit.duration, Duration::from_secs(3));
        assert!(!session.is_active());
    }

    /// A HELLO2 that arrives after the stream stopped, before its transport
    /// noticed (up to a liveness tick), used to open a session nothing would
    /// serve: no heartbeats, and the PC counted it as connected. It is turned
    /// away until a transport resumes.
    #[test]
    fn a_hello_after_the_stream_stopped_is_turned_away_until_it_runs_again() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut session = Session::new(500);
        assert_eq!(session.shut_down(now, StreamStop::User), None);
        let refused = session.handle_control(peer, 0, identified_hello(7, 77), &TestConfig, now);
        assert_eq!(parse_ack(&refused.replies[0].1).status, HelloStatus::Busy);
        assert!(!session.is_active());

        session.transport_resumed(now, false);
        let admitted = session.handle_control(peer, 0, identified_hello(8, 77), &TestConfig, now);
        assert_eq!(admitted.new_target, Some(peer));
        assert!(session.is_active());
    }

    /// A transport starting after a stop retires the stop's goodbye only if
    /// the stream runs again; one starting while the stream is stopping keeps
    /// it, and keeps clients out. A stop after the start keeps its goodbye.
    #[test]
    fn a_starting_transport_retires_only_a_goodbye_from_before_the_stream_ran_again() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let admit = |session: &mut Session, nonce| {
            session.handle_control(peer, 0, identified_hello(nonce, 77), &TestConfig, now)
        };

        // Stopped while no transport ran; Start brought the stream back.
        let mut session = Session::new(500);
        admit(&mut session, 1);
        session.shut_down(now, StreamStop::User).unwrap();
        assert!(
            session.transport_resumed(now, false),
            "a stale goodbye is retired"
        );
        assert_eq!(session.take_goodbye(), None);
        assert_eq!(admit(&mut session, 2).new_target, Some(peer));

        // Stopped while this transport was starting: the goodbye is current.
        let mut session = Session::new(500);
        admit(&mut session, 3);
        session.shut_down(now, StreamStop::User).unwrap();
        assert!(!session.transport_resumed(now, true));
        assert!(
            session.take_goodbye().is_some(),
            "the current goodbye is kept"
        );
        let refused = admit(&mut session, 4);
        assert_eq!(parse_ack(&refused.replies[0].1).status, HelloStatus::Busy);

        // Stopped once the transport ran: kept, for the transport to send.
        let mut session = Session::new(500);
        admit(&mut session, 5);
        assert!(!session.transport_resumed(now, false), "nothing to retire");
        session.shut_down(now, StreamStop::User).unwrap();
        assert!(session.take_goodbye().is_some());
    }

    /// Quitting or Stop ends the stream for good: the iPad hears why, from
    /// the transport on its way out, instead of waiting out a lost signal.
    /// A restart tells it to connect again.
    #[test]
    fn a_stopped_stream_tells_its_ipad() {
        let now = Instant::now();
        let usb = PeerId::usb(17);
        let wifi = addr([10, 0, 0, 5], 50000);
        for (peer, why, bye, reason) in [
            (
                usb,
                StreamStop::Quit,
                ByeReason::UserDisconnect,
                EndReason::Shutdown,
            ),
            (
                wifi,
                StreamStop::Handoff,
                ByeReason::Error,
                EndReason::Shutdown,
            ),
            (
                wifi,
                StreamStop::User,
                ByeReason::UserDisconnect,
                EndReason::Stopped,
            ),
            (
                usb,
                StreamStop::GaveUp(StageFailure::Encoder),
                ByeReason::Error,
                EndReason::Failed(StageFailure::Encoder),
            ),
        ] {
            let mut session = Session::new(500);
            session.handle_control(peer, 0, identified_hello(6, 77), &TestConfig, now);
            let id = session.session_id().unwrap();
            let end = session.shut_down(now, why).unwrap();
            assert_eq!(end.reason, reason);
            let (to, bytes) = session.take_goodbye().expect("a goodbye");
            let (header, message) = eternal_wire::v2::control::parse_control(&bytes).unwrap();
            assert_eq!(
                (to, header.session_id, message),
                (peer, id, ControlMessage::Bye(bye))
            );
            assert_eq!(session.take_goodbye(), None, "sent once");
        }
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
                frags_received: false,
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
        crate::logging::prepare_log_capture();
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
    fn power_hello(nonce: u32, feature_caps: u16) -> ControlMessage {
        let ControlMessage::Hello2(mut hello) = hello(nonce, 50000) else {
            unreachable!()
        };
        hello.device_id = 77;
        hello.feature_caps = feature_caps;
        ControlMessage::Hello2(hello)
    }

    /// Windows taking the PC down ends the session at once, with the summary
    /// a quit writes. An iPad that understands it is told why by the next
    /// pass; the reason says sleep or power off.
    #[test]
    fn the_pc_going_down_tells_an_ipad_that_understands_it() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        for (event, bye) in [
            (PowerEvent::Sleep, ByeReason::HostSleeping),
            (PowerEvent::Shutdown, ByeReason::HostPoweringOff),
            (PowerEvent::Logoff, ByeReason::HostPoweringOff),
        ] {
            let mut session = Session::new(500);
            session.handle_control(peer, 0, power_hello(1, FEATURE_POWER_BYE), &TestConfig, now);
            let id = session.session_id().unwrap();
            let end = session.power_down(now, event).expect("the session ended");
            assert_eq!((end.client_nonce, end.reason), (1, EndReason::Shutdown));
            assert!(!session.is_active());
            assert!(session.power_goodbye_pending());
            let next = session.tick(&TestConfig, false, now);
            assert_eq!(goodbye_of(&next), (peer, id, bye), "{event:?}");
            assert!(next.client_lost);
            assert_eq!(next.ended, None, "the summary was the caller's to write");
            assert!(!session.power_goodbye_pending());
        }
    }

    /// An iPad from before these reasons drops a goodbye it can't read, so
    /// it gets none, as before. Its session still ends with a summary, and
    /// the next pass lets the transport forget it.
    #[test]
    fn an_older_ipad_gets_no_goodbye_when_the_pc_goes_down() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut session = Session::new(500);
        session.handle_control(
            peer,
            0,
            power_hello(1, FEATURE_SUPPORTS_NACK),
            &TestConfig,
            now,
        );
        let end = session.power_down(now, PowerEvent::Sleep).unwrap();
        assert_eq!(end.reason, EndReason::Shutdown);
        assert!(!session.power_goodbye_pending());
        let next = session.tick(&TestConfig, false, now);
        assert_eq!(next.goodbye, None);
        assert!(next.client_lost, "the transport forgets it");
        assert!(!session.tick(&TestConfig, false, now).client_lost, "once");
    }

    /// While the PC goes down a HELLO2 gets no answer, as it wouldn't a
    /// moment later: a session started now would be cut without a word.
    /// Waking opens it again, and so does a cancelled shutdown, or the hold
    /// running out when Windows never said it woke.
    #[test]
    fn a_pc_going_down_answers_no_hello_until_it_is_back() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut session = Session::new(500);
        assert_eq!(
            session.power_down(now, PowerEvent::Sleep),
            None,
            "nobody streamed"
        );
        let unanswered =
            session.handle_control(peer, 0, power_hello(2, FEATURE_POWER_BYE), &TestConfig, now);
        assert!(unanswered.replies.is_empty() && unanswered.new_target.is_none());
        assert!(!session.is_active());
        session.power_up();
        let admitted =
            session.handle_control(peer, 0, power_hello(3, FEATURE_POWER_BYE), &TestConfig, now);
        assert_eq!(admitted.new_target, Some(peer));

        let mut session = Session::new(500);
        session.power_down(now, PowerEvent::Shutdown);
        let later = now + POWER_DOWN_HOLD;
        let admitted = session.handle_control(
            peer,
            0,
            power_hello(4, FEATURE_POWER_BYE),
            &TestConfig,
            later,
        );
        assert_eq!(admitted.new_target, Some(peer), "the hold ran out");
    }

    /// A goodbye no transport sent before the PC slept is for a session
    /// that is over by the time it wakes: dropped. Any other goodbye stays.
    #[test]
    fn waking_drops_only_the_goodbye_the_pc_going_down_left() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut session = Session::new(500);
        session.handle_control(peer, 0, power_hello(1, FEATURE_POWER_BYE), &TestConfig, now);
        session.power_down(now, PowerEvent::Sleep).unwrap();
        session.power_up();
        assert_eq!(session.take_goodbye(), None);

        let mut session = Session::new(500);
        session.power_down(now, PowerEvent::Sleep);
        session.power_up();
        session.handle_control(peer, 0, power_hello(2, FEATURE_POWER_BYE), &TestConfig, now);
        session.shut_down(now, StreamStop::Quit).unwrap();
        session.power_up();
        assert!(session.take_goodbye().is_some(), "a quit's goodbye stays");
    }

    /// The PC woke after hours: the session ended when it went to sleep, so
    /// the first pass after waking ends nothing again. It used to expire the
    /// session then, as a silence as long as the sleep.
    #[test]
    fn waking_ends_nothing_a_second_time() {
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        for caps in [FEATURE_POWER_BYE, 0] {
            let mut session = Session::new(500);
            session.handle_control(peer, 0, power_hello(1, caps), &TestConfig, now);
            session.power_down(now, PowerEvent::Sleep).unwrap();
            session.tick(&TestConfig, false, now);
            let woke = now + Duration::from_secs(3 * 3600);
            session.power_up();
            session.transport_resumed(woke, false);
            let first = session.tick(&TestConfig, true, woke);
            assert_eq!(
                (first.ended, first.goodbye, first.client_lost),
                (None, None, false)
            );
        }
    }

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
            max_media_dgram: 0,
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

    /// Feeds `message` as the datagram a Wi-Fi or USB link delivers.
    fn datagram(
        session: &mut Session,
        peer: PeerId,
        session_id: u32,
        message: ControlMessage,
        now: Instant,
    ) -> Actions {
        let bytes = eternal_wire::v2::control::encode_control(session_id, 1, &message);
        let (header, message) = eternal_wire::v2::control::parse_control(&bytes).unwrap();
        session.handle_datagram(peer, &header, message, bytes.len(), &TestConfig, now)
    }

    fn ping() -> ControlMessage {
        ControlMessage::Ping(eternal_wire::v2::control::Ping { t1_us: 1 })
    }

    /// Until something of its session comes back, a Wi-Fi client gets its
    /// first picture and heartbeats, but no more media. Its answer asks for
    /// a keyframe, as frames were held back, and the stream goes on.
    #[test]
    fn a_wifi_client_gets_its_first_picture_then_waits_to_be_heard() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let ipad = addr([10, 0, 0, 5], 50000);
        session.handle_control(ipad, 0, identified_hello(1, 77), &TestConfig, now);
        let id = session.session_id().unwrap();

        assert!(
            session.may_send_media(id, 300 * 1024, now),
            "the first keyframe"
        );
        assert!(session.may_send_media(id, UNANSWERED_MEDIA_BUDGET - 300 * 1024, now));
        assert!(!session.may_send_media(id, 1, now), "the budget is spent");
        let heartbeat = session.tick(&TestConfig, true, now + Duration::from_secs(1));
        assert_eq!(heartbeat.replies.len(), 1, "heartbeats go on");

        let answer = datagram(&mut session, ipad, id, ping(), now + Duration::from_secs(1));
        assert!(answer.force_idr, "a keyframe starts the picture again");
        assert!(session.may_send_media(id, 4 * UNANSWERED_MEDIA_BUDGET, now));
    }

    /// A frame that doesn't fit is held back, and so is every one after it
    /// until the client answers, even one that would fit: a frame after a
    /// gap can't be decoded.
    #[test]
    fn nothing_follows_a_held_back_frame_until_the_answer() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let ipad = addr([10, 0, 0, 5], 50000);
        session.handle_control(ipad, 0, identified_hello(1, 77), &TestConfig, now);
        let id = session.session_id().unwrap();
        assert!(!session.may_send_media(id, UNANSWERED_MEDIA_BUDGET + 1, now));
        assert!(!session.may_send_media(id, 100, now));
        datagram(&mut session, ipad, id, ping(), now);
        assert!(session.may_send_media(id, 100, now));
    }

    /// Whatever the iPad sends first counts, and an answer before the
    /// budget ran out needs no keyframe. A packet with another session's
    /// id, from another address, or a repeat of the HELLO2 is no answer.
    #[test]
    fn any_packet_of_the_session_from_its_address_is_its_answer() {
        let answers = [
            ping(),
            ControlMessage::ReceiverReport(ReceiverReport::default()),
            ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: 7,
                last_complete_seq: 0,
                reason: KeyframeReason::Startup,
                max_media_dgram: 0,
            }),
            ControlMessage::Nack(Nack {
                stream_epoch: 7,
                frame_seq: 1,
                frag_count: 2,
                missing: vec![1],
            }),
        ];
        for answer in answers {
            let mut session = Session::new(1234);
            let now = Instant::now();
            let ipad = addr([10, 0, 0, 5], 50000);
            session.handle_control(ipad, 0, identified_hello(1, 77), &TestConfig, now);
            let id = session.session_id().unwrap();
            datagram(&mut session, ipad, id, identified_hello(1, 77), now);
            datagram(&mut session, ipad, id.wrapping_add(1), ping(), now);
            datagram(&mut session, addr([10, 0, 0, 6], 50000), id, ping(), now);
            assert!(!session.may_send_media(id, UNANSWERED_MEDIA_BUDGET + 1, now));
            let heard = datagram(&mut session, ipad, id, answer.clone(), now);
            assert!(
                session.may_send_media(id, UNANSWERED_MEDIA_BUDGET + 1, now),
                "{answer:?}"
            );

            let mut early = Session::new(1234);
            early.handle_control(ipad, 0, identified_hello(1, 77), &TestConfig, now);
            let id = early.session_id().unwrap();
            assert!(early.may_send_media(id, 1000, now));
            let heard_early = datagram(&mut early, ipad, id, answer.clone(), now);
            assert_eq!(
                heard_early.force_idr,
                heard.force_idr && matches!(answer, ControlMessage::KeyframeRequest(_)),
                "{answer:?}"
            );
            assert!(heard.force_idr, "{answer:?}");
        }
    }

    /// The PC opens USB tunnels itself, to an iPad on the cable: they
    /// stream from the start.
    #[test]
    fn usb_streams_before_the_ipad_is_heard() {
        let mut session = Session::new(1234);
        let usb = PeerId::usb(17);
        let actions = datagram(
            &mut session,
            usb,
            0,
            identified_hello(1, 77),
            Instant::now(),
        );
        let id = session.session_id().unwrap();
        assert_eq!(actions.new_target, Some(usb));
        assert!(session.may_send_media(id, 10 * UNANSWERED_MEDIA_BUDGET, Instant::now()));
    }

    /// A new session from an address whose last one ended before it was
    /// heard, by a new HELLO2 or the liveness timeout, gets no media until
    /// it is, for a minute. An address that was heard starts afresh.
    #[test]
    fn an_address_that_was_never_heard_gets_no_new_budget_for_a_minute() {
        let mut session = Session::new(1234);
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let ipad = addr([10, 0, 0, 5], 50000);
        datagram(&mut session, ipad, 0, identified_hello(1, 77), at(0));
        let first = session.session_id().unwrap();
        datagram(&mut session, ipad, 0, identified_hello(2, 77), at(100));
        let second = session.session_id().unwrap();
        assert_ne!(first, second);
        assert!(
            !session.may_send_media(second, 1, at(100)),
            "superseded unheard"
        );

        let expired = session.tick(&TestConfig, false, at(3200));
        assert_eq!(expired.ended.unwrap().reason, EndReason::Liveness);
        datagram(&mut session, ipad, 0, identified_hello(3, 77), at(5000));
        let third = session.session_id().unwrap();
        assert!(
            !session.may_send_media(third, 1, at(5000)),
            "expired unheard"
        );
        session.tick(&TestConfig, false, at(8200));
        assert_eq!(session.last_end(), Some((third, EndReason::Liveness)));

        datagram(&mut session, ipad, 0, identified_hello(4, 77), at(68_000));
        let early = session.session_id().unwrap();
        assert!(
            !session.may_send_media(early, 1, at(68_000)),
            "not yet a minute"
        );
        session.tick(&TestConfig, false, at(71_200));

        datagram(&mut session, ipad, 0, identified_hello(5, 77), at(132_000));
        let fresh = session.session_id().unwrap();
        assert!(
            session.may_send_media(fresh, 1000, at(132_000)),
            "a minute later"
        );

        datagram(&mut session, ipad, fresh, ping(), at(132_010));
        datagram(&mut session, ipad, 0, identified_hello(6, 77), at(133_000));
        let reconnect = session.session_id().unwrap();
        assert!(
            session.may_send_media(reconnect, 1000, at(133_000)),
            "it was heard"
        );
    }

    /// A HELLO2 with the same device id from another address takes over a
    /// session that was never heard, with what it already spent.
    #[test]
    fn an_address_change_with_the_same_device_id_brings_no_new_budget() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        datagram(
            &mut session,
            addr([10, 0, 0, 5], 50000),
            0,
            identified_hello(1, 77),
            now,
        );
        let first = session.session_id().unwrap();
        assert!(session.may_send_media(first, UNANSWERED_MEDIA_BUDGET, now));

        let later = now + Duration::from_millis(50);
        let moved = addr([10, 0, 0, 9], 50000);
        let actions = datagram(&mut session, moved, 0, identified_hello(2, 77), later);
        assert_eq!(actions.new_target, Some(moved));
        let second = session.session_id().unwrap();
        assert_ne!(first, second);
        assert!(!session.may_send_media(second, 1, later));
    }

    /// Media to clients not yet heard counts against one host-wide budget,
    /// whatever their address, which comes back over a minute. It still
    /// holds once more addresses than the memory keeps have come and gone.
    #[test]
    fn the_host_budget_covers_every_address_even_with_the_memory_full() {
        let mut session = Session::new(1234);
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let first = addr([10, 0, 0, 5], 50000);
        datagram(&mut session, first, 0, identified_hello(1, 77), at(0));
        let id = session.session_id().unwrap();
        assert!(session.may_send_media(id, UNANSWERED_MEDIA_BUDGET, at(0)));
        session.tick(&TestConfig, false, at(3200));
        assert_eq!(session.last_end(), Some((id, EndReason::Liveness)));

        // Every slot of the memory taken by other addresses.
        for host in 0..UNANSWERED_KEPT as u8 {
            let other = addr([10, 0, 1, host], 50000);
            datagram(
                &mut session,
                other,
                0,
                identified_hello(10 + u32::from(host), 88),
                at(4000 + u64::from(host)),
            );
        }
        session.tick(&TestConfig, false, at(7200));
        assert!(session.session_id().is_none());

        datagram(&mut session, first, 0, identified_hello(2, 77), at(9000));
        let back = session.session_id().unwrap();
        assert!(!session.may_send_media(back, 200 * 1024, at(9000)));
        session.tick(&TestConfig, false, at(12_200));

        let fresh = addr([10, 0, 2, 1], 50000);
        datagram(&mut session, fresh, 0, identified_hello(3, 99), at(13_000));
        let other = session.session_id().unwrap();
        assert!(
            !session.may_send_media(other, 200 * 1024, at(13_000)),
            "another address"
        );
        session.tick(&TestConfig, false, at(16_200));

        datagram(&mut session, fresh, 0, identified_hello(4, 99), at(80_000));
        let refilled = session.session_id().unwrap();
        assert!(
            session.may_send_media(refilled, UNANSWERED_MEDIA_BUDGET, at(80_000)),
            "a minute after the last spend"
        );
    }

    /// An address that keeps ending sessions before it is heard holds one
    /// entry in the memory, refreshed in place, and can't push another
    /// address out of it.
    #[test]
    fn a_churning_address_keeps_one_entry_in_the_memory() {
        let mut session = Session::new(1234);
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let first = addr([10, 0, 0, 5], 50000);
        datagram(&mut session, first, 0, identified_hello(1, 77), at(0));
        session.tick(&TestConfig, false, at(3200));

        let churn = addr([10, 0, 0, 6], 50000);
        for nonce in 0..2 * UNANSWERED_KEPT as u32 {
            datagram(
                &mut session,
                churn,
                0,
                identified_hello(10 + nonce, 88),
                at(4000 + u64::from(nonce)),
            );
        }
        session.tick(&TestConfig, false, at(7200));
        assert_eq!(session.unanswered.len(), 2);

        datagram(&mut session, first, 0, identified_hello(2, 77), at(8000));
        let back = session.session_id().unwrap();
        assert!(!session.may_send_media(back, 1000, at(8000)));
    }

    /// An iPad's HELLO2 as shipped since 0.3.0: the generic "iPad" name and
    /// its device id.
    fn shipped_hello(nonce: u32, device_id: u64) -> ControlMessage {
        let ControlMessage::Hello2(mut hello) = identified_hello(nonce, device_id) else {
            unreachable!()
        };
        hello.device_name = "iPad".to_string();
        ControlMessage::Hello2(hello)
    }

    /// A refusal over Wi-Fi is never larger than the HELLO2 it answers. It
    /// leaves out the PC name and host caps when they don't fit (iPads read
    /// only the status and version of a refusal), and a HELLO2 too small
    /// for even that gets no answer. USB answers stay whole.
    #[test]
    fn a_refusal_is_never_larger_than_its_hello() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        datagram(
            &mut session,
            addr([10, 0, 0, 5], 50000),
            0,
            shipped_hello(1, 77),
            now,
        );

        let other = addr([10, 0, 0, 6], 50000);
        let shipped = shipped_hello(2, 88);
        let shipped_len = eternal_wire::v2::control::encode_control(0, 1, &shipped).len();
        assert_eq!(shipped_len, 71);
        let busy = datagram(&mut session, other, 0, shipped, now);
        let (to, reply) = &busy.replies[0];
        assert_eq!(*to, other);
        assert!(
            reply.len() <= shipped_len,
            "{} > {shipped_len}",
            reply.len()
        );
        let ack = parse_ack(reply);
        assert_eq!(
            (ack.status, ack.client_nonce, ack.accepted_version),
            (HelloStatus::Busy, 2, 2)
        );
        assert_eq!((ack.host_name.as_str(), ack.host_caps), ("", 0));

        let ControlMessage::Hello2(mut named) = shipped_hello(3, 88) else {
            unreachable!()
        };
        named.device_name = "A much longer iPad name here".to_string();
        let named = ControlMessage::Hello2(named);
        let named_len = eternal_wire::v2::control::encode_control(0, 1, &named).len();
        let busy = datagram(&mut session, other, 0, named, now);
        let reply = &busy.replies[0].1;
        assert!(reply.len() <= named_len);
        assert_eq!(parse_ack(reply).host_name, "TEST-HOST", "it fits whole");

        let ControlMessage::Hello2(mut bare) = hello(4, 50000) else {
            unreachable!()
        };
        bare.device_name.clear();
        let bare = ControlMessage::Hello2(bare);
        assert_eq!(
            eternal_wire::v2::control::encode_control(0, 1, &bare).len(),
            38
        );
        let unanswered = datagram(&mut session, other, 0, bare.clone(), now);
        assert!(unanswered.replies.is_empty());
        assert_eq!(
            unanswered.ended.unwrap().reason,
            EndReason::Refused(HelloStatus::Busy),
            "still one refused attempt"
        );

        let usb = datagram(&mut session, PeerId::usb(17), 0, bare, now);
        assert_eq!(parse_ack(&usb.replies[0].1).host_name, "TEST-HOST");
    }

    /// The golden HELLO2 of a 0.3.0 iPad, byte for byte, as the second
    /// device: a refusal that fits it, which that iPad's parser reads (the
    /// `hello_ack_busy_bare` vector). As the only one: a session that
    /// streams once its first ping arrives.
    #[test]
    fn a_shipped_ipads_golden_hello_is_answered_within_its_size() {
        let golden = include_str!("../../../proto/testdata/v2_vectors.txt")
            .lines()
            .find_map(|line| line.strip_prefix("hello2_v030 "))
            .unwrap();
        let bytes: Vec<u8> = (0..golden.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&golden[at..at + 2], 16).unwrap())
            .collect();
        let feed = |session: &mut Session, peer: PeerId, now: Instant| {
            let (header, message) = eternal_wire::v2::control::parse_control(&bytes).unwrap();
            session.handle_datagram(peer, &header, message, bytes.len(), &TestConfig, now)
        };
        let now = Instant::now();
        let ipad = addr([10, 0, 0, 6], 50000);

        let mut session = Session::new(1234);
        datagram(
            &mut session,
            addr([10, 0, 0, 5], 50000),
            0,
            shipped_hello(1, 77),
            now,
        );
        let busy = feed(&mut session, ipad, now);
        let reply = &busy.replies[0].1;
        assert!(
            reply.len() <= bytes.len(),
            "{} > {}",
            reply.len(),
            bytes.len()
        );
        assert_eq!(parse_ack(reply).status, HelloStatus::Busy);

        let mut session = Session::new(1234);
        let accepted = feed(&mut session, ipad, now);
        let ack = parse_ack(&accepted.replies[0].1);
        assert_eq!(
            (ack.status, ack.host_name.as_str()),
            (HelloStatus::Ok, "TEST-HOST")
        );
        datagram(&mut session, ipad, ack.session_id, ping(), now);
        assert!(session.may_send_media(ack.session_id, 10 * UNANSWERED_MEDIA_BUDGET, now));
    }

    /// Session ids come from a keyed hash, so two instances with the same
    /// seed hand out different ids.
    #[test]
    fn session_ids_differ_between_instances() {
        let ipad = addr([10, 0, 0, 5], 50000);
        let ids = |session: &mut Session| -> Vec<u32> {
            (1..=4)
                .map(|nonce| {
                    session.handle_control(
                        ipad,
                        0,
                        identified_hello(nonce, 77),
                        &TestConfig,
                        Instant::now(),
                    );
                    session.session_id().unwrap()
                })
                .collect()
        };
        let first = ids(&mut Session::new(1234));
        let steps: Vec<u32> = first.windows(2).map(|p| p[1].wrapping_sub(p[0])).collect();
        assert!(steps.windows(2).any(|p| p[0] != p[1]), "{first:?}");
        assert_ne!(first, ids(&mut Session::new(1234)), "same seed, other ids");
    }

    fn paired_hello(nonce: u32, feature_caps: u16) -> Hello2 {
        let mut hello = pair_hello(nonce);
        hello.auth_token = [7; 16];
        hello.feature_caps = feature_caps;
        hello
    }

    fn ask_while(
        session: &mut Session,
        config: &PairConfig,
        state: NotStreaming,
        hello: Hello2,
    ) -> Actions {
        session.set_not_streaming(Some(state));
        pair_send(
            session,
            config,
            addr([192, 0, 2, 1], 50000),
            hello,
            Instant::now(),
        )
    }

    /// A paired iPad that asks while no stream runs hears why, and nothing
    /// starts: no session, no media target, no pairing token.
    #[test]
    fn a_new_ipad_hears_why_the_pc_is_not_streaming() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        for (state, status, reason) in [
            (NotStreaming::Stopped, HelloStatus::Stopped, ""),
            (NotStreaming::Restarting, HelloStatus::Restarting, ""),
            (
                NotStreaming::Failed(FailedReason::DisplayChanging),
                HelloStatus::Failed,
                "display_changing",
            ),
        ] {
            let actions = ask_while(
                &mut session,
                &config,
                state,
                paired_hello(1, FEATURE_STANDBY_STATUS),
            );
            let ack = pair_ack(&actions);
            assert_eq!((ack.status, ack.reason.as_str()), (status, reason));
            assert_eq!((ack.session_id, ack.auth_token), (0, [0; 16]));
            assert!(actions.new_target.is_none() && !actions.force_idr);
            assert!(!session.is_active());
        }
        // Streaming again: the same HELLO2 connects.
        session.set_not_streaming(None);
        let actions = pair_send(
            &mut session,
            &config,
            addr([192, 0, 2, 1], 50000),
            paired_hello(1, FEATURE_STANDBY_STATUS),
            Instant::now(),
        );
        assert_eq!(pair_ack(&actions).status, HelloStatus::Ok);
        assert!(session.is_active());
    }

    /// 0.3.4 and older drop statuses past RateLimited. While the PC restarts
    /// they hear nothing and keep retrying, as before; a stopped or failed
    /// PC turns them down with Error, a card they have.
    #[test]
    fn an_old_ipad_gets_only_statuses_it_knows() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let restarting = ask_while(
            &mut session,
            &config,
            NotStreaming::Restarting,
            paired_hello(1, 0),
        );
        assert!(restarting.replies.is_empty() && restarting.ended.is_none());
        for state in [
            NotStreaming::Stopped,
            NotStreaming::Failed(FailedReason::Encoder),
        ] {
            let ack = pair_ack(&ask_while(&mut session, &config, state, paired_hello(2, 0)));
            assert_eq!(ack.status, HelloStatus::Error);
            assert!(ack.reason.is_empty());
        }
        assert!(!session.is_active());
    }

    /// Only an iPad that may connect learns the stream state. Wrong codes
    /// count toward the rate limit here too, and the right code is not
    /// spent, so it still works once the stream starts.
    #[test]
    fn pairing_still_decides_who_hears_it() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let mut unpaired = pair_hello(1);
        unpaired.feature_caps = FEATURE_STANDBY_STATUS;
        for state in [
            NotStreaming::Stopped,
            NotStreaming::Restarting,
            NotStreaming::Failed(FailedReason::Capture),
        ] {
            let ack = pair_ack(&ask_while(&mut session, &config, state, unpaired.clone()));
            assert_eq!(ack.status, HelloStatus::Unauthorized);
            assert!(ack.reason.is_empty());
        }
        let mut statuses = Vec::new();
        for attempt in 1..=5 {
            let mut guess = unpaired.clone();
            guess.client_nonce = 10 + attempt;
            guess.pairing_code = 1_000_000 + attempt;
            statuses.push(
                pair_ack(&ask_while(
                    &mut session,
                    &config,
                    NotStreaming::Stopped,
                    guess,
                ))
                .status,
            );
        }
        assert_eq!(statuses.last(), Some(&HelloStatus::RateLimited));

        let config = pair_config(true);
        let code = config.0.lock().code();
        let mut right = unpaired;
        right.client_nonce = 20;
        right.pairing_code = code;
        let ack = pair_ack(&ask_while(
            &mut session,
            &config,
            NotStreaming::Stopped,
            right.clone(),
        ));
        assert_eq!(
            (ack.status, ack.auth_token),
            (HelloStatus::Stopped, [0; 16])
        );
        assert_eq!(config.0.lock().code(), code, "the code stays valid");
        session.set_not_streaming(None);
        let accepted = pair_send(
            &mut session,
            &config,
            addr([192, 0, 2, 1], 50000),
            right,
            Instant::now(),
        );
        assert_eq!(pair_ack(&accepted).status, HelloStatus::Ok);
    }

    /// Stop ends the session and closes the PC to clients; the supervisor
    /// then says why, and a HELLO2 hears it instead of Busy. A quit, which
    /// no supervisor state follows, still turns it away with Busy.
    #[test]
    fn a_stop_is_answered_with_why_not_busy() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let now = Instant::now();
        session.shut_down(now, StreamStop::Quit);
        let busy = pair_send(
            &mut session,
            &config,
            addr([192, 0, 2, 1], 50000),
            paired_hello(1, FEATURE_STANDBY_STATUS),
            now,
        );
        assert_eq!(pair_ack(&busy).status, HelloStatus::Busy);
        session.shut_down(now, StreamStop::User);
        let ack = pair_ack(&ask_while(
            &mut session,
            &config,
            NotStreaming::Stopped,
            paired_hello(2, FEATURE_STANDBY_STATUS),
        ));
        assert_eq!(ack.status, HelloStatus::Stopped);
    }

    #[test]
    fn the_version_and_update_refusals_come_first() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let mut old = paired_hello(1, FEATURE_STANDBY_STATUS);
        old.proto_min = 1;
        old.proto_max = 1;
        let ack = pair_ack(&ask_while(
            &mut session,
            &config,
            NotStreaming::Stopped,
            old,
        ));
        assert_eq!(ack.status, HelloStatus::VersionUnsupported);
        assert!(session.begin_update(false));
        let ack = pair_ack(&ask_while(
            &mut session,
            &config,
            NotStreaming::Stopped,
            paired_hello(2, FEATURE_STANDBY_STATUS),
        ));
        assert_eq!(ack.status, HelloStatus::Busy);
    }

    /// A session kept across a restart stays, and its iPad's repeated
    /// HELLO2 hears that the PC is restarting instead of the old OK.
    #[test]
    fn a_session_kept_across_a_restart_stays() {
        let config = pair_config(true);
        let mut session = Session::new(10);
        let hello = paired_hello(1, FEATURE_STANDBY_STATUS);
        let peer = addr([192, 0, 2, 1], 50000);
        let accepted = pair_send(&mut session, &config, peer, hello.clone(), Instant::now());
        assert_eq!(pair_ack(&accepted).status, HelloStatus::Ok);
        let ack = pair_ack(&ask_while(
            &mut session,
            &config,
            NotStreaming::Restarting,
            hello,
        ));
        assert_eq!(ack.status, HelloStatus::Restarting);
        assert!(session.is_active());
    }

    /// The stream that is stopping still takes HELLO2s until its transport
    /// ends. Its not-streaming answers are never larger than the HELLO2
    /// either, as the standby's aren't.
    #[test]
    fn a_not_streaming_answer_is_never_larger_than_the_hello() {
        let config = pair_config(false);
        let mut session = Session::new(10);
        let peer = addr([192, 0, 2, 1], 50000);
        let mut small = pair_hello(1);
        small.device_name = String::new();
        small.device_id = 0;
        let unpadded =
            eternal_wire::v2::control::encode_control(0, 1, &ControlMessage::Hello2(small.clone()));
        // The same small HELLO2 claiming the bit, without its padding.
        let mut claimed = unpadded.clone();
        claimed[26..28].copy_from_slice(&FEATURE_STANDBY_STATUS.to_le_bytes());
        let mut padded_hello = small;
        padded_hello.client_nonce = 2;
        padded_hello.feature_caps = FEATURE_STANDBY_STATUS;
        let padded =
            eternal_wire::v2::control::encode_control(0, 2, &ControlMessage::Hello2(padded_hello));
        session.set_not_streaming(Some(NotStreaming::Failed(FailedReason::Encoder)));
        let mut answer = |datagram: &[u8]| {
            let (header, message) = eternal_wire::v2::control::parse_control(datagram).unwrap();
            session.handle_datagram(
                peer,
                &header,
                message,
                datagram.len(),
                &config,
                Instant::now(),
            )
        };
        for small in [&unpadded, &claimed] {
            assert_eq!(small.len(), 38);
            let actions = answer(small);
            assert!(
                actions.replies.is_empty(),
                "{} bytes answered",
                actions.replies[0].1.len()
            );
        }
        let actions = answer(&padded);
        let (_, reply) = &actions.replies[0];
        assert!(reply.len() <= padded.len());
        let (_, ControlMessage::HelloAck(ack)) =
            eternal_wire::v2::control::parse_control(reply).unwrap()
        else {
            panic!("expected an ACK");
        };
        assert_eq!(
            (ack.status, ack.reason.as_str()),
            (HelloStatus::Failed, "encoder")
        );
    }

    /// Every answer to a Wi-Fi HELLO2 is bounded by its size, the version
    /// and pairing refusals too, while no stream runs and while one does. A
    /// guess at the code carries the HELLO2's extension, so it hears the
    /// short refusal; a padded HELLO2 hears whole answers. Over USB the
    /// answers stay whole.
    #[test]
    fn every_hello_answer_is_bounded_while_not_streaming() {
        use eternal_wire::v2::control::{encode_control, parse_control};
        let peer = addr([192, 0, 2, 1], 50000);
        let send = |session: &mut Session, config: &PairConfig, hello: &Hello2| {
            let datagram = encode_control(0, 1, &ControlMessage::Hello2(hello.clone()));
            let (header, message) = parse_control(&datagram).unwrap();
            let actions = session.handle_datagram(
                peer,
                &header,
                message,
                datagram.len(),
                config,
                Instant::now(),
            );
            for (_, reply) in &actions.replies {
                assert!(
                    reply.len() <= datagram.len(),
                    "{} > {}",
                    reply.len(),
                    datagram.len()
                );
            }
            actions
                .replies
                .first()
                .map(|(_, reply)| match parse_control(reply) {
                    Ok((_, ControlMessage::HelloAck(ack))) => ack.status,
                    other => panic!("not an ACK: {other:?}"),
                })
        };
        let small = |nonce: u32, caps: u16| {
            let mut hello = pair_hello(nonce);
            hello.device_name = String::new();
            hello.device_id = 0;
            hello.feature_caps = caps;
            hello
        };

        let config = pair_config(true);
        let mut session = Session::new(10);
        session.set_not_streaming(Some(NotStreaming::Stopped));
        // The version check.
        let mut old_version = small(1, 0);
        old_version.proto_min = 1;
        old_version.proto_max = 1;
        assert_eq!(send(&mut session, &config, &old_version), None);
        old_version.feature_caps = FEATURE_STANDBY_STATUS;
        assert_eq!(
            send(&mut session, &config, &old_version),
            Some(HelloStatus::VersionUnsupported)
        );
        // The pairing checks: no code, then wrong codes until rate limited.
        assert_eq!(send(&mut session, &config, &small(2, 0)), None);
        for attempt in 0..6 {
            let mut guess = small(10 + attempt, 0);
            guess.pairing_code = 1_000_000 + attempt;
            let expected = if attempt < 3 {
                HelloStatus::Unauthorized
            } else {
                HelloStatus::RateLimited
            };
            assert_eq!(
                send(&mut session, &config, &guess),
                Some(expected),
                "attempt {attempt}"
            );
        }
        let mut padded = small(20, FEATURE_STANDBY_STATUS);
        padded.pairing_code = 1_000_020;
        assert_eq!(
            send(&mut session, &config, &padded),
            Some(HelloStatus::RateLimited)
        );

        // Streaming: the same bound, and an attempt with no answer that
        // fits is still one refused attempt.
        let config = pair_config(true);
        let mut session = Session::new(10);
        assert_eq!(send(&mut session, &config, &small(30, 0)), None);
        let mut guess = small(31, 0);
        guess.pairing_code = 1_000_031;
        assert_eq!(
            send(&mut session, &config, &guess),
            Some(HelloStatus::Unauthorized)
        );

        // Over USB, stopped or streaming, the answers stay whole.
        let usb = PeerId::usb(17);
        let over_usb = |session: &mut Session, hello: &Hello2| {
            let datagram = encode_control(0, 1, &ControlMessage::Hello2(hello.clone()));
            let (header, message) = parse_control(&datagram).unwrap();
            let actions = session.handle_datagram(
                usb,
                &header,
                message,
                datagram.len(),
                &config,
                Instant::now(),
            );
            assert!(actions.replies[0].1.len() > datagram.len());
            pair_ack(&actions)
        };
        let mut session = Session::new(10);
        let mut old_version = small(40, 0);
        old_version.proto_min = 1;
        old_version.proto_max = 1;
        let refused = over_usb(&mut session, &old_version);
        assert_eq!(refused.status, HelloStatus::VersionUnsupported);
        assert_eq!(refused.host_name, "TEST-HOST");
        let mut paired = small(41, 0);
        paired.auth_token = [7; 16];
        session.set_not_streaming(Some(NotStreaming::Stopped));
        let stopped = over_usb(&mut session, &paired);
        assert_eq!(
            (stopped.status, stopped.host_name.as_str()),
            (HelloStatus::Error, "TEST-HOST")
        );
    }
}
