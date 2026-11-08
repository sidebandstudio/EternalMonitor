//! Device discovery and USB tunnel lifetime, independent of video pacing.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;

use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tracing::{debug, info, warn};

use super::apple_usb::{self, AppleUsbState};
use super::congestion::{is_no_buffer_space, SendCongestion};
use super::link::{CloseReport, FramedLink, Link, LinkId, PeerId, UdpLink};
use super::usbmuxd::{self, BoxedTunnel, Client};
use crate::stats::PIPELINE_STATS;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static HUB: OnceLock<UsbHub> = OnceLock::new();

pub enum LinkEvent {
    Datagram(PeerId, Vec<u8>),
    Closed(LinkId),
}

/// A device worker opened a tunnel; it closes when the worker drops its guard.
struct UsbEvent {
    generation: u64,
    link: FramedLink,
    opened: Opened,
}

/// How a tunnel came to be, for its connected line.
#[derive(Clone, Debug, Default)]
struct Opened {
    /// The service's Connect and the preamble.
    connect: Duration,
    /// Since the device was listed, or since its last tunnel closed.
    waited: Duration,
    /// Connects that failed in that time, and the last one's error.
    failed: u32,
    last_error: Option<String>,
}

impl Opened {
    fn failed_with(&mut self, error: String) {
        self.failed += 1;
        self.last_error = Some(error);
    }
}

/// What the hub reports to the pipeline generation that currently owns it.
enum HubEvent {
    Connected {
        generation: u64,
        link: Arc<FramedLink>,
        opened: Opened,
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

/// USB tunnels outlive pipeline generations. A tunnel used to belong to the
/// generation that opened it, so every pipeline restart (settings change,
/// watchdog, the restart that brings up the extended display) closed it. The
/// iPad saw "USB disconnected" and reconnected, and with Extended display
/// selected that reconnect triggered the next restart: USB extended display
/// could loop like this for hours without showing a picture. The hub keeps
/// device workers and tunnels on its own runtime, and each generation borrows
/// the live tunnels while it runs, as it already does with UDP sessions.
struct UsbHub {
    runtime: tokio::runtime::Handle,
    state: Arc<Mutex<HubState>>,
}

#[derive(Default)]
struct HubState {
    links: HashMap<LinkId, (u64, Arc<FramedLink>)>,
    subscriber: Option<(u64, mpsc::Sender<HubEvent>)>,
    next_token: u64,
    /// The device supervisor and what it was started with: the direct-test
    /// address and the IDR flag of the `SharedControl` it serves.
    supervisor: Option<(SourceKey, AbortHandle)>,
}

type SourceKey = (Option<String>, usize);

impl UsbHub {
    fn get() -> &'static UsbHub {
        HUB.get_or_init(|| {
            // Tunnel writers pace video, so they get the stream's scheduling
            // class, like the per-generation runtime that used to own them.
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            builder
                .worker_threads(2)
                .thread_name("usb-hub")
                .enable_all();
            #[cfg(target_os = "macos")]
            builder.on_thread_start(crate::capture::timing::set_stream_qos);
            let runtime = builder.build().expect("USB hub runtime");
            let handle = runtime.handle().clone();
            // The hub lives for the whole process.
            std::mem::forget(runtime);
            UsbHub {
                runtime: handle,
                state: Arc::new(Mutex::new(HubState::default())),
            }
        })
    }

    /// Makes the caller the owner of all tunnel events. Returns the tunnels
    /// that are already open, so the first heartbeat can reach them.
    fn subscribe(&self, force_idr: &Arc<AtomicBool>) -> Subscription {
        let (tx, rx) = mpsc::channel(256);
        let key: SourceKey = (direct_endpoint(), Arc::as_ptr(force_idr) as usize);
        let mut state = self.state.lock();
        if state
            .supervisor
            .as_ref()
            .is_none_or(|(running, _)| *running != key)
        {
            // First use, or tests switching the direct endpoint or the
            // SharedControl. Production starts the supervisor exactly once.
            if let Some((_, task)) = state.supervisor.take() {
                task.abort();
            }
            state.links.clear();
            let (event_tx, event_rx) = mpsc::channel(256);
            let supervisor = self
                .runtime
                .spawn(supervise(event_tx, Arc::clone(force_idr)));
            self.runtime
                .spawn(run_hub(event_rx, Arc::clone(&self.state)));
            state.supervisor = Some((key, supervisor.abort_handle()));
        }
        let open = state
            .links
            .iter()
            .map(|(id, (generation, link))| (*id, *generation, Arc::clone(link)))
            .collect();
        state.next_token += 1;
        let token = state.next_token;
        state.subscriber = Some((token, tx));
        Subscription {
            token,
            events: rx,
            open,
        }
    }

    fn unsubscribe(&self, token: u64) {
        let mut state = self.state.lock();
        if state
            .subscriber
            .as_ref()
            .is_some_and(|(owner, _)| *owner == token)
        {
            state.subscriber = None;
        }
    }
}

struct Subscription {
    token: u64,
    events: mpsc::Receiver<HubEvent>,
    open: Vec<(LinkId, u64, Arc<FramedLink>)>,
}

fn subscriber(state: &Mutex<HubState>) -> Option<mpsc::Sender<HubEvent>> {
    state.lock().subscriber.as_ref().map(|(_, tx)| tx.clone())
}

/// Owns tunnels for the hub: forwards their datagrams to whichever pipeline
/// generation is subscribed and drops them while none is (a restart gap).
async fn run_hub(mut events: mpsc::Receiver<UsbEvent>, state: Arc<Mutex<HubState>>) {
    while let Some(UsbEvent {
        generation,
        mut link,
        opened,
    }) = events.recv().await
    {
        let id = link.id();
        let mut inbound = link.take_inbound().expect("new USB link owns its reader");
        let link = Arc::new(link);
        state
            .lock()
            .links
            .insert(id, (generation, Arc::clone(&link)));
        if let Some(tx) = subscriber(&state) {
            let _ = tx
                .send(HubEvent::Connected {
                    generation,
                    link,
                    opened,
                })
                .await;
        }
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            while let Some((peer, bytes)) = inbound.recv().await {
                if let Some(tx) = subscriber(&state) {
                    let _ = tx
                        .send(HubEvent::Datagram {
                            generation,
                            peer,
                            bytes,
                        })
                        .await;
                }
            }
            close_link(&state, generation, id).await;
        });
    }
}

