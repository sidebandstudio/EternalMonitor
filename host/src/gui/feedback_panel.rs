//! Send feedback, the one-time ask, and the GitHub star card.
//!
//! Automation names: the sidebar's "Send feedback" button opens a dialog
//! labelled "Send feedback" with "Open in my email app" and "Copy message";
//! the star card is a button named "Enjoying EternalMonitor?", or
//! "Thank you!" once clicked.

use std::sync::OnceLock;

use eframe::egui::{
    self, pos2, vec2, Color32, CursorIcon, FontFamily, FontId, Frame, Margin, Rect, Response,
    Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType,
};
use tracing::info;

use super::icons::{self, Icon};
use super::theme::*;
use super::widgets::{self, Tone};
use super::{AnalyzerApp, ClientSnapshot, Copied, CAPTURE_VIRTUAL_SENTINEL};
use crate::feedback::{self, Kind, EMAIL};
use crate::transport::link::LinkId;

pub const REPO_URL: &str = "https://github.com/sidebandstudio/EternalMonitor";

const ASK_TITLE: &str = "How's EternalMonitor working for you? :)";
const ASK_BODY: &str = "We're a small studio building EternalMonitor so students, and anyone with an iPad sitting around, can put it to work. We read every message, and your feedback shapes what we fix next.";

/// The Send feedback dialog while it is open.
#[derive(Default)]
pub(super) struct Draft {
    kind: Kind,
    message: String,
    exclude_details: bool,
    note: Option<Note>,
}

/// What the dialog says after a button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Note {
    Copied,
    CopiedLogs,
    NoLogs,
    Opened,
    Shortened,
}

enum Action {
    Email,
    CopyMessage,
    CopyWithLogs,
    CopyAddress,
    Close,
}

/// Read once: neither changes while the app runs.
fn graphics() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(crate::gpu::adapter_names)
}

fn windows() -> Option<String> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(crate::diagnostics::windows_version)
        .clone()
}

/// What "Include app and PC details" adds. The iPad is described by its
/// screen; its name, the PC's name and every address stay out.
pub(super) fn details(second_screen: bool, client: &ClientSnapshot) -> feedback::Details {
    feedback::Details {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        windows: windows(),
        graphics: graphics().to_vec(),
        second_screen,
        ipad: client.info.as_ref().map(|info| feedback::Ipad {
            usb: client
                .peer
                .is_some_and(|p| matches!(p.link, LinkId::Usb { .. })),
            screen_px: info.screen_px,
            refresh_hz: info.refresh_hz,
        }),
    }
}

impl AnalyzerApp {
    pub(super) fn open_feedback(&mut self) {
        self.feedback = Some(Draft::default());
    }

    /// Feedback was sent or the ask answered: the ask never shows again.
    fn finish_feedback_ask(&mut self) {
        if !self.feedback_ask.done {
            self.feedback_ask.finish();
            self.persist_settings();
        }
    }

    /// Counts long sessions toward the one-time ask. Called every frame.
    pub(super) fn observe_session(&mut self, client: &ClientSnapshot) {
        let session = client
            .session_id
            .zip(client.info.as_ref().map(|info| info.connected_at.elapsed()));
        if self.feedback_ask.observe(session) {
            info!(
                sessions = self.feedback_ask.long_sessions,
                "Counted a long streaming session toward the feedback ask"
            );
            self.persist_settings();
        }
    }

    /// The one-time ask, on the Stream page while no iPad is connected.
    pub(super) fn feedback_ask_card(&mut self, ui: &mut Ui, client: &ClientSnapshot) {
        if !self.feedback_ask.due(client.info.is_some()) {
            return;
        }
        let (mut send, mut later) = (false, false);
        widgets::banner(ui, ACCENT, Icon::Mail, ASK_TITLE, ASK_BODY, 230.0, |ui| {
            later = widgets::button(ui, Tone::Ghost, None, "Not now").clicked();
            send = widgets::button(ui, Tone::Primary, None, "Send feedback").clicked();
        });
        ui.add_space(widgets::GAP - 10.0);
        if send || later {
            self.finish_feedback_ask();
        }
        if send {
            self.open_feedback();
        }
    }

