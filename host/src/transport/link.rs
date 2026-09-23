//! Datagram links. TCP tunnels preserve packet boundaries with EMLINK framing
//! and discard queued video when a reader falls behind.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified, MAX_DGRAM_SIZE};
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::JoinHandle;

pub const PREAMBLE: &[u8; 8] = b"EMLINK\x01\x00";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const FRAME_LIMIT: usize = 2;
const BYTE_LIMIT: usize = 1024 * 1024;
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
    revision: u64,
    awaiting_keyframe: bool,
    discarded: Option<(u32, u32)>,
}

impl VideoQueue {
    fn clear_for_recovery(&mut self) -> u64 {
        let dropped = self.frames.len() as u64;
        self.frames.clear();
        self.bytes = 0;
        self.revision = self.revision.wrapping_add(1);
        self.awaiting_keyframe = true;
        dropped
    }

    /// Returns whole frames dropped. All dependent P frames are discarded
    /// until a new keyframe starts. Old queued keyframes can also be superseded
    /// so a stalled connection always makes room for the newest recovery frame.
    fn push(&mut self, bytes: &[u8]) -> io::Result<u64> {
        let (header, _) = MediaHeader::decode(bytes).map_err(|_| invalid("video header"))?;
        let key = (header.stream_epoch, header.frame_seq);
        if self.discarded == Some(key) {
            return Ok(0);
        }
        if self.awaiting_keyframe {
            if header.is_keyframe && header.frag_index == 0 {
                self.awaiting_keyframe = false;
            } else {
                self.discarded = Some(key);
                return Ok(1);
            }
        }

        let mut dropped = 0;
        if header.frag_index == 0 {
            if self.frames.back().is_some_and(|frame| !frame.complete()) {
                return Err(invalid("unfinished video access unit"));
            }
            if self.frames.len() >= FRAME_LIMIT || self.bytes + bytes.len() > BYTE_LIMIT {
                dropped += self.clear_for_recovery();
                if !header.is_keyframe {
                    self.discarded = Some(key);
                    return Ok(dropped + 1);
                }
                self.awaiting_keyframe = false;
            }
            self.frames.push_back(Frame {
                epoch: header.stream_epoch,
                seq: header.frame_seq,
                frag_count: header.frag_count,
                datagrams: Vec::with_capacity(usize::from(header.frag_count).min(1024)),
                bytes: 0,
            });
        }
        let Some(frame) = self.frames.back_mut() else {
            return Err(invalid("missing first fragment"));
        };
        if (frame.epoch, frame.seq) != key
            || frame.frag_count != header.frag_count
            || frame.datagrams.len() != usize::from(header.frag_index)
        {
            return Err(invalid("video fragment order"));
        }
        if self.bytes + bytes.len() > BYTE_LIMIT {
            dropped += self.clear_for_recovery();
            self.discarded = Some(key);
            return Ok(dropped);
        }
        frame.bytes += bytes.len();
        frame.datagrams.push(bytes.to_vec());
        self.bytes += bytes.len();
        Ok(dropped)
    }

    fn pop(&mut self) -> Option<(u64, Frame)> {
        if !self.frames.front().is_some_and(Frame::complete) {
            return None;
        }
        let frame = self.frames.pop_front().unwrap();
        self.bytes -= frame.bytes;
        Some((self.revision, frame))
    }
}

#[derive(Default)]
struct Outbound {
    controls: VecDeque<Vec<u8>>,
    video: VideoQueue,
}

#[derive(Default)]
pub struct FramedStats {
    pub frames_dropped: AtomicU64,
}

struct WriterState {
    queue: Mutex<Outbound>,
    ready: Notify,
    closed: watch::Sender<bool>,
    force_idr: Arc<AtomicBool>,
    stats: Arc<FramedStats>,
}

impl WriterState {
    fn record_drops(&self, dropped: u64) {
        if dropped > 0 {
            self.stats
                .frames_dropped
                .fetch_add(dropped, Ordering::Relaxed);
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
        let dropped = {
            let mut queue = self.state.queue.lock();
            if matches!(classify(datagram), Classified::Media { .. }) {
                queue.video.push(datagram)?
            } else {
                if queue.controls.len() >= CONTROL_LIMIT {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "USB control queue full",
                    ));
                }
                queue.controls.push_back(datagram.to_vec());
                0
            }
        };
        self.state.record_drops(dropped);
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
        let (control, frame) = {
            let mut queue = state.queue.lock();
            match queue.controls.pop_front() {
                Some(packet) => (Some(packet), None),
                None => (None, queue.video.pop()),
            }
        };
        if let Some(packet) = control {
            tokio::select! {
                biased;
                _ = closed.changed() => return Ok(()),
                sent = write_datagram(&mut writer, &packet) => sent?,
            }
        } else if let Some((revision, frame)) = frame {
            let count = frame.datagrams.len();
            for (index, packet) in frame.datagrams.into_iter().enumerate() {
                // Never cancel a partial packet and then reuse the byte stream.
                // Finish its framing before abandoning the rest of a stale frame.
                tokio::select! {
                    biased;
                    _ = closed.changed() => return Ok(()),
                    sent = write_datagram(&mut writer, &packet) => sent?,
                }
                if index + 1 < count && state.queue.lock().video.revision != revision {
                    state.record_drops(1);
                    break;
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = closed.changed() => return Ok(()),
                _ = state.ready.notified() => {},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::control::{encode_control, ControlMessage, Ping};
    use eternal_wire::v2::media::MEDIA_HEADER_SIZE;

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
        let mut queue = VideoQueue::default();
        assert_eq!(queue.push(&packet(1, true, 0, 2, 64)).unwrap(), 0);
        assert!(
            queue.pop().is_none(),
            "partial frames cannot reach the writer"
        );
        queue.push(&packet(1, true, 1, 2, 64)).unwrap();
        queue.push(&packet(2, false, 0, 1, 64)).unwrap();
        assert_eq!(queue.frames.len(), 2);
        assert_eq!(queue.push(&packet(3, false, 0, 2, 64)).unwrap(), 3);
        assert!(queue.frames.is_empty());
        assert_eq!(queue.bytes, 0);
        assert_eq!(queue.push(&packet(3, false, 1, 2, 64)).unwrap(), 0);
        assert_eq!(queue.push(&packet(4, false, 0, 1, 64)).unwrap(), 1);
        assert_eq!(queue.push(&packet(5, true, 0, 1, 64)).unwrap(), 0);
        assert_eq!(queue.pop().unwrap().1.seq, 5);
        assert_eq!(queue.bytes, 0);
    }

    #[test]
    fn queue_limits_bytes_even_when_a_single_keyframe_is_too_large() {
        let mut queue = VideoQueue::default();
        let mut dropped = 0;
        for index in 0..1000 {
            dropped += queue
                .push(&packet(1, true, index, 1000, MAX_DGRAM_SIZE))
                .unwrap();
            assert!(queue.bytes <= BYTE_LIMIT);
            assert!(queue.frames.len() <= FRAME_LIMIT);
        }
        assert_eq!(dropped, 1);
        assert_eq!(queue.bytes, 0);
        queue.push(&packet(2, true, 0, 1, 64)).unwrap();
        assert_eq!(queue.pop().unwrap().1.seq, 2);
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

    #[tokio::test]
    async fn bounded_writer_recovers_after_stall_without_breaking_partial_packet_framing() {
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
        let first = read_datagram(&mut client).await.unwrap();
        assert_eq!(MediaHeader::decode(&first).unwrap().0.frame_seq, 1);
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
