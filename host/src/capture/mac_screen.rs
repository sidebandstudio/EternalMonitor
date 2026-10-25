//! What the macOS host captures, when real capture is on
//! (`SettingsFile::mac_second_screen`; off by default, so nothing creates a
//! display by itself and the synthetic source stays the default).
//!
//! Second screen comes first. The capture loop asks [`Targets`] which
//! display to capture on every pass, so a client connecting, leaving, or
//! having its saved profile changed re-targets capture inside the running
//! generation: no pipeline restart, and nothing borrowed from the Windows
//! virtual display's state. While a client is connected (or within the
//! grace period after it left) the display owner keeps a virtual display
//! for it and capture follows it; otherwise, or if the virtual display
//! cannot be made, the main display is mirrored. Mirror mode is the
//! fallback, not a mode of its own yet.
//!
//! Input is not wired here. Posting CGEvents waits on the transport
//! security decision (plan-v2 point 2); until then the relay keeps
//! recording, as on every non-Windows host.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use objc2_core_graphics::CGMainDisplayID;
use tracing::{info, warn};

use super::sck::{CaptureFailure, DisplayChoice, Worker, HEARTBEAT_SLICE, LIST_POLL};
use super::FrameSlot;
use crate::control::SharedControl;
use crate::display_profile::{ClientScreen, ProfileIo, ProfileStore, ProfileView};
use crate::settings::SettingsFile;
use crate::virtual_display::mac::{CgVirtualDisplays, ModeChange};
use crate::virtual_display::owner::DisplayOwner;
use crate::virtual_display::{LiveClient, SecondScreenStatus, Want};

/// How long, in wall-clock time, a new display may take to appear in
/// ScreenCaptureKit's list before the main display is mirrored instead
/// (macOS 15.7 has been seen not to list a virtual display at all).
const LIST_WAIT: Duration = Duration::from_secs(4);
/// How often a display that was not listed is checked again.
const LIST_RECHECK: Duration = Duration::from_secs(5);

/// Whether ScreenCaptureKit offers a display. A call can block for many
/// seconds, so it runs on a [`Worker`], never the capture thread.
pub(crate) trait ContentLister: Send {
    fn is_listed(&self, display_id: u32) -> Result<bool, CaptureFailure>;
}

struct ScreenCaptureKitLister;

impl ContentLister for ScreenCaptureKitLister {
    fn is_listed(&self, display_id: u32) -> Result<bool, CaptureFailure> {
        super::sck::stream::display_listed(display_id)
    }
}

static OWNER: OnceLock<Option<DisplayOwner>> = OnceLock::new();

/// The process's display owner, started on first use. It follows the
/// transport's session in `shared`, which alone decides which device is
/// served and when a client is gone.
fn owner(shared: &SharedControl) -> Option<&'static DisplayOwner> {
    OWNER
        .get_or_init(|| {
            spawn_owner(
                || CgVirtualDisplays::new(ModeChange::Recreate),
                shared,
                ProfileStore::beside_settings(),
            )
            .map(|(owner, _)| owner)
            .inspect_err(|error| warn!(%error, "Could not start the display owner"))
            .ok()
        })
        .as_ref()
}

/// A display owner over `make`'s displays for the session in `shared`. Its
/// profiles go through a [`ProfileIo`] thread over `store`, so the owner
/// thread never touches the disk: it reads the published view and hands
/// first-time saves over.
fn spawn_owner<M, F>(
    make: F,
    shared: &SharedControl,
    store: ProfileStore,
) -> std::io::Result<(DisplayOwner, ProfileView)>
where
    M: crate::virtual_display::VirtualDisplayManager + 'static,
    F: FnOnce() -> Result<M, crate::virtual_display::DisplayError> + Send + 'static,
{
    let profiles = ProfileIo::spawn(store)?;
    let (view, live) = (profiles.view(), profiles.view());
    let shared = shared.clone();
    let owner = DisplayOwner::spawn(
        make,
        move || live_client(&shared, &live),
        move |device_id, profile| profiles.save_if_absent(device_id, profile),
    )?;
    Ok((owner, view))
}

/// Second screen was turned off: remove its display now. Starts nothing if
/// no display was ever made.
pub(super) fn second_screen_off() {
    if let Some(Some(owner)) = OWNER.get() {
        if let Err(error) = owner.want(off()) {
            warn!(%error, "The second-screen display did not go away");
        }
    }
}

fn off() -> Want {
    Want {
        enabled: false,
        grace: Duration::ZERO,
        device: None,
    }
}

/// The client of the transport's live session, if there is one.
fn live_info(shared: &SharedControl) -> Option<crate::transport::session::ClientInfo> {
    let session = shared.session.lock();
    if session.is_active() {
        session.client_info()
    } else {
        None
    }
}

/// The live session as the display owner reads it on every turn: the
/// connected client and the saved profile for its device, from memory.
fn live_client(shared: &SharedControl, profiles: &ProfileView) -> Option<LiveClient> {
    let info = live_info(shared)?;
    let client = ClientScreen {
        device_id: info.device_id,
        screen_px: info.screen_px,
        refresh_hz: info.refresh_hz,
    };
    Some(LiveClient {
        client,
        device_name: info.device_name,
        saved: profiles.get(client.device_id),
    })
}

