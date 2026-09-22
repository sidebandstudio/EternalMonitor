//! Device discovery and USB tunnel lifetime, independent of video pacing.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinHandle, JoinSet};
use tracing::{debug, info};

use super::link::{FramedLink, Link, LinkId, PeerId, UdpLink};
use super::usbmuxd::{self, BoxedTunnel, Client};
use crate::stats::PIPELINE_STATS;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

pub enum LinkEvent {
    Datagram(PeerId, Vec<u8>),
    Closed(LinkId),
}

enum UsbEvent {
    Connected {
        generation: u64,
        link: FramedLink,
    },
    Datagram {
        generation: u64,
        peer: PeerId,
        bytes: Vec<u8>,
    },
    Closed {
        generation: u64,
        id: LinkId,
    },
}

struct ActiveLink {
    generation: u64,
    link: FramedLink,
    reported_drops: u64,
    forwarding: JoinHandle<()>,
}

impl ActiveLink {
    fn report_drops(&mut self) {
        let total = self.link.stats().frames_dropped.load(Ordering::Relaxed);
        PIPELINE_STATS.lock().usb_frames_dropped += total.saturating_sub(self.reported_drops);
        self.reported_drops = total;
    }
}

impl Drop for ActiveLink {
    fn drop(&mut self) {
        self.report_drops();
        self.forwarding.abort();
    }
}

pub struct TransportLinks {
    pub udp: UdpLink,
    usb: HashMap<LinkId, ActiveLink>,
    events: mpsc::Receiver<UsbEvent>,
    event_tx: mpsc::Sender<UsbEvent>,
    supervisor: JoinHandle<()>,
}

impl TransportLinks {
    pub fn new(socket: UdpSocket, force_idr: Arc<AtomicBool>) -> Self {
        let (event_tx, events) = mpsc::channel(256);
        let supervisor_tx = event_tx.clone();
        let supervisor = tokio::spawn(supervise(supervisor_tx, force_idr));
        Self {
            udp: UdpLink::new(socket),
            usb: HashMap::new(),
            events,
            event_tx,
            supervisor,
        }
    }

    pub async fn send_to(&self, bytes: &[u8], peer: PeerId) -> io::Result<()> {
        match peer.link {
            LinkId::Udp => self.udp.send(peer, bytes).await,
            id => match self.usb.get(&id) {
                Some(active) => active.link.send(peer, bytes).await,
                None => Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "USB tunnel unavailable",
                )),
            },
        }
    }

    pub fn update_stats(&mut self) {
        for active in self.usb.values_mut() {
            active.report_drops();
        }
        if !self.usb.is_empty() {
            PIPELINE_STATS.lock().usb_link_state = "Connected".into();
        }
    }

    pub async fn receive(&mut self) -> io::Result<LinkEvent> {
        loop {
            let event = tokio::select! {
                received = self.udp.receive() => {
                    let (peer, bytes) = received?;
                    return Ok(LinkEvent::Datagram(peer, bytes));
                }
                event = self.events.recv() => event,
            };
            match event {
                Some(UsbEvent::Connected {
                    generation,
                    mut link,
                }) => {
                    let id = link.id();
                    let mut inbound = link.take_inbound().expect("new USB link owns its reader");
                    let events = self.event_tx.clone();
                    let forwarding = tokio::spawn(async move {
                        while let Some((peer, bytes)) = inbound.recv().await {
                            if events
                                .send(UsbEvent::Datagram {
                                    generation,
                                    peer,
                                    bytes,
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        let _ = events.send(UsbEvent::Closed { generation, id }).await;
                    });
                    self.usb.insert(
                        id,
                        ActiveLink {
                            generation,
                            link,
                            reported_drops: 0,
                            forwarding,
                        },
                    );
                    info!(?id, generation, "USB tunnel connected");
                    self.update_stats();
                }
                Some(UsbEvent::Datagram {
                    generation,
                    peer,
                    bytes,
                }) => {
                    if self
                        .usb
                        .get(&peer.link)
                        .is_some_and(|active| active.generation == generation)
                    {
                        return Ok(LinkEvent::Datagram(peer, bytes));
                    }
                }
                Some(UsbEvent::Closed { generation, id }) => {
                    if self
                        .usb
                        .get(&id)
                        .is_some_and(|active| active.generation == generation)
                    {
                        self.usb.remove(&id);
                        if self.usb.is_empty() {
                            PIPELINE_STATS.lock().usb_link_state =
                                "Waiting for the iPad app".into();
                        }
                        info!(?id, generation, "USB tunnel closed");
                        return Ok(LinkEvent::Closed(id));
                    }
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "USB supervisor stopped",
                    ))
                }
            }
        }
    }
}

