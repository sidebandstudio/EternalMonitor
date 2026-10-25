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

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use objc2_core_graphics::CGMainDisplayID;
use tracing::{info, warn};

use super::sck::stream::DisplayChoice;
use super::sck::CaptureFailure;
use super::FrameSlot;
use crate::control::SharedControl;
use crate::display_profile::ClientScreen;
use crate::settings::SettingsFile;
use crate::virtual_display::mac::{CgVirtualDisplays, ModeChange};
use crate::virtual_display::owner::DisplayOwner;
use crate::virtual_display::{SecondScreenStatus, Want, Wanted};

/// How long a new display may take to appear in ScreenCaptureKit's list
/// before the main display is mirrored instead (macOS 15.7 has been seen
/// not to list a virtual display at all).
const LIST_WAIT: Duration = Duration::from_secs(4);
const LIST_POLL: Duration = Duration::from_millis(250);
/// How often a display that was not listed is checked again.
const LIST_RECHECK: Duration = Duration::from_secs(5);

/// Whether ScreenCaptureKit offers a display.
pub(crate) trait ContentLister {
    fn is_listed(&self, display_id: u32) -> Result<bool, CaptureFailure>;
}

struct ScreenCaptureKitLister;

impl ContentLister for ScreenCaptureKitLister {
    fn is_listed(&self, display_id: u32) -> Result<bool, CaptureFailure> {
        super::sck::stream::display_listed(display_id)
    }
}

static OWNER: OnceLock<Option<DisplayOwner>> = OnceLock::new();

/// The process's display owner, started on first use.
fn owner() -> Option<&'static DisplayOwner> {
    OWNER
        .get_or_init(|| {
            DisplayOwner::spawn(|| CgVirtualDisplays::new(ModeChange::Recreate))
                .inspect_err(|error| warn!(%error, "Could not start the display owner"))
                .ok()
        })
        .as_ref()
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
        client: None,
        grace: Duration::ZERO,
    }
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
    fn save(&mut self, settings: &SettingsFile);
}

/// `settings.json`, re-read when its modification time moves.
#[derive(Default)]
struct SettingsOnDisk {
    seen: Option<Option<SystemTime>>,
}

impl SettingsSource for SettingsOnDisk {
    fn changed(&mut self) -> Option<SettingsFile> {
        let modified = SettingsFile::modified();
        if self.seen == Some(modified) {
            return None;
        }
        self.seen = Some(modified);
        Some(SettingsFile::load())
    }

    fn save(&mut self, settings: &SettingsFile) {
        settings.save();
        self.seen = Some(SettingsFile::modified());
    }
}

