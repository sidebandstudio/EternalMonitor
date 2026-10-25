//! The host-side display profile saved per client device: the mode a
//! second-screen virtual display is created with for that device.
//!
//! The iPhone Duo is the reason it exists. Its inner screen is 2670×1878
//! landscape and its outer screen 1398×2034, and HELLO2 reports whichever
//! is showing when it connects. The Mac display always takes the inner
//! size, even on a first connection made folded, and keeps it across fold
//! changes: the client letterboxes it onto the outer screen
//! (`fit` below) instead of the host resizing the display, which would
//! move the user's windows (and on macOS 26 may leave a display that can
//! no longer be removed). See `~/Services/fleet/tasks/mac-host/plan-v2.md`,
//! point 5.
//!
//! A saved profile wins over the size the client reports, so a device keeps
//! the display its user last had. Profiles live in `settings.json`
//! (`SettingsFile::display_profiles`).

use serde::{Deserialize, Serialize};

/// The Duo's inner screen, landscape.
pub const DUO_INNER: (u32, u32) = (2670, 1878);
/// The Duo's outer screen, portrait-native as the client reports it.
pub const DUO_OUTER: (u32, u32) = (1398, 2034);

/// A display mode in pixels. With `hidpi` the desktop is half that in
/// points (a @2x Retina display), so the pixel sizes are kept even.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayProfile {
    pub width_px: u32,
    pub height_px: u32,
    pub refresh_hz: u32,
    pub hidpi: bool,
}

impl DisplayProfile {
    /// The desktop size in points.
    pub fn points(&self) -> (u32, u32) {
        if self.hidpi {
            (self.width_px / 2, self.height_px / 2)
        } else {
            (self.width_px, self.height_px)
        }
    }
}

/// What a client said about its screen in HELLO2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientScreen {
    pub device_id: u64,
    pub screen_px: (u16, u16),
    pub refresh_hz: u8,
}

/// Whether a reported screen is either of the Duo's, in either orientation.
pub fn is_duo_screen(screen_px: (u16, u16)) -> bool {
    let (w, h) = (u32::from(screen_px.0), u32::from(screen_px.1));
    [DUO_INNER, DUO_OUTER]
        .iter()
        .any(|&(a, b)| (w, h) == (a, b) || (w, h) == (b, a))
}

/// The mode to create for `client`: its saved profile if it has one, the
/// Duo's inner size for a Duo whichever screen it reports, and otherwise
/// the client's own panel in landscape. The refresh is the client's (60
/// when it reports none); 120 Hz is best effort, so the display always
/// offers 60 too (`fallback_refresh`).
pub fn profile_for(client: ClientScreen, saved: Option<DisplayProfile>) -> DisplayProfile {
    if let Some(saved) = saved {
        return saved;
    }
    let refresh_hz = match client.refresh_hz {
        0 => 60,
        hz => u32::from(hz),
    };
    let (width_px, height_px) = if is_duo_screen(client.screen_px) {
        DUO_INNER
    } else {
        let (w, h) = (u32::from(client.screen_px.0), u32::from(client.screen_px.1));
        (w.max(h) & !1, w.min(h) & !1)
    };
    DisplayProfile {
        width_px,
        height_px,
        refresh_hz,
        hidpi: true,
    }
}

/// The rate every display also offers, so a 120 Hz mode that does not hold
/// has a 60 Hz one beside it.
pub fn fallback_refresh(profile: DisplayProfile) -> Option<u32> {
    (profile.refresh_hz != 60).then_some(60)
}

/// How the client shows a display whose shape differs from its screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitMode {
    /// All of the desktop, with bars.
    Fit,
    /// The whole screen, cropping the desktop.
    Fill,
}

/// Where a `content` picture lands on a `screen`, both in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub scale: f64,
    /// Unused screen, summed over both sides, on each axis (Fit).
    pub bars: (f64, f64),
    /// Desktop pixels cut off, summed over both sides, on each axis (Fill).
    pub cropped: (f64, f64),
}