async fn close_link(state: &Mutex<HubState>, generation: u64, id: LinkId) {
    let removed = {
        let mut state = state.lock();
        let current = state.links.get(&id).is_some_and(|(g, _)| *g == generation);
        if current {
            state.links.remove(&id);
        }
        current
    };
    if removed {
        if let Some(tx) = subscriber(state) {
            let _ = tx.send(HubEvent::Closed { generation, id }).await;
        }
    }
}

struct ActiveLink {
    generation: u64,
    link: Arc<FramedLink>,
    reported_drops: u64,
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
    }
}

pub struct TransportLinks {
    pub udp: UdpLink,
    /// UDP sends the system had no buffer space for.
    pub congestion: Mutex<SendCongestion>,
    usb: HashMap<LinkId, ActiveLink>,
    events: mpsc::Receiver<HubEvent>,
    _subscription: HubToken,
}

/// The hub's events go to this owner until it is dropped.
struct HubToken(u64);

impl Drop for HubToken {
    fn drop(&mut self) {
        UsbHub::get().unsubscribe(self.0);
    }
}

impl TransportLinks {
    pub fn new(socket: UdpSocket, force_idr: Arc<AtomicBool>) -> Self {
        let subscription = UsbHub::get().subscribe(&force_idr);
        let mut links = Self {
            udp: UdpLink::new(socket),
            congestion: Mutex::default(),
            usb: HashMap::new(),
            events: subscription.events,
            _subscription: HubToken(subscription.token),
        };
        for (id, generation, link) in subscription.open {
            info!(?id, generation, "USB tunnel kept across pipeline restart");
            links.adopt(generation, link);
        }
        links
    }

    /// Tests: a link over a stream they hold, as if its cable connected.
    #[cfg(test)]
    pub(crate) fn adopt_for_test(&mut self, link: Arc<FramedLink>) {
        self.adopt(0, link);
    }

