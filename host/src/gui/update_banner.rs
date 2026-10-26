//! The update banner above every page, the update popup, the update rows in
//! Settings, and the notices shown when an iPad cannot connect because of a
//! version difference or runs an older app.

use std::time::{Duration, Instant};

use eframe::egui::{self, vec2, Color32, Frame, Margin, Rect, Sense, Stroke, Ui};

use super::icons::{self, Icon};
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{update_toast, AnalyzerApp, ClientSnapshot, Page, StatsSnapshot, StreamState};
use crate::updates::{CheckStatus, Notice, OlderSide, IPAD_OUT_OF_DATE, WEBSITE_DOWNLOAD};

/// The popup's last line: what Update now does from here.
pub(super) fn popup_hint(installs_in_place: bool) -> &'static str {
    if installs_in_place {
        "Windows will ask for permission, then EternalMonitor restarts."
    } else {
        "Update now opens the download page."
    }
}

/// How long no iPad must have been connected before the popup may open. A
/// dropped iPad reconnects by itself: its first try comes 1 s later, its
/// second about 15 s later (1 s backoff, a 12 s connect window, 2 s backoff).
/// 20 s outlasts both, so a reconnect loop never flashes the popup.
pub(super) const POPUP_QUIET: Duration = Duration::from_secs(20);

/// Whether an iPad is connected, or was less than `quiet` ago. `last` is when
/// one was last seen; updated here every frame.
pub(super) fn ipad_nearby(
    connected: bool,
    last: &mut Option<Instant>,
    now: Instant,
    quiet: Duration,
) -> bool {
    if connected {
        *last = Some(now);
        return true;
    }
    last.is_some_and(|at| now.saturating_duration_since(at) < quiet)
}

/// What the update popup does this frame, given what else is going on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PopupGate {
    Show,
    /// Not yet: it opens once the user is on Stream and between clicks.
    Wait,
    /// Off screen, without counting as Later, so it comes back afterwards.
    Hide,
}