/// Picks the display to capture, every time the capture loop asks.
pub(crate) struct Targets<'a> {
    shared: SharedControl,
    owner: &'a DisplayOwner,
    settings_source: Box<dyn SettingsSource + 'a>,
    settings: SettingsFile,
    lister: Box<dyn ContentLister + 'a>,
    /// Sleeps between listing checks, keeping the stage's heartbeats.
    wait: Box<dyn FnMut(Duration) + 'a>,
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
        lister: Box<dyn ContentLister + 'a>,
        wait: Box<dyn FnMut(Duration) + 'a>,
        main: u32,
    ) -> Self {
        Self {
            shared,
            owner,
            settings_source,
            settings: SettingsFile::default(),
            lister,
            wait,
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

    /// Whether ScreenCaptureKit offers `id`: at once if it was seen before,
    /// after a bounded wait for a display it has not been asked about, and
    /// every `LIST_RECHECK` for one it did not offer.
    fn capturable(&mut self, id: u32, now: Instant) -> bool {
        if self.listed == Some(id) {
            return true;
        }
        let listed = match self.unlisted {
            Some((pending, next)) if pending == id && now < next => return false,
            Some((pending, _)) if pending == id => self.lister.is_listed(id).unwrap_or(false),
            _ => {
                let mut listed = false;
                let polls = LIST_WAIT.as_millis() / LIST_POLL.as_millis();
                for poll in 0..=polls {
                    listed = self.lister.is_listed(id).unwrap_or(false);
                    if listed || poll == polls {
                        break;
                    }
                    (self.wait)(LIST_POLL);
                }
                listed
            }
        };
        if listed {
            self.listed = Some(id);
            self.unlisted = None;
        } else {
            self.unlisted = Some((id, now + LIST_RECHECK));
        }
        listed
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
        let client = {
            let session = self.shared.session.lock();
            if session.is_active() {
                session.client_info()
            } else {
                None
            }
        };
        let wanted = client.map(|info| {
            let client = ClientScreen {
                device_id: info.device_id,
                screen_px: info.screen_px,
                refresh_hz: info.refresh_hz,
            };
            Wanted {
                client,
                device_name: info.device_name,
                saved: self.settings.display_profile(client.device_id),
            }
        });
        let device_id = wanted.as_ref().map(|wanted| wanted.client.device_id);
        let want = Want {
            enabled: true,
            client: wanted,
            grace: Duration::from_secs(self.settings.mac_second_screen_grace_secs.into()),
        };
        match self.owner.want(want) {
            Ok(Some(second)) => {
                if let Some(device_id) = device_id {
                    if self
                        .settings
                        .remember_display_profile(device_id, second.profile)
                    {
                        self.settings_source.save(&self.settings);
                    }
                }
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

impl Targets<'_> {
    /// This capture generation is over, so nothing watches the client any
    /// more: start the grace period as if it had left. A restart's next
    /// generation sees the client again well within it.
    pub(crate) fn capture_ended(&mut self) {
        if !self.settings.mac_second_screen {
            return;
        }
        let unobserved = Want {
            enabled: true,
            client: None,
            grace: Duration::from_secs(self.settings.mac_second_screen_grace_secs.into()),
        };
        if let Err(error) = self.owner.want(unobserved) {
            warn!(%error, "Could not hand the second-screen display back to the owner");
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
    let Some(owner) = owner() else {
        return super::sck::stream::run_capture_loop(
            slot,
            shared,
            generation,
            frame_number,
            &mut Fixed(main),
        );
    };
    let wait_shared = shared.clone();
    let mut targets = Targets::new(
        shared.clone(),
        owner,
        Box::new(SettingsOnDisk::default()),
        Box::new(ScreenCaptureKitLister),
        Box::new(move |duration| {
            // A bounded wait inside one capture pass: keep the stage alive.
            let until = Instant::now() + duration;
            while Instant::now() < until {
                super::idle_heartbeat(&wait_shared, crate::clock::host_now_us() / 1000);
                std::thread::sleep(Duration::from_millis(50).min(until - Instant::now()));
            }
        }),
        main,
    );
    let result =
        super::sck::stream::run_capture_loop(slot, shared, generation, frame_number, &mut targets);
    targets.capture_ended();
    result
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
    use std::sync::mpsc;

    pub(crate) const MAIN: u32 = 1;

    /// Settings held in memory; saves are counted, never written.
    pub(crate) struct MemorySettings {
        pub(crate) next: Option<SettingsFile>,
        pub(crate) saved: mpsc::Sender<SettingsFile>,
    }

    impl SettingsSource for MemorySettings {
        fn changed(&mut self) -> Option<SettingsFile> {
            self.next.take()
        }
        fn save(&mut self, settings: &SettingsFile) {
            let _ = self.saved.send(settings.clone());
        }
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

    /// A display owner over fake displays that reports what it does.
    pub(crate) fn fake_owner() -> (DisplayOwner, mpsc::Receiver<Op>) {
        use crate::virtual_display::{DisplayError, DisplaySpec, VirtualDisplay};
        let (ops_tx, ops) = mpsc::channel();
        struct Reporting(FakeDisplays, mpsc::Sender<Op>);
        impl crate::virtual_display::VirtualDisplayManager for Reporting {
            fn create(&mut self, spec: &DisplaySpec) -> Result<VirtualDisplay, DisplayError> {
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
        let owner = DisplayOwner::spawn(move || Ok(Reporting(FakeDisplays::default(), ops_tx)))
            .expect("owner thread");
        (owner, ops)
    }

    /// ScreenCaptureKit, faked: lists a display after so many checks.
    pub(crate) struct FakeLister {
        pub(crate) listed_after: Option<u32>,
        pub(crate) checks: std::rc::Rc<std::cell::Cell<u32>>,
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
            let checks = self.checks.get();
            self.checks.set(checks + 1);
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
            Box::new(|_| {}),
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
        let (owner, ops) = fake_owner();
        let (saved_tx, saved) = mpsc::channel();
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
                saved: saved_tx,
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
        // The Duo connects, folded. The next pass captures its display.
        connect(&shared, 0xd0, (1398, 2034));
        for _ in 0..3 {
            answers.push(targets.display(now));
        }
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
        assert_eq!(saved.try_iter().count(), 1);
        assert!(crate::supervisor::generation_is_current(generation));
    }

    #[test]
    fn a_changed_saved_profile_re_targets_to_the_new_display() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner();
        let mut settings = enabled();
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(settings.clone()),
                saved: mpsc::channel().0,
            }),
            FakeLister::listed(),
        );
        connect(&shared, 0xd0, (2670, 1878));
        let now = Instant::now();
        let first = targets.display(now);
        // The user saves the outer size for this device.
        settings.remember_display_profile(
            0xd0,
            crate::display_profile::DisplayProfile {
                width_px: 2034,
                height_px: 1398,
                refresh_hz: 120,
                hidpi: true,
            },
        );
        targets.settings_source = Box::new(MemorySettings {
            next: Some(settings),
            saved: mpsc::channel().0,
        });
        let second = targets.display(now);
        assert_ne!(second, first);
        assert_ne!(second, MAIN);
        let ops: Vec<_> = ops.try_iter().collect();
        assert!(
            matches!(ops[..], [Op::Create(_), Op::Remove(id), Op::Create(_)] if id == first),
            "{ops:?}"
        );
    }

    #[test]
    fn a_restart_keeps_the_display_and_a_stopped_pipeline_starts_the_grace_period() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner();
        let settings = || {
            Box::new(MemorySettings {
                next: Some(enabled()),
                saved: mpsc::channel().0,
            })
        };
        connect(&shared, 0xd0, (2670, 1878));
        let mut first = targets(&shared, &owner, settings(), FakeLister::listed());
        let id = first.display(Instant::now());
        // A restart: the generation ends, the next one sees the client.
        first.capture_ended();
        let mut second = targets(&shared, &owner, settings(), FakeLister::listed());
        assert_eq!(second.display(Instant::now()), id);
        let made: Vec<_> = ops.try_iter().collect();
        assert!(matches!(made[..], [Op::Create(_)]), "{made:?}");
        // Streaming stops: the owner's grace clock is running.
        second.capture_ended();
        assert!(ops.try_recv().is_err(), "kept for the grace period");
    }

    #[test]
    fn turning_second_screen_off_while_running_mirrors_and_removes_the_display() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner();
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
                saved: mpsc::channel().0,
            }),
            FakeLister::listed(),
        );
        connect(&shared, 0xd0, (2670, 1878));
        let virtual_id = targets.display(Instant::now());
        targets.settings_source = Box::new(MemorySettings {
            next: Some(SettingsFile::default()),
            saved: mpsc::channel().0,
        });
        assert_eq!(targets.display(Instant::now()), MAIN);
        let ops: Vec<_> = ops.try_iter().collect();
        assert!(
            matches!(ops[..], [Op::Create(_), Op::Remove(id)] if id == virtual_id),
            "{ops:?}"
        );
    }
    /// A lister whose answer can change, and the time a test has waited.
    struct Scripted {
        listed: std::rc::Rc<std::cell::Cell<bool>>,
        checks: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl ContentLister for Scripted {
        fn is_listed(&self, _display_id: u32) -> Result<bool, CaptureFailure> {
            self.checks.set(self.checks.get() + 1);
            Ok(self.listed.get())
        }
    }

    #[test]
    fn a_new_display_is_captured_once_screencapturekit_lists_it() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, _ops) = fake_owner();
        let lister = FakeLister::after(Some(3));
        let checks = lister.checks.clone();
        let waited = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
        let waited_in = waited.clone();
        let mut targets = Targets::new(
            shared.clone(),
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
                saved: mpsc::channel().0,
            }),
            Box::new(lister),
            Box::new(move |d| waited_in.set(waited_in.get() + d)),
            MAIN,
        );
        connect(&shared, 0xd0, (2670, 1878));
        let id = targets.display(Instant::now());
        assert_ne!(id, MAIN);
        assert_eq!(checks.get(), 4);
        assert_eq!(waited.get(), LIST_POLL * 3);
        assert_eq!(*shared.second_screen.lock(), SecondScreenStatus::Active(id));
        // Seen once, never asked again.
        targets.display(Instant::now());
        assert_eq!(checks.get(), 4);
    }

    #[test]
    fn a_display_screencapturekit_never_lists_falls_back_to_mirroring_and_is_kept() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, ops) = fake_owner();
        let listed = std::rc::Rc::new(std::cell::Cell::new(false));
        let checks = std::rc::Rc::new(std::cell::Cell::new(0));
        let waited = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
        let waited_in = waited.clone();
        let mut targets = Targets::new(
            shared.clone(),
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
                saved: mpsc::channel().0,
            }),
            Box::new(Scripted {
                listed: listed.clone(),
                checks: checks.clone(),
            }),
            Box::new(move |d| waited_in.set(waited_in.get() + d)),
            MAIN,
        );
        connect(&shared, 0xd0, (2670, 1878));
        let start = Instant::now();
        // A bounded wait, then the main display.
        assert_eq!(targets.display(start), MAIN);
        assert_eq!(waited.get(), LIST_WAIT);
        assert_eq!(checks.get(), 17);
        let Ok(Op::Create(_)) = ops.try_recv() else {
            panic!("the display was made")
        };
        let SecondScreenStatus::NotCapturable(id) = *shared.second_screen.lock() else {
            panic!("a clear status")
        };
        // Until the recheck, capture stays on the main display and nothing
        // waits or asks again.
        assert_eq!(targets.display(start + Duration::from_secs(1)), MAIN);
        assert_eq!(checks.get(), 17);
        // A later check finds it: capture moves to it, the same display.
        listed.set(true);
        assert_eq!(targets.display(start + LIST_RECHECK), id);
        assert_eq!(checks.get(), 18);
        assert_eq!(waited.get(), LIST_WAIT, "rechecks do not wait");
        assert!(ops.try_recv().is_err(), "kept, never removed or remade");
    }

    #[test]
    fn a_stream_that_cannot_find_the_virtual_display_mirrors_instead_of_failing() {
        let shared = SharedControl::new(0, 15_000_000);
        let (owner, _ops) = fake_owner();
        let mut targets = targets(
            &shared,
            &owner,
            Box::new(MemorySettings {
                next: Some(enabled()),
                saved: mpsc::channel().0,
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
