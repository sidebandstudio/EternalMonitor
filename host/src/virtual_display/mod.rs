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

use std::collections::BTreeMap;
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

    /// The active display and the device it was made for.
    pub fn active_for(&self) -> Option<(u64, VirtualDisplay)> {
        self.active
    }

    /// The display for `client`, made or reused. A device that reconnects keeps its display
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

/// The live session's client, as the transport holds it now, with the
/// saved profile for its device. The display owner reads this itself on
/// every turn; it never comes from a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveClient {
    pub client: ClientScreen,
    pub device_name: String,
    pub saved: Option<DisplayProfile>,
}

/// A capture pass asking for "the display for the current session, or
/// mirror".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    /// `SettingsFile::mac_second_screen`.
    pub enabled: bool,
    /// How long a display outlives its client.
    pub grace: Duration,
    /// The device this capture pass saw connected, if any. Only a hint: it
    /// must match the live session's device for the request to make or
    /// replace a display; otherwise the request is stale.
    pub device: Option<u64>,
}

/// The shortest grace period; a setting below this, including 0, is
/// raised to it, so a session that drops for a moment keeps its display.
pub const MIN_GRACE: Duration = Duration::from_secs(5);

/// How long after a first-time save that did not happen (another writer
/// held the profiles' lock) the same device's is tried again.
pub const SAVE_RETRY: Duration = Duration::from_secs(5);

/// When a display goes away. A pipeline restart or a capture retry never
/// removes it; only these do:
/// - its client's session has been gone for the grace period, counted from
///   when the transport's session state ([`Keeper::observe`]) first showed
///   no session. Capture never reports a disconnect: a connected client's
///   display never expires, whatever capture is doing or how long it pauses;
/// - second screen is turned off;
/// - the live session's device profile changes size (`SecondScreen::ensure`),
///   or another device's session goes live and capture asks for it;
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
    /// Saves the default profile of a device seen for the first time,
    /// never replacing a saved one.
    save_first: Box<dyn FnMut(u64, DisplayProfile)>,
    /// The device whose own request was served with no saved profile, and
    /// the profile its display was made in, until the store has one.
    unsaved: Option<(u64, DisplayProfile)>,
    /// When each device's first-time save may next be tried.
    next_save: BTreeMap<u64, Instant>,
}

impl<M: VirtualDisplayManager> Keeper<M> {
    pub fn new(manager: M) -> Self {
        Self {
            screen: SecondScreen::new(manager),
            gone_since: None,
            grace: MIN_GRACE,
            save_first: Box::new(|_, _| {}),
            unsaved: None,
            next_save: BTreeMap::new(),
        }
    }

    /// Saves, through `save`, the profile a device with no saved profile
    /// gets the first time its own request is served
    /// ([`Self::save_first_profile`]). Nothing else writes profiles: a
    /// saved one changes only by an explicit edit.
    pub fn saving_first_profiles(
        mut self,
        save: impl FnMut(u64, DisplayProfile) + 'static,
    ) -> Self {
        self.save_first = Box::new(save);
        self
    }

    /// One turn of the display owner, the single source of truth for
    /// which device it serves. `live` is the transport's session as it is
    /// now. It is observed first and expiry is checked against it; only
    /// then is the request (if any) answered, against `live` too. A request
    /// was snapshotted earlier and may be stale: a stale one changes
    /// nothing.
    pub fn serve(
        &mut self,
        live: Option<&LiveClient>,
        want: Option<Want>,
        now: Instant,
    ) -> Option<Result<Option<VirtualDisplay>, DisplayError>> {
        self.observe(live.is_some(), now);
        if let Err(error) = self.expire(now) {
            tracing::warn!(%error, "The second-screen display did not go away");
        }
        want.map(|want| self.answer(want, live))
    }

