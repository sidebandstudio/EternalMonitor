//! Datagram links. TCP tunnels preserve packet boundaries with EMLINK framing
//! and discard queued video when a reader falls behind.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use eternal_wire::v2::audio::{AudioHeader, AUDIO_FLAG_DISCONTINUITY};
use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified, MAX_DGRAM_SIZE};
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::JoinHandle;

pub const PREAMBLE: &[u8; 8] = b"EMLINK\x01\x00";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// Room for a keyframe burst to drain: about 100 ms at 60 fps. Two frames
/// overflowed behind every keyframe on a slow PC's usbmuxd.
const FRAME_LIMIT: usize = 6;
const BYTE_LIMIT: usize = 1024 * 1024;
const KEYFRAME_BACKOFF_MIN: Duration = Duration::from_millis(250);
const KEYFRAME_BACKOFF_MAX: Duration = Duration::from_secs(2);
/// This long without a lost frame, and the next request is immediate again.
const KEYFRAME_BACKOFF_RESET: Duration = Duration::from_secs(5);
const CONTROL_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkId {
    Udp,
    Usb { device_id: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PeerId {
    pub link: LinkId,
    pub addr: Option<SocketAddr>,
}

impl PeerId {
    pub fn udp(addr: SocketAddr) -> Self {
        Self {
            link: LinkId::Udp,
            addr: Some(addr),
        }
    }

    pub fn usb(device_id: u32) -> Self {
        Self {
            link: LinkId::Usb { device_id },
            addr: None,
        }
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.link, self.addr) {
            (LinkId::Udp, Some(addr)) => addr.fmt(f),
            (LinkId::Udp, None) => f.write_str("waiting for client"),
            (LinkId::Usb { device_id }, _) => write!(f, "USB {device_id}"),
        }
    }
}

pub type Inbound = (PeerId, Vec<u8>);

pub trait Link {
    fn id(&self) -> LinkId;
    fn receive(&mut self) -> impl Future<Output = io::Result<Inbound>> + Send;
    fn send(&self, peer: PeerId, datagram: &[u8]) -> impl Future<Output = io::Result<()>> + Send;
}

pub struct UdpLink {
    pub(crate) socket: UdpSocket,
}

impl UdpLink {
    pub fn new(socket: UdpSocket) -> Self {
        Self { socket }
    }
}

impl Link for UdpLink {
    fn id(&self) -> LinkId {
        LinkId::Udp
    }

    async fn receive(&mut self) -> io::Result<Inbound> {
        let mut buffer = [0; 2048];
        let (len, peer) = self.socket.recv_from(&mut buffer).await?;
        Ok((PeerId::udp(peer), buffer[..len].to_vec()))
    }

    async fn send(&self, peer: PeerId, datagram: &[u8]) -> io::Result<()> {
        let Some(addr) = peer.addr.filter(|_| peer.link == LinkId::Udp) else {
            return Err(invalid("UDP destination"));
        };
        check_length(datagram.len())?;
        self.socket.send_to(datagram, addr).await.map(|_| ())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn check_length(len: usize) -> io::Result<()> {
    if !(1..=MAX_DGRAM_SIZE).contains(&len) {
        return Err(invalid("EMLINK packet length"));
    }
    Ok(())
}

pub async fn read_preamble(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
    let mut bytes = [0; 8];
    tokio::time::timeout(HANDSHAKE_TIMEOUT, reader.read_exact(&mut bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "EMLINK preamble timeout"))??;
    if &bytes != PREAMBLE {
        return Err(invalid("EMLINK preamble"));
    }
    Ok(())
}

pub async fn read_datagram(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let len = usize::from(reader.read_u16_le().await?);
    check_length(len)?;
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

pub async fn write_datagram(
    writer: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
) -> io::Result<()> {
    check_length(bytes.len())?;
    let mut framed = [0; MAX_DGRAM_SIZE + 2];
    framed[..2].copy_from_slice(&(bytes.len() as u16).to_le_bytes());
    framed[2..2 + bytes.len()].copy_from_slice(bytes);
    writer.write_all(&framed[..2 + bytes.len()]).await
}

struct Frame {
    epoch: u32,
    seq: u32,
    frag_count: u16,
    datagrams: Vec<Vec<u8>>,
    bytes: usize,
}

impl Frame {
    fn complete(&self) -> bool {
        self.datagrams.len() == usize::from(self.frag_count)
    }
}

/// Complete access units are the unit of delivery and eviction. A partial
/// frame cannot leave the queue, even if its fragments arrive in separate calls.
#[derive(Default)]
struct VideoQueue {
    frames: VecDeque<Frame>,
    bytes: usize,
    awaiting_keyframe: bool,
    discarded: Option<(u32, u32)>,
    keyframes: KeyframeBackoff,
    /// The session and stream epoch of the latest frame.
    stream: Option<(u32, u32)>,
}

impl VideoQueue {
    /// Returns frames lost. A full queue drops the newest frame, never the
    /// queued ones: those still decode, and the frame already being written
    /// is never cut short. Dropping the queue to admit newer frames sent the
    /// iPad partial keyframes, each of which asked for another keyframe, the
    /// biggest frame of all. Dependents of a lost frame are discarded until a
    /// keyframe, and a keyframe always passes. It replaces what is still
    /// queued only when it cannot fit behind it, and those frames count as
    /// lost. Neither how long a frame waited, how often frames arrive, nor
    /// how long a write has stood still says the tunnel is behind: one large
    /// frame can take a second to write through a slow cable that still
    /// moves, a still desktop can leave a paused tunnel nothing newer to
    /// send, and the frames queued behind either still decode.
    fn push(&mut self, bytes: &[u8], now: Instant) -> io::Result<u64> {
        let (header, _) = MediaHeader::decode(bytes).map_err(|_| invalid("video header"))?;
        let key = (header.stream_epoch, header.frame_seq);
        if self.discarded == Some(key) {
            return Ok(0);
        }
        let mut dropped = 0;
        if header.frag_index == 0 {
            if self.frames.back().is_some_and(|frame| !frame.complete()) {
                return Err(invalid("unfinished video access unit"));
            }
            let stream = (header.session_id, header.stream_epoch);
            if self
                .stream
                .replace(stream)
                .is_some_and(|last| last != stream)
            {
                // A new stream on a reused tunnel owes nothing to the old
                // one's losses: its first request must not wait out their
                // backoff.
                self.keyframes = KeyframeBackoff::default();
            }
            if header.is_keyframe {
                // Queued frames are older and the keyframe needs none of them,
                // but they still decode. They are replaced only when the
                // keyframe cannot fit behind them: here when no frame fits,
                // and below when its bytes do not.
                if self.frames.len() >= FRAME_LIMIT {
                    dropped = self.frames.len() as u64;
                    self.frames.clear();
                    self.bytes = 0;
                }
                self.awaiting_keyframe = false;
                self.keyframes.arrived();
            } else if self.awaiting_keyframe {
                self.discarded = Some(key);
                return Ok(1);
            } else if self.frames.len() >= FRAME_LIMIT || self.bytes + bytes.len() > BYTE_LIMIT {
                self.lose(key, now);
                return Ok(1);
            }
            self.frames.push_back(Frame {
                epoch: header.stream_epoch,
                seq: header.frame_seq,
                frag_count: header.frag_count,
                datagrams: Vec::with_capacity(usize::from(header.frag_count).min(1024)),
                bytes: 0,
            });
        }
        let Some(frame) = self.frames.back() else {
            return Err(invalid("missing first fragment"));
        };
        if (frame.epoch, frame.seq) != key
            || frame.frag_count != header.frag_count
            || frame.datagrams.len() != usize::from(header.frag_index)
        {
            return Err(invalid("video fragment order"));
        }
        if header.is_keyframe && self.bytes + bytes.len() > BYTE_LIMIT {
            // Only the newest frame can be partial, so all before it are
            // older and complete.
            let keyframe = self.frames.pop_back().unwrap();
            dropped += self.frames.len() as u64;
            self.frames.clear();
            self.bytes = keyframe.bytes;
            self.frames.push_back(keyframe);
        }
        if self.bytes + bytes.len() > BYTE_LIMIT {
            // Only the newest frame can be partial; the others stay.
            let frame = self.frames.pop_back().unwrap();
            self.bytes -= frame.bytes;
            self.lose(key, now);
            return Ok(dropped + 1);
        }
        let frame = self.frames.back_mut().unwrap();
        frame.bytes += bytes.len();
        frame.datagrams.push(bytes.to_vec());
        self.bytes += bytes.len();
        Ok(dropped)
    }

    fn lose(&mut self, key: (u32, u32), now: Instant) {
        self.discarded = Some(key);
        self.awaiting_keyframe = true;
        self.keyframes.lost(now);
    }

    fn pop(&mut self) -> Option<Frame> {
        if !self.frames.front().is_some_and(Frame::complete) {
            return None;
        }
        let frame = self.frames.pop_front().unwrap();
        self.bytes -= frame.bytes;
        Some(frame)
    }
}

/// When a lost frame may ask the encoder for a keyframe. Every request used
/// to be immediate, and on a link too slow for keyframe bursts each new
/// keyframe overflowed the queue again. The first request is immediate, then
/// the wait doubles while the link keeps losing frames.
#[derive(Default)]
struct KeyframeBackoff {
    pending: bool,
    next_allowed: Option<Instant>,
    interval: Duration,
    last_loss: Option<Instant>,
}

impl KeyframeBackoff {
    fn lost(&mut self, now: Instant) {
        if self
            .last_loss
            .is_some_and(|at| now.saturating_duration_since(at) >= KEYFRAME_BACKOFF_RESET)
        {
            self.next_allowed = None;
            self.interval = Duration::ZERO;
        }
        self.last_loss = Some(now);
        self.pending = true;
    }

    /// A keyframe reached the queue; nothing more needs to be requested.
    fn arrived(&mut self) {
        self.pending = false;
    }

    /// Whether to request a keyframe now. Each request starts the next wait.
    fn due(&mut self, now: Instant) -> bool {
        if !self.pending || self.next_allowed.is_some_and(|at| now < at) {
            return false;
        }
        self.pending = false;
        let interval = self.interval.max(KEYFRAME_BACKOFF_MIN);
        self.next_allowed = Some(now + interval);
        self.interval = (interval * 2).min(KEYFRAME_BACKOFF_MAX);
        true
    }

    /// When a deferred request falls due, if one is waiting.
    fn deadline(&self) -> Option<Instant> {
        self.next_allowed.filter(|_| self.pending)
    }
}

#[derive(Default)]
struct Outbound {
    controls: VecDeque<Vec<u8>>,
    /// Control datagrams ever queued; [`WriterState::controls_written`]
    /// catches up as the writer finishes them.
    controls_queued: u64,
    audio: AudioQueue,
    video: VideoQueue,
}

#[derive(Default)]
struct AudioQueue {
    packets: VecDeque<(Instant, Vec<u8>)>,
    reset_pending: bool,
}

impl AudioQueue {
    fn push(&mut self, bytes: &[u8], now: Instant) -> io::Result<u64> {
        AudioHeader::decode(bytes).map_err(|_| invalid("audio header"))?;
        let dropped = u64::from(self.packets.len() == crate::audio::QUEUE_CAPACITY);
        if dropped > 0 {
            self.packets.pop_front();
            self.reset_pending = true;
        }
        self.packets.push_back((now, bytes.to_vec()));
        Ok(dropped)
    }

    fn pop(&mut self, now: Instant) -> (Option<Vec<u8>>, u64) {
        let mut dropped = 0;
        while let Some((at, mut bytes)) = self.packets.pop_front() {
            if now.saturating_duration_since(at) > Duration::from_millis(100) {
                dropped += 1;
                self.reset_pending = true;
                continue;
            }
            if std::mem::take(&mut self.reset_pending) {
                bytes[4] |= AUDIO_FLAG_DISCONTINUITY;
            }
            return (Some(bytes), dropped);
        }
        (None, dropped)
    }
}

#[derive(Default)]
pub struct FramedStats {
    pub frames_dropped: AtomicU64,
    /// Dropped frames that show the tunnel falling behind; ABR's signal.
    pub congestion_drops: AtomicU64,
    pub audio_packets_dropped: AtomicU64,
}

/// Every tunnel's [`FramedStats::congestion_drops`], for ABR.
static TUNNEL_CONGESTION: AtomicU64 = AtomicU64::new(0);

/// Frames all USB tunnels have dropped because they fell behind, since the
/// process started.
pub fn tunnel_congestion_drops() -> u64 {
    TUNNEL_CONGESTION.load(Ordering::Relaxed)
}

struct WriterState {
    queue: Mutex<Outbound>,
    ready: Notify,
    closed: watch::Sender<bool>,
    /// Control datagrams fully written to the stream, in queue order.
    controls_written: watch::Sender<u64>,
    force_idr: Arc<AtomicBool>,
    stats: Arc<FramedStats>,
}

impl WriterState {
    /// The next control or audio datagram, and whether it is a control.
    fn take_priority(&self) -> Option<(Vec<u8>, bool)> {
        let mut queue = self.queue.lock();
        if let Some(control) = queue.controls.pop_front() {
            return Some((control, true));
        }
        let (audio, dropped) = queue.audio.pop(Instant::now());
        self.stats
            .audio_packets_dropped
            .fetch_add(dropped, Ordering::Relaxed);
        audio.map(|audio| (audio, false))
    }

    fn wrote(&self, control: bool) {
        if control {
            self.controls_written.send_modify(|written| *written += 1);
        }
    }

    fn record_drops(&self, video: &mut VideoQueue, dropped: u64, now: Instant) {
        self.stats
            .frames_dropped
            .fetch_add(dropped, Ordering::Relaxed);
        self.stats
            .congestion_drops
            .fetch_add(dropped, Ordering::Relaxed);
        TUNNEL_CONGESTION.fetch_add(dropped, Ordering::Relaxed);
        if video.keyframes.due(now) {
            self.force_idr.store(true, Ordering::SeqCst);
        }
    }
}

pub struct FramedLink {
    peer: PeerId,
    state: Arc<WriterState>,
    inbound: Option<mpsc::Receiver<Inbound>>,
    tasks: [JoinHandle<()>; 2],
}

impl FramedLink {
    /// The host is the connecting side and writes the preamble before packets.
    pub async fn connect<T>(
        mut stream: T,
        peer: PeerId,
        force_idr: Arc<AtomicBool>,
    ) -> io::Result<Self>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.write_all(PREAMBLE))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "EMLINK write preamble timeout")
            })??;
        Ok(Self::start(stream, peer, force_idr))
    }

    pub async fn accept<T>(
        mut stream: T,
        peer: PeerId,
        force_idr: Arc<AtomicBool>,
    ) -> io::Result<Self>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        read_preamble(&mut stream).await?;
        Ok(Self::start(stream, peer, force_idr))
    }

    fn start<T>(stream: T, peer: PeerId, force_idr: Arc<AtomicBool>) -> Self
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut reader, writer) = tokio::io::split(stream);
        let (inbound_tx, inbound_rx) = mpsc::channel(128);
        let (closed, mut read_closed) = watch::channel(false);
        let state = Arc::new(WriterState {
            queue: Mutex::new(Outbound::default()),
            ready: Notify::new(),
            closed,
            controls_written: watch::channel(0).0,
            force_idr,
            stats: Arc::new(FramedStats::default()),
        });
        let read_state = Arc::clone(&state);
        let reading = tokio::spawn(async move {
            loop {
                let packet = tokio::select! {
                    biased;
                    _ = read_closed.changed() => break,
                    packet = read_datagram(&mut reader) => packet,
                };
                let Ok(bytes) = packet else {
                    break;
                };
                tokio::select! {
                    biased;
                    _ = read_closed.changed() => break,
                    sent = inbound_tx.send((peer, bytes)) => if sent.is_err() { break; },
                }
            }
            let _ = read_state.closed.send(true);
        });
        let write_state = Arc::clone(&state);
        let writing = tokio::spawn(async move {
            let _ = write_loop(writer, &write_state).await;
            let _ = write_state.closed.send(true);
        });
        Self {
            peer,
            state,
            inbound: Some(inbound_rx),
            tasks: [reading, writing],
        }
    }

    pub fn take_inbound(&mut self) -> Option<mpsc::Receiver<Inbound>> {
        self.inbound.take()
    }
    pub fn stats(&self) -> Arc<FramedStats> {
        Arc::clone(&self.state.stats)
    }
    pub fn is_closed(&self) -> bool {
        *self.state.closed.borrow()
    }

    pub fn closed_signal(&self) -> watch::Receiver<bool> {
        self.state.closed.subscribe()
    }

    pub fn close_guard(&self) -> CloseGuard {
        CloseGuard(self.state.closed.clone())
    }

    /// Waits until every control datagram queued so far has been written to
    /// the stream, for at most `timeout`. Dropping the link aborts its writer,
    /// so a last message must be flushed first.
    pub async fn flush(&self, timeout: Duration) -> io::Result<()> {
        let queued = self.state.queue.lock().controls_queued;
        let mut written = self.state.controls_written.subscribe();
        let mut closed = self.state.closed.subscribe();
        let flushed = async {
            loop {
                if *written.borrow_and_update() >= queued {
                    return Ok(());
                }
                if *closed.borrow_and_update() {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "USB tunnel closed",
                    ));
                }
                tokio::select! {
                    _ = written.changed() => {}
                    _ = closed.changed() => {}
                }
            }
        };
        tokio::time::timeout(timeout, flushed)
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "USB tunnel did not take the message",
                ))
            })
    }
}

