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
use crate::stats::{PipelineStats, PIPELINE_STATS};

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
/// Each tunnel's opening and close are logged here, once, whether or not a
/// pipeline is subscribed.
async fn run_hub(mut events: mpsc::Receiver<UsbEvent>, state: Arc<Mutex<HubState>>) {
    while let Some(UsbEvent {
        generation,
        mut link,
        opened,
    }) = events.recv().await
    {
        log_connected(generation, &opened);
        let id = link.id();
        let mut inbound = link.take_inbound().expect("new USB link owns its reader");
        let link = Arc::new(link);
        let closing = Arc::clone(&link);
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
            // The reader has stopped, so the link knows what ended it.
            log_closed(generation, closing.close_report());
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
        for (_, generation, link) in subscription.open {
            info!(generation, "USB tunnel kept across pipeline restart");
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

/// How long the service's Connect and the preamble took, how long the
/// worker waited for the tunnel, and the connects that failed meanwhile.
fn log_connected(generation: u64, opened: &Opened) {
    info!(
        generation,
        connect_ms = opened.connect.as_millis() as u64,
        waited_ms = opened.waited.as_millis() as u64,
        failed = opened.failed,
        last_error = opened.last_error.as_deref().unwrap_or("none"),
        "USB tunnel connected"
    );
}

/// What the host saw end the tunnel first, after how long, and how long
/// the iPad had been quiet: a timer shows as the same `open_ms` every time.
/// `seen_first` is the first end this host saw, not necessarily what
/// started it: the iPad or Apple's service closing shows as `peer_eof`.
fn log_closed(generation: u64, report: Option<CloseReport>) {
    let ms = |duration: Duration| duration.as_millis() as u64;
    info!(
        generation,
        seen_first = %report.map_or("unknown", |report| report.cause.as_str()),
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
    let mut poll = tokio::time::interval(Duration::from_secs(2));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    supervise_devices(client, events, force_idr, &PIPELINE_STATS, poll).await;
}

/// When the supervisor polls Apple's device list next, and the time it
/// judges that poll by.
trait PollClock {
    fn next(&mut self) -> impl std::future::Future<Output = tokio::time::Instant> + Send;
}

impl PollClock for tokio::time::Interval {
    async fn next(&mut self) -> tokio::time::Instant {
        self.tick().await;
        tokio::time::Instant::now()
    }
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

/// Polls Apple's device list and runs a worker for each listed device. What
/// a poll finds goes to `stats`, which the PC window reads.
async fn supervise_devices(
    client: Arc<Client>,
    events: mpsc::Sender<UsbEvent>,
    force_idr: Arc<AtomicBool>,
    stats: &Mutex<PipelineStats>,
    mut poll: impl PollClock,
) {
    let mut workers = JoinSet::new();
    let mut handles: HashMap<u32, AbortHandle> = HashMap::new();
    let mut unreachable = AppleUsbState::default();
    let mut failures = ListFailures::default();
    loop {
        tokio::select! {
            now = poll.next() => {
                let devices = client.list_devices().await;
                let available = devices.is_ok();
                let list_error = devices.as_ref().err().map(ToString::to_string);
                let devices = devices.unwrap_or_default();
                let kept = failures.keep_workers(available, handles.len(), now);
                let ids: HashSet<_> = if kept {
                    handles.keys().copied().collect()
                } else {
                    devices.iter().map(|device| device.device_id).collect()
                };
                // Without the service the cable is invisible to us; ask
                // Windows whether an iPad is attached so the GUI can say why.
                // A poll that keeps the tunnels shows nothing new: they work.
                let cabled = if available || kept {
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
                if !kept {
                    let mut stats = stats.lock();
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

    /// Every tunnel's opening and close are logged once, by the hub, whether
    /// or not a pipeline is subscribed: with none (a restart gap) they used
    /// to go unlogged. The lines carry the tunnel generation, not the device.
    #[tokio::test(start_paused = true)]
    async fn the_hub_logs_each_tunnel_once_with_or_without_a_pipeline() {
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
        crate::logging::prepare_log_capture();
        let _capture = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .without_time()
                    .with_target(false)
                    .with_writer(move || writer.clone()),
            ),
        );
        let state = Arc::new(Mutex::new(HubState::default()));
        let (events, hub_events) = mpsc::channel(4);
        tokio::spawn(run_hub(hub_events, Arc::clone(&state)));
        let open = |generation| async move {
            let (host, mut ipad) = tokio::io::duplex(64 * 1024);
            let link = FramedLink::connect(host, PeerId::usb(3), Arc::new(AtomicBool::new(false)))
                .await
                .unwrap();
            super::super::link::read_preamble(&mut ipad).await.unwrap();
            let opened = Opened {
                connect: Duration::from_millis(5),
                waited: Duration::from_millis(1_200),
                failed: 2,
                last_error: Some("usbmuxd Result 3".into()),
            };
            let event = UsbEvent {
                generation,
                link,
                opened,
            };
            (event, ipad)
        };
        let gone = |state: Arc<Mutex<HubState>>| async move {
            for _ in 0..1000 {
                if state.lock().links.is_empty() {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("the hub kept a closed tunnel");
        };

        // No pipeline: a restart gap.
        let (event, ipad) = open(41).await;
        events.send(event).await.unwrap();
        tokio::time::sleep(Duration::from_millis(9_600)).await;
        drop(ipad);
        gone(Arc::clone(&state)).await;

        // A pipeline gets the events, and the lines are not logged again.
        let (pipeline, mut hub) = mpsc::channel(8);
        state.lock().subscriber = Some((1, pipeline));
        let (event, ipad) = open(42).await;
        events.send(event).await.unwrap();
        assert!(matches!(
            hub.recv().await,
            Some(HubEvent::Connected { generation: 42, .. })
        ));
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        drop(ipad);
        assert!(matches!(
            hub.recv().await,
            Some(HubEvent::Closed { generation: 42, .. })
        ));

        let text = String::from_utf8(lines.0.lock().clone()).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let connected = |generation| {
            format!(
                "INFO USB tunnel connected generation={generation} connect_ms=5 waited_ms=1200 \
                 failed=2 last_error=\"usbmuxd Result 3\""
            )
        };
        assert_eq!(
            lines,
            [
                connected(41),
                "INFO USB tunnel closed generation=41 seen_first=peer_eof open_ms=9600 \
                 quiet_ms=9600"
                    .into(),
                connected(42),
                "INFO USB tunnel closed generation=42 seen_first=peer_eof open_ms=1500 \
                 quiet_ms=1500"
                    .into(),
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
        failed: Arc<AtomicU64>,
        tunnels: mpsc::Receiver<tokio::net::TcpStream>,
        client: Arc<Client>,
    }

    impl FlakyService {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = Arc::new(Client::new(usbmuxd::Endpoint::Tcp(
                listener.local_addr().unwrap(),
            )));
            let failing = Arc::new(AtomicBool::new(false));
            let failed = Arc::new(AtomicU64::new(0));
            let (tunnel_tx, tunnels) = mpsc::channel(4);
            let state = (failing.clone(), failed.clone());
            tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (failing, failed) = state.clone();
                    let tunnel_tx = tunnel_tx.clone();
                    tokio::spawn(async move {
                        let (tag, request) = usbmuxd::read_message(&mut stream).await.unwrap();
                        let kind = request.as_dictionary().unwrap()["MessageType"]
                            .as_string()
                            .unwrap()
                            .to_string();
                        let reply = if kind == "ListDevices" {
                            if failing.load(Ordering::SeqCst) {
                                failed.fetch_add(1, Ordering::SeqCst);
                                return;
                            }
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
    }

    /// Poll times a test hands the supervisor one at a time. It hears when
    /// each poll has finished: the supervisor asks for the next one.
    struct Polls {
        finished: mpsc::Sender<()>,
        at: mpsc::Receiver<tokio::time::Instant>,
    }

    impl PollClock for Polls {
        async fn next(&mut self) -> tokio::time::Instant {
            let _ = self.finished.send(()).await;
            self.at.recv().await.expect("the test's next poll")
        }
    }

    /// The test's side of [`Polls`].
    struct Poller {
        at: mpsc::Sender<tokio::time::Instant>,
        polled: mpsc::Receiver<()>,
        start: tokio::time::Instant,
    }

    impl Poller {
        /// Polls `secs` after the start, and returns once that poll has
        /// finished.
        async fn poll(&mut self, secs: u64) {
            let at = self.start + Duration::from_secs(secs);
            self.at.send(at).await.unwrap();
            self.polled.recv().await.unwrap();
        }
    }

    /// A device list that fails once leaves the live tunnel open; one that
    /// keeps failing closes it once the failures have lasted
    /// [`LIST_OUTAGE`], and once the list answers again the iPad gets a new
    /// tunnel. While the tunnel is kept, the PC window goes on showing USB
    /// as it was, not "Open Apple Devices". A single failed list used to
    /// close the tunnel at once. Each poll's time is the test's, so how the
    /// machine schedules the test changes nothing.
    #[tokio::test]
    async fn a_failed_device_list_keeps_the_tunnel_until_the_service_is_down() {
        let mut service = FlakyService::start().await;
        let stats: &'static Mutex<PipelineStats> =
            Box::leak(Box::new(Mutex::new(PipelineStats::default())));
        let (finished, mut polled) = mpsc::channel(1);
        let (at, polls) = mpsc::channel(1);
        let (events, mut opened) = mpsc::channel(4);
        let supervisor = tokio::spawn(supervise_devices(
            Arc::clone(&service.client),
            events,
            Arc::new(AtomicBool::new(false)),
            stats,
            Polls {
                finished,
                at: polls,
            },
        ));
        polled.recv().await.unwrap();
        let mut poller = Poller {
            at,
            polled,
            start: tokio::time::Instant::now(),
        };
        let shown = || {
            let stats = stats.lock();
            (
                stats.usb_service_reachable,
                stats.usb_devices,
                stats.usb_link_state.clone(),
            )
        };
        let streaming = (true, 1, "Waiting for the iPad app".to_string());

        poller.poll(0).await;
        let _ipad = service.tunnel().await;
        let first = opened.recv().await.unwrap();
        assert_eq!(shown(), streaming);

        // One failed list between good ones.
        service.failing.store(true, Ordering::SeqCst);
        poller.poll(2).await;
        assert_eq!(service.failed.load(Ordering::SeqCst), 1);
        assert!(!first.link.is_closed(), "one failed list closed the tunnel");
        assert_eq!(shown(), streaming, "a kept tunnel shown as unavailable");
        service.failing.store(false, Ordering::SeqCst);
        poller.poll(4).await;
        assert!(!first.link.is_closed());
        assert!(service.tunnels.try_recv().is_err(), "a second tunnel");

        // Failures in a row: kept until they have lasted the outage.
        service.failing.store(true, Ordering::SeqCst);
        for secs in [6, 8, 10, 12, 14] {
            poller.poll(secs).await;
            assert!(!first.link.is_closed(), "closed at {secs} s");
            assert_eq!(shown(), streaming, "at {secs} s");
        }
        poller.poll(16).await;
        let mut closed = first.link.closed_signal();
        tokio::time::timeout(Duration::from_secs(20), closed.wait_for(|closed| *closed))
            .await
            .expect("the tunnel closes once the service is down")
            .unwrap();
        assert_eq!(
            shown(),
            (false, 0, "Apple device service unavailable".to_string())
        );

        service.failing.store(false, Ordering::SeqCst);
        poller.poll(18).await;
        let _ipad = service.tunnel().await;
        let second = opened.recv().await.unwrap();
        assert!(second.generation > first.generation);
        assert!(!second.link.is_closed());
        assert_eq!(shown(), streaming);
        supervisor.abort();
    }
}
