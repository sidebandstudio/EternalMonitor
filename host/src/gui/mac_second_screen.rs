//! The Mac host's Second screen switch and its status line, at the top of
//! Stream. Video only: input is not posted to the Mac yet.

use eframe::egui::{self, RichText, Ui};

use super::theme::*;
use super::widgets::{self, Tone};
use super::{AnalyzerApp, ClientSnapshot, StatsSnapshot};
use crate::capture::recovery::CaptureBlock;
use crate::virtual_display::SecondScreenStatus;

/// What the status line says, and whether it offers System Settings.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StatusLine {
    pub text: String,
    pub open_settings: bool,
}

impl AnalyzerApp {
    pub(super) fn mac_second_screen_row(
        &mut self,
        ui: &mut Ui,
        snap: &StatsSnapshot,
        client: &ClientSnapshot,
    ) {
        let was_on = self.settings_mac_second_screen;
        let mut on = was_on;
        let status = self.control.shared.second_screen.lock().clone();
        let line = status_line(
            on,
            &status,
            client.capture_block,
            client.info.as_ref().map(|info| info.device_name.as_str()),
            snap.capture_resolution,
            snap.pipeline_running,
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.label(RichText::new("Second screen").family(medium()).color(TEXT));
            widgets::toggle(ui, &mut on, "Second screen");
        });
        ui.add_space(2.0);
        let color = if line.open_settings {
            WARNING
        } else {
            TEXT_MUTED
        };
        ui.add(egui::Label::new(RichText::new(line.text).color(color)).wrap());
        if line.open_settings {
            ui.add_space(6.0);
            if widgets::button(ui, Tone::Secondary, None, "Open System Settings").clicked() {
                super::open_screen_recording_settings();
            }
        }
        if on != was_on {
            self.set_mac_second_screen(on, snap.pipeline_running);
        }
    }
}

/// The header's word for what the device shows.
pub(super) fn mode_label(on: bool, status: &SecondScreenStatus) -> &'static str {
    match status {
        _ if !on => "Test picture",
        SecondScreenStatus::Active(_) => super::CAPTURE_VIRTUAL_LABEL,
        SecondScreenStatus::Off => "Starting",
        _ => "Mirroring",
    }
}

/// The line under the switch. Screen Recording comes first: without it
/// nothing else can work.
pub(super) fn status_line(
    on: bool,
    status: &SecondScreenStatus,
    block: Option<CaptureBlock>,
    device: Option<&str>,
    (width, height): (u32, u32),
    running: bool,
) -> StatusLine {
    let line = |text: String| StatusLine {
        text,
        open_settings: false,
    };
    if on && running && block == Some(CaptureBlock::ScreenRecordingDenied) {
        return StatusLine {
            text: "Screen Recording is off: open System Settings".into(),
            open_settings: true,
        };
    }
    if !on {
        return line("Off. Your device shows a test picture until you turn this on.".into());
    }
    if !running {
        return line("On. It starts when you start streaming.".into());
    }
    line(match (status, device) {
        (SecondScreenStatus::Active(_), Some(device)) if width > 0 && height > 0 => {
            format!("Second screen: on the {device} ({width}×{height})")
        }
        (SecondScreenStatus::Active(_), Some(device)) => format!("Second screen: on the {device}"),
        (SecondScreenStatus::Active(_), None) => {
            "Second screen: kept for your device until it reconnects".into()
        }
        (SecondScreenStatus::Mirroring, _) => {
            "Mirroring your main display (no device connected)".into()
        }
        (SecondScreenStatus::NotCapturable(_), _) => {
            "Mirroring: macOS didn't list the new display yet".into()
        }
        (SecondScreenStatus::Unavailable(_), _) => {
            "Mirroring: macOS couldn't add the second screen".into()
        }
        (SecondScreenStatus::Off, _) => "Starting the second screen".into(),
    })
}
