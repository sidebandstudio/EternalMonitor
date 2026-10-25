//! Second-screen virtual displays: one per connected client, made in the
//! client's saved display profile (`crate::display_profile`).
//!
//! [`VirtualDisplayManager`] creates, changes and removes displays and
//! reports their IDs and bounds. [`mac`] implements it with the private
//! `CGVirtualDisplay` API, the way Chromium's test helper and the phase 0
//! probe do. [`SecondScreen`] is the lifecycle on top: it keeps one display
//! for the connected device, reuses it when the same device reconnects (a
//! Duo folding or unfolding keeps its inner-size display), and replaces it
//! when another device takes over.
//!
//! The trait is also the seam for a display-owner helper process. macOS
//! removes a virtual display when the process that made it exits, and on
//! macOS 26 a display whose mode changed is reported to go away only that
//! way. If the phase 0 gate (spike c) confirms it, a restartable helper
//! owns the displays and an implementation of this trait forwards each call
//! to it; nothing above the trait changes. The Windows host's virtual
//! display (`vdd.rs`) is a driver that is switched on and then found among
//! the outputs, with no display ID or mode at creation, so it stays on its
//! own path.

#[cfg(target_os = "macos")]
pub mod mac;
pub mod owner;

use std::time::{Duration, Instant};

use crate::display_profile::{profile_for, ClientScreen, DisplayProfile};
use crate::input::mac::display::DisplayBounds;

/// "EM": the vendor ID every EternalMonitor virtual display reports.
pub const VENDOR_ID: u32 = 0x454d;
/// The host's product ID. The phase 0 probe uses 0x0d00, so its saved
/// arrangement never applies to the host's displays.
pub const PRODUCT_ID: u32 = 0x0d01;

/// What to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySpec {
    pub name: String,
    /// With the vendor and product IDs, this is the identity macOS keys a
    /// display's saved arrangement by, so it is stable per device.
    pub serial: u32,
    pub profile: DisplayProfile,
}

/// A display that exists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualDisplay {
    /// `CGDirectDisplayID`.
    pub id: u32,
    pub serial: u32,
    pub profile: DisplayProfile,
    /// In global points, as placed when it came online.
    pub bounds: DisplayBounds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayError {
    /// The private API is missing or changed shape on this macOS.
    Unavailable(String),
    CreateFailed(String),
    /// The display was made but its modes were refused.
    ModeRejected(String),
    /// The display did not come online in time.
    NotOnline(u32),
    /// The display was released but is still online.
    StillOnline(u32),
}

impl std::fmt::Display for DisplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => write!(f, "virtual displays are unavailable: {why}"),
            Self::CreateFailed(why) => write!(f, "virtual display creation failed: {why}"),
            Self::ModeRejected(why) => write!(f, "virtual display mode rejected: {why}"),
            Self::NotOnline(id) => write!(f, "virtual display {id} did not come online"),
            Self::StillOnline(id) => {
                write!(f, "virtual display {id} is still online after removal")
            }
        }
    }
}

impl std::error::Error for DisplayError {}

pub trait VirtualDisplayManager {
    fn create(&mut self, spec: &DisplaySpec) -> Result<VirtualDisplay, DisplayError>;
    /// Put `display` in `profile`. The result may be a new display (a new
    /// ID) if the implementation recreates rather than switches modes.
    fn change_mode(
        &mut self,
        display: &VirtualDisplay,
        profile: DisplayProfile,
    ) -> Result<VirtualDisplay, DisplayError>;
    fn remove(&mut self, display: &VirtualDisplay) -> Result<(), DisplayError>;
    /// The display's bounds now; the user may have moved it. `None` once it
    /// is gone.
    fn bounds(&self, display: &VirtualDisplay) -> Option<DisplayBounds>;
}

/// A stable, non-zero serial for a device.
pub fn serial_for(device_id: u64) -> u32 {
    match (device_id ^ (device_id >> 32)) as u32 {
        0 => 1,
        serial => serial,
    }
}