/// A device worker owns this guard after handing the link to the transport.
/// Removing the device or stopping its supervisor closes both tunnel halves.
pub struct CloseGuard(watch::Sender<bool>);
impl Drop for CloseGuard {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

impl Link for FramedLink {
    fn id(&self) -> LinkId {
        self.peer.link
    }

    async fn receive(&mut self) -> io::Result<Inbound> {
        self.inbound
            .as_mut()
            .ok_or_else(|| invalid("inbound stream already taken"))?
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "USB tunnel closed"))
    }

    async fn send(&self, peer: PeerId, datagram: &[u8]) -> io::Result<()> {
        if peer != self.peer {
            return Err(invalid("USB destination"));
        }
        if self.is_closed() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "USB tunnel closed",
            ));
        }
        check_length(datagram.len())?;
        {
            let mut queue = self.state.queue.lock();
            if matches!(classify(datagram), Classified::Media { .. }) {
                let now = Instant::now();
                let dropped = queue.video.push(datagram, now)?;
                self.state.record_drops(&mut queue.video, dropped, now);
            } else if matches!(classify(datagram), Classified::Audio { .. }) {
                let dropped = queue.audio.push(datagram, Instant::now())?;
                self.state
                    .stats
                    .audio_packets_dropped
                    .fetch_add(dropped, Ordering::Relaxed);
            } else {
                if queue.controls.len() >= CONTROL_LIMIT {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "USB control queue full",
                    ));
                }
                queue.controls.push_back(datagram.to_vec());
                queue.controls_queued += 1;
            }
        }
        self.state.ready.notify_one();
        Ok(())
    }
}