    /// The display to capture for the live session, or `None` to mirror.
    /// Only a request whose device matches the live session's can make or
    /// replace a display, and then only if the live device or its profile's
    /// size differs from the display's. Any other request is answered from
    /// the current state: the live device's display, or the one kept for a
    /// session that ended less than the grace period ago, or mirror (the
    /// next pass, which sees the live device, gets its display). No request
    /// expires the display: only `observe` and `expire` do. Only a matching
    /// request for a device with no saved profile marks one to be saved,
    /// its default, and the answer never waits for it.
    fn answer(
        &mut self,
        want: Want,
        live: Option<&LiveClient>,
    ) -> Result<Option<VirtualDisplay>, DisplayError> {
        self.grace = want.grace.max(MIN_GRACE);
        if !want.enabled {
            if self.screen.active().is_some() {
                tracing::info!("Second screen turned off; removing its display");
            }
            self.release()?;
            return Ok(None);
        }
        let current = self.screen.active_for();
        match live {
            Some(live) if want.device == Some(live.client.device_id) => {
                let display = self
                    .screen
                    .ensure(live.client, &live.device_name, live.saved)?;
                self.gone_since = None;
                if live.saved.is_none() {
                    self.unsaved = Some((live.client.device_id, display.profile));
                }
                Ok(Some(display))
            }
            Some(live) => {
                if want.device.is_some() {
                    tracing::debug!(
                        live = live.client.device_id,
                        "A capture request for another device is stale; changing nothing"
                    );
                }
                Ok(current
                    .filter(|(device_id, _)| *device_id == live.client.device_id)
                    .map(|(_, display)| display))
            }
            None => Ok(current.map(|(_, display)| display)),
        }
    }