    fn adopt(&mut self, generation: u64, link: Arc<FramedLink>) {
        // Drops already counted by an earlier generation are not new.
        let reported_drops = link.stats().frames_dropped.load(Ordering::Relaxed);
        self.usb.insert(
            link.id(),
            ActiveLink {
                generation,
                link,
                reported_drops,
            },
        );
        self.update_stats();
    }

    pub async fn send_to(&self, bytes: &[u8], peer: PeerId) -> io::Result<()> {
        match peer.link {
            LinkId::Udp => {
                let sent = self.udp.send(peer, bytes).await;
                if let Err(error) = &sent {
                    if is_no_buffer_space(error) {
                        self.congestion.lock().refused(error, super::now());
                    }
                }
                sent
            }
            id => match self.usb.get(&id) {
                Some(active) => active.link.send(peer, bytes).await,
                None => Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "USB tunnel unavailable",
                )),
            },
        }
    }

    /// Waits, for at most `timeout`, until what was sent to `peer` has been
    /// written out. UDP sends are written when they return.
    pub async fn flush(&self, peer: PeerId, timeout: Duration) -> io::Result<()> {
        match peer.link {
            LinkId::Udp => Ok(()),
            id => match self.usb.get(&id) {
                Some(active) => active.link.flush(timeout).await,
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
                Some(HubEvent::Connected {
                    generation,
                    link,
                    opened,
                }) => {
                    info!(
                        id = ?link.id(),
                        generation,
                        connect_ms = opened.connect.as_millis() as u64,
                        waited_ms = opened.waited.as_millis() as u64,
                        failed = opened.failed,
                        last_error = opened.last_error.as_deref().unwrap_or("none"),
                        "USB tunnel connected"
                    );
                    self.adopt(generation, link);
                }
                Some(HubEvent::Datagram {
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
                Some(HubEvent::Closed { generation, id }) => {
                    if self
                        .usb
                        .get(&id)
                        .is_some_and(|active| active.generation == generation)
                    {
                        let report = self
                            .usb
                            .remove(&id)
                            .and_then(|active| active.link.close_report());
                        if self.usb.is_empty() {
                            PIPELINE_STATS.lock().usb_link_state =
                                "Waiting for the iPad app".into();
                        }
                        log_closed(id, generation, report);
                        return Ok(LinkEvent::Closed(id));
                    }
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "USB hub handed the tunnels to a newer pipeline",
                    ))
                }
            }
        }
    }

    /// Lets go of the USB tunnels and returns the UDP socket, for the next
    /// owner of the stream port.
    pub fn into_udp_socket(self) -> UdpSocket {
        self.udp.socket
    }
}