/// The display name macOS shows in Displays settings.
pub fn display_name(device_name: &str) -> String {
    let device = device_name.trim();
    if device.is_empty() {
        "EternalMonitor".to_string()
    } else {
        format!("EternalMonitor ({device})")
    }
}

/// One second-screen display for the connected client.
pub struct SecondScreen<M: VirtualDisplayManager> {
    manager: M,
    active: Option<(u64, VirtualDisplay)>,
}

impl<M: VirtualDisplayManager> SecondScreen<M> {
    pub fn new(manager: M) -> Self {
        Self {
            manager,
            active: None,
        }
    }

    pub fn active(&self) -> Option<&VirtualDisplay> {
        self.active.as_ref().map(|(_, display)| display)
    }

    /// The display for `client`, made or reused, and the profile it uses
    /// (for the caller to save). A device that reconnects keeps its display
    /// whatever screen it reports now, so a Duo's fold never resizes it.
    /// Only a different saved profile changes it: a new refresh rate is a
    /// mode change, and a new size replaces the display.
    pub fn ensure(
        &mut self,
        client: ClientScreen,
        device_name: &str,
        saved: Option<DisplayProfile>,
    ) -> Result<VirtualDisplay, DisplayError> {
        let profile = profile_for(client, saved);
        if let Some((device_id, display)) = self.active {
            let same_size = (
                display.profile.width_px,
                display.profile.height_px,
                display.profile.hidpi,
            ) == (profile.width_px, profile.height_px, profile.hidpi);
            if device_id == client.device_id && same_size {
                let display = if display.profile == profile {
                    display
                } else {
                    self.manager.change_mode(&display, profile)?
                };
                let display = VirtualDisplay {
                    bounds: self.manager.bounds(&display).unwrap_or(display.bounds),
                    ..display
                };
                self.active = Some((device_id, display));
                return Ok(display);
            }
            self.release()?;
        }
        let display = self.manager.create(&DisplaySpec {
            name: display_name(device_name),
            serial: serial_for(client.device_id),
            profile,
        })?;
        self.active = Some((client.device_id, display));
        Ok(display)
    }

    /// Remove the display, when its client is gone for good or the host
    /// quits. The display is forgotten even if removal fails: there is
    /// nothing more this process can do for it.
    pub fn release(&mut self) -> Result<(), DisplayError> {
        match self.active.take() {
            Some((_, display)) => self.manager.remove(&display),
            None => Ok(()),
        }
    }
}

/// What the second screen is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SecondScreenStatus {
    /// Second screen is off, or nothing has asked yet.
    #[default]
    Off,
    /// No client: the main display is mirrored.
    Mirroring,
    /// Capturing this virtual display.
    Active(u32),
    /// The display exists but ScreenCaptureKit does not offer it yet. The
    /// main display is mirrored meanwhile and the display is kept, so a
    /// later check can switch to it.
    NotCapturable(u32),
    /// The display could not be made; the main display is mirrored.
    Unavailable(String),
}

/// A client asking for its display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub client: ClientScreen,
    pub device_name: String,
    pub saved: Option<DisplayProfile>,
}

/// What capture currently wants from the second screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    /// `SettingsFile::mac_second_screen`.
    pub enabled: bool,
    /// The connected client, if one is.
    pub client: Option<Wanted>,
    /// How long a display outlives its client.
    pub grace: Duration,
}

/// The shortest grace period; a setting below this, including 0, is
/// raised to it, so a session that drops for a moment keeps its display.
pub const MIN_GRACE: Duration = Duration::from_secs(5);

/// When a display goes away. A pipeline restart or a capture retry never
/// removes it; only these do:
/// - its client's session has been gone for the grace period, counted from
///   when the transport's session state ([`Keeper::observe`]) first showed
///   no session. Capture only ever reports presence: a connected client's
///   display never expires, whatever capture is doing or how long it pauses;
/// - second screen is turned off;
/// - the device's profile changes size (`SecondScreen::ensure`), or
///   another device takes over;
/// - the host quits (`release`).
///
/// A client that reconnects within the grace period gets the same display,
/// so its windows stay where they were.
pub struct Keeper<M: VirtualDisplayManager> {
    screen: SecondScreen<M>,
    /// When the active display's client was first seen gone; `None` while
    /// it is connected.
    gone_since: Option<Instant>,
    grace: Duration,
}