    /// Saves the profile a matching request marked, after the owner has
    /// answered it. `live` is the session the turn was served against. The
    /// save is tried only while that device is live with no saved profile,
    /// and at most once every `SAVE_RETRY` per device: the store skips it
    /// rather than wait for a busy lock, and a later turn of the owner
    /// tries again. It is dropped once the store has a profile for the
    /// device (saved here, or edited meanwhile) or another device is live.
    pub fn save_first_profile(&mut self, live: Option<&LiveClient>, now: Instant) {
        let Some((device_id, profile)) = self.unsaved else {
            return;
        };
        match live {
            Some(live) if live.client.device_id == device_id && live.saved.is_none() => {}
            Some(_) => {
                self.unsaved = None;
                return;
            }
            None => return,
        }
        self.next_save.retain(|_, at| *at > now);
        if self.next_save.contains_key(&device_id) {
            return;
        }
        self.next_save.insert(device_id, now + SAVE_RETRY);
        (self.save_first)(device_id, profile);
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
    const IPAD: u64 = 7;

    fn live(client: ClientScreen) -> LiveClient {
        LiveClient {
            client,
            device_name: "Duo".into(),
            saved: None,
        }
    }

    /// A capture pass that saw `device`.
    fn saw(device: Option<&ClientScreen>, grace: Duration) -> Want {
        Want {
            enabled: true,
            grace,
            device: device.map(|client| client.device_id),
        }
    }

    /// One owner turn: the live session, a request, the display answered,
    /// then any first-time save.
    fn turn(
        keeper: &mut Keeper<FakeDisplays>,
        live: Option<&LiveClient>,
        want: Want,
        now: Instant,
    ) -> Result<Option<VirtualDisplay>, DisplayError> {
        let answer = keeper.serve(live, Some(want), now).unwrap();
        keeper.save_first_profile(live, now);
        answer
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
        let duo = live(client(DUO, (2670, 1878)));
        let display = turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), Duration::ZERO),
            start,
        )
        .unwrap()
        .unwrap();
        assert_eq!(keeper.expires_at(), None);
        // Connected: capture passes that saw no client, at any time, keep it.
        for hours in [0, 1, 24, 240] {
            let at = start + Duration::from_secs(hours * 3600);
            assert_eq!(
                turn(&mut keeper, Some(&duo), saw(None, Duration::ZERO), at),
                Ok(Some(display))
            );
        }
        assert_eq!(removals(&keeper), 0);
        // Grace 0 means the minimum, counted from the session's end.
        let gone = start + Duration::from_secs(900_000);
        keeper.serve(None, None, gone);
        assert_eq!(keeper.expires_at(), Some(gone + MIN_GRACE));
    }

    #[test]
    fn a_long_gap_between_generations_keeps_a_connected_clients_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = live(client(DUO, (2670, 1878)));
        let grace = Duration::from_secs(5);
        let display = turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), grace),
            start,
        )
        .unwrap()
        .unwrap();
        // No capture pass for 30 s; the owner's timer sees the session up.
        for second in 1..=30 {
            keeper.serve(Some(&duo), None, start + Duration::from_secs(second));
        }
        let back = start + Duration::from_secs(30);
        assert_eq!(
            turn(&mut keeper, Some(&duo), saw(Some(&duo.client), grace), back),
            Ok(Some(display))
        );
        assert_eq!(removals(&keeper), 0);
        // The session ends: removed once the grace period has passed.
        let gone = back + Duration::from_secs(1);
        keeper.serve(None, None, gone);
        keeper.serve(None, None, gone + grace - Duration::from_millis(1));
        assert_eq!(removals(&keeper), 0);
        keeper.serve(None, None, gone + grace);
        assert_eq!(removals(&keeper), 1);
    }

    #[test]
    fn grace_counts_from_the_sessions_end_and_then_removes_the_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = live(client(DUO, (2670, 1878)));
        let display = turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), GRACE),
            start,
        )
        .unwrap()
        .unwrap();
        let gone = start + Duration::from_secs(600);
        keeper.serve(None, None, gone);
        assert_eq!(keeper.expires_at(), Some(gone + GRACE));
        // During grace, capture keeps the kept display; later turns do not
        // move the start.
        assert_eq!(
            turn(
                &mut keeper,
                None,
                saw(None, GRACE),
                gone + Duration::from_secs(30)
            ),
            Ok(Some(display))
        );
        assert_eq!(keeper.expires_at(), Some(gone + GRACE));
        keeper.serve(None, None, gone + GRACE - Duration::from_millis(1));
        assert_eq!(removals(&keeper), 0);
        keeper.serve(None, None, gone + GRACE);
        assert_eq!(removals(&keeper), 1);
        assert!(keeper.screen.manager.online.is_empty());
        assert_eq!(keeper.expires_at(), None);
        assert_eq!(
            turn(&mut keeper, None, saw(None, GRACE), gone + GRACE * 2),
            Ok(None)
        );
    }

    #[test]
    fn a_reconnect_just_before_expiry_keeps_the_same_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = live(client(DUO, (2670, 1878)));
        let display = turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), GRACE),
            start,
        )
        .unwrap()
        .unwrap();
        let gone = start + Duration::from_secs(5);
        for second in 0..60 {
            let at = gone + Duration::from_secs(second);
            assert_eq!(
                turn(&mut keeper, None, saw(None, GRACE), at),
                Ok(Some(display))
            );
        }
        let back = gone + GRACE - Duration::from_millis(1);
        assert_eq!(
            turn(&mut keeper, Some(&duo), saw(Some(&duo.client), GRACE), back),
            Ok(Some(display))
        );
        assert_eq!(keeper.expires_at(), None, "connected again: disarmed");
        keeper.serve(Some(&duo), None, gone + GRACE * 2);
        assert_eq!(removals(&keeper), 0);
        assert_eq!(
            keeper.screen.manager.ops.len(),
            1,
            "one create, nothing else"
        );
    }

    #[test]
    fn a_stale_request_cannot_remove_a_reconnected_clients_display() {
        // Round 4: a pass that saw no client reaches the owner at the
        // deadline, after the client reconnected.
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = live(client(DUO, (2670, 1878)));
        let grace = Duration::from_secs(5);
        let display = turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), grace),
            start,
        )
        .unwrap()
        .unwrap();
        let gone = start + Duration::from_secs(10);
        keeper.serve(None, None, gone);
        let stale = saw(None, grace);
        assert_eq!(
            turn(&mut keeper, Some(&duo), stale, gone + grace),
            Ok(Some(display))
        );
        assert_eq!(removals(&keeper), 0);
        assert_eq!(keeper.expires_at(), None);
        // Without the reconnect, the live state expires it at the deadline.
        let mut keeper = Keeper::new(FakeDisplays::default());
        turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), grace),
            start,
        )
        .unwrap();
        keeper.serve(None, None, gone);
        assert_eq!(
            turn(&mut keeper, None, saw(None, grace), gone + grace),
            Ok(None)
        );
        assert_eq!(removals(&keeper), 1);
    }

    #[test]
    fn a_stale_request_for_another_device_cannot_replace_the_live_clients_display() {
        // Sol's sequence, round 5.
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let b = live(client(DUO, (2670, 1878)));
        let a = live(client(IPAD, (1668, 2420)));
        let grace = Duration::from_secs(60);
        let display = turn(&mut keeper, Some(&b), saw(Some(&b.client), grace), start)
            .unwrap()
            .unwrap();
        // 1. B's session ends; its display is kept during grace.
        let t = start + Duration::from_secs(10);
        keeper.serve(None, None, t);
        // 2. A connects briefly, and a capture pass snapshots A.
        let stale = saw(Some(&a.client), grace);
        // 3. A disconnects and B reconnects before that request arrives.
        // 4. It arrives with B live: nothing changes, B keeps its display.
        let answer = keeper.serve(Some(&b), Some(stale), t + Duration::from_secs(2));
        assert_eq!(answer, Some(Ok(Some(display))));
        assert_eq!(
            keeper.screen.manager.ops.len(),
            1,
            "no create, remove or change"
        );
        // B's next pass gets the same display.
        assert_eq!(
            turn(
                &mut keeper,
                Some(&b),
                saw(Some(&b.client), grace),
                t + Duration::from_secs(3)
            ),
            Ok(Some(display))
        );
        assert_eq!(keeper.screen.manager.ops.len(), 1);
    }

    #[test]
    fn a_real_device_switch_replaces_the_display_once() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let a = live(client(IPAD, (1668, 2420)));
        let b = live(client(DUO, (2670, 1878)));
        let first = turn(&mut keeper, Some(&a), saw(Some(&a.client), GRACE), start)
            .unwrap()
            .unwrap();
        // A's session ends, B's goes live. A pass that still saw A, or no
        // one, changes nothing: B has no display yet, so mirror meanwhile.
        let t = start + Duration::from_secs(5);
        assert_eq!(
            turn(&mut keeper, Some(&b), saw(Some(&a.client), GRACE), t),
            Ok(None)
        );
        assert_eq!(turn(&mut keeper, Some(&b), saw(None, GRACE), t), Ok(None));
        assert_eq!(keeper.screen.manager.ops.len(), 1);
        // B's own pass replaces A's display, once.
        let second = turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), t)
            .unwrap()
            .unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(keeper.screen.manager.ops[1], Op::Remove(first.id));
        assert!(matches!(keeper.screen.manager.ops[2], Op::Create(_)));
        for _ in 0..3 {
            assert_eq!(
                turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), t),
                Ok(Some(second))
            );
        }
        assert_eq!(keeper.screen.manager.ops.len(), 3, "replaced once");
        assert_eq!(keeper.screen.manager.online, [second]);
    }

    #[test]
    fn stale_requests_from_an_earlier_generation_never_change_anything() {
        let start = Instant::now();
        let a = live(client(IPAD, (1668, 2420)));
        let b = live(client(DUO, (2670, 1878)));
        let stale = [saw(Some(&a.client), GRACE), saw(None, GRACE)];
        // With nothing made yet, B live: no display is made for A or none.
        let mut keeper = Keeper::new(FakeDisplays::default());
        for want in stale.clone() {
            assert_eq!(turn(&mut keeper, Some(&b), want, start), Ok(None));
        }
        // With no session at all, a request that saw A makes nothing.
        assert_eq!(
            turn(&mut keeper, None, saw(Some(&a.client), GRACE), start),
            Ok(None)
        );
        assert!(keeper.screen.manager.ops.is_empty());
        // With B's display live, nothing removes, replaces or changes it.
        let display = turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), start)
            .unwrap()
            .unwrap();
        for want in stale {
            for seconds in [0, 30, 3600] {
                let at = start + Duration::from_secs(seconds);
                assert_eq!(
                    turn(&mut keeper, Some(&b), want.clone(), at),
                    Ok(Some(display))
                );
            }
        }
        assert_eq!(keeper.screen.manager.ops.len(), 1);
    }

    #[test]
    fn a_saved_profile_of_another_size_for_the_live_device_replaces_its_display() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let mut duo = live(client(DUO, (2670, 1878)));
        let first = turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), GRACE),
            start,
        )
        .unwrap()
        .unwrap();
        duo.saved = Some(DisplayProfile {
            width_px: 2034,
            height_px: 1398,
            refresh_hz: 120,
            hidpi: true,
        });
        let second = turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), GRACE),
            start,
        )
        .unwrap()
        .unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(removals(&keeper), 1);
        keeper.release().unwrap();
        assert_eq!(removals(&keeper), 2);
        assert!(keeper.screen.manager.online.is_empty());
    }

    /// The first-time profile saves a keeper made.
    type Saves = std::rc::Rc<std::cell::RefCell<Vec<(u64, DisplayProfile)>>>;

    /// A keeper that records the first-time profile saves it makes.
    fn recording() -> (Keeper<FakeDisplays>, Saves) {
        let saves = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = saves.clone();
        let keeper = Keeper::new(FakeDisplays::default())
            .saving_first_profiles(move |device, profile| log.borrow_mut().push((device, profile)));
        (keeper, saves)
    }

    #[test]
    fn only_a_new_devices_own_request_saves_a_profile_and_only_its_default() {
        let start = Instant::now();
        let (mut keeper, saves) = recording();
        let mut b = live(client(DUO, (1398, 2034)));
        let a = live(client(IPAD, (1668, 2420)));
        // Passes that do not match B's live session save nothing.
        turn(&mut keeper, Some(&b), saw(None, GRACE), start).unwrap();
        turn(&mut keeper, Some(&b), saw(Some(&a.client), GRACE), start).unwrap();
        turn(&mut keeper, None, saw(Some(&b.client), GRACE), start).unwrap();
        assert!(saves.borrow().is_empty());
        // B's own first pass saves its default, the inner size.
        let display = turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), start)
            .unwrap()
            .unwrap();
        assert_eq!(*saves.borrow(), [(DUO, display.profile)]);
        assert_eq!(
            (display.profile.width_px, display.profile.height_px),
            (2670, 1878)
        );
        // From then on the store has it: nothing more is saved, by matching
        // passes, stale ones, or idle ones during grace.
        b.saved = Some(display.profile);
        turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), start).unwrap();
        turn(&mut keeper, Some(&b), saw(Some(&a.client), GRACE), start).unwrap();
        keeper.serve(None, None, start + Duration::from_secs(1));
        assert_eq!(
            turn(
                &mut keeper,
                None,
                saw(None, GRACE),
                start + Duration::from_secs(2)
            ),
            Ok(Some(display))
        );
        assert_eq!(saves.borrow().len(), 1);
    }

    #[test]
    fn a_first_save_that_did_not_happen_is_retried_every_save_retry_until_saved() {
        let start = Instant::now();
        // The store's lock is busy: every attempt is recorded, none saves.
        let (mut keeper, attempts) = recording();
        let mut b = live(client(DUO, (1398, 2034)));
        let a = live(client(IPAD, (1668, 2420)));
        let display = turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), start)
            .unwrap()
            .unwrap();
        assert_eq!(*attempts.borrow(), [(DUO, display.profile)]);
        // Passes and idle turns within SAVE_RETRY answer as before and try
        // nothing more.
        for ms in [1, 100, 2_000, 4_999] {
            let t = start + Duration::from_millis(ms);
            assert_eq!(
                turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), t),
                Ok(Some(display))
            );
            keeper.serve(Some(&b), None, t);
            keeper.save_first_profile(Some(&b), t);
        }
        assert_eq!(attempts.borrow().len(), 1);
        // The owner's next turn after SAVE_RETRY tries again, even an idle
        // one or one answering a stale request.
        let t = start + SAVE_RETRY;
        assert_eq!(
            turn(&mut keeper, Some(&b), saw(Some(&a.client), GRACE), t),
            Ok(Some(display))
        );
        assert_eq!(attempts.borrow().len(), 2);
        keeper.serve(Some(&b), None, t + SAVE_RETRY);
        keeper.save_first_profile(Some(&b), t + SAVE_RETRY);
        assert_eq!(attempts.borrow().len(), 3);
        // Not while the session is gone; nor once the store has a profile.
        let later = t + SAVE_RETRY * 3;
        keeper.serve(None, None, later);
        keeper.save_first_profile(None, later);
        assert_eq!(attempts.borrow().len(), 3);
        b.saved = Some(display.profile);
        for n in 4..8 {
            turn(
                &mut keeper,
                Some(&b),
                saw(Some(&b.client), GRACE),
                start + SAVE_RETRY * n,
            )
            .unwrap();
        }
        assert_eq!(attempts.borrow().len(), 3);
        assert_eq!(keeper.screen.manager.ops.len(), 1, "one display throughout");
    }

    #[test]
    fn another_live_device_drops_a_first_save_that_did_not_happen() {
        let start = Instant::now();
        let (mut keeper, attempts) = recording();
        let b = live(client(DUO, (1398, 2034)));
        let a = live(client(IPAD, (1668, 2420)));
        turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), start).unwrap();
        // A takes over, with a saved profile: B's save is not tried for it.
        let a = LiveClient {
            saved: Some(profile_for(a.client, None)),
            ..a
        };
        let t = start + SAVE_RETRY;
        turn(&mut keeper, Some(&a), saw(Some(&b.client), GRACE), t).unwrap();
        keeper.serve(Some(&a), None, t + SAVE_RETRY);
        keeper.save_first_profile(Some(&a), t + SAVE_RETRY);
        assert_eq!(attempts.borrow().len(), 1);
    }

    #[test]
    fn an_edited_profile_survives_a_stale_request_and_applies_on_the_next_matching_pass() {
        // Sol's round 6, variant 1.
        let start = Instant::now();
        let (mut keeper, saves) = recording();
        let inner = profile_for(client(DUO, (2670, 1878)), None);
        let mut b = LiveClient {
            saved: Some(inner),
            ..live(client(DUO, (2670, 1878)))
        };
        let a = live(client(IPAD, (1668, 2420)));
        let first = turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), start)
            .unwrap()
            .unwrap();
        // B's saved profile is edited to the outer size; a stale A request
        // reaches the owner first and gets B's display, unchanged.
        let outer = DisplayProfile {
            width_px: 2034,
            height_px: 1398,
            ..inner
        };
        b.saved = Some(outer);
        let t = start + Duration::from_secs(1);
        assert_eq!(
            turn(&mut keeper, Some(&b), saw(Some(&a.client), GRACE), t),
            Ok(Some(first))
        );
        assert_eq!(keeper.screen.manager.ops.len(), 1);
        // B's next own pass recreates at the edited size, once.
        let second = turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), t)
            .unwrap()
            .unwrap();
        assert_eq!(second.profile, outer);
        assert_eq!(keeper.screen.manager.ops[1], Op::Remove(first.id));
        assert_eq!(
            turn(&mut keeper, Some(&b), saw(Some(&b.client), GRACE), t),
            Ok(Some(second))
        );
        assert_eq!(keeper.screen.manager.ops.len(), 3);
        assert!(
            saves.borrow().is_empty(),
            "a saved profile is never written"
        );
    }

    #[test]
    fn turning_second_screen_off_removes_the_display_at_once() {
        let start = Instant::now();
        let mut keeper = Keeper::new(FakeDisplays::default());
        let duo = live(client(DUO, (2670, 1878)));
        turn(
            &mut keeper,
            Some(&duo),
            saw(Some(&duo.client), GRACE),
            start,
        )
        .unwrap();
        let off = Want {
            enabled: false,
            ..saw(Some(&duo.client), GRACE)
        };
        assert_eq!(turn(&mut keeper, Some(&duo), off.clone(), start), Ok(None));
        assert_eq!(removals(&keeper), 1);
        assert_eq!(turn(&mut keeper, Some(&duo), off, start), Ok(None));
        assert_eq!(removals(&keeper), 1);
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