/// The host is quitting: remove the display rather than leave it to the
/// process exit.
pub fn release_at_quit() {
    if let Some(Some(owner)) = OWNER.get() {
        if let Err(error) = owner.release() {
            warn!(%error, "The second-screen display did not go away at quit");
        }
    }
}

/// Where the second-screen settings come from: `settings.json`, re-read
/// only when it changes.
pub(crate) trait SettingsSource {
    /// The settings, if they changed since the last call (always on the
    /// first).
    fn changed(&mut self) -> Option<SettingsFile>;
}

/// Whether this generation should capture for real. A settings file that
/// cannot be read right now (a partial write by another program) keeps the
/// last good answer rather than reading as "off", which would remove the
/// display.
pub(super) fn second_screen_wanted() -> bool {
    let Some(path) = crate::settings::settings_path() else {
        return false;
    };
    match SettingsFile::read(&path) {
        Ok(settings) => settings.mac_second_screen,
        Err(error) => {
            warn!(%error, "settings.json cannot be read; keeping the last good settings");
            SettingsFile::last_good(&path).is_some_and(|settings| settings.mac_second_screen)
        }
    }
}

/// `settings.json`, re-read when its modification time moves. Capture only
/// reads it; the display profiles are in their own file. A read or parse
/// failure keeps the last valid settings and leaves that modification time
/// unseen, so the next pass tries again.
struct SettingsOnDisk {
    path: Option<PathBuf>,
    seen: Option<Option<SystemTime>>,
    warned: Option<Option<SystemTime>>,
    /// Handed out if the very first read fails.
    fallback: Option<SettingsFile>,
}

impl SettingsOnDisk {
    fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            seen: None,
            warned: None,
            fallback: None,
        }
    }

    /// The live host's reader, starting from the last good settings.
    fn live() -> Self {
        let path = crate::settings::settings_path();
        Self {
            fallback: path.as_deref().and_then(SettingsFile::last_good),
            ..Self::at(path)
        }
    }
}

impl SettingsSource for SettingsOnDisk {
    fn changed(&mut self) -> Option<SettingsFile> {
        let path = self.path.as_deref()?;
        let modified = std::fs::metadata(path)
            .ok()
            .and_then(|meta| meta.modified().ok());
        if self.seen == Some(modified) {
            return None;
        }
        match SettingsFile::read(path) {
            Ok(settings) => {
                self.seen = Some(modified);
                self.fallback = None;
                Some(settings)
            }
            Err(error) => {
                if self.warned != Some(modified) {
                    self.warned = Some(modified);
                    warn!(path = %path.display(), %error, "settings.json cannot be read; keeping the last good settings");
                }
                self.fallback.take()
            }
        }
    }
}

/// Picks the display to capture, every time the capture loop asks.
pub(crate) struct Targets<'a> {
    shared: SharedControl,
    owner: &'a DisplayOwner,
    settings_source: Box<dyn SettingsSource + 'a>,
    settings: SettingsFile,
    listing: Worker<bool>,
    /// Stamps the stage's heartbeats while the capture thread waits.
    heartbeat: Box<dyn FnMut() + 'a>,
    main: u32,
    /// The display ScreenCaptureKit was last seen to offer.
    listed: Option<u32>,
    /// A display it did not offer, and when to check it again.
    unlisted: Option<(u32, Instant)>,
}

impl<'a> Targets<'a> {
    pub(crate) fn new(
        shared: SharedControl,
        owner: &'a DisplayOwner,
        settings_source: Box<dyn SettingsSource + 'a>,
        lister: Box<dyn ContentLister>,
        heartbeat: Box<dyn FnMut() + 'a>,
        main: u32,
    ) -> Self {
        Self {
            shared,
            owner,
            settings_source,
            settings: SettingsFile::default(),
            listing: Worker::spawn("display-listing", move |display_id| {
                lister.is_listed(display_id).unwrap_or(false)
            }),
            heartbeat,
            main,
            listed: None,
            unlisted: None,
        }
    }

    fn set_status(&self, status: SecondScreenStatus) {
        let mut current = self.shared.second_screen.lock();
        if *current == status {
            return;
        }
        match &status {
            SecondScreenStatus::Off => info!("Second screen off"),
            SecondScreenStatus::Mirroring => {
                info!("No client connected; mirroring the main display until one is")
            }
            SecondScreenStatus::Active(id) => info!(id, "Capturing the second-screen display"),
            SecondScreenStatus::NotCapturable(id) => warn!(
                id,
                "macOS has not made the second-screen display available to capture; \
                 mirroring the main display and checking again every few seconds"
            ),
            SecondScreenStatus::Unavailable(why) => {
                warn!(%why, "Second screen unavailable; mirroring the main display")
            }
        }
        *current = status;
    }