impl<M: VirtualDisplayManager> Keeper<M> {
    pub fn new(manager: M) -> Self {
        Self {
            screen: SecondScreen::new(manager),
            gone_since: None,
            grace: MIN_GRACE,
        }
    }

    /// One turn of the display owner. The live session state is observed
    /// first and expiry is checked against it; only then is the request
    /// (if any) answered. A request was snapshotted earlier and may be
    /// stale: on its own it can never remove the display.
    pub fn serve(
        &mut self,
        session_active: bool,
        want: Option<Want>,
        now: Instant,
    ) -> Option<Result<Option<VirtualDisplay>, DisplayError>> {
        self.observe(session_active, now);
        if let Err(error) = self.expire(now) {
            tracing::warn!(%error, "The second-screen display did not go away");
        }
        want.map(|want| self.want(want))
    }

    /// The display to capture: the client's, or the one kept for a client
    /// whose session ended less than the grace period ago. `None` means
    /// mirror. Never expires the display: a request without a client only
    /// says that capture pass saw none.
    pub fn want(&mut self, want: Want) -> Result<Option<VirtualDisplay>, DisplayError> {
        self.grace = want.grace.max(MIN_GRACE);
        if !want.enabled {
            if self.screen.active().is_some() {
                tracing::info!("Second screen turned off; removing its display");
            }
            self.release()?;
            return Ok(None);
        }
        if let Some(wanted) = want.client {
            let display = self
                .screen
                .ensure(wanted.client, &wanted.device_name, wanted.saved)?;
            self.gone_since = None;
            return Ok(Some(display));
        }
        // No client in this request is not news of a disconnect: only
        // `observe` starts the grace period, and only `expire` ends it.
        Ok(self.screen.active().copied())
    }

    /// The transport's session state. An active session keeps the display;
    /// the first observation without one starts the grace period.
    pub fn observe(&mut self, session_active: bool, now: Instant) {
        if session_active {
            self.gone_since = None;
        } else if self.screen.active().is_some() && self.gone_since.is_none() {
            tracing::info!(grace = ?self.grace, "Second-screen session gone; keeping its display for the grace period");
            self.gone_since = Some(now);
        }
    }

    /// Removes the display once its client has been gone for the grace
    /// period. The owner calls this on its own timer too, so a display is
    /// removed on time even while no pipeline runs.
    pub fn expire(&mut self, now: Instant) -> Result<(), DisplayError> {
        match self.expires_at() {
            Some(at) if now >= at => {
                tracing::info!(grace = ?self.grace, "Client gone for the grace period; removing its display");
                self.release()
            }
            _ => Ok(()),
        }
    }

    /// When the active display will be removed if no client comes back;
    /// `None` while its client is connected.
    pub fn expires_at(&self) -> Option<Instant> {
        self.screen.active()?;
        self.gone_since.map(|at| at + self.grace)
    }

