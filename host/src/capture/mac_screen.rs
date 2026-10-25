//! What the macOS host captures, when real capture is on
//! (`SettingsFile::mac_second_screen`; off by default, so nothing creates a
//! display by itself and the synthetic source stays the default).
//!
//! Second screen comes first: while a client is connected, a virtual
//! display is made (or kept) for it in its saved profile, and
//! ScreenCaptureKit captures that. With no client the display is removed,
//! as on Windows, and the main display is mirrored. If the virtual display
//! cannot be made, the main display is mirrored instead: mirror mode is the
//! fallback, not a mode of its own yet.
//!
//! Input is not wired here. Posting CGEvents waits on the transport
//! security decision (plan-v2 point 2); until then the relay keeps
//! recording, as on every non-Windows host.

use std::sync::OnceLock;
use std::time::Duration;

use objc2_core_graphics::CGMainDisplayID;
use tracing::{info, warn};

use super::FrameSlot;
use crate::control::SharedControl;
use crate::display_profile::ClientScreen;
use crate::settings::SettingsFile;
use crate::virtual_display::mac::{CgVirtualDisplays, ModeChange};
use crate::virtual_display::owner::DisplayOwner;
use crate::virtual_display::{Want, Wanted};

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
        let off = Want {
            enabled: false,
            client: None,
            grace: Duration::ZERO,
        };
        if let Err(error) = owner.want(off) {
            warn!(%error, "The second-screen display did not go away");
        }
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

/// The display to capture this generation. A restart never removes the
/// display: with no client connected the owner keeps it for the grace
/// period, and capture stays on it.
fn choose_display(shared: &SharedControl) -> u32 {
    let main = CGMainDisplayID();
    let client = {
        let session = shared.session.lock();
        if session.is_active() {
            session.client_info()
        } else {
            None
        }
    };
    let Some(owner) = owner() else {
        return main;
    };
    let mut settings = SettingsFile::load();
    let wanted = client.map(|info| {
        let client = ClientScreen {
            device_id: info.device_id,
            screen_px: info.screen_px,
            refresh_hz: info.refresh_hz,
        };
        Wanted {
            client,
            device_name: info.device_name,
            saved: settings.display_profile(client.device_id),
        }
    });
    let device_id = wanted.as_ref().map(|wanted| wanted.client.device_id);
    let want = Want {
        enabled: true,
        client: wanted,
        grace: Duration::from_secs(settings.mac_second_screen_grace_secs.into()),
    };
    match owner.want(want) {
        Ok(Some(second)) => {
            if let Some(device_id) = device_id {
                if settings.remember_display_profile(device_id, second.profile) {
                    settings.save();
                }
            }
            info!(
                id = second.id,
                bounds = ?second.bounds,
                profile = ?second.profile,
                "Capturing the second-screen display"
            );
            second.id
        }
        Ok(None) => {
            info!("No client connected; mirroring the main display until one is");
            main
        }
        Err(error) => {
            warn!(%error, "Second screen unavailable; mirroring the main display");
            main
        }
    }
}

pub(super) fn run_capture_loop(
    slot: &FrameSlot,
    shared: SharedControl,
    generation: u64,
    frame_number: &mut u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let display_id = choose_display(&shared);
    super::sck::stream::run_capture_loop(slot, shared, generation, frame_number, display_id)
}
