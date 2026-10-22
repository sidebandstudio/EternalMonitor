//! The update banner above every page, the update rows in Settings, and the
//! notice shown when an iPad cannot connect because of a version difference.

use eframe::egui::{self, Ui};

use super::icons::Icon;
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{update_toast, AnalyzerApp, ClientSnapshot, StatsSnapshot, StreamState};
use crate::updates::{CheckStatus, Notice, OlderSide, WEBSITE_DOWNLOAD};

impl AnalyzerApp {
    pub(super) fn update_banner(
        &mut self,
        ui: &mut Ui,
        snap: &StatsSnapshot,
        client: &ClientSnapshot,
    ) {
        let streaming = StreamState::of(snap, client) == StreamState::Streaming;
        if self.updates.tick(streaming) {
            // Setup is waiting for this process to exit. Close the normal way
            // so the stream, virtual display and mDNS shut down cleanly.
            update_toast::hide();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if streaming {
            // A connected iPad means the versions work together.
            crate::updates::clear_mismatch();
        } else if let Some(side) = crate::updates::recent_mismatch() {
            self.mismatch_banner(ui, side);
        }
        let Some(notice) = self.updates.notice() else {
            return;
        };
        let mut action = None;
        match &notice {
            Notice::Available(version) => widgets::banner(
                ui,
                ACCENT,
                Icon::Download,
                &format!("EternalMonitor {version} is available"),
                "Windows will ask for permission, then EternalMonitor restarts. Update the iPad app in TestFlight too.",
                190.0,
                |ui| {
                    if widgets::button(ui, Tone::Ghost, None, "Later").clicked() {
                        action = Some(Action::Later);
                    }
                    if widgets::button(ui, Tone::Primary, None, "Update now").clicked() {
                        action = Some(Action::UpdateNow);
                    }
                },
            ),
            Notice::Queued(_) => widgets::banner(
                ui,
                ACCENT,
                Icon::Download,
                "The update will install when you stop streaming",
                "Your iPad stays connected until then. Stop and update closes the stream now.",
                230.0,
                |ui| {
                    if widgets::button(ui, Tone::Ghost, None, "Cancel").clicked() {
                        action = Some(Action::Cancel);
                    }
                    if widgets::button(ui, Tone::Secondary, None, "Stop and update").clicked() {
                        action = Some(Action::Force);
                    }
                },
            ),
            Notice::Downloading(version, fraction) => {
                let body = match fraction {
                    Some(fraction) => format!("{:.0}% downloaded.", fraction * 100.0),
                    None => "Starting the download.".into(),
                };
                widgets::banner(
                    ui,
                    ACCENT,
                    Icon::Download,
                    &format!("Downloading EternalMonitor {version}"),
                    &body,
                    170.0,
                    |ui| {
                        ui.add(
                            egui::ProgressBar::new(fraction.unwrap_or(0.0))
                                .desired_width(160.0)
                                .fill(ACCENT),
                        );
                    },
                );
            }
            Notice::Installing(version) => widgets::banner(
                ui,
                ACCENT,
                Icon::Download,
                &format!("Installing EternalMonitor {version}"),
                "If Windows asks for permission, click Yes. EternalMonitor closes and opens again by itself.",
                0.0,
                |_| {},
            ),
            Notice::Failed(message) => widgets::banner(
                ui,
                WARNING,
                Icon::Warning,
                "The update did not finish",
                message,
                340.0,
                |ui| {
                    if widgets::button(ui, Tone::Ghost, None, "Later").clicked() {
                        action = Some(Action::Later);
                    }
                    if widgets::button(ui, Tone::Secondary, None, "Try again").clicked() {
                        action = Some(Action::UpdateNow);
                    }
                    widgets::link(ui, "Download from the website", WEBSITE_DOWNLOAD);
                },
            ),
            Notice::Updated(version) => widgets::banner(
                ui,
                ACCENT,
                Icon::Check,
                &format!("EternalMonitor is updated to {version}"),
                "If the iPad app asks for an update, open TestFlight on the iPad and tap Update.",
                60.0,
                |ui| {
                    if widgets::button(ui, Tone::Secondary, None, "OK").clicked() {
                        action = Some(Action::Later);
                    }
                },
            ),
        }
        ui.add_space(GAP - 10.0);
        if let Some(action) = action {
            self.update_action(action);
        }
    }

    fn update_action(&mut self, action: Action) {
        update_toast::hide();
        match action {
            Action::UpdateNow => self.updates.update_now(),
            Action::Force => self.updates.force_now(),
            Action::Cancel => self.updates.cancel(),
            Action::Later => self.updates.later(),
        }
    }

    fn mismatch_banner(&mut self, ui: &mut Ui, side: OlderSide) {
        let available = self.updates.available_version().is_some();
        let (title, body) = crate::updates::mismatch_text(side, available);
        let mut action = None;
        match side {
            OlderSide::Pc => {
                widgets::banner(ui, WARNING, Icon::Warning, title, body, 300.0, |ui| {
                    widgets::link(ui, "Download from the website", WEBSITE_DOWNLOAD);
                    let label = crate::updates::mismatch_button(available);
                    if widgets::button(ui, Tone::Primary, None, label).clicked() {
                        action = Some(available);
                    }
                });
            }
            OlderSide::Ipad => {
                widgets::banner(ui, WARNING, Icon::Warning, title, body, 60.0, |ui| {
                    if widgets::button(ui, Tone::Secondary, None, "OK").clicked() {
                        crate::updates::clear_mismatch();
                    }
                });
            }
        }
        match action {
            Some(true) => self.update_action(Action::UpdateNow),
            Some(false) => self.updates.check_now(),
            None => {}
        }
        ui.add_space(GAP - 10.0);
    }

    /// The rows in Settings → General.
    pub(super) fn update_settings(&mut self, ui: &mut Ui) {
        let mut automatic = self.settings_check_updates;
        widgets::setting_row(
            ui,
            "Check for updates automatically",
            "Looks for a new version once a day and tells you when one is ready.",
            64.0,
            |ui| {
                widgets::toggle(ui, &mut automatic, "Automatic update checks");
            },
        );
        if automatic != self.settings_check_updates {
            self.set_check_updates(automatic);
        }
        widgets::divider(ui);
        let status = self.updates.check_status();
        let available = self.updates.available_version();
        let description = match (status, &available) {
            (CheckStatus::Checking, _) => "Checking for updates…".to_string(),
            (_, Some(version)) => format!("Version {version} is ready to install."),
            (CheckStatus::UpToDate, None) => "You have the latest version.".to_string(),
            (CheckStatus::Failed, None) => {
                "Could not check for updates. Check your internet connection.".to_string()
            }
            _ => "Click Check now to look for a new version.".to_string(),
        };
        let mut clicked = false;
        widgets::setting_row(
            ui,
            &format!("Version {}", env!("CARGO_PKG_VERSION")),
            &description,
            110.0,
            |ui| {
                let label = if available.is_some() {
                    "Update now"
                } else {
                    "Check now"
                };
                clicked = widgets::button_ex(
                    ui,
                    Tone::Secondary,
                    None,
                    label,
                    status != CheckStatus::Checking,
                    0.0,
                )
                .clicked();
            },
        );
        if clicked {
            if available.is_some() {
                self.update_action(Action::UpdateNow);
            } else {
                self.updates.check_now();
            }
        }
    }
}

enum Action {
    UpdateNow,
    Force,
    Cancel,
    Later,
}