    pub(super) fn draw_feedback_panel(&mut self, ctx: &egui::Context, client: &ClientSnapshot) {
        let Some(mut draft) = self.feedback.take() else {
            return;
        };
        let details = details(
            self.settings_capture_display == CAPTURE_VIRTUAL_SENTINEL,
            client,
        );
        let address_copied = self.recently_copied(Copied::Email);
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("feedback-dialog"))
            .backdrop_color(Color32::from_black_alpha(175))
            .frame(
                Frame::new()
                    .fill(SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, BORDER_STRONG))
                    .corner_radius(18.0)
                    .inner_margin(Margin::same(26))
                    .shadow(egui::Shadow {
                        offset: [0, 24],
                        blur: 64,
                        spread: 0,
                        color: Color32::from_black_alpha(170),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(overline("Send feedback"));
                ui.label(
                    egui::RichText::new("Tell us how it's going")
                        .family(semibold())
                        .size(20.0)
                        .color(TEXT),
                );
                widgets::paragraph(
                    ui,
                    muted("We're a small studio, and every message reaches the people who build EternalMonitor. Problems, ideas and kind words all help."),
                );
                ui.add_space(8.0);

                // Everything between the greeting and the buttons scrolls, so
                // a long message or a small window never pushes the buttons
                // off screen.
                let pinned = PINNED_HEIGHT + if draft.note.is_some() { 40.0 } else { 0.0 };
                let body_height = (ui.ctx().content_rect().height() - pinned).max(110.0);
                egui::ScrollArea::vertical()
                    .id_salt("feedback-body")
                    .max_height(body_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        let before = draft.kind;
                        widgets::segmented(
                            ui,
                            &mut draft.kind,
                            &[
                                (Kind::Problem, Kind::Problem.label()),
                                (Kind::Idea, Kind::Idea.label()),
                                (Kind::Praise, Kind::Praise.label()),
                            ],
                        );
                        ui.add_space(4.0);
                        if message_box(ui, &mut draft) || draft.kind != before {
                            draft.note = None;
                        }
                        widgets::paragraph(ui, faint(kind_reply(draft.kind)));
                        ui.add_space(6.0);

                        let mut include = !draft.exclude_details;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            widgets::toggle(ui, &mut include, "Include app and PC details");
                            ui.label(egui::RichText::new("Include app and PC details").color(TEXT));
                        });
                        if include == draft.exclude_details {
                            draft.exclude_details = !include;
                            draft.note = None;
                        }
                        if include {
                            widgets::paragraph(ui, faint(details.summary()));
                        }
                        widgets::paragraph(
                            ui,
                            faint("To include logs, click Copy with logs and paste into your email. Logs can include this PC's name and network addresses."),
                        );
                    });
                ui.add_space(10.0);

                let width = ui.available_width();
                if widgets::button_ex(
                    ui,
                    Tone::Primary,
                    Some(Icon::Mail),
                    "Open in my email app",
                    true,
                    width,
                )
                .clicked()
                {
                    action = Some(Action::Email);
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if widgets::button(ui, Tone::Secondary, Some(Icon::Copy), "Copy message")
                        .clicked()
                    {
                        action = Some(Action::CopyMessage);
                    }
                    if widgets::button(ui, Tone::Secondary, Some(Icon::Logs), "Copy with logs")
                        .clicked()
                    {
                        action = Some(Action::CopyWithLogs);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, Tone::Ghost, None, "Close").clicked() {
                            action = Some(Action::Close);
                        }
                    });
                });
                if let Some(note) = draft.note {
                    let text = match note {
                        Note::Copied => "Copied. Paste it into an email to hello@sideband.studio",
                        Note::CopiedLogs => "Copied with logs. Paste it into an email to hello@sideband.studio",
                        Note::NoLogs => "Copied, but there were no logs to add. Paste it into an email to hello@sideband.studio",
                        Note::Opened => "Your email app should open with the message ready. If nothing opens, click Copy message instead.",
                        Note::Shortened => "Your message is long, so the email has a shortened version. The full text is on your clipboard to paste in.",
                    };
                    ui.add_space(2.0);
                    widgets::paragraph(
                        ui,
                        egui::RichText::new(text)
                            .size(SMALL)
                            .color(if matches!(note, Note::Copied | Note::CopiedLogs) {
                                ACCENT
                            } else {
                                TEXT_MUTED
                            }),
                    );
                }
                ui.add_space(8.0);
                widgets::divider(ui);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(muted("No email app? Write to"));
                    ui.label(mono(EMAIL, 12.5));
                    let (icon, tip) = if address_copied {
                        (Icon::Check, "Copied")
                    } else {
                        (Icon::Copy, "Copy email address")
                    };
                    if widgets::icon_button(ui, icon, tip).clicked() {
                        action = Some(Action::CopyAddress);
                    }
                });
            });

        let include = (!draft.exclude_details).then_some(&details);
        match action {
            Some(Action::Email) => {
                let (url, shortened) = feedback::mailto(draft.kind, &draft.message, include);
                if shortened {
                    ctx.copy_text(feedback::full_text(draft.kind, &draft.message, include));
                }
                open_email(ctx, &url);
                draft.note = Some(if shortened {
                    Note::Shortened
                } else {
                    Note::Opened
                });
                self.finish_feedback_ask();
            }
            Some(Action::CopyMessage) => {
                ctx.copy_text(feedback::full_text(draft.kind, &draft.message, include));
                draft.note = Some(Note::Copied);
                self.finish_feedback_ask();
            }
            Some(Action::CopyWithLogs) => {
                let message = feedback::full_text(draft.kind, &draft.message, include);
                draft.note = Some(match crate::logging::session_log_text() {
                    Some(logs) => {
                        ctx.copy_text(feedback::with_logs(&message, &logs));
                        Note::CopiedLogs
                    }
                    None => {
                        ctx.copy_text(message);
                        Note::NoLogs
                    }
                });
                self.finish_feedback_ask();
            }
            Some(Action::CopyAddress) => {
                self.copy(ctx, Copied::Email, EMAIL.to_string());
            }
            Some(Action::Close) => return,
            None => {}
        }
        if !modal.should_close() {
            self.feedback = Some(draft);
        }
    }
}