    pub fn release(&mut self) -> Result<(), DisplayError> {
        self.gone_since = None;
        self.screen.release()
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A display manager that only keeps books.
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Op {
        Create(DisplaySpec),
        ChangeMode(u32, DisplayProfile),
        Remove(u32),
    }

    #[derive(Debug, Default)]
    pub struct FakeDisplays {
        pub ops: Vec<Op>,
        pub online: Vec<VirtualDisplay>,
        pub next_id: u32,
        pub fail_create: Option<DisplayError>,
        pub fail_remove: bool,
        /// Change mode by destroying and recreating, as macOS 26 needs.
        pub recreate_on_change: bool,
    }

    impl FakeDisplays {
        fn bounds_for(profile: DisplayProfile) -> DisplayBounds {
            let (w, h) = profile.points();
            // Right of a 3440-pt main display.
            DisplayBounds {
                x: 3440.0,
                y: 0.0,
                width: f64::from(w),
                height: f64::from(h),
            }
        }
    }

    impl VirtualDisplayManager for FakeDisplays {
        fn create(&mut self, spec: &DisplaySpec) -> Result<VirtualDisplay, DisplayError> {
            self.ops.push(Op::Create(spec.clone()));
            if let Some(error) = self.fail_create.clone() {
                return Err(error);
            }
            self.next_id += 1;
            let display = VirtualDisplay {
                id: 100 + self.next_id,
                serial: spec.serial,
                profile: spec.profile,
                bounds: Self::bounds_for(spec.profile),
            };
            self.online.push(display);
            Ok(display)
        }

        fn change_mode(
            &mut self,
            display: &VirtualDisplay,
            profile: DisplayProfile,
        ) -> Result<VirtualDisplay, DisplayError> {
            self.ops.push(Op::ChangeMode(display.id, profile));
            let slot = self
                .online
                .iter_mut()
                .find(|online| online.id == display.id)
                .ok_or(DisplayError::NotOnline(display.id))?;
            if self.recreate_on_change {
                self.next_id += 1;
                slot.id = 100 + self.next_id;
            }
            slot.profile = profile;
            slot.bounds = Self::bounds_for(profile);
            Ok(*slot)
        }

        fn remove(&mut self, display: &VirtualDisplay) -> Result<(), DisplayError> {
            self.ops.push(Op::Remove(display.id));
            if self.fail_remove {
                return Err(DisplayError::StillOnline(display.id));
            }
            self.online.retain(|online| online.id != display.id);
            Ok(())
        }

        fn bounds(&self, display: &VirtualDisplay) -> Option<DisplayBounds> {
            self.online
                .iter()
                .find(|online| online.id == display.id)
                .map(|online| online.bounds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeDisplays, Op};
    use super::*;

    const DUO: u64 = 0x0123_4567_89ab_cdef;

    fn client(device_id: u64, screen_px: (u16, u16)) -> ClientScreen {
        ClientScreen {
            device_id,
            screen_px,
            refresh_hz: 120,
        }
    }

    #[test]
    fn a_folded_first_connection_makes_the_inner_display_and_a_fold_keeps_it() {
        let mut screen = SecondScreen::new(FakeDisplays::default());
        let display = screen
            .ensure(client(DUO, (1398, 2034)), "Ali's Duo", None)
            .unwrap();
        assert_eq!(
            (display.profile.width_px, display.profile.height_px),
            (2670, 1878)
        );
        assert_eq!(
            (display.bounds.width, display.bounds.height),
            (1335.0, 939.0)
        );
        // Unfolded, then folded again: same display, no mode change.
        for screen_px in [(1878, 2670), (1398, 2034), (2034, 1398)] {
            assert_eq!(
                screen.ensure(client(DUO, screen_px), "Ali's Duo", None),
                Ok(display)
            );
        }
        assert_eq!(
            screen.manager.ops,
            [Op::Create(DisplaySpec {
                name: "EternalMonitor (Ali's Duo)".into(),
                serial: serial_for(DUO),
                profile: display.profile,
            })]
        );
    }

    #[test]
    fn a_moved_display_reports_its_new_bounds() {
        let mut screen = SecondScreen::new(FakeDisplays::default());
        let display = screen.ensure(client(DUO, (2670, 1878)), "", None).unwrap();
        screen.manager.online[0].bounds.x = -1335.0;
        let again = screen.ensure(client(DUO, (2670, 1878)), "", None).unwrap();
        assert_eq!(again.id, display.id);
        assert_eq!(again.bounds.x, -1335.0);
    }

    #[test]
    fn a_changed_saved_refresh_changes_the_mode_even_by_recreation() {
        let mut screen = SecondScreen::new(FakeDisplays {
            recreate_on_change: true,
            ..Default::default()
        });
        let first = screen.ensure(client(DUO, (2670, 1878)), "", None).unwrap();
        let at_60 = DisplayProfile {
            refresh_hz: 60,
            ..first.profile
        };
        let second = screen
            .ensure(client(DUO, (2670, 1878)), "", Some(at_60))
            .unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(second.profile, at_60);
        assert_eq!(screen.active(), Some(&second));
        assert_eq!(screen.manager.ops[1], Op::ChangeMode(first.id, at_60));
    }

    #[test]
    fn a_saved_profile_of_another_size_replaces_the_display() {
        let mut screen = SecondScreen::new(FakeDisplays::default());
        let first = screen.ensure(client(DUO, (2670, 1878)), "", None).unwrap();
        let outer = DisplayProfile {
            width_px: 2034,
            height_px: 1398,
            refresh_hz: 60,
            hidpi: true,
        };
        let second = screen
            .ensure(client(DUO, (2670, 1878)), "", Some(outer))
            .unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(second.profile, outer);
        assert_eq!(screen.manager.ops[1], Op::Remove(first.id));
        assert!(matches!(screen.manager.ops[2], Op::Create(_)));
        assert_eq!(screen.manager.online, [second]);
    }

    #[test]
    fn another_device_replaces_the_display_and_release_removes_it() {
        let mut screen = SecondScreen::new(FakeDisplays::default());
        let duo = screen.ensure(client(DUO, (2670, 1878)), "", None).unwrap();
        let ipad = screen
            .ensure(client(7, (1668, 2420)), "iPad", None)
            .unwrap();
        assert_ne!(duo.id, ipad.id);
        assert_eq!(screen.manager.ops[1], Op::Remove(duo.id));
        assert_eq!(screen.manager.online, [ipad]);
        screen.release().unwrap();
        assert!(screen.manager.online.is_empty());
        assert!(screen.active().is_none());
        // Releasing twice does nothing.
        screen.release().unwrap();
        assert_eq!(screen.manager.ops.len(), 4);
    }

    #[test]
    fn failures_are_reported_and_leave_nothing_active() {
        let mut screen = SecondScreen::new(FakeDisplays {
            fail_create: Some(DisplayError::Unavailable("no CGVirtualDisplay".into())),
            ..Default::default()
        });
        assert!(matches!(
            screen.ensure(client(DUO, (2670, 1878)), "", None),
            Err(DisplayError::Unavailable(_))
        ));
        assert!(screen.active().is_none());

        let mut screen = SecondScreen::new(FakeDisplays {
            fail_remove: true,
            ..Default::default()
        });
        let display = screen.ensure(client(DUO, (2670, 1878)), "", None).unwrap();
        assert_eq!(screen.release(), Err(DisplayError::StillOnline(display.id)));
        assert!(
            screen.active().is_none(),
            "forgotten even when removal fails"
        );
        // A failed removal while switching devices stops the switch.
        let mut screen = SecondScreen::new(FakeDisplays {
            fail_remove: true,
            ..Default::default()
        });
        screen.ensure(client(DUO, (2670, 1878)), "", None).unwrap();
        assert!(screen.ensure(client(7, (1668, 2420)), "", None).is_err());
    }

    const GRACE: Duration = Duration::from_secs(60);

    fn want(client: Option<ClientScreen>) -> Want {
        want_with(client, GRACE)
    }

    fn want_with(client: Option<ClientScreen>, grace: Duration) -> Want {
        Want {
            enabled: true,
            client: client.map(|client| Wanted {
                client,
                device_name: "Duo".into(),
                saved: None,
            }),
            grace,
        }
    }

    fn removals(keeper: &Keeper<FakeDisplays>) -> usize {
        keeper
            .screen
            .manager
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Remove(_)))
            .count()
    }

    #[test]
    fn a_connected_clients_display_never_expires_even_with_grace_0() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        let display = keeper
            .want(want_with(Some(duo), Duration::ZERO))
            .unwrap()
            .unwrap();
        // Connected: no expiry is armed, whatever time passes and whatever
        // capture reports (a pass without the client is not a disconnect).
        assert_eq!(keeper.expires_at(), None);
        for hours in [0, 1, 24] {
            let at = start + Duration::from_secs(hours * 3600);
            keeper.observe(true, at);
            assert_eq!(
                keeper.want(want_with(None, Duration::ZERO)),
                Ok(Some(display))
            );
            keeper.expire(at).unwrap();
        }
        assert_eq!(removals(&keeper), 0);
        // Grace 0 means the minimum, counted from the session's end.
        let gone = start + Duration::from_secs(90_000);
        keeper.observe(false, gone);
        assert_eq!(keeper.expires_at(), Some(gone + MIN_GRACE));
    }