impl PopupGate {
    /// `connected`: an iPad is connected or just was, so this window may be
    /// in its picture. `other`: another dialog or ask is up. Either hides an open
    /// popup; at most one dialog shows at a time.
    pub(super) fn of(
        open: bool,
        connected: bool,
        other: bool,
        on_stream: bool,
        busy: bool,
    ) -> Self {
        if connected || other {
            PopupGate::Hide
        } else if !open && (!on_stream || busy) {
            PopupGate::Wait
        } else {
            PopupGate::Show
        }
    }
}

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
                if self.updates.installs_in_place() {
                    "Windows will ask for permission, then EternalMonitor restarts. Update the iPad app in TestFlight too."
                } else {
                    "Update now opens the download page. Install the new version, then open EternalMonitor again. Update the iPad app in TestFlight too."
                },
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
            self.update_action(ui.ctx(), action);
        }
    }

    /// A small card over the window, once per version and then at most every
    /// three days (see `updates::State::popup`). It waits while an iPad has a
    /// session, even while the stream restarts, and for [`POPUP_QUIET`] after,
    /// since this window may be on the screen the iPad shows. It also waits
    /// while the QR code, Send feedback, the feedback ask or a menu is open.
    /// It opens only on the Stream page, between clicks and while nobody
    /// types. The banner offers the update meanwhile.
    pub(super) fn update_popup(&mut self, ctx: &egui::Context, client: &ClientSnapshot) {
        // A session outlives capture and encoder restarts, a display change
        // and the second screen attaching, while the pipeline is briefly off.
        let connected = ipad_nearby(
            client.info.is_some(),
            &mut self.ipad_last_seen,
            Instant::now(),
            self.popup_quiet,
        );
        let Some(popup) = self.updates.popup() else {
            self.update_popup_open = false;
            return;
        };
        let other = self.show_qr_modal
            || self.feedback.is_some()
            || self.feedback_ask.due(client.info.is_some())
            || egui::Popup::is_any_open(ctx);
        let busy = ctx.egui_wants_keyboard_input() || ctx.input(|i| i.pointer.any_down());
        match PopupGate::of(
            self.update_popup_open,
            connected,
            other,
            self.page == Page::Stream,
            busy,
        ) {
            PopupGate::Show => self.update_popup_open = true,
            PopupGate::Wait => return,
            PopupGate::Hide => {
                self.update_popup_open = false;
                return;
            }
        }
        let hint = popup_hint(self.updates.installs_in_place());
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("update-popup"))
            .backdrop_color(Color32::from_black_alpha(140))
            .frame(
                Frame::new()
                    .fill(SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, BORDER_STRONG))
                    .corner_radius(16.0)
                    .inner_margin(Margin::same(24))
                    .shadow(egui::Shadow {
                        offset: [0, 20],
                        blur: 56,
                        spread: 0,
                        color: Color32::from_black_alpha(160),
                    }),
            )
            .show(ctx, |ui| {
                ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
                    node.set_role(egui::accesskit::Role::Dialog);
                    node.set_label("Update ready");
                });
                ui.set_width(340.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                let (badge, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter()
                    .circle_filled(badge.center(), 20.0, mix(SURFACE_RAISED, ACCENT, 14));
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(badge.center(), vec2(20.0, 20.0)),
                    Icon::Download,
                    ACCENT,
                );
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(popup.title())
                            .family(semibold())
                            .size(18.0)
                            .color(TEXT),
                    )
                    .wrap(),
                );
                ui.add(egui::Label::new(strong(popup.message())).wrap());
                if let Some(note) = &popup.note {
                    ui.add(egui::Label::new(muted(note)).wrap());
                }
                ui.add(egui::Label::new(faint(hint)).wrap());
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::button(ui, Tone::Primary, None, "Update now").clicked() {
                        action = Some(Action::UpdateNow);
                    }
                    if widgets::button(ui, Tone::Ghost, None, "Later").clicked() {
                        action = Some(Action::Later);
                    }
                });
            });
        // Escape or a click beside the card means Later.
        if action.is_none() && modal.should_close() {
            action = Some(Action::Later);
        }
        if let Some(action) = action {
            self.updates.popup_closed();
            self.update_popup_open = false;
            if matches!(action, Action::UpdateNow) {
                self.update_action(ctx, action);
            }
        }
    }

    /// Stream page: the connected iPad's app is older than 0.3.2. It works,
    /// but only the PC can say an update is waiting. Hidden for the rest of
    /// this run once dismissed.
    pub(super) fn ipad_update_notice(&mut self, ui: &mut Ui, client: &ClientSnapshot) {
        let old_app = client
            .info
            .as_ref()
            .is_some_and(|info| crate::updates::ipad_app_out_of_date(info.feature_caps));
        if !old_app || self.ipad_notice_dismissed {
            return;
        }
        let (title, body) = IPAD_OUT_OF_DATE;
        widgets::banner(ui, ACCENT, Icon::Download, title, body, 60.0, |ui| {
            if widgets::button(ui, Tone::Secondary, None, "OK").clicked() {
                self.ipad_notice_dismissed = true;
            }
        });
        ui.add_space(GAP - 10.0);
    }

    fn update_action(&mut self, ctx: &egui::Context, action: Action) {
        update_toast::hide();
        match action {
            Action::UpdateNow if !self.updates.installs_in_place() => {
                ctx.open_url(egui::OpenUrl::new_tab(WEBSITE_DOWNLOAD));
            }
            Action::UpdateNow => self.updates.update_now(),
            Action::Force => self.updates.force_now(),
            Action::Cancel => self.updates.cancel(),
            Action::Later => self.updates.later(),
        }
    }

    fn mismatch_banner(&mut self, ui: &mut Ui, side: OlderSide) {
        let (title, body) = crate::updates::mismatch_text(side);
        let mut action = None;
        match side {
            OlderSide::Pc => {
                let available = self.updates.available_version().is_some();
                widgets::banner(ui, WARNING, Icon::Warning, title, body, 300.0, |ui| {
                    widgets::link(ui, "Download from the website", WEBSITE_DOWNLOAD);
                    let label = if available {
                        "Update now"
                    } else {
                        "Check for updates"
                    };
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
            Some(true) => self.update_action(ui.ctx(), Action::UpdateNow),
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
                self.update_action(ui.ctx(), Action::UpdateNow);
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