/// Room the dialog keeps outside its scrolling middle: margins, greeting,
/// buttons and the email line.
const PINNED_HEIGHT: f32 = 330.0;
/// The message box shows five to eight lines, then scrolls inside itself.
const MESSAGE_ROW: f32 = 18.0;

/// The message box, with its own border so the border stays put while long
/// text scrolls inside it. Returns whether the text changed.
fn message_box(ui: &mut Ui, draft: &mut Draft) -> bool {
    let id = ui.make_persistent_id("feedback-message");
    let focused = ui.memory(|m| m.has_focus(id));
    let hint = match draft.kind {
        Kind::Problem => "What happened, and what were you doing when it did?",
        Kind::Idea => "What would make EternalMonitor better for you?",
        Kind::Praise => "What do you like about EternalMonitor?",
    };
    let mut changed = false;
    Frame::new()
        .fill(CANVAS)
        .stroke(Stroke::new(
            1.0,
            if focused { ACCENT } else { BORDER_STRONG },
        ))
        .corner_radius(8.0)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("feedback-message-scroll")
                .min_scrolled_height(MESSAGE_ROW * 5.0)
                .max_height(MESSAGE_ROW * 8.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    changed = ui
                        .add(
                            egui::TextEdit::multiline(&mut draft.message)
                                .id(id)
                                .frame(Frame::NONE)
                                .hint_text(egui::RichText::new(hint).color(TEXT_FAINT))
                                .font(FontId::new(BODY, FontFamily::Proportional))
                                .desired_rows(5)
                                .desired_width(f32::INFINITY),
                        )
                        .changed();
                });
        });
    changed
}

/// A line under the message box that answers the kind chosen.
fn kind_reply(kind: Kind) -> &'static str {
    match kind {
        Kind::Problem => "Sorry it's giving you trouble. Say what you were doing when it happened, and we'll look into it.",
        Kind::Idea => "Ideas from people using EternalMonitor decide what we build next.",
        Kind::Praise => "Thank you! Hearing what works keeps a small studio going.",
    }
}

/// Opens a `mailto:` link in the user's email app.
fn open_email(ctx: &egui::Context, url: &str) {
    // eframe hands every link to the default web browser on Windows, which
    // may not pass a mailto: link on. Explorer opens it with the app Windows
    // has for email, or asks which one to use.
    #[cfg(windows)]
    {
        let _ = ctx;
        if let Err(error) = std::process::Command::new("explorer.exe").arg(url).spawn() {
            tracing::warn!(error = %error, "Could not open the email app");
        }
    }
    #[cfg(not(windows))]
    ctx.open_url(egui::OpenUrl::same_tab(url));
}