pub fn fit(content: (u32, u32), screen: (u32, u32), mode: FitMode) -> Placement {
    let (cw, ch) = (f64::from(content.0), f64::from(content.1));
    let (sw, sh) = (f64::from(screen.0), f64::from(screen.1));
    let (sx, sy) = (sw / cw, sh / ch);
    let scale = match mode {
        FitMode::Fit => sx.min(sy),
        FitMode::Fill => sx.max(sy),
    };
    let (shown_w, shown_h) = (cw * scale, ch * scale);
    Placement {
        scale,
        bars: ((sw - shown_w).max(0.0), (sh - shown_h).max(0.0)),
        cropped: (
            ((shown_w - sw) / scale).max(0.0),
            ((shown_h - sh) / scale).max(0.0),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(screen_px: (u16, u16), refresh_hz: u8) -> ClientScreen {
        ClientScreen {
            device_id: 0xd0,
            screen_px,
            refresh_hz,
        }
    }

    const INNER: DisplayProfile = DisplayProfile {
        width_px: 2670,
        height_px: 1878,
        refresh_hz: 120,
        hidpi: true,
    };

    #[test]
    fn a_duo_gets_the_inner_size_whichever_screen_it_connects_on() {
        for screen in [(1878, 2670), (2670, 1878), (1398, 2034), (2034, 1398)] {
            assert_eq!(profile_for(client(screen, 120), None), INNER, "{screen:?}");
        }
        assert_eq!(INNER.points(), (1335, 939));
        assert_eq!(fallback_refresh(INNER), Some(60));
    }

    #[test]
    fn an_ipad_gets_its_panel_in_landscape() {
        let profile = profile_for(client((1668, 2420), 120), None);
        assert_eq!(
            profile,
            DisplayProfile {
                width_px: 2420,
                height_px: 1668,
                refresh_hz: 120,
                hidpi: true
            }
        );
        assert_eq!(profile.points(), (1210, 834));
        // Odd panels round down to even pixels; no refresh means 60.
        let odd = profile_for(client((1621, 2161), 0), None);
        assert_eq!(
            (odd.width_px, odd.height_px, odd.refresh_hz),
            (2160, 1620, 60)
        );
        assert_eq!(fallback_refresh(odd), None);
    }

    #[test]
    fn a_saved_profile_wins_over_what_the_client_reports() {
        let saved = DisplayProfile {
            width_px: 2034,
            height_px: 1398,
            refresh_hz: 60,
            hidpi: true,
        };
        assert_eq!(profile_for(client((1878, 2670), 120), Some(saved)), saved);
    }

    #[test]
    fn the_inner_display_on_the_folded_outer_screen() {
        // Landscape outer screen: about 47 px of side bars.
        let landscape = fit(DUO_INNER, (2034, 1398), FitMode::Fit);
        assert!((landscape.bars.0 - 46.4).abs() < 0.5, "{landscape:?}");
        assert_eq!(landscape.bars.1, 0.0);
        // Portrait outer screen: about 1,051 px of bars above and below.
        let portrait = fit(DUO_INNER, DUO_OUTER, FitMode::Fit);
        assert!((portrait.bars.1 - 1050.7).abs() < 0.5, "{portrait:?}");
        assert_eq!(portrait.bars.0, 0.0);
        // Fill on the portrait outer screen crops most of the desktop's
        // width: only 1398 / (2034 / 1878) ≈ 1291 of 2670 px stay visible.
        let fill = fit(DUO_INNER, DUO_OUTER, FitMode::Fill);
        assert_eq!(fill.bars, (0.0, 0.0));
        assert!((fill.cropped.0 - (2670.0 - 1290.8)).abs() < 0.5, "{fill:?}");
        assert_eq!(fill.cropped.1, 0.0);
        // On the inner screen itself nothing is lost either way.
        for mode in [FitMode::Fit, FitMode::Fill] {
            let same = fit(DUO_INNER, DUO_INNER, mode);
            assert_eq!(
                (same.scale, same.bars, same.cropped),
                (1.0, (0.0, 0.0), (0.0, 0.0))
            );
        }
    }

    #[test]
    fn only_the_duos_two_screens_count_as_a_duo() {
        assert!(is_duo_screen((2670, 1878)));
        assert!(is_duo_screen((2034, 1398)));
        assert!(!is_duo_screen((2420, 1668)));
        assert!(!is_duo_screen((2670, 1398)));
    }
}