    /// Whether ScreenCaptureKit offers `id`. A display seen before is
    /// capturable at once. A new one gets up to `LIST_WAIT` of wall-clock
    /// time, heartbeats included, however slow each call is; a call still
    /// running then is left to finish while the main display is mirrored.
    /// A display that was not listed is asked about again every
    /// `LIST_RECHECK` without blocking.
    fn capturable(&mut self, id: u32, now: Instant) -> bool {
        if self.listed == Some(id) {
            return true;
        }
        let listed = match self.unlisted {
            Some((pending, next)) if pending == id => {
                let listed = matches!(self.listing.answer(Duration::ZERO), Some((answered, true)) if answered == id);
                if !listed && now >= next && !self.listing.busy() {
                    self.listing.ask(id);
                    self.unlisted = Some((id, now + LIST_RECHECK));
                }
                listed
            }
            _ => {
                let listed = self.wait_listed(id);
                if !listed {
                    self.unlisted = Some((id, now + LIST_RECHECK));
                }
                listed
            }
        };
        if listed {
            self.listed = Some(id);
            self.unlisted = None;
        }
        listed
    }

    fn wait_listed(&mut self, id: u32) -> bool {
        let deadline = Instant::now() + LIST_WAIT;
        let mut ask_at = Instant::now();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            if now >= ask_at {
                self.listing.ask(id);
            }
            let slice = HEARTBEAT_SLICE.min(deadline - now);
            match self.listing.answer(slice) {
                Some((answered, true)) if answered == id => return true,
                // An answer about this display, or a stale one about
                // another: ask again after a pause.
                Some(_) => ask_at = Instant::now() + LIST_POLL,
                None if !self.listing.busy() => std::thread::sleep(slice),
                None => {}
            }
            (self.heartbeat)();
        }
    }

    fn choose(&mut self, now: Instant) -> u32 {
        if let Some(settings) = self.settings_source.changed() {
            self.settings = settings;
        }
        if !self.settings.mac_second_screen {
            // Turned off while running: remove the display now and mirror
            // until the next generation goes back to the synthetic source.
            if let Err(error) = self.owner.want(off()) {
                warn!(%error, "The second-screen display did not go away");
            }
            self.set_status(SecondScreenStatus::Off);
            return self.main;
        }
        // The owner reads the live session itself; the device this pass
        // saw is only a hint, which a later session can make stale.
        let want = Want {
            enabled: true,
            grace: Duration::from_secs(self.settings.mac_second_screen_grace_secs.into()),
            device: live_info(&self.shared).map(|info| info.device_id),
        };
        match self.owner.want(want) {
            // Capture never writes profiles: the owner saves a new
            // device's default, and only explicit edits change one.
            Ok(Some(second)) => {
                if self.capturable(second.id, now) {
                    self.set_status(SecondScreenStatus::Active(second.id));
                    second.id
                } else {
                    self.set_status(SecondScreenStatus::NotCapturable(second.id));
                    self.main
                }
            }
            Ok(None) => {
                self.set_status(SecondScreenStatus::Mirroring);
                self.main
            }
            Err(error) => {
                self.set_status(SecondScreenStatus::Unavailable(error.to_string()));
                self.main
            }
        }
    }
}

impl DisplayChoice for Targets<'_> {
    fn display(&mut self, now: Instant) -> u32 {
        self.choose(now)
    }

    /// The stream could not find a display it was given. For the virtual
    /// display that means mirroring until a later check finds it; it is
    /// never a pipeline failure. For the main display it is.
    fn not_listed(&mut self, display_id: u32, now: Instant) -> bool {
        if display_id == self.main {
            return false;
        }
        if self.listed == Some(display_id) {
            self.listed = None;
        }
        self.unlisted = Some((display_id, now + LIST_RECHECK));
        self.set_status(SecondScreenStatus::NotCapturable(display_id));
        true
    }
}

pub(super) fn run_capture_loop(
    slot: &FrameSlot,
    shared: SharedControl,
    generation: u64,
    frame_number: &mut u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let main = CGMainDisplayID();
    let Some(owner) = owner(&shared) else {
        return super::sck::stream::run_capture_loop(
            slot,
            shared,
            generation,
            frame_number,
            &mut Fixed(main),
        );
    };
    let heartbeat_shared = shared.clone();
    let mut targets = Targets::new(
        shared.clone(),
        owner,
        Box::new(SettingsOnDisk::live()),
        Box::new(ScreenCaptureKitLister),
        // A bounded wait inside one capture pass: keep the stage alive.
        Box::new(move || {
            super::idle_heartbeat(&heartbeat_shared, crate::clock::host_now_us() / 1000)
        }),
        main,
    );
    super::sck::stream::run_capture_loop(slot, shared, generation, frame_number, &mut targets)
}

/// The main display, when there is no display owner at all.
struct Fixed(u32);

impl DisplayChoice for Fixed {
    fn display(&mut self, _now: Instant) -> u32 {
        self.0
    }

    fn not_listed(&mut self, _display_id: u32, _now: Instant) -> bool {
        false
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::virtual_display::fake::{FakeDisplays, Op};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{mpsc, Arc};

    pub(crate) const MAIN: u32 = 1;

    /// Settings held in memory.
    pub(crate) struct MemorySettings {
        pub(crate) next: Option<SettingsFile>,
    }

    impl SettingsSource for MemorySettings {
        fn changed(&mut self) -> Option<SettingsFile> {
            self.next.take()
        }
    }

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        // Path-safe on every platform: no Debug output in the name.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "eternal-mac-screen-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn enabled() -> SettingsFile {
        SettingsFile {
            mac_second_screen: true,
            ..Default::default()
        }
    }