// ── Sidebar ─────────────────────────────────────────────────────────────────

/// A sidebar row that runs an action instead of opening a page.
pub(super) fn sidebar_action(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, SIDEBAR.lerp_to_gamma(SURFACE, hover));
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            10.0,
            Stroke::new(1.5, mix(CANVAS, ACCENT, 70)),
            StrokeKind::Outside,
        );
    }
    icons::paint(
        painter,
        Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(16.0, 16.0)),
        icon,
        TEXT_FAINT.lerp_to_gamma(TEXT_MUTED, hover),
    );
    painter.text(
        pos2(rect.left() + 38.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::new(13.5, medium()),
        TEXT_MUTED.lerp_to_gamma(TEXT, hover),
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

impl AnalyzerApp {
    /// "Enjoying EternalMonitor?" above Send feedback. One click opens the
    /// repository and turns the card into a thank-you, for good.
    pub(super) fn star_card(&mut self, ui: &mut Ui) {
        let thanked = self.star_clicked;
        let (title, subtitle) = if thanked {
            ("Thank you!", "It helps others find us")
        } else {
            (
                "Enjoying EternalMonitor?",
                "A GitHub star helps others put a spare iPad to work",
            )
        };
        let width = ui.available_width();
        let pad = 9.0;
        let tile = 26.0;
        let text_left = pad + tile + 9.0;
        let text_width = width - text_left - pad;
        let title_galley = ui.painter().layout(
            title.to_string(),
            FontId::new(12.0, medium()),
            TEXT,
            text_width,
        );
        let subtitle_galley = ui.painter().layout(
            subtitle.to_string(),
            FontId::new(CAPTION, FontFamily::Proportional),
            TEXT_MUTED,
            text_width,
        );
        let text_height = title_galley.size().y + 2.0 + subtitle_galley.size().y;
        let height = text_height.max(tile) + pad * 2.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, title));
        if response.clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(REPO_URL));
            if !self.star_clicked {
                self.star_clicked = true;
                info!("GitHub star card clicked");
                self.persist_settings();
            }
        }
        if !ui.is_rect_visible(rect) {
            return;
        }
        let hover =
            ui.ctx()
                .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.16);
        let painter = ui.painter();
        if hover > 0.0 {
            // A soft accent glow behind the card.
            painter.add(
                egui::Shadow {
                    offset: [0, 0],
                    blur: 18,
                    spread: 0,
                    color: ACCENT.gamma_multiply(0.14 * hover),
                }
                .as_shape(rect, 10.0),
            );
        }
        painter.rect(
            rect,
            10.0,
            SURFACE,
            Stroke::new(
                1.0,
                BORDER_STRONG.lerp_to_gamma(mix(SURFACE, ACCENT, 70), hover),
            ),
            StrokeKind::Inside,
        );
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                12.0,
                Stroke::new(1.5, mix(CANVAS, ACCENT, 70)),
                StrokeKind::Outside,
            );
        }
        let tile_rect = Rect::from_min_size(
            pos2(rect.left() + pad, rect.center().y - tile / 2.0),
            vec2(tile, tile),
        );
        painter.rect(
            tile_rect,
            8.0,
            mix(SURFACE, ACCENT, if thanked { 16 } else { 10 }),
            Stroke::new(1.0, mix(SURFACE, ACCENT, 26)),
            StrokeKind::Inside,
        );
        // The star tips a little toward the pointer.
        icons::paint_star(
            painter,
            tile_rect.center() + vec2(0.0, 0.5),
            7.0,
            -0.22 * hover,
            ACCENT,
            thanked,
        );
        let text_top = rect.center().y - text_height / 2.0;
        painter.galley(
            pos2(rect.left() + text_left, text_top),
            title_galley.clone(),
            TEXT,
        );
        painter.galley(
            pos2(
                rect.left() + text_left,
                text_top + title_galley.size().y + 2.0,
            ),
            subtitle_galley,
            TEXT_MUTED,
        );
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
}
