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

use objc2_core_graphics::CGMainDisplayID;
use tracing::{info, warn};

use super::FrameSlot;
use crate::control::SharedControl;
use crate::display_profile::ClientScreen;
use crate::settings::SettingsFile;
use crate::virtual_display::mac::{CgVirtualDisplays, ModeChange};
use crate::virtual_display::owner::DisplayOwner;

/// The process's display owner, started on first use.
fn owner() -> Option<&'static DisplayOwner> {
    static OWNER: OnceLock<Option<DisplayOwner>> = OnceLock::new();
    OWNER
        .get_or_init(|| {
            DisplayOwner::spawn(|| CgVirtualDisplays::new(ModeChange::Recreate))
                .inspect_err(|error| warn!(%error, "Could not start the display owner"))
                .ok()
        })
        .as_ref()
}

/// The display to capture this generation.
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
    let Some(info) = client else {
        if let Err(error) = owner.release() {
            warn!(%error, "The second-screen display did not go away");
        }
        info!("No client connected; mirroring the main display until one is");
        return main;
    };
    let client = ClientScreen {
        device_id: info.device_id,
        screen_px: info.screen_px,
        refresh_hz: info.refresh_hz,
    };
    let mut settings = SettingsFile::load();
    match owner.ensure(
        client,
        &info.device_name,
        settings.display_profile(client.device_id),
    ) {
        Ok(second) => {
            if settings.remember_display_profile(client.device_id, second.profile) {
                settings.save();
            }
            info!(
                id = second.id,
                bounds = ?second.bounds,
                profile = ?second.profile,
                "Capturing the second-screen display"
            );
            second.id
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