    pub(crate) fn connect(shared: &SharedControl, device_id: u64, screen: (u16, u16)) {
        use crate::transport::link::PeerId;
        use crate::transport::session::ConfigSource;
        use eternal_wire::v2::control::{ControlMessage, Hello2, StreamConfig, CAP_DECODE_HEVC};
        struct Config;
        impl ConfigSource for Config {
            fn stream_config(&self) -> StreamConfig {
                StreamConfig {
                    stream_epoch: 1,
                    width: 640,
                    height: 360,
                    fps: 60,
                    codec: 0,
                    flags: 0,
                    bitrate_bps: 15_000_000,
                }
            }
            fn host_name(&self) -> String {
                "TEST".into()
            }
        }
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 1,
            listen_port: 50000,
            decoder_caps: CAP_DECODE_HEVC,
            feature_caps: 0,
            screen_px_w: screen.0,
            screen_px_h: screen.1,
            screen_pt_w: screen.0 / 2,
            screen_pt_h: screen.1 / 2,
            refresh_hz: 120,
            device_name: "Duo".into(),
            device_id,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let peer = PeerId::udp(std::net::SocketAddr::from(([10, 0, 0, 2], 50000)));
        shared
            .session
            .lock()
            .handle_control(peer, 0, hello, &Config, Instant::now());
        assert!(shared.client_connected());
    }

    /// A display owner over fake displays that reports what it does. It
    /// follows `shared`'s session as the host does, with no profile store.
    pub(crate) fn fake_owner(shared: &SharedControl) -> (DisplayOwner, mpsc::Receiver<Op>) {
        let (owner, ops, _) = fake_owner_with(|| {}, shared, None);
        (owner, ops)
    }

    /// The same, with its profile store at `profiles`, as the host's is
    /// beside `settings.json`, running `during_create` while a display is
    /// being made. Wired as the host's is (`spawn_owner`); the view is the
    /// owner's.
    pub(crate) fn fake_owner_with(
        during_create: impl Fn() + Send + 'static,
        shared: &SharedControl,
        profiles: Option<PathBuf>,
    ) -> (DisplayOwner, mpsc::Receiver<Op>, ProfileView) {
        use crate::virtual_display::{DisplayError, DisplaySpec, VirtualDisplay};
        let (ops_tx, ops) = mpsc::channel();
        struct Reporting(FakeDisplays, mpsc::Sender<Op>, Box<dyn Fn() + Send>);
        impl crate::virtual_display::VirtualDisplayManager for Reporting {
            fn create(&mut self, spec: &DisplaySpec) -> Result<VirtualDisplay, DisplayError> {
                (self.2)();
                let _ = self.1.send(Op::Create(spec.clone()));
                self.0.create(spec)
            }
            fn change_mode(
                &mut self,
                display: &VirtualDisplay,
                profile: crate::display_profile::DisplayProfile,
            ) -> Result<VirtualDisplay, DisplayError> {
                let _ = self.1.send(Op::ChangeMode(display.id, profile));
                self.0.change_mode(display, profile)
            }
            fn remove(&mut self, display: &VirtualDisplay) -> Result<(), DisplayError> {
                let _ = self.1.send(Op::Remove(display.id));
                self.0.remove(display)
            }
            fn bounds(
                &self,
                display: &VirtualDisplay,
            ) -> Option<crate::input::mac::display::DisplayBounds> {
                self.0.bounds(display)
            }
        }
        let during_create: Box<dyn Fn() + Send> = Box::new(during_create);
        let (owner, view) = spawn_owner(
            move || Ok(Reporting(FakeDisplays::default(), ops_tx, during_create)),
            shared,
            ProfileStore::at(profiles),
        )
        .expect("owner thread");
        (owner, ops, view)
    }

    /// ScreenCaptureKit, faked: lists a display after so many checks.
    pub(crate) struct FakeLister {
        pub(crate) listed_after: Option<u32>,
        pub(crate) checks: Arc<AtomicU32>,
    }

    impl FakeLister {
        pub(crate) fn listed() -> Self {
            Self::after(Some(0))
        }

        pub(crate) fn after(listed_after: Option<u32>) -> Self {
            Self {
                listed_after,
                checks: Default::default(),
            }
        }
    }

    impl ContentLister for FakeLister {
        fn is_listed(&self, display_id: u32) -> Result<bool, CaptureFailure> {
            assert_ne!(display_id, MAIN, "only virtual displays are checked");
            let checks = self.checks.fetch_add(1, Ordering::SeqCst);
            Ok(self.listed_after.is_some_and(|after| checks >= after))
        }
    }

