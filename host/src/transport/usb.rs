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
use super::link::{FramedLink, Link, LinkId, PeerId, UdpLink};
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
}

/// What the hub reports to the pipeline generation that currently owns it.
enum HubEvent {
    Connected {
        generation: u64,
        link: Arc<FramedLink>,
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
            let _ = tx.send(HubEvent::Connected { generation, link }).await;
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
                Some(HubEvent::Connected { generation, link }) => {
                    info!(id = ?link.id(), generation, "USB tunnel connected");
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
                    if events.send(UsbEvent { generation, link }).await.is_err() {
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
    supervise_devices(client, events, force_idr).await;
}

/// How long Apple's device list may keep failing before the service counts
/// as down and the devices' workers stop, closing their tunnels. A single
/// failed poll used to close every live tunnel, though the service and the
/// tunnels through it were fine.
const LIST_OUTAGE: Duration = Duration::from_secs(10);

/// Device-list polls that failed in a row, and when the first one did.
#[derive(Debug, Default)]
struct ListFailures {
    count: u32,
    since: Option<tokio::time::Instant>,
}

impl ListFailures {
    /// Whether a poll keeps the running workers, whatever it listed: its
    /// list failed, but the failures have not lasted [`LIST_OUTAGE`] yet.
    /// Only a list that answers can show a device gone.
    fn keep_workers(&mut self, listed: bool, workers: usize, now: tokio::time::Instant) -> bool {
        if listed {
            if self.count > 0 {
                info!(
                    failed_polls = self.count,
                    "Apple's device list answers again"
                );
            }
            *self = Self::default();
            return false;
        }
        self.count = self.count.saturating_add(1);
        let since = *self.since.get_or_insert(now);
        if workers == 0 {
            return false;
        }
        if now.saturating_duration_since(since) < LIST_OUTAGE {
            info!(
                failed_polls = self.count,
                "Apple's device list failed — keeping the USB tunnels"
            );
            return true;
        }
        info!(
            failed_polls = self.count,
            failing_ms = now.saturating_duration_since(since).as_millis() as u64,
            "Apple's device list keeps failing — closing the USB tunnels"
        );
        false
    }
}

async fn supervise_devices(
    client: Arc<Client>,
    events: mpsc::Sender<UsbEvent>,
    force_idr: Arc<AtomicBool>,
) {
    let mut workers = JoinSet::new();
    let mut handles: HashMap<u32, AbortHandle> = HashMap::new();
    let mut poll = tokio::time::interval(Duration::from_secs(2));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut unreachable = AppleUsbState::default();
    let mut failures = ListFailures::default();
    loop {
        tokio::select! {
            _ = poll.tick() => {
                let devices = client.list_devices().await;
                let available = devices.is_ok();
                let devices = devices.unwrap_or_default();
                let ids: HashSet<_> = if failures.keep_workers(available, handles.len(), tokio::time::Instant::now()) {
                    handles.keys().copied().collect()
                } else {
                    devices.iter().map(|device| device.device_id).collect()
                };
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

    /// `cargo test --release` runs this; other builds may use the endpoint.
    #[cfg(not(any(debug_assertions, feature = "usb-direct")))]
    #[test]
    fn release_builds_ignore_the_direct_endpoint() {
        std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:9");
        assert_eq!(direct_endpoint(), None);
        std::env::remove_var("ETERNAL_USB_DIRECT");
    }

    /// A failed device list keeps the workers that run, until the failures
    /// last [`LIST_OUTAGE`]: then the service counts as down and they stop,
    /// as every failed poll used to stop them. Each step is logged with the
    /// count of failed polls.
    #[test]
    fn failed_device_lists_keep_the_workers_for_a_while() {
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
        let start = tokio::time::Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        let mut failures = ListFailures::default();
        crate::logging::prepare_log_capture();
        tracing::subscriber::with_default(subscriber, || {
            // One failed poll between good ones.
            assert!(!failures.keep_workers(true, 1, at(0)));
            assert!(failures.keep_workers(false, 1, at(2)));
            assert!(!failures.keep_workers(true, 1, at(4)));
            // Failures in a row, until they have lasted the outage.
            for secs in [6, 8, 10, 12, 14] {
                assert!(failures.keep_workers(false, 1, at(secs)), "{secs} s");
            }
            assert!(!failures.keep_workers(false, 1, at(16)));
            // Nothing left to keep, nothing more to say.
            assert!(!failures.keep_workers(false, 0, at(18)));
            assert!(!failures.keep_workers(true, 0, at(20)));
            // Without workers a failure keeps nothing, but still counts.
            assert!(!failures.keep_workers(false, 0, at(22)));
            assert!(!failures.keep_workers(false, 1, at(32)));
        });
        let text = String::from_utf8(lines.0.lock().clone()).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let keeping = |n: u32| {
            format!("INFO Apple's device list failed — keeping the USB tunnels failed_polls={n}")
        };
        assert_eq!(
            lines,
            [
                keeping(1),
                "INFO Apple's device list answers again failed_polls=1".into(),
                keeping(1),
                keeping(2),
                keeping(3),
                keeping(4),
                keeping(5),
                "INFO Apple's device list keeps failing — closing the USB tunnels \
                 failed_polls=6 failing_ms=10000"
                    .into(),
                "INFO Apple's device list answers again failed_polls=7".into(),
                "INFO Apple's device list keeps failing — closing the USB tunnels \
                 failed_polls=2 failing_ms=10000"
                    .into(),
            ]
        );
    }

    /// Apple's device service whose list fails on demand: it drops the
    /// request unanswered. It lists one USB iPad otherwise, and hands every
    /// Connect's stream to the test as that iPad's end of a tunnel.
    struct FlakyService {
        failing: Arc<AtomicBool>,
        fail_next: Arc<std::sync::atomic::AtomicU32>,
        listed: Arc<AtomicU64>,
        failed: Arc<AtomicU64>,
        tunnels: mpsc::Receiver<tokio::net::TcpStream>,
        client: Arc<Client>,
    }

    impl FlakyService {
        async fn start() -> Self {
            use std::sync::atomic::AtomicU32;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = Arc::new(Client::new(usbmuxd::Endpoint::Tcp(
                listener.local_addr().unwrap(),
            )));
            let failing = Arc::new(AtomicBool::new(false));
            let fail_next = Arc::new(AtomicU32::new(0));
            let (listed, failed) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
            let (tunnel_tx, tunnels) = mpsc::channel(4);
            let state = (
                failing.clone(),
                fail_next.clone(),
                listed.clone(),
                failed.clone(),
            );
            tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (failing, fail_next, listed, failed) = state.clone();
                    let tunnel_tx = tunnel_tx.clone();
                    tokio::spawn(async move {
                        let (tag, request) = usbmuxd::read_message(&mut stream).await.unwrap();
                        let kind = request.as_dictionary().unwrap()["MessageType"]
                            .as_string()
                            .unwrap()
                            .to_string();
                        let reply = if kind == "ListDevices" {
                            let once = fail_next
                                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                                    n.checked_sub(1)
                                })
                                .is_ok();
                            if once || failing.load(Ordering::SeqCst) {
                                failed.fetch_add(1, Ordering::SeqCst);
                                return;
                            }
                            listed.fetch_add(1, Ordering::SeqCst);
                            let device = plist::Dictionary::from_iter([
                                ("DeviceID".to_string(), plist::Value::from(3u64)),
                                (
                                    "Properties".to_string(),
                                    plist::Value::Dictionary(plist::Dictionary::from_iter([(
                                        "ConnectionType".to_string(),
                                        plist::Value::from("USB"),
                                    )])),
                                ),
                            ]);
                            plist::Dictionary::from_iter([(
                                "DeviceList".to_string(),
                                plist::Value::Array(vec![plist::Value::Dictionary(device)]),
                            )])
                        } else {
                            plist::Dictionary::from_iter([
                                ("MessageType".to_string(), plist::Value::from("Result")),
                                ("Number".to_string(), plist::Value::from(0u64)),
                            ])
                        };
                        let bytes =
                            usbmuxd::encode_message(tag, &plist::Value::Dictionary(reply)).unwrap();
                        tokio::io::AsyncWriteExt::write_all(&mut stream, &bytes)
                            .await
                            .unwrap();
                        if kind == "Connect" {
                            let _ = tunnel_tx.send(stream).await;
                        }
                    });
                }
            });
            Self {
                failing,
                fail_next,
                listed,
                failed,
                tunnels,
                client,
            }
        }

        /// The iPad's end of the next tunnel, past its preamble.
        async fn tunnel(&mut self) -> tokio::net::TcpStream {
            let mut ipad = tokio::time::timeout(Duration::from_secs(20), self.tunnels.recv())
                .await
                .expect("a tunnel")
                .unwrap();
            super::super::link::read_preamble(&mut ipad).await.unwrap();
            ipad
        }

        async fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
            tokio::time::timeout(Duration::from_secs(30), async {
                while !done(self) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{what}"));
        }
    }

    /// A device list that fails once leaves the live tunnel open; one that
    /// keeps failing closes it after [`LIST_OUTAGE`], and once the list
    /// answers again the iPad gets a new tunnel. A single failed list used
    /// to close the tunnel at once.
    #[tokio::test]
    async fn a_failed_device_list_keeps_the_tunnel_until_the_service_is_down() {
        let mut service = FlakyService::start().await;
        let (events, mut opened) = mpsc::channel(4);
        let supervisor = tokio::spawn(supervise_devices(
            Arc::clone(&service.client),
            events,
            Arc::new(AtomicBool::new(false)),
        ));
        let _ipad = service.tunnel().await;
        let first = opened.recv().await.unwrap();

        service.fail_next.store(1, Ordering::SeqCst);
        let listed = service.listed.load(Ordering::SeqCst);
        service
            .until("a failed list, then a good one", |service| {
                service.failed.load(Ordering::SeqCst) == 1
                    && service.listed.load(Ordering::SeqCst) > listed + 1
            })
            .await;
        assert!(!first.link.is_closed(), "one failed list closed the tunnel");
        assert!(service.tunnels.try_recv().is_err(), "a second tunnel");

        service.failing.store(true, Ordering::SeqCst);
        let failing_from = tokio::time::Instant::now();
        let mut closed = first.link.closed_signal();
        tokio::time::timeout(Duration::from_secs(30), closed.wait_for(|closed| *closed))
            .await
            .expect("the tunnel closes once the service is down")
            .unwrap();
        let failing_for = failing_from.elapsed();
        assert!(failing_for >= LIST_OUTAGE, "{failing_for:?}");
        assert!(service.failed.load(Ordering::SeqCst) >= 6);

        service.failing.store(false, Ordering::SeqCst);
        let _ipad = service.tunnel().await;
        let second = opened.recv().await.unwrap();
        assert!(second.generation > first.generation);
        assert!(!second.link.is_closed());
        supervisor.abort();
    }
}
