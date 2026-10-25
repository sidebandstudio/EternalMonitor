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
    /// whatever screen it reports now, so a Duo's fold never resizes it;
    /// only a different saved profile changes its mode.
    pub fn ensure(
        &mut self,
        client: ClientScreen,
        device_name: &str,
        saved: Option<DisplayProfile>,
    ) -> Result<VirtualDisplay, DisplayError> {
        let profile = profile_for(client, saved);
        if let Some((device_id, display)) = self.active {
            if device_id == client.device_id {
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
    fn a_changed_saved_profile_changes_the_mode_even_by_recreation() {
        let mut screen = SecondScreen::new(FakeDisplays {
            recreate_on_change: true,
            ..Default::default()
        });
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
        assert_eq!(screen.active(), Some(&second));
        assert_eq!(screen.manager.ops[1], Op::ChangeMode(first.id, outer));
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

    #[test]
    fn serials_are_stable_and_never_zero() {
        assert_eq!(serial_for(DUO), serial_for(DUO));
        assert_ne!(serial_for(DUO), serial_for(DUO + 1));
        assert_eq!(serial_for(0), 1);
        assert_eq!(serial_for(0x0000_0001_0000_0001), 1);
        assert_eq!(display_name("  "), "EternalMonitor");
    }
}