    pub(crate) fn targets<'a>(
        shared: &SharedControl,
        owner: &'a DisplayOwner,
        settings: Box<dyn SettingsSource + 'a>,
        lister: FakeLister,
    ) -> Targets<'a> {
        Targets::new(
            shared.clone(),
            owner,
            settings,
            Box::new(lister),
            Box::new(|| {}),
            MAIN,
        )
    }

    /// What the capture loop does with the answers: it starts a stream
    /// whenever the chosen display changes, within one generation.
    pub(crate) fn streams_started(answers: &[u32]) -> Vec<u32> {
        let mut started: Vec<u32> = Vec::new();
        for &answer in answers {
            if started.last() != Some(&answer) {
                started.push(answer);
            }
        }
        started
    }

    #[test]
    fn a_client_connecting_re_targets_capture_to_its_virtual_display() {
        let shared = SharedControl::new(0, 15_000_000);
        let generation =
            crate::supervisor::CURRENT_GENERATION.load(std::sync::atomic::Ordering::SeqCst);
        let dir = temp_dir("connect");
        let profiles = dir.join("display-profiles.json");
        let (owner, ops, _) = fake_owner_with(|| {}, &shared, Some(profiles.clone()));
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
            }),
            FakeLister::listed(),
        );
        let now = Instant::now();
        let mut answers = Vec::new();
        // Startup: no client, so the main display.
        for _ in 0..3 {
            answers.push(targets.display(now));
        }
        assert!(ops.try_recv().is_err(), "no display made without a client");
        // The Duo connects, folded. The next pass captures its display,
        // and its default profile is saved, once: later passes find it.
        connect(&shared, 0xd0, (1398, 2034));
        answers.push(targets.display(now));
        let written = saved_soon(&profiles);
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(10));
            answers.push(targets.display(now));
        }
        assert_eq!(on_disk(&profiles), written);
        let Ok(Op::Create(spec)) = ops.try_recv() else {
            panic!("expected the display to be made")
        };
        assert_eq!(
            (spec.profile.width_px, spec.profile.height_px),
            (2670, 1878)
        );
        let virtual_id = *answers.last().unwrap();
        assert_ne!(virtual_id, MAIN);
        assert_eq!(streams_started(&answers), [MAIN, virtual_id]);
        // Made once, its profile saved once, and the generation never
        // changed: no pipeline restart was involved.
        assert!(ops.try_recv().is_err());
        assert_eq!(
            ProfileStore::at(Some(profiles)).get(0xd0),
            Some(spec.profile)
        );
        assert!(crate::supervisor::generation_is_current(generation));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_changed_saved_profile_re_targets_to_the_new_display() {
        let shared = SharedControl::new(0, 15_000_000);
        let dir = temp_dir("profile-change");
        let profiles = dir.join("display-profiles.json");
        let (owner, ops, view) = fake_owner_with(|| {}, &shared, Some(profiles.clone()));
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
            }),
            FakeLister::listed(),
        );
        connect(&shared, 0xd0, (2670, 1878));
        let now = Instant::now();
        let first = targets.display(now);
        // The outer size is saved for this device (the file's time moves).
        std::thread::sleep(Duration::from_millis(20));
        let outer = crate::display_profile::DisplayProfile {
            width_px: 2034,
            height_px: 1398,
            refresh_hz: 120,
            hidpi: true,
        };
        assert!(ProfileStore::at(Some(profiles.clone())).remember(0xd0, outer));
        // The owner's view has it within a reload; the next pass applies it.
        until_viewed(&view, 0xd0, outer);
        let second = targets.display(now);
        assert_eq!(targets.display(now), second);
        // The edit stands: nothing wrote the old size back.
        assert_eq!(ProfileStore::at(Some(profiles)).get(0xd0), Some(outer));
        std::fs::remove_dir_all(dir).unwrap();
        assert_ne!(second, first);
        assert_ne!(second, MAIN);
        let ops: Vec<_> = ops.try_iter().collect();
        assert!(
            matches!(ops[..], [Op::Create(_), Op::Remove(id), Op::Create(_)] if id == first),
            "{ops:?}"
        );
    }

    #[test]
    fn a_long_gap_between_generations_keeps_the_display_until_the_session_ends() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner(&shared);
        let settings = || {
            Box::new(MemorySettings {
                next: Some(SettingsFile {
                    mac_second_screen_grace_secs: 5,
                    ..enabled()
                }),
            })
        };
        connect(&shared, 0xd0, (2670, 1878));
        let mut first = targets(&shared, &owner, settings(), FakeLister::listed());
        let id = first.display(Instant::now());
        // The generation ends; none follows for longer than the grace
        // period, while the session stays active.
        drop(first);
        std::thread::sleep(crate::virtual_display::MIN_GRACE + Duration::from_secs(1));
        let mut second = targets(&shared, &owner, settings(), FakeLister::listed());
        assert_eq!(second.display(Instant::now()), id);
        let made: Vec<_> = ops.try_iter().collect();
        assert!(matches!(made[..], [Op::Create(_)]), "{made:?}");
        drop(second);
        // The transport loses the session: removed after the grace period.
        let gone = Instant::now();
        let _ = shared
            .session
            .lock()
            .link_lost(crate::transport::link::LinkId::Udp);
        assert!(!shared.client_connected());
        assert_eq!(
            ops.recv_timeout(Duration::from_secs(10)),
            Ok(Op::Remove(id))
        );
        assert!(gone.elapsed() >= crate::virtual_display::MIN_GRACE);
    }

    /// What is on disk, and when it was last written.
    fn on_disk(path: &std::path::Path) -> (String, SystemTime) {
        (
            std::fs::read_to_string(path).unwrap(),
            std::fs::metadata(path).unwrap().modified().unwrap(),
        )
    }

    /// Waits for the owner's view of the profiles to show `profile` for
    /// `device_id`: an edit reaches it within a reload.
    fn until_viewed(
        view: &ProfileView,
        device_id: u64,
        profile: crate::display_profile::DisplayProfile,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while view.get(device_id) != Some(profile) {
            assert!(
                Instant::now() < deadline,
                "the edit never reached the owner"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// `on_disk` once the owner's first-time save, which follows its
    /// answer, has written the file.
    fn saved_soon(path: &std::path::Path) -> (String, SystemTime) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "the first profile was never saved"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        on_disk(path)
    }

    #[test]
    fn stale_and_idle_passes_never_write_over_an_edited_profile() {
        // Sol's round 6. The owner's live session is the Duo's (B); `seen`
        // is what a stale capture pass snapshotted: an iPad (A).
        let live = SharedControl::new(0, 15_000_000);
        let seen = SharedControl::new(0, 15_000_000);
        let dir = temp_dir("stale-device");
        let path = dir.join("display-profiles.json");
        let (owner, ops, view) = fake_owner_with(|| {}, &live, Some(path.clone()));
        let settings = || {
            Box::new(MemorySettings {
                next: Some(enabled()),
            })
        };
        connect(&live, 0xd0, (2670, 1878));
        connect(&seen, 0x1a, (1668, 2420));
        let mut current = targets(&live, &owner, settings(), FakeLister::listed());
        let mut stale = targets(&seen, &owner, settings(), FakeLister::listed());
        let first = current.display(Instant::now());
        assert_ne!(first, MAIN);
        // 1. B's saved profile is edited to the outer size.
        std::thread::sleep(Duration::from_millis(20));
        let outer = crate::display_profile::DisplayProfile {
            width_px: 2034,
            height_px: 1398,
            refresh_hz: 120,
            hidpi: true,
        };
        assert!(ProfileStore::at(Some(path.clone())).remember(0xd0, outer));
        let edited = on_disk(&path);
        until_viewed(&view, 0xd0, outer);
        // 2. A stale A request arrives first: it gets B's display as it is,
        //    and the store is not touched.
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(stale.display(Instant::now()), first);
        assert_eq!(on_disk(&path), edited);
        // 3. B's session ends; an idle pass during grace keeps capturing
        //    the kept display and does not touch the store either.
        let _ = live
            .session
            .lock()
            .link_lost(crate::transport::link::LinkId::Udp);
        assert!(!live.client_connected());
        for pass in [&mut current, &mut stale] {
            std::thread::sleep(Duration::from_millis(10));
            assert_eq!(pass.display(Instant::now()), first);
        }
        assert_eq!(on_disk(&path), edited);
        assert!(matches!(
            ops.try_iter().collect::<Vec<_>>()[..],
            [Op::Create(_)]
        ));
        // 4. B reconnects: its next own pass recreates at the edited size.
        connect(&live, 0xd0, (2670, 1878));
        let second = current.display(Instant::now());
        assert_ne!(second, first);
        let ops: Vec<_> = ops.try_iter().collect();
        assert!(
            matches!(&ops[..], [Op::Remove(id), Op::Create(spec)] if *id == first && spec.profile == outer),
            "{ops:?}"
        );
        let mut saved = ProfileStore::at(Some(path.clone()));
        assert_eq!(saved.get(0xd0), Some(outer));
        assert_eq!(saved.get(0x1a), None);
        assert_eq!(on_disk(&path), edited);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Writes settings the way the host does: a temporary file, renamed.
    pub(crate) fn write_settings(path: &std::path::Path, settings: &SettingsFile) {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(settings).unwrap()).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    #[test]
    fn turning_second_screen_off_during_creation_stays_off() {
        let dir = temp_dir("race");
        let settings_path = dir.join("settings.json");
        write_settings(&settings_path, &enabled());
        let user_path = settings_path.clone();
        // The user turns second screen off while the display is being made.
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops, _) = fake_owner_with(
            move || {
                std::thread::sleep(Duration::from_millis(20));
                write_settings(&user_path, &SettingsFile::default());
            },
            &shared,
            Some(dir.join("display-profiles.json")),
        );
        let mut targets = Targets::new(
            shared.clone(),
            &owner,
            Box::new(SettingsOnDisk::at(Some(settings_path.clone()))),
            Box::new(FakeLister::listed()),
            Box::new(|| {}),
            MAIN,
        );
        connect(&shared, 0xd0, (2670, 1878));
        let id = targets.display(Instant::now());
        assert_ne!(id, MAIN, "the display made before the edit is captured");
        // The profile was saved, and the user's edit was not overwritten.
        saved_soon(&dir.join("display-profiles.json"));
        let on_disk = SettingsFile::load_from(&settings_path);
        assert!(!on_disk.mac_second_screen, "flipped back on");
        assert_eq!(
            ProfileStore::at(Some(dir.join("display-profiles.json")))
                .get(0xd0)
                .map(|p| (p.width_px, p.height_px)),
            Some((2670, 1878))
        );
        // The next pass sees the edit: the display goes, the main display
        // is mirrored.
        assert_eq!(targets.display(Instant::now()), MAIN);
        let ops: Vec<_> = ops.try_iter().collect();
        assert!(
            matches!(ops[..], [Op::Create(_), Op::Remove(removed)] if removed == id),
            "{ops:?}"
        );
        assert!(!SettingsFile::load_from(&settings_path).mac_second_screen);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_damaged_settings_file_keeps_the_display_and_the_last_valid_settings() {
        let dir = temp_dir("damaged");
        let path = dir.join("settings.json");
        let valid = |grace: u32, on: bool| SettingsFile {
            mac_second_screen: on,
            mac_second_screen_grace_secs: grace,
            ..Default::default()
        };
        write_settings(&path, &valid(30, true));
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner(&shared);
        let mut targets = Targets::new(
            shared.clone(),
            &owner,
            Box::new(SettingsOnDisk::at(Some(path.clone()))),
            Box::new(FakeLister::listed()),
            Box::new(|| {}),
            MAIN,
        );
        connect(&shared, 0xd0, (2670, 1878));
        let id = targets.display(Instant::now());
        assert_ne!(id, MAIN);
        // Another program writes in place and is caught half-way, then
        // leaves invalid JSON: each pass keeps the display and the values.
        for broken in ["{\"mac_second_screen\": tr", "", "not json"] {
            std::thread::sleep(Duration::from_millis(10));
            std::fs::write(&path, broken).unwrap();
            for _ in 0..3 {
                assert_eq!(targets.display(Instant::now()), id, "{broken:?}");
            }
            assert!(targets.settings.mac_second_screen);
            assert_eq!(targets.settings.mac_second_screen_grace_secs, 30);
        }
        // The next valid write is picked up.
        std::thread::sleep(Duration::from_millis(10));
        write_settings(&path, &valid(90, true));
        assert_eq!(targets.display(Instant::now()), id);
        assert_eq!(targets.settings.mac_second_screen_grace_secs, 90);
        assert!(ops.try_iter().all(|op| !matches!(op, Op::Remove(_))));
        std::thread::sleep(Duration::from_millis(10));
        write_settings(&path, &valid(90, false));
        assert_eq!(targets.display(Instant::now()), MAIN);
        assert!(matches!(ops.try_recv(), Ok(Op::Remove(removed)) if removed == id));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_first_read_that_fails_starts_from_the_last_good_settings() {
        let dir = temp_dir("first-read");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{").unwrap();
        let mut reader = SettingsOnDisk {
            fallback: Some(enabled()),
            ..SettingsOnDisk::at(Some(path.clone()))
        };
        assert!(reader.changed().unwrap().mac_second_screen);
        // Still broken: nothing new, and nothing marked as seen.
        assert!(reader.changed().is_none());
        std::thread::sleep(Duration::from_millis(10));
        write_settings(&path, &SettingsFile::default());
        assert!(!reader.changed().unwrap().mac_second_screen);
        assert!(reader.changed().is_none(), "read once per change");
        // A missing file is the defaults, not an error.
        let mut missing = SettingsOnDisk::at(Some(dir.join("absent.json")));
        assert!(!missing.changed().unwrap().mac_second_screen);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn turning_second_screen_off_while_running_mirrors_and_removes_the_display() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner(&shared);
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
            }),
            FakeLister::listed(),
        );
        connect(&shared, 0xd0, (2670, 1878));
        let virtual_id = targets.display(Instant::now());
        targets.settings_source = Box::new(MemorySettings {
            next: Some(SettingsFile::default()),
        });
        assert_eq!(targets.display(Instant::now()), MAIN);
        let ops: Vec<_> = ops.try_iter().collect();
        assert!(
            matches!(ops[..], [Op::Create(_), Op::Remove(id)] if id == virtual_id),
            "{ops:?}"
        );
    }
    /// A lister whose answer can change, which can be slow, and which
    /// records how many calls ever ran at once.
    #[derive(Clone, Default)]
    struct Scripted {
        listed: Arc<AtomicBool>,
        checks: Arc<AtomicU32>,
        delay: Duration,
        running: Arc<AtomicU32>,
        most_at_once: Arc<AtomicU32>,
    }

    impl ContentLister for Scripted {
        fn is_listed(&self, _display_id: u32) -> Result<bool, CaptureFailure> {
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_at_once.fetch_max(running, Ordering::SeqCst);
            self.checks.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            self.running.fetch_sub(1, Ordering::SeqCst);
            Ok(self.listed.load(Ordering::SeqCst))
        }
    }

    fn scripted_targets<'a>(
        shared: &SharedControl,
        owner: &'a DisplayOwner,
        lister: &Scripted,
        beats: Arc<parking_lot::Mutex<Vec<Instant>>>,
    ) -> Targets<'a> {
        let beat_shared = shared.clone();
        Targets::new(
            shared.clone(),
            owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
            }),
            Box::new(lister.clone()),
            Box::new(move || {
                beats.lock().push(Instant::now());
                crate::capture::idle_heartbeat(&beat_shared, crate::clock::host_now_us() / 1000);
            }),
            MAIN,
        )
    }

    fn longest_gap(start: Instant, beats: &[Instant], end: Instant) -> Duration {
        std::iter::once(start)
            .chain(beats.iter().copied())
            .chain(std::iter::once(end))
            .collect::<Vec<_>>()
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .max()
            .unwrap()
    }

    #[test]
    fn a_new_display_is_captured_once_screencapturekit_lists_it() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, _ops) = fake_owner(&shared);
        let lister = FakeLister::after(Some(3));
        let checks = lister.checks.clone();
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
            }),
            lister,
        );
        connect(&shared, 0xd0, (2670, 1878));
        let start = Instant::now();
        let id = targets.display(start);
        assert_ne!(id, MAIN);
        assert_eq!(checks.load(Ordering::SeqCst), 4);
        // Three pauses between answers, well inside the bound.
        assert!(start.elapsed() >= LIST_POLL * 3 && start.elapsed() < LIST_WAIT);
        assert_eq!(*shared.second_screen.lock(), SecondScreenStatus::Active(id));
        // Seen once, never asked again.
        targets.display(Instant::now());
        assert_eq!(checks.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_slow_lister_reaches_the_mirror_fallback_in_four_seconds_with_heartbeats() {
        let shared = SharedControl::new(0, 15_000_000);
        let machine = crate::supervisor::Machine::new(1);
        let (owner, ops) = fake_owner(&shared);
        // Every call takes 10 s, longer than the 3 s watchdog.
        let lister = Scripted {
            delay: Duration::from_secs(10),
            ..Default::default()
        };
        let beats = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mut targets = scripted_targets(&shared, &owner, &lister, beats.clone());
        connect(&shared, 0xd0, (2670, 1878));
        let start = Instant::now();
        assert_eq!(targets.display(start), MAIN);
        let took = start.elapsed();
        assert!(
            took >= LIST_WAIT && took < LIST_WAIT + Duration::from_millis(500),
            "{took:?}"
        );
        let gap = longest_gap(start, &beats.lock(), Instant::now());
        assert!(gap < Duration::from_millis(500), "heartbeat gap {gap:?}");
        assert_eq!(
            crate::supervisor::wedge_reason_at(
                &shared,
                &machine,
                crate::clock::host_now_us() / 1000
            ),
            None,
            "the watchdog sees a live stage"
        );
        let SecondScreenStatus::NotCapturable(id) = *shared.second_screen.lock() else {
            panic!("a clear status")
        };
        assert!(matches!(ops.try_recv(), Ok(Op::Create(_))));
        // The call still running is never joined by another, even when a
        // recheck falls due, and later passes do not wait for it.
        for later in [Duration::from_secs(1), LIST_RECHECK, LIST_RECHECK * 2] {
            let pass = Instant::now();
            assert_eq!(targets.display(start + later), MAIN);
            assert!(pass.elapsed() < Duration::from_millis(100));
        }
        assert_eq!(lister.checks.load(Ordering::SeqCst), 1);
        assert_eq!(lister.most_at_once.load(Ordering::SeqCst), 1);
        assert!(ops.try_recv().is_err(), "the display is kept");
        let _ = id;
    }

    #[test]
    fn a_display_screencapturekit_never_lists_falls_back_to_mirroring_and_is_kept() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner(&shared);
        let lister = Scripted::default();
        let beats = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mut targets = scripted_targets(&shared, &owner, &lister, beats);
        connect(&shared, 0xd0, (2670, 1878));
        let start = Instant::now();
        // A bounded wait, asking every LIST_POLL, then the main display.
        assert_eq!(targets.display(start), MAIN);
        assert!(start.elapsed() >= LIST_WAIT);
        // Let a call sent just before the deadline reach the lister.
        std::thread::sleep(Duration::from_millis(50));
        let asked = lister.checks.load(Ordering::SeqCst);
        // About one question per LIST_POLL plus a heartbeat slice.
        assert!((8..=17).contains(&asked), "{asked}");
        let Ok(Op::Create(_)) = ops.try_recv() else {
            panic!("the display was made")
        };
        let SecondScreenStatus::NotCapturable(id) = *shared.second_screen.lock() else {
            panic!("a clear status")
        };
        // Until the recheck, capture stays on the main display and nothing
        // is asked.
        assert_eq!(targets.display(start + Duration::from_secs(1)), MAIN);
        assert_eq!(lister.checks.load(Ordering::SeqCst), asked);
        // The recheck asks without waiting; once listed, the next pass
        // moves capture to the same display.
        lister.listed.store(true, Ordering::SeqCst);
        let pass = Instant::now();
        assert_eq!(targets.display(start + LIST_RECHECK), MAIN);
        assert!(
            pass.elapsed() < Duration::from_millis(100),
            "rechecks do not wait"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut chosen = MAIN;
        while chosen == MAIN && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            chosen = targets.display(start + LIST_RECHECK);
        }
        assert_eq!(chosen, id);
        assert_eq!(lister.checks.load(Ordering::SeqCst), asked + 1);
        assert!(ops.try_recv().is_err(), "kept, never removed or remade");
    }

    #[test]
    fn a_stream_that_cannot_find_the_virtual_display_mirrors_instead_of_failing() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, _ops) = fake_owner(&shared);
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
            }),
            FakeLister::listed(),
        );
        connect(&shared, 0xd0, (2670, 1878));
        let now = Instant::now();
        let id = targets.display(now);
        // The stream start reports the display missing: ask again, and get
        // the main display, not a pipeline failure.
        assert!(targets.not_listed(id, now));
        assert_eq!(targets.display(now), MAIN);
        assert_eq!(
            *shared.second_screen.lock(),
            SecondScreenStatus::NotCapturable(id)
        );
        // The main display missing is a real failure.
        assert!(!targets.not_listed(MAIN, now));
    }
}