/// Which end closed the tunnel first, after how long, and how long the
/// iPad had been quiet: a timer shows as the same `open_ms` every time.
fn log_closed(id: LinkId, generation: u64, report: Option<CloseReport>) {
    let ms = |duration: Duration| duration.as_millis() as u64;
    info!(
        ?id,
        generation,
        cause = %report.map_or("unknown", |report| report.cause.as_str()),
        open_ms = report.map_or(0, |report| ms(report.open)),
        quiet_ms = report.map_or(0, |report| ms(report.quiet)),
        "USB tunnel closed"
    );
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
    let mut opened = Opened::default();
    let mut waiting_since = tokio::time::Instant::now();
    loop {
        let started = tokio::time::Instant::now();
        match source.connect().await {
            Ok(stream) => match FramedLink::connect(stream, peer, Arc::clone(&force_idr)).await {
                Ok(link) => {
                    let mut closed = link.closed_signal();
                    let _close_on_exit = link.close_guard();
                    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
                    opened.connect = started.elapsed();
                    opened.waited = started.saturating_duration_since(waiting_since);
                    let opened = std::mem::take(&mut opened);
                    if events
                        .send(UsbEvent {
                            generation,
                            link,
                            opened,
                        })
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
                    waiting_since = tokio::time::Instant::now();
                }
                Err(error) => {
                    debug!(%peer, %error, "USB framing handshake failed");
                    opened.failed_with(format!("framing handshake: {error}"));
                }
            },
            Err(error) => {
                debug!(%peer, %error, backoff, "USB app unavailable; retrying");
                opened.failed_with(error.to_string());
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff + 1).min(5);
    }
}

/// Which builds obey `ETERNAL_USB_DIRECT`: debug builds and builds with
/// the `usb-direct` feature. A function of the two flags, so a debug test
/// can check the release combination too.
const fn direct_allowed(debug_assertions: bool, usb_direct: bool) -> bool {
    debug_assertions || usb_direct
}

/// Decided at compile time; a release build drops the read below.
const DIRECT_ALLOWED: bool = direct_allowed(cfg!(debug_assertions), cfg!(feature = "usb-direct"));

/// `ETERNAL_USB_DIRECT`: a test's TCP endpoint in place of the USB tunnels,
/// where this build allows one.
fn direct_endpoint() -> Option<String> {
    direct_value(DIRECT_ALLOWED, || std::env::var("ETERNAL_USB_DIRECT").ok())
}

fn direct_value(allowed: bool, requested: impl FnOnce() -> Option<String>) -> Option<String> {
    if allowed {
        requested()
    } else {
        None
    }
}

async fn supervise(events: mpsc::Sender<UsbEvent>, force_idr: Arc<AtomicBool>) {
    if let Some(address) = direct_endpoint() {
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
    let mut unreachable = AppleUsbState::default();
    loop {
        tokio::select! {
            _ = poll.tick() => {
                let devices = client.list_devices().await;
                let available = devices.is_ok();
                let list_error = devices.as_ref().err().map(ToString::to_string);
                let devices = devices.unwrap_or_default();
                let ids: HashSet<_> = devices.iter().map(|device| device.device_id).collect();
                // Without the service the cable is invisible to us; ask
                // Windows whether an iPad is attached so the GUI can say why.
                let cabled = if available {
                    AppleUsbState::default()
                } else {
                    tokio::task::spawn_blocking(apple_usb::probe).await.unwrap_or_default()
                };
                if cabled != unreachable && cabled.cabled_devices > 0 {
                    if cabled.apple_devices_installed {
                        warn!("An iPad is plugged in over USB, but Apple Devices is not running. Open Apple Devices to stream over the cable");
                    } else {
                        warn!("An iPad is plugged in over USB, but Apple Devices is not installed. Get it from the Microsoft Store to stream over the cable");
                    }
                }
                unreachable = cabled;
                {
                    let mut stats = PIPELINE_STATS.lock();
                    stats.usb_service_reachable = available;
                    stats.usb_devices = devices.len();
                    stats.usb_cabled_devices = cabled.cabled_devices;
                    stats.apple_devices_installed = cabled.apple_devices_installed;
                    if !available { stats.usb_link_state = "Apple device service unavailable".into(); }
                    else if devices.is_empty() { stats.usb_link_state = "Waiting for an iPad".into(); }
                    else if stats.usb_link_state != "Connected" { stats.usb_link_state = "Waiting for the iPad app".into(); }
                }
                handles.retain(|id, task| {
                    if ids.contains(id) {
                        return !task.is_finished();
                    }
                    // Stopping the worker closes its tunnel, if it has one.
                    info!(
                        reason = %if available { "device_gone" } else { "list_failed" },
                        error = list_error.as_deref().unwrap_or("none"),
                        "USB device worker stopped"
                    );
                    task.abort();
                    false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_debug_and_usb_direct_builds_read_the_direct_endpoint() {
        // A release build: no debug assertions, no feature.
        assert!(!direct_allowed(false, false));
        assert!(direct_allowed(true, false));
        assert!(direct_allowed(false, true));
        assert!(direct_allowed(true, true));
        assert_eq!(
            DIRECT_ALLOWED,
            cfg!(debug_assertions) || cfg!(feature = "usb-direct")
        );
        // Not allowed: the variable is not even read.
        let requested = || -> Option<String> { panic!("read in a build that ignores it") };
        assert_eq!(direct_value(false, requested), None);
        assert_eq!(
            direct_value(true, || Some("127.0.0.1:0".into())).as_deref(),
            Some("127.0.0.1:0")
        );
        assert_eq!(direct_value(true, || None), None);
    }

    /// The close line says which end closed the tunnel first and when, so a
    /// daily check can tell a timer (the same `open_ms` every time) from an
    /// unplugged cable or Apple's service letting go.
    #[test]
    fn the_close_line_says_who_closed_the_tunnel_and_when() {
        use super::super::link::CloseCause;
        use tracing_subscriber::prelude::*;
        #[derive(Clone, Default)]
        struct Lines(Arc<Mutex<Vec<u8>>>);
        impl io::Write for Lines {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let lines = Lines::default();
        let writer = lines.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .without_time()
                .with_target(false)
                .with_writer(move || writer.clone()),
        );
        crate::logging::prepare_log_capture();
        tracing::subscriber::with_default(subscriber, || {
            let report = CloseReport {
                cause: CloseCause::ReadFailed(io::ErrorKind::ConnectionReset),
                open: Duration::from_millis(9_612),
                quiet: Duration::from_millis(4),
            };
            log_closed(PeerId::usb(3).link, 12, Some(report));
            log_closed(PeerId::usb(3).link, 13, None);
        });
        let text = String::from_utf8(lines.0.lock().clone()).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert_eq!(
            lines,
            [
                "INFO USB tunnel closed id=Usb { device_id: 3 } generation=12 \
                 cause=peer_reset open_ms=9612 quiet_ms=4",
                "INFO USB tunnel closed id=Usb { device_id: 3 } generation=13 \
                 cause=unknown open_ms=0 quiet_ms=0",
            ]
        );
    }

    /// The connected line says how long the worker waited for the tunnel and
    /// how many connects failed meanwhile: Apple's service refuses a Connect
    /// while the app isn't listening, and a tunnel the iPad closed starts
    /// the count again.
    #[tokio::test]
    async fn a_tunnel_says_how_it_was_opened() {
        let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(Client::new(usbmuxd::Endpoint::Tcp(
            service.local_addr().unwrap(),
        )));
        // Answers each Connect with `result`, and returns the connection.
        let answer = |result: u64| {
            let service = &service;
            async move {
                let (mut stream, _) = service.accept().await.unwrap();
                let (tag, _) = usbmuxd::read_message(&mut stream).await.unwrap();
                let reply = plist::Value::Dictionary(plist::Dictionary::from_iter([
                    ("MessageType".to_string(), plist::Value::from("Result")),
                    ("Number".to_string(), plist::Value::from(result)),
                ]));
                tokio::io::AsyncWriteExt::write_all(
                    &mut stream,
                    &usbmuxd::encode_message(tag, &reply).unwrap(),
                )
                .await
                .unwrap();
                stream
            }
        };
        let (events, mut opened) = mpsc::channel(4);
        let worker = tokio::spawn(device_worker(
            Source::Device(client, 3),
            events,
            Arc::new(AtomicBool::new(false)),
        ));
        answer(3).await;
        let mut ipad = answer(0).await;
        let first = opened.recv().await.unwrap();
        assert_eq!(first.opened.failed, 1, "{:?}", first.opened);
        assert_eq!(first.opened.last_error.as_deref(), Some("usbmuxd Result 3"));
        assert!(first.opened.waited >= Duration::from_secs(1));

        super::super::link::read_preamble(&mut ipad).await.unwrap();
        drop(ipad);
        let _ipad = answer(0).await;
        let second = opened.recv().await.unwrap();
        assert_eq!(
            first.link.close_report().map(|report| report.cause),
            Some(super::super::link::CloseCause::PeerEof)
        );
        assert_eq!(second.opened.failed, 0, "{:?}", second.opened);
        assert_eq!(second.opened.last_error, None);
        assert!(second.opened.waited >= Duration::from_secs(1));
        assert!(second.generation > first.generation);
        worker.abort();
    }

    /// `cargo test --release` runs this; other builds may use the endpoint.
    #[cfg(not(any(debug_assertions, feature = "usb-direct")))]
    #[test]
    fn release_builds_ignore_the_direct_endpoint() {
        std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:9");
        assert_eq!(direct_endpoint(), None);
        std::env::remove_var("ETERNAL_USB_DIRECT");
    }
}