impl Drop for TransportLinks {
    fn drop(&mut self) {
        self.supervisor.abort();
    }
}

#[derive(Clone)]
enum Source {
    Direct(String),
    Device(Arc<Client>, u32),
}

impl Source {
    fn peer(&self) -> PeerId {
        PeerId::usb(match self {
            Self::Direct(_) => 0,
            Self::Device(_, id) => *id,
        })
    }

    async fn connect(&self) -> Result<BoxedTunnel, usbmuxd::Error> {
        match self {
            Self::Device(client, id) => client.connect_device(*id, usbmuxd::APP_PORT).await,
            Self::Direct(address) => {
                let socket =
                    tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(address))
                        .await
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::TimedOut, "direct USB connect timeout")
                        })??;
                socket.set_nodelay(true)?;
                // A large kernel send queue would hide a stalled app and turn
                // queued video into latency before our frame limit can act.
                let _ = socket2::SockRef::from(&socket).set_send_buffer_size(32 * 1024);
                Ok(Box::new(socket))
            }
        }
    }
}

async fn device_worker(source: Source, events: mpsc::Sender<UsbEvent>, force_idr: Arc<AtomicBool>) {
    let peer = source.peer();
    let mut backoff = 1;
    loop {
        match source.connect().await {
            Ok(stream) => match FramedLink::connect(stream, peer, Arc::clone(&force_idr)).await {
                Ok(link) => {
                    let mut closed = link.closed_signal();
                    let _close_on_exit = link.close_guard();
                    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
                    if events
                        .send(UsbEvent::Connected { generation, link })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    loop {
                        if *closed.borrow() {
                            break;
                        }
                        if closed.changed().await.is_err() {
                            break;
                        }
                    }
                    backoff = 1;
                }
                Err(error) => debug!(%peer, %error, "USB framing handshake failed"),
            },
            Err(error) => debug!(%peer, %error, backoff, "USB app unavailable; retrying"),
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff + 1).min(5);
    }
}

async fn supervise(events: mpsc::Sender<UsbEvent>, force_idr: Arc<AtomicBool>) {
    if let Ok(address) = std::env::var("ETERNAL_USB_DIRECT") {
        {
            let mut stats = PIPELINE_STATS.lock();
            stats.usb_service_reachable = true;
            stats.usb_devices = 1;
            stats.usb_link_state = "Waiting for direct test listener".into();
        }
        device_worker(Source::Direct(address), events, force_idr).await;
        return;
    }

    let client = Arc::new(Client::new(usbmuxd::Endpoint::default()));
    let mut workers = JoinSet::new();
    let mut handles: HashMap<u32, AbortHandle> = HashMap::new();
    let mut poll = tokio::time::interval(Duration::from_secs(2));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = poll.tick() => {
                let devices = client.list_devices().await;
                let available = devices.is_ok();
                let devices = devices.unwrap_or_default();
                let ids: HashSet<_> = devices.iter().map(|device| device.device_id).collect();
                {
                    let mut stats = PIPELINE_STATS.lock();
                    stats.usb_service_reachable = available;
                    stats.usb_devices = devices.len();
                    if !available { stats.usb_link_state = "Apple device service unavailable".into(); }
                    else if devices.is_empty() { stats.usb_link_state = "Waiting for an iPad".into(); }
                    else if stats.usb_link_state != "Connected" { stats.usb_link_state = "Waiting for the iPad app".into(); }
                }
                handles.retain(|id, task| {
                    if !ids.contains(id) { task.abort(); false } else { !task.is_finished() }
                });
                for id in ids {
                    handles.entry(id).or_insert_with(|| workers.spawn(device_worker(
                        Source::Device(Arc::clone(&client), id), events.clone(), Arc::clone(&force_idr))));
                }
            }
            _ = workers.join_next(), if !workers.is_empty() => {},
        }
    }
}