    #[test]
    fn a_long_gap_between_generations_keeps_a_connected_clients_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        let grace = Duration::from_secs(5);
        let display = keeper.want(want_with(Some(duo), grace)).unwrap().unwrap();
        // Capture ends and no generation asks for 30 s, while the session
        // stays up: the owner keeps observing it every second.
        for second in 1..=30 {
            let at = start + Duration::from_secs(second);
            keeper.observe(true, at);
            keeper.expire(at).unwrap();
        }
        let back = start + Duration::from_secs(30);
        assert_eq!(keeper.want(want_with(Some(duo), grace)), Ok(Some(display)));
        assert_eq!(removals(&keeper), 0);
        // The session ends: removed once the grace period has passed.
        let gone = back + Duration::from_secs(1);
        keeper.observe(false, gone);
        keeper
            .expire(gone + grace - Duration::from_millis(1))
            .unwrap();
        assert_eq!(removals(&keeper), 0);
        keeper.expire(gone + grace).unwrap();
        assert_eq!(removals(&keeper), 1);
    }

    #[test]
    fn a_slow_check_while_connected_keeps_the_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        let display = keeper.want(want(Some(duo))).unwrap().unwrap();
        let late = start + GRACE * 3;
        keeper.expire(late).unwrap();
        assert_eq!(keeper.want(want(Some(duo))), Ok(Some(display)));
        assert_eq!(removals(&keeper), 0);
    }

    #[test]
    fn grace_counts_from_the_sessions_end_and_then_removes_the_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        keeper.want(want(Some(duo))).unwrap();
        let gone = start + Duration::from_secs(600);
        keeper.observe(true, gone - Duration::from_secs(1));
        keeper.observe(false, gone);
        assert_eq!(keeper.expires_at(), Some(gone + GRACE));
        // Later observations without a session do not move the start.
        keeper.observe(false, gone + Duration::from_secs(30));
        keeper.want(want(None)).unwrap();
        assert_eq!(keeper.expires_at(), Some(gone + GRACE));
        keeper
            .expire(gone + GRACE - Duration::from_millis(1))
            .unwrap();
        assert_eq!(removals(&keeper), 0);
        keeper.expire(gone + GRACE).unwrap();
        assert_eq!(removals(&keeper), 1);
        assert!(keeper.screen.manager.online.is_empty());
        assert_eq!(keeper.expires_at(), None);
        assert_eq!(keeper.want(want(None)), Ok(None));
    }

    #[test]
    fn a_reconnect_just_before_expiry_keeps_the_same_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        let display = keeper.want(want(Some(duo))).unwrap().unwrap();
        let gone = start + Duration::from_secs(5);
        keeper.observe(false, gone);
        for second in 0..60 {
            let at = gone + Duration::from_secs(second);
            keeper.observe(false, at);
            assert_eq!(keeper.want(want(None)), Ok(Some(display)));
            keeper.expire(at).unwrap();
        }
        let back = gone + GRACE - Duration::from_millis(1);
        keeper.observe(true, back);
        assert_eq!(keeper.want(want(Some(duo))), Ok(Some(display)));
        assert_eq!(keeper.expires_at(), None, "connected again: disarmed");
        keeper.expire(gone + GRACE * 2).unwrap();
        assert_eq!(removals(&keeper), 0);
        assert_eq!(
            keeper.screen.manager.ops.len(),
            1,
            "one create, nothing else"
        );
        let again = back + Duration::from_secs(10);
        keeper.observe(false, again);
        assert_eq!(keeper.expires_at(), Some(again + GRACE));
    }

    #[test]
    fn a_stale_request_cannot_remove_a_reconnected_clients_display() {
        // Sol's sequence, through the owner's turn (`serve`).
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        let grace = Duration::from_secs(5);
        let display = keeper
            .serve(true, Some(want_with(Some(duo), grace)), start)
            .unwrap()
            .unwrap()
            .unwrap();
        // The session drops; the owner's timer observes it.
        let gone = start + Duration::from_secs(10);
        keeper.serve(false, None, gone);
        assert_eq!(keeper.expires_at(), Some(gone + grace));
        // 1. A capture pass snapshots no client and queues its request.
        let stale = want_with(None, grace);
        // 2. The client reconnects within the grace period (nothing has
        //    observed it yet).
        // 3. The stale request reaches the owner exactly at the deadline.
        let answer = keeper.serve(true, Some(stale), gone + grace);
        // 4. The live session was observed first: the display survives.
        assert_eq!(answer, Some(Ok(Some(display))));
        assert_eq!(removals(&keeper), 0);
        assert_eq!(keeper.expires_at(), None);
        // And the next capture pass with the client gets the same ID.
        assert_eq!(
            keeper.serve(true, Some(want_with(Some(duo), grace)), gone + grace * 2),
            Some(Ok(Some(display)))
        );
        assert_eq!(
            keeper.screen.manager.ops.len(),
            1,
            "one create, nothing else"
        );
        // Without the reconnect, the same request at the deadline finds the
        // session still gone, and the live state expires the display.
        let mut keeper = Keeper::new(FakeDisplays::default());
        keeper.serve(true, Some(want_with(Some(duo), grace)), start);
        keeper.serve(false, None, gone);
        assert_eq!(
            keeper.serve(false, Some(want_with(None, grace)), gone + grace),
            Some(Ok(None))
        );
        assert_eq!(removals(&keeper), 1);
    }

    #[test]
    fn a_request_without_a_client_never_removes_the_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        let display = keeper.want(want(Some(duo))).unwrap().unwrap();
        // Session connected: any number of passes without a client, at any
        // time, through the owner's turn.
        for hours in [0, 1, 24, 240] {
            let at = start + Duration::from_secs(hours * 3600);
            assert_eq!(
                keeper.serve(true, Some(want(None)), at),
                Some(Ok(Some(display)))
            );
        }
        // Even with the grace period armed and long past, a request alone
        // does not remove it; only expiry from the live state does.
        keeper.observe(false, start);
        for _ in 0..3 {
            assert_eq!(keeper.want(want(None)), Ok(Some(display)));
        }
        assert_eq!(removals(&keeper), 0);
        keeper.expire(start + GRACE).unwrap();
        assert_eq!(removals(&keeper), 1);
    }

    #[test]
    fn turning_second_screen_off_removes_the_display_at_once() {
        let mut keeper = Keeper::new(FakeDisplays::default());
        keeper.want(want(Some(client(DUO, (2670, 1878))))).unwrap();
        let off = Want {
            enabled: false,
            ..want(Some(client(DUO, (2670, 1878))))
        };
        assert_eq!(keeper.want(off.clone()), Ok(None));
        assert_eq!(removals(&keeper), 1);
        assert_eq!(keeper.want(off), Ok(None));
        assert_eq!(removals(&keeper), 1);
    }

    #[test]
    fn a_size_change_and_quitting_remove_the_display() {
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = client(DUO, (2670, 1878));
        let first = keeper.want(want(Some(duo))).unwrap().unwrap();
        let outer = Want {
            client: Some(Wanted {
                client: duo,
                device_name: "Duo".into(),
                saved: Some(DisplayProfile {
                    width_px: 2034,
                    height_px: 1398,
                    refresh_hz: 120,
                    hidpi: true,
                }),
            }),
            ..want(None)
        };
        let second = keeper.want(outer).unwrap().unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(removals(&keeper), 1);
        keeper.release().unwrap();
        assert_eq!(removals(&keeper), 2);
        assert!(keeper.screen.manager.online.is_empty());
    }

    #[test]
    fn serials_are_stable_and_never_zero() {
        assert_eq!(serial_for(DUO), serial_for(DUO));
        assert_ne!(serial_for(DUO), serial_for(DUO + 1));
        assert_eq!(serial_for(0), 1);
        assert_eq!(serial_for(0x0000_0001_0000_0001), 1);
        assert_eq!(display_name("  "), "EternalMonitor");
    }
}