impl Drop for FramedLink {
    fn drop(&mut self) {
        let _ = self.state.closed.send(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn write_loop(mut writer: impl AsyncWrite + Unpin, state: &WriterState) -> io::Result<()> {
    let mut closed = state.closed.subscribe();
    loop {
        if *closed.borrow() {
            return Ok(());
        }
        let (priority, frame) = match state.take_priority() {
            Some(packet) => (Some(packet), None),
            None => (None, state.queue.lock().video.pop()),
        };
        if let Some((packet, control)) = priority {
            tokio::select! {
                biased;
                _ = closed.changed() => return Ok(()),
                sent = write_datagram(&mut writer, &packet) => sent?,
            }
            state.wrote(control);
        } else if let Some(frame) = frame {
            // A frame that has started is finished: the iPad cannot use part
            // of one, and over a lossless tunnel would count the rest as lost.
            for packet in frame.datagrams {
                tokio::select! {
                    biased;
                    _ = closed.changed() => return Ok(()),
                    sent = write_datagram(&mut writer, &packet) => sent?,
                }
                // Audio and control can pass between video datagrams, without
                // ever interrupting a partly written TCP frame prefix/body.
                while let Some((priority, control)) = state.take_priority() {
                    tokio::select! {
                        biased;
                        _ = closed.changed() => return Ok(()),
                        sent = write_datagram(&mut writer, &priority) => sent?,
                    }
                    state.wrote(control);
                }
            }
        } else {
            // A deferred keyframe request must not wait for the next frame:
            // a still desktop may not send one.
            let deferred = state.queue.lock().video.keyframes.deadline();
            tokio::select! {
                biased;
                _ = closed.changed() => return Ok(()),
                _ = state.ready.notified() => {},
                _ = tokio::time::sleep_until(deferred.unwrap_or_else(Instant::now)), if deferred.is_some() => {
                    let mut queue = state.queue.lock();
                    state.record_drops(&mut queue.video, 0, Instant::now());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::control::{encode_control, ControlMessage, Ping};
    use eternal_wire::v2::media::MEDIA_HEADER_SIZE;

    #[test]
    fn audio_queue_drops_old_packets_and_marks_recovery_without_growing_latency() {
        let mut queue = AudioQueue::default();
        let now = Instant::now();
        let packet = |seq| {
            AudioHeader {
                session_id: 1,
                stream_epoch: 7,
                audio_seq: seq,
                capture_ts_us: 1000,
                discontinuity: false,
            }
            .encode(&[0xF8, 0xFF, 0xFE])
            .unwrap()
        };
        for seq in 0..4 {
            assert_eq!(queue.push(&packet(seq), now).unwrap(), u64::from(seq == 3));
        }
        assert_eq!(queue.packets.len(), 3);
        let (bytes, dropped) = queue.pop(now);
        assert_eq!(dropped, 0);
        let (header, _) = AudioHeader::decode(bytes.as_ref().unwrap()).unwrap();
        assert_eq!(header.audio_seq, 1);
        assert!(header.discontinuity);
        let later = now + Duration::from_millis(101);
        assert_eq!(queue.pop(later), (None, 2));
        queue.push(&packet(4), later).unwrap();
        let (bytes, dropped) = queue.pop(later);
        assert_eq!(dropped, 0);
        assert!(
            AudioHeader::decode(bytes.as_ref().unwrap())
                .unwrap()
                .0
                .discontinuity
        );
        assert!(queue.push(&[0; 12], now).is_err());
    }

    fn packet(seq: u32, keyframe: bool, index: u16, count: u16, size: usize) -> Vec<u8> {
        let mut bytes = vec![0xAC; size];
        MediaHeader {
            session_id: 1,
            stream_epoch: 1,
            frame_seq: seq,
            frag_index: index,
            frag_count: count,
            is_keyframe: keyframe,
            is_retransmit: false,
            capture_ts_us: 1,
            payload_len: (size - MEDIA_HEADER_SIZE) as u16,
        }
        .encode_into(&mut bytes);
        bytes
    }

    #[test]
    fn queue_holds_whole_frames_and_discards_dependencies_until_a_new_keyframe() {
        let now = Instant::now();
        let mut queue = VideoQueue::default();
        assert_eq!(queue.push(&packet(1, true, 0, 2, 64), now).unwrap(), 0);
        assert!(
            queue.pop().is_none(),
            "partial frames cannot reach the writer"
        );
        queue.push(&packet(1, true, 1, 2, 64), now).unwrap();
        for seq in 2..=FRAME_LIMIT as u32 {
            assert_eq!(queue.push(&packet(seq, false, 0, 1, 64), now).unwrap(), 0);
        }
        assert_eq!(queue.frames.len(), FRAME_LIMIT);
        // The newest frame is dropped, with its fragments and dependents.
        let next = FRAME_LIMIT as u32 + 1;
        assert_eq!(queue.push(&packet(next, false, 0, 2, 64), now).unwrap(), 1);
        assert_eq!(queue.push(&packet(next, false, 1, 2, 64), now).unwrap(), 0);
        assert_eq!(
            queue.push(&packet(next + 1, false, 0, 1, 64), now).unwrap(),
            1
        );
        assert_eq!(queue.frames.len(), FRAME_LIMIT, "queued frames stay");
        assert_eq!(queue.pop().unwrap().seq, 1);
        assert_eq!(queue.pop().unwrap().seq, 2);
        // A keyframe goes out after the frames still queued.
        assert_eq!(
            queue
                .push(
                    &packet(next + 2, true, 0, 1, 64),
                    now + Duration::from_secs(1)
                )
                .unwrap(),
            0
        );
        for seq in 3..=FRAME_LIMIT as u32 {
            assert_eq!(queue.pop().unwrap().seq, seq);
        }
        assert_eq!(queue.pop().unwrap().seq, next + 2);
        assert!(queue.pop().is_none());
        assert_eq!(queue.bytes, 0);
    }

    #[test]
    fn queue_limits_bytes_even_when_a_single_keyframe_is_too_large() {
        let now = Instant::now();
        let mut queue = VideoQueue::default();
        let mut dropped = 0;
        for index in 0..1000 {
            dropped += queue
                .push(&packet(1, true, index, 1000, MAX_DGRAM_SIZE), now)
                .unwrap();
            assert!(queue.bytes <= BYTE_LIMIT);
            assert!(queue.frames.len() <= FRAME_LIMIT);
        }
        assert_eq!(dropped, 1);
        assert_eq!(queue.bytes, 0);
        queue.push(&packet(2, true, 0, 1, 64), now).unwrap();
        assert_eq!(queue.pop().unwrap().seq, 2);
    }

    fn packet_in(stream: (u32, u32), seq: u32, keyframe: bool) -> Vec<u8> {
        let mut bytes = packet(seq, keyframe, 0, 1, 64);
        MediaHeader {
            session_id: stream.0,
            stream_epoch: stream.1,
            ..MediaHeader::decode(&bytes).unwrap().0
        }
        .encode_into(&mut bytes);
        bytes
    }

    /// A keyframe that would be cut short by the queue's bounds replaces
    /// what is queued, however briefly it waited. Its own bytes decide, not
    /// its fragment count.
    #[test]
    fn a_keyframe_replaces_queued_frames_it_does_not_fit_behind() {
        let now = Instant::now();
        let mut queue = VideoQueue::default();
        queue.push(&packet(1, true, 0, 1, 64), now).unwrap();
        queue.pop().unwrap();
        for seq in 2..=FRAME_LIMIT as u32 + 1 {
            assert_eq!(queue.push(&packet(seq, false, 0, 1, 64), now).unwrap(), 0);
        }
        let next = FRAME_LIMIT as u32 + 2;
        assert_eq!(
            queue.push(&packet(next, true, 0, 1, 64), now).unwrap(),
            FRAME_LIMIT as u64,
            "no room for another frame"
        );
        assert_eq!(queue.pop().unwrap().seq, next);
        // Full datagrams, but for a short last one, that just fit beside a
        // small frame, although their count times a full datagram does not.
        let full = BYTE_LIMIT / MAX_DGRAM_SIZE;
        let keyframe = |queue: &mut VideoQueue, seq, last| {
            let count = full as u16 + 1;
            let mut dropped = 0;
            for index in 0..count {
                let size = if index + 1 == count {
                    last
                } else {
                    MAX_DGRAM_SIZE
                };
                dropped += queue
                    .push(&packet(seq, true, index, count, size), now)
                    .unwrap();
            }
            dropped
        };
        queue.push(&packet(next + 1, false, 0, 1, 64), now).unwrap();
        let room = BYTE_LIMIT - full * MAX_DGRAM_SIZE - 64;
        assert_eq!(keyframe(&mut queue, next + 2, room), 0, "it fits");
        assert_eq!(queue.frames.len(), 2);
        assert_eq!(queue.bytes, BYTE_LIMIT);
        assert_eq!(queue.pop().unwrap().seq, next + 1);
        assert_eq!(queue.pop().unwrap().seq, next + 2);
        queue.push(&packet(next + 3, false, 0, 1, 64), now).unwrap();
        assert_eq!(
            keyframe(&mut queue, next + 4, room + 1),
            1,
            "the keyframe does not fit in the bytes left"
        );
        assert_eq!(queue.frames.len(), 1);
        assert_eq!(queue.pop().unwrap().seq, next + 4);
        assert_eq!(queue.bytes, 0);
    }

    /// How long a frame waited, and how closely frames arrived, say nothing
    /// about the tunnel: one large frame can take a second to write through
    /// a slow cable that still moves. A keyframe that fits goes out after
    /// the frames queued before it.
    #[test]
    fn a_keyframe_keeps_queued_frames_however_long_they_waited() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut queue = VideoQueue::default();
        queue.push(&packet(1, true, 0, 1, 64), start).unwrap();
        queue.pop().unwrap();
        queue.push(&packet(2, false, 0, 1, 64), at(65)).unwrap();
        queue.push(&packet(3, false, 0, 1, 64), at(66)).unwrap();
        assert_eq!(queue.push(&packet(4, true, 0, 1, 64), at(1066)).unwrap(), 0);
        for seq in 2..=4 {
            assert_eq!(queue.pop().unwrap().seq, seq);
        }
    }

    /// A pipeline restart keeps the USB tunnel. Its new stream must not
    /// inherit the old stream's backoff: at the longest wait, the new
    /// stream's first request was deferred by almost two seconds.
    #[test]
    fn a_new_stream_on_the_same_tunnel_asks_for_a_keyframe_at_once() {
        let start = Instant::now();
        let mut queue = VideoQueue::default();
        let overflow = |queue: &mut VideoQueue, stream, first: u32, now| {
            queue.push(&packet_in(stream, first, true), now).unwrap();
            for seq in first + 1..=first + FRAME_LIMIT as u32 + 1 {
                queue.push(&packet_in(stream, seq, false), now).unwrap();
            }
            queue.keyframes.due(now)
        };
        // Overflow every 250 ms until the backoff reaches its longest wait.
        let mut now = start;
        let mut requests = Vec::new();
        for round in 0..8 {
            if overflow(&mut queue, (1, 1), round * 10 + 1, now) {
                requests.push(now - start);
            }
            now += Duration::from_millis(250);
        }
        let ms = Duration::from_millis;
        assert_eq!(requests, [ms(0), ms(250), ms(750), ms(1750)]);
        assert!(!overflow(&mut queue, (1, 1), 100, now));
        assert_eq!(
            queue.keyframes.deadline(),
            Some(start + ms(1750) + KEYFRAME_BACKOFF_MAX)
        );
        assert!(
            overflow(&mut queue, (1, 2), 1, now),
            "a new epoch asks at once"
        );
        now += Duration::from_millis(50);
        assert!(
            !overflow(&mut queue, (1, 2), 20, now),
            "then backs off again"
        );
        assert!(
            overflow(&mut queue, (2, 2), 1, now),
            "and so does a new session"
        );
    }

    #[test]
    fn keyframe_requests_back_off_while_frames_keep_being_lost() {
        let start = Instant::now();
        let mut backoff = KeyframeBackoff::default();
        let mut requests = Vec::new();
        // A frame is lost every 50 ms; a keyframe arrives after each request.
        for tick in 0..100u64 {
            let now = start + Duration::from_millis(50 * tick);
            backoff.lost(now);
            if backoff.due(now) {
                requests.push(50 * tick);
                backoff.arrived();
            }
        }
        assert_eq!(requests, [0, 250, 750, 1750, 3750]);
        // A link that stays clean asks at once again.
        let later = start + Duration::from_millis(4950 + 5000);
        backoff.lost(later);
        assert!(backoff.due(later));
        // A deferred request names when it falls due, and a keyframe that
        // arrives first cancels it.
        backoff.lost(later + Duration::from_millis(10));
        assert!(!backoff.due(later + Duration::from_millis(10)));
        assert_eq!(backoff.deadline(), Some(later + KEYFRAME_BACKOFF_MIN));
        backoff.arrived();
        assert_eq!(backoff.deadline(), None);
        assert!(!backoff.due(later + Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn framing_handles_partial_reads_and_rejects_bad_preambles_and_lengths() {
        let (mut writer, mut reader) = tokio::io::duplex(3);
        let sending = tokio::spawn(async move {
            writer.write_all(PREAMBLE).await.unwrap();
            write_datagram(&mut writer, &[0xCA; MAX_DGRAM_SIZE])
                .await
                .unwrap();
            write_datagram(&mut writer, &[0xFE]).await.unwrap();
        });
        read_preamble(&mut reader).await.unwrap();
        assert_eq!(
            read_datagram(&mut reader).await.unwrap(),
            [0xCA; MAX_DGRAM_SIZE]
        );
        assert_eq!(read_datagram(&mut reader).await.unwrap(), [0xFE]);
        sending.await.unwrap();
        for preamble in [b"EMLINK\x02\x00", b"EMLINK\x01\x01", b"BADBAD!!"] {
            assert_eq!(
                read_preamble(&mut preamble.as_slice())
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
        for length in [0u16, 1401, u16::MAX] {
            assert_eq!(
                read_datagram(&mut length.to_le_bytes().as_slice())
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
        assert!(read_datagram(&mut [4, 0, 1, 2].as_slice()).await.is_err());
        assert!(write_datagram(&mut tokio::io::sink(), &[]).await.is_err());
        assert!(write_datagram(&mut tokio::io::sink(), &[0; 1401])
            .await
            .is_err());
    }

    #[tokio::test]
    async fn silent_listener_peer_times_out_and_dropping_the_link_closes_io() {
        let (server, _silent) = tokio::io::duplex(32);
        let failed =
            FramedLink::accept(server, PeerId::usb(7), Arc::new(AtomicBool::new(false))).await;
        assert!(matches!(failed, Err(e) if e.kind() == io::ErrorKind::TimedOut));
        let (host, mut client) = tokio::io::duplex(32);
        let link = FramedLink::connect(host, PeerId::usb(7), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        read_preamble(&mut client).await.unwrap();
        drop(link);
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), client.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }

    /// The frame being written is finished however many newer frames
    /// overflow behind it. Cutting it short sent the iPad a partial keyframe
    /// over a lossless cable: lost fragments in its report, and another
    /// keyframe request.
    #[tokio::test]
    async fn bounded_writer_finishes_the_frame_in_flight_and_recovers_with_a_keyframe() {
        let (host, mut client) = tokio::io::duplex(64);
        let force_idr = Arc::new(AtomicBool::new(false));
        let peer = PeerId::usb(7);
        let mut link = FramedLink::connect(host, peer, Arc::clone(&force_idr))
            .await
            .unwrap();
        read_preamble(&mut client).await.unwrap();
        // Stall partway through a packet while newer complete frames overflow.
        for index in 0..4 {
            link.send(peer, &packet(1, true, index, 4, 1400))
                .await
                .unwrap();
        }
        tokio::task::yield_now().await;
        for seq in 2..10 {
            link.send(peer, &packet(seq, false, 0, 1, 1400))
                .await
                .unwrap();
        }
        assert!(link.stats().frames_dropped.load(Ordering::Relaxed) > 0);
        assert!(force_idr.load(Ordering::SeqCst));
        link.send(peer, &packet(10, true, 0, 1, 100)).await.unwrap();
        for index in 0..4 {
            let datagram =
                tokio::time::timeout(Duration::from_millis(100), read_datagram(&mut client))
                    .await
                    .unwrap()
                    .unwrap();
            let header = MediaHeader::decode(&datagram).unwrap().0;
            assert_eq!((header.frame_seq, header.frag_index), (1, index));
        }
        let recovery = tokio::time::timeout(Duration::from_millis(100), read_datagram(&mut client))
            .await
            .unwrap()
            .unwrap();
        let header = MediaHeader::decode(&recovery).unwrap().0;
        assert_eq!(header.frame_seq, 10);
        assert!(header.is_keyframe);
        let ping = encode_control(1, 1, &ControlMessage::Ping(Ping { t1_us: 123 }));
        write_datagram(&mut client, &ping).await.unwrap();
        let (source, inbound) = link.receive().await.unwrap();
        assert_eq!(source, peer);
        assert_eq!(inbound, ping);
        link.send(peer, &ping).await.unwrap();
        assert_eq!(read_datagram(&mut client).await.unwrap(), ping);
    }

    /// A link that overflows again right after a keyframe waits before it
    /// asks for the next one, and still asks once the wait is over, even if
    /// no other frame comes.
    #[tokio::test]
    async fn overflow_right_after_a_keyframe_defers_the_next_request() {
        let (host, mut client) = tokio::io::duplex(64);
        let force_idr = Arc::new(AtomicBool::new(false));
        let peer = PeerId::usb(7);
        let link = FramedLink::connect(host, peer, Arc::clone(&force_idr))
            .await
            .unwrap();
        read_preamble(&mut client).await.unwrap();
        let overflow = |first: u32| (first..first + 10).map(|seq| packet(seq, false, 0, 1, 1400));
        link.send(peer, &packet(1, true, 0, 1, 1400)).await.unwrap();
        for datagram in overflow(2) {
            link.send(peer, &datagram).await.unwrap();
        }
        assert!(
            force_idr.swap(false, Ordering::SeqCst),
            "the first loss asks at once"
        );
        link.send(peer, &packet(20, true, 0, 1, 1400))
            .await
            .unwrap();
        for datagram in overflow(21) {
            link.send(peer, &datagram).await.unwrap();
        }
        assert!(
            !force_idr.load(Ordering::SeqCst),
            "a keyframe that overflowed again must not be requested at once"
        );
        // Let the writer drain, then wait out the backoff with no new frames.
        let mut drained = 0;
        while let Ok(Ok(_)) =
            tokio::time::timeout(Duration::from_millis(50), read_datagram(&mut client)).await
        {
            drained += 1;
        }
        assert!(drained > 0);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !force_idr.load(Ordering::SeqCst) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            force_idr.load(Ordering::SeqCst),
            "the deferred request is sent"
        );
    }

    /// Sends frame 1 in four full datagrams to a link whose peer has not
    /// read yet, so that the writer is left in the middle of it.
    async fn link_writing_frame_one() -> (FramedLink, tokio::io::DuplexStream, PeerId) {
        let (host, mut client) = tokio::io::duplex(4096);
        let peer = PeerId::usb(7);
        let link = FramedLink::connect(host, peer, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        read_preamble(&mut client).await.unwrap();
        for index in 0..4 {
            link.send(peer, &packet(1, true, index, 4, 1400))
                .await
                .unwrap();
        }
        tokio::task::yield_now().await;
        assert!(link.state.queue.lock().video.frames.is_empty());
        (link, client, peer)
    }

    /// A frame in flight says nothing about the tunnel. On a healthy cable
    /// the sender can push a frame and a keyframe while the writer is still
    /// on the frame before; every picture goes out, and nothing counts as
    /// congestion.
    #[tokio::test(start_paused = true)]
    async fn a_keyframe_behind_a_busy_writer_keeps_frames_that_did_not_wait() {
        let (link, mut client, peer) = link_writing_frame_one().await;
        link.send(peer, &packet(2, false, 0, 1, 1400))
            .await
            .unwrap();
        link.send(peer, &packet(3, true, 0, 1, 1400)).await.unwrap();
        let mut delivered = Vec::new();
        for _ in 0..6 {
            let datagram = read_datagram(&mut client).await.unwrap();
            let header = MediaHeader::decode(&datagram).unwrap().0;
            if header.frag_index == 0 {
                delivered.push(header.frame_seq);
            }
        }
        assert_eq!(delivered, [1, 2, 3]);
        assert_eq!(link.stats().frames_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(link.stats().congestion_drops.load(Ordering::Relaxed), 0);
    }

    /// A tunnel that stands still says nothing about what is queued: on a
    /// still desktop nothing newer may come. A frame and a keyframe that
    /// arrive while a write has stood still for 250 ms go out after it, and
    /// nothing counts as lost or asks for a keyframe. Dropping them when the
    /// write finished left the iPad with no picture at all.
    #[tokio::test(start_paused = true)]
    async fn a_tunnel_that_stands_still_loses_nothing_it_queued() {
        let (link, mut client, peer) = link_writing_frame_one().await;
        tokio::time::sleep(Duration::from_millis(120)).await;
        link.send(peer, &packet(2, false, 0, 1, 1400))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(130)).await;
        link.send(peer, &packet(3, true, 0, 1, 1400)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut delivered = Vec::new();
        while let Ok(datagram) =
            tokio::time::timeout(Duration::from_secs(1), read_datagram(&mut client)).await
        {
            let header = MediaHeader::decode(&datagram.unwrap()).unwrap().0;
            if header.frag_index + 1 == header.frag_count {
                delivered.push(header.frame_seq);
            }
        }
        assert_eq!(delivered, [1, 2, 3]);
        assert_eq!(link.stats().frames_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(link.stats().congestion_drops.load(Ordering::Relaxed), 0);
        assert!(!link.state.force_idr.load(Ordering::SeqCst));
    }

    /// A last message is written before the link goes, and a peer that
    /// stopped reading holds that up only for the given time.
    #[tokio::test]
    async fn flush_waits_for_queued_controls_but_not_forever() {
        let (host, mut client) = tokio::io::duplex(64);
        let peer = PeerId::usb(7);
        let link = FramedLink::connect(host, peer, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        read_preamble(&mut client).await.unwrap();
        let ping = encode_control(1, 1, &ControlMessage::Ping(Ping { t1_us: 123 }));
        link.send(peer, &ping).await.unwrap();
        link.flush(Duration::from_millis(250)).await.unwrap();
        drop(link);
        assert_eq!(read_datagram(&mut client).await.unwrap(), ping);

        let (host, mut stalled) = tokio::io::duplex(64);
        let link = FramedLink::connect(host, peer, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        read_preamble(&mut stalled).await.unwrap();
        for _ in 0..4 {
            link.send(peer, &ping).await.unwrap();
        }
        let start = Instant::now();
        let flushed = link.flush(Duration::from_millis(100)).await;
        assert!(matches!(flushed, Err(e) if e.kind() == io::ErrorKind::TimedOut));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn udp_link_preserves_the_source_and_routes_replies() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let mut link = UdpLink::new(socket);
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"packet", address).await.unwrap();
        let (peer, bytes) = link.receive().await.unwrap();
        assert_eq!(peer, PeerId::udp(client.local_addr().unwrap()));
        link.send(peer, &bytes).await.unwrap();
        let mut buffer = [0; 32];
        let (len, source) = client.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..len], b"packet");
        assert_eq!(source, address);
        assert!(link
            .send(PeerId::usb(7), b"invalid destination")
            .await
            .is_err());
    }
}
