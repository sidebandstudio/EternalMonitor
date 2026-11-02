//! Components for the host window. Every interactive piece reports a role
//! and label through AccessKit, so UI Automation and the snapshot harness
//! address them by their visible text. Whatever shows translated text also
//! carries its English text as its AutomationId ([`automation_id`]), the
//! same in every language.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Align, Color32, CursorIcon, FontFamily, FontId,
    FontSelection, Frame, Key, Label, Layout, Margin, Mesh, Pos2, Rect, Response, RichText, Sense,
    Shape, Stroke, StrokeKind, TextFormat, TextWrapMode, Ui, WidgetInfo, WidgetText, WidgetType,
};

use crate::i18n;

use super::icons::{self, Icon};
use super::theme::*;

pub const GAP: f32 = 16.0;
const BUTTON_HEIGHT: f32 = 34.0;

// ── Motion ──────────────────────────────────────────────────────────────────

/// 0 → 1 over `seconds` from the frame something first shows (or shows again
/// after a frame away), eased out. Pages and notices fade and settle in with
/// it. Nothing repaints once it reaches 1.
pub fn appear(ui: &Ui, id: egui::Id, seconds: f32) -> f32 {
    let ctx = ui.ctx();
    let frame = ctx.cumulative_frame_nr();
    let last_shown = ctx.data_mut(|d| d.get_temp::<u64>(id));
    ctx.data_mut(|d| d.insert_temp(id, frame));
    if last_shown.is_none_or(|shown| shown + 1 < frame) {
        // Start from invisible; a new id would otherwise snap to 1.
        ctx.animate_bool_with_time(id, false, 0.0);
    }
    ctx.animate_bool_with_time_and_easing(id, true, seconds, egui::emath::easing::cubic_out)
}

// ── Containers ──────────────────────────────────────────────────────────────

pub fn card() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12.0)
        .inner_margin(Margin::same(20))
}

/// Card whose rows run edge to edge and carry their own vertical padding.
pub fn list_card() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12.0)
        .inner_margin(Margin::symmetric(20, 2))
}

/// Title on the left, optional controls on the right, on one 34-point row.
pub fn card_header(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(width, BUTTON_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            let response = ui.label(heading(title));
            automation_id(&response, title);
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        },
    );
}

pub fn divider(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, BORDER));
}

pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    overline_label(ui, text);
    ui.add_space(2.0);
}

/// An [`overline`], in capitals as it reads, its English id too.
pub fn overline_label(ui: &mut Ui, text: &str) -> Response {
    let response = ui.add(Label::new(overline(text)));
    if let Some(names) = i18n::automation(text) {
        set_automation(
            &response,
            &names.id.to_uppercase(),
            &names.name.to_uppercase(),
        );
    }
    response
}

/// Wrapped text that fills the available width.
pub fn paragraph(ui: &mut Ui, text: RichText) -> Response {
    ui.add(Label::new(text).wrap())
}

/// Gives a widget that shows `shown`, a translation, its English text as
/// its AutomationId (AccessKit `author_id`), and as its name the text in
/// reading order: Arabic is drawn reordered (see `i18n::bidi`), a screen
/// reader needs it as written. Call it after egui has described the widget
/// (`Response::widget_info`, or adding an egui widget), which would replace
/// the name again. Never for a text field: what someone typed is theirs.
pub fn automation_id(response: &Response, shown: &str) {
    if let Some(names) = i18n::automation(shown) {
        set_automation(response, &names.id, &names.name);
    }
}

/// The node's AutomationId `id`, and `name` where egui put its text: a
/// label's or a combo box's value, any other widget's label.
fn set_automation(response: &Response, id: &str, name: &str) {
    use egui::accesskit::Role;
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_author_id(id);
        match node.role() {
            Role::Label | Role::ComboBox => node.set_value(name),
            _ => node.set_label(name),
        }
    });
}

/// `shown` in `style`, wrapped to the available width. Arabic is broken into
/// lines in reading order before each line is reordered for drawing (see
/// `i18n::bidi`); egui would otherwise wrap the reordered text.
pub fn wrapped(ui: &mut Ui, shown: &str, style: impl Fn(String) -> RichText) -> Response {
    let Some(logical) = i18n::logical(shown) else {
        let response = ui.add(Label::new(style(shown.to_string())).wrap());
        automation_id(&response, shown);
        return response;
    };
    let width = ui.available_width() - 1.0;
    let lines = i18n::bidi::wrap(logical, width, |line| {
        WidgetText::from(style(line.to_string()))
            .into_galley(
                ui,
                Some(TextWrapMode::Extend),
                f32::INFINITY,
                FontSelection::Default,
            )
            .size()
            .x
    });
    let response = ui.add(Label::new(style(lines)).extend());
    automation_id(&response, shown);
    response
}

/// `shown` in `style` on one line, cut short with "…" where it does not fit.
/// Arabic is cut at the end of its sentence before it is reordered (see
/// [`fit_line`]); egui would cut the drawn right end, its start.
pub fn truncated(ui: &mut Ui, shown: &str, style: impl Fn(String) -> RichText) -> Response {
    let text = fit_line(ui, shown, ui.available_width() - 1.0, &style);
    let response = ui.add(Label::new(style(text)).truncate());
    automation_id(&response, shown);
    response
}

/// `shown` cut to `width` at the end of its sentence where it holds
/// right-to-left text drawn reordered: a translation, or a status line of
/// parts joined with " · " left to right, which keeps the parts that fit and
/// cuts the first that does not. Other text is left for egui to cut.
pub(super) fn fit_line(
    ui: &Ui,
    shown: &str,
    width: f32,
    style: &impl Fn(String) -> RichText,
) -> String {
    let measure = |text: &str| {
        WidgetText::from(style(text.to_string()))
            .into_galley(
                ui,
                Some(TextWrapMode::Extend),
                f32::INFINITY,
                FontSelection::Default,
            )
            .size()
            .x
    };
    if !i18n::is_rtl() || measure(shown) <= width {
        return shown.to_string();
    }
    if let Some(logical) = i18n::reading_order(shown) {
        return i18n::bidi::truncate(&logical, width, measure);
    }
    let mut kept = String::new();
    for part in shown.split_inclusive('·') {
        let joined = format!("{kept}{part}");
        if measure(&joined) <= width {
            kept = joined;
            continue;
        }
        let core = part.trim_end_matches('·').trim();
        let Some(logical) = i18n::reading_order(core) else {
            return joined;
        };
        let before = format!("{kept}{}", &part[..part.find(core).unwrap_or(0)]);
        let rest = i18n::bidi::truncate(&logical, width - measure(&before), measure);
        return before + &rest;
    }
    kept
}

// ── Buttons ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Secondary,
    Danger,
    Ghost,
}

pub fn button(ui: &mut Ui, tone: Tone, icon: Option<Icon>, label: &str) -> Response {
    button_ex(ui, tone, icon, label, true, 0.0)
}

/// The width [`button`] gives `label`, with `icon`, in the language shown.
pub fn button_width(ui: &Ui, icon: Option<Icon>, label: &str) -> f32 {
    let (galley, icon_width) = button_content(ui, icon, label);
    icon_width + galley.size().x + 28.0
}

/// A button's label laid out, and the room its icon takes.
fn button_content(ui: &Ui, icon: Option<Icon>, label: &str) -> (std::sync::Arc<egui::Galley>, f32) {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::new(13.0, medium()),
        Color32::PLACEHOLDER,
    );
    let icon_width = if icon.is_some() { 16.0 + 7.0 } else { 0.0 };
    (galley, icon_width)
}

/// `min_width` > 0 stretches the button, e.g. to fill a dialog.
pub fn button_ex(
    ui: &mut Ui,
    tone: Tone,
    icon: Option<Icon>,
    label: &str,
    enabled: bool,
    min_width: f32,
) -> Response {
    let (galley, icon_width) = button_content(ui, icon, label);
    let content_width = icon_width + galley.size().x;
    let size = vec2((content_width + 28.0).max(min_width), BUTTON_HEIGHT);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    automation_id(&response, label);

    if ui.is_rect_visible(rect) {
        let hovered = enabled && response.hovered();
        let pressed = enabled && response.is_pointer_button_down_on();
        // Hover colours fade in and out; a press is immediate.
        let hover = ui
            .ctx()
            .animate_bool_with_time(response.id.with("hover"), hovered, 0.12);
        let (rest, hovered_fill, pressed_fill, stroke, text) = match tone {
            Tone::Primary => (
                ACCENT,
                ACCENT_HOVER,
                ACCENT_PRESSED,
                Stroke::NONE,
                ON_ACCENT,
            ),
            Tone::Secondary => (
                SURFACE_RAISED,
                SURFACE_HOVER,
                mix(SURFACE_HOVER, TEXT, 4),
                Stroke::new(
                    1.0,
                    BORDER_STRONG.lerp_to_gamma(mix(BORDER_STRONG, TEXT, 12), hover),
                ),
                TEXT,
            ),
            Tone::Danger => (
                tint(DANGER),
                mix(SURFACE, DANGER, 15),
                mix(SURFACE, DANGER, 22),
                Stroke::new(1.0, tint_border(DANGER)),
                mix(DANGER, TEXT, 25),
            ),
            Tone::Ghost => (
                Color32::TRANSPARENT,
                SURFACE_RAISED,
                SURFACE_HOVER,
                Stroke::NONE,
                TEXT_MUTED.lerp_to_gamma(TEXT, hover),
            ),
        };
        let fill = if pressed {
            pressed_fill
        } else {
            rest.lerp_to_gamma(hovered_fill, hover)
        };
        let alpha = if enabled { 1.0 } else { 0.45 };
        let painter = ui.painter();
        painter.rect(
            rect,
            8.0,
            fill.gamma_multiply(alpha),
            Stroke::new(stroke.width, stroke.color.gamma_multiply(alpha)),
            StrokeKind::Inside,
        );
        focus_ring(ui, &response, rect, 8.0);
        let text = text.gamma_multiply(alpha);
        let mut x = rect.center().x - content_width / 2.0;
        if let Some(icon) = icon {
            icons::paint(
                painter,
                Rect::from_center_size(pos2(x + 8.0, rect.center().y), vec2(15.0, 15.0)),
                icon,
                text,
            );
            x += icon_width;
        }
        painter.galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            text,
        );
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Square icon-only button. `label` is its tooltip and accessible name.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    automation_id(&response, label);
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let pressed = response.is_pointer_button_down_on();
        let hover = ui
            .ctx()
            .animate_bool_with_time(response.id.with("hover"), hovered, 0.12);
        let fill = if pressed {
            SURFACE_HOVER
        } else {
            Color32::TRANSPARENT.lerp_to_gamma(SURFACE_RAISED, hover)
        };
        ui.painter().rect_filled(rect, 8.0, fill);
        focus_ring(ui, &response, rect, 8.0);
        icons::paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), vec2(15.0, 15.0)),
            icon,
            TEXT_MUTED.lerp_to_gamma(TEXT, hover),
        );
    }
    response
        .on_hover_text(label)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// Text link that opens `url` in the browser.
pub fn link(ui: &mut Ui, label: &str, url: &str) -> Response {
    let response = link_like(ui, label, Icon::External, WidgetType::Link);
    if response.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
    response.on_hover_text(url)
}

/// Looks like [`link`] with `icon` after the text, but only reports the
/// click, for actions that open something inside the window.
pub fn link_button(ui: &mut Ui, label: &str, icon: Icon) -> Response {
    link_like(ui, label, icon, WidgetType::Button)
}

fn link_like(ui: &mut Ui, label: &str, icon: Icon, role: WidgetType) -> Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::new(SMALL, medium()),
        Color32::PLACEHOLDER,
    );
    let size = galley.size() + vec2(16.0 + 5.0, 0.0);
    let (rect, response) = ui.allocate_exact_size(vec2(size.x, size.y.max(20.0)), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(role, true, label));
    automation_id(&response, label);
    if ui.is_rect_visible(rect) {
        let hover =
            ui.ctx()
                .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
        let color = ACCENT.lerp_to_gamma(ACCENT_HOVER, hover);
        let text_pos = pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
        let text_width = galley.size().x;
        ui.painter().galley(text_pos, galley, color);
        icons::paint(
            ui.painter(),
            Rect::from_center_size(
                pos2(rect.left() + text_width + 5.0 + 6.0, rect.center().y),
                vec2(11.0, 11.0),
            ),
            icon,
            color,
        );
        focus_ring(ui, &response, rect.expand(3.0), 6.0);
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn focus_ring(ui: &Ui, response: &Response, rect: Rect, radius: f32) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.5),
            radius + 2.5,
            Stroke::new(1.5, mix(CANVAS, ACCENT, 70)),
            StrokeKind::Outside,
        );
    }
}

// ── Inputs ──────────────────────────────────────────────────────────────────

/// On/off switch. `label` is its accessible name in English. In a row
/// whose title is translated it is named by that title as a screen reader
/// reads it, so a switch reads in the same language as its row. Its
/// AutomationId is its row's title in English, or `label` outside a row.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let value = *on;
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, value, label));
    let title = row_title(ui);
    match title.as_deref().and_then(i18n::automation) {
        Some(names) if i18n::active() != i18n::Lang::En => {
            set_automation(&response, &names.id, &names.name)
        }
        Some(names) => set_automation(&response, &names.id, label),
        None => set_automation(&response, title.as_deref().unwrap_or(label), label),
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(response.id, value, 0.12);
        let hovered = response.hovered();
        let off_track = if hovered {
            mix(SURFACE_HOVER, TEXT, 6)
        } else {
            SURFACE_HOVER
        };
        let on_track = if hovered { ACCENT_HOVER } else { ACCENT };
        let track = off_track.lerp_to_gamma(on_track, t);
        let painter = ui.painter();
        painter.rect(
            rect,
            11.0,
            track,
            Stroke::new(1.0, BORDER_STRONG.gamma_multiply(1.0 - t)),
            StrokeKind::Inside,
        );
        let knob_x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
        let knob = TEXT_MUTED.lerp_to_gamma(ON_ACCENT, t);
        painter.circle_filled(pos2(knob_x, rect.center().y), 8.0, knob);
        focus_ring(ui, &response, rect, 11.0);
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Mutually exclusive options in one pill-shaped control.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let font = FontId::new(13.0, medium());
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), Color32::PLACEHOLDER)
        })
        .collect();
    let segment_width = galleys
        .iter()
        .map(|g| g.size().x + 24.0)
        .fold(44.0_f32, f32::max);
    let pad = 3.0;
    let size = vec2(segment_width * options.len() as f32 + pad * 2.0, 32.0);
    let (outer, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(
        outer,
        9.0,
        CANVAS,
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );

    let mut changed = false;
    let base_id = ui
        .id()
        .with(("segmented", outer.min.x as i32, outer.min.y as i32));
    let rects: Vec<Rect> = (0..options.len())
        .map(|i| {
            Rect::from_min_size(
                pos2(
                    outer.left() + pad + segment_width * i as f32,
                    outer.top() + pad,
                ),
                vec2(segment_width, size.y - pad * 2.0),
            )
        })
        .collect();
    let responses: Vec<Response> = rects
        .iter()
        .enumerate()
        .map(|(i, rect)| ui.interact(*rect, base_id.with(i), Sense::click()))
        .collect();
    for (response, (option, _)) in responses.iter().zip(options) {
        if response.clicked() && *value != *option {
            *value = *option;
            changed = true;
        }
    }
    // The selected pill slides to the chosen segment.
    if let Some(selected) = options.iter().position(|(option, _)| *value == *option) {
        let x =
            ui.ctx()
                .animate_value_with_time(base_id.with("pill"), rects[selected].left(), 0.16);
        let pill = Rect::from_min_size(pos2(x, rects[selected].top()), rects[selected].size());
        ui.painter().rect(
            pill,
            7.0,
            SURFACE_HOVER,
            Stroke::new(1.0, BORDER_STRONG),
            StrokeKind::Inside,
        );
    }
    let enabled = ui.is_enabled();
    for (((option, label), galley), (rect, response)) in
        options.iter().zip(galleys).zip(rects.iter().zip(responses))
    {
        let selected = *value == *option;
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::SelectableLabel, enabled, selected, label)
        });
        automation_id(&response, label);
        let hover = ui.ctx().animate_bool_with_time(
            response.id.with("hover"),
            response.hovered() && !selected,
            0.12,
        );
        if hover > 0.0 {
            ui.painter().rect_filled(
                *rect,
                7.0,
                Color32::TRANSPARENT.lerp_to_gamma(SURFACE, hover),
            );
        }
        focus_ring(ui, &response, *rect, 7.0);
        let color = if selected {
            TEXT
        } else {
            TEXT_MUTED.lerp_to_gamma(TEXT, hover)
        };
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, color);
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
    changed
}

/// Horizontal slider snapping to `step`. Left and right arrows adjust it
/// when focused.
pub fn slider(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step: f32,
    width: f32,
    label: &str,
) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    let rail = Rect::from_center_size(rect.center(), vec2(width - 18.0, 4.0));
    let snap = |v: f32| ((v / step).round() * step).clamp(lo, hi);

    if let Some(pointer) = response.interact_pointer_pos() {
        if response.is_pointer_button_down_on() || response.clicked() {
            let t = ((pointer.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
            let next = snap(lo + t * (hi - lo));
            if next != *value {
                *value = next;
                response.mark_changed();
            }
        }
    }
    if response.has_focus() {
        // Left and right adjust the value instead of moving focus.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            )
        });
        let delta = ui.input(|i| {
            let mut d = 0.0;
            if i.key_pressed(Key::ArrowRight) {
                d += step;
            }
            if i.key_pressed(Key::ArrowLeft) {
                d -= step;
            }
            d
        });
        if delta != 0.0 {
            let next = snap(*value + delta);
            if next != *value {
                *value = next;
                response.mark_changed();
            }
        }
    }
    let current = *value;
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::slider(enabled, f64::from(current), label));

    if ui.is_rect_visible(rect) {
        let t = ((current - lo) / (hi - lo)).clamp(0.0, 1.0);
        let knob_x = rail.left() + t * rail.width();
        let painter = ui.painter();
        painter.rect_filled(rail, 2.0, SURFACE_HOVER);
        painter.rect_filled(
            Rect::from_min_max(rail.min, pos2(knob_x, rail.max.y)),
            2.0,
            ACCENT,
        );
        let active = response.hovered() || response.dragged();
        // The knob grows a little while it is being handled.
        let radius = 7.5
            + ui.ctx()
                .animate_bool_with_time(response.id.with("knob"), active, 0.1);
        let center = pos2(knob_x, rail.center().y);
        painter.circle_filled(
            center + vec2(0.0, 1.0),
            radius + 1.0,
            Color32::from_black_alpha(90),
        );
        painter.circle_filled(center, radius, TEXT);
        if response.has_focus() {
            painter.circle_stroke(
                center,
                radius + 3.0,
                Stroke::new(1.5, mix(CANVAS, ACCENT, 70)),
            );
        }
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

// ── Rows ────────────────────────────────────────────────────────────────────

/// A settings row: title and description on the left, `control` on the
/// right, vertically centred. `control_width` reserves room for the control.
pub fn setting_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    control_width: f32,
    control: impl FnOnce(&mut Ui),
) {
    row_with_leading(ui, None, title, description, control_width, control);
}

/// Like [`setting_row`] with an icon tile in front of the text.
pub fn icon_row(
    ui: &mut Ui,
    icon: Icon,
    icon_color: Color32,
    title: &str,
    description: &str,
    control_width: f32,
    control: impl FnOnce(&mut Ui),
) {
    row_with_leading(
        ui,
        Some((icon, icon_color)),
        title,
        description,
        control_width,
        control,
    );
}

fn row_with_leading(
    ui: &mut Ui,
    leading: Option<(Icon, Color32)>,
    title: &str,
    description: &str,
    control_width: f32,
    control: impl FnOnce(&mut Ui),
) {
    let width = ui.available_width();
    let leading_width = if leading.is_some() { 32.0 + 14.0 } else { 0.0 };
    let gap = 20.0;
    let text_width = (width - leading_width - control_width - gap).max(140.0);
    ui.add_space(14.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if leading.is_some() {
            ui.add_space(leading_width);
        }
        let text = ui
            .allocate_ui_with_layout(vec2(text_width, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(text_width);
                ui.spacing_mut().item_spacing.y = 3.0;
                wrapped(ui, title, strong);
                if !description.is_empty() {
                    wrapped(ui, description, muted);
                }
            })
            .response;
        let height = text.rect.height().max(32.0);
        if let Some((icon, color)) = leading {
            let tile = Rect::from_center_size(
                pos2(
                    text.rect.left() - leading_width + 16.0,
                    text.rect.top() + height / 2.0,
                ),
                vec2(32.0, 32.0),
            );
            ui.painter().rect(
                tile,
                9.0,
                mix(SURFACE, color, 8),
                Stroke::new(1.0, mix(SURFACE, color, 18)),
                StrokeKind::Inside,
            );
            icons::paint(
                ui.painter(),
                Rect::from_center_size(tile.center(), vec2(16.0, 16.0)),
                icon,
                color,
            );
        }
        ui.add_space(gap);
        // The control's [`toggle`] is named by the title.
        ui.data_mut(|d| d.insert_temp(row_title_id(), title.to_string()));
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), height),
            Layout::right_to_left(Align::Center),
            control,
        );
        ui.data_mut(|d| d.remove::<String>(row_title_id()));
    });
    ui.add_space(14.0);
}

/// Where a row keeps its title while its control draws.
fn row_title_id() -> egui::Id {
    egui::Id::new("setting row title")
}

/// The title of the row whose control is drawing, as shown.
fn row_title(ui: &Ui) -> Option<String> {
    ui.data(|d| d.get_temp::<String>(row_title_id()))
}

/// Label on the left, value on the right, one line.
pub fn stat_line(ui: &mut Ui, label: &str, value: &str) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(width, 26.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            let response = ui.label(RichText::new(label).size(SMALL).color(TEXT_MUTED));
            automation_id(&response, label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    Label::new(
                        RichText::new(value)
                            .family(medium())
                            .size(SMALL)
                            .color(TEXT),
                    )
                    .truncate(),
                );
            });
        },
    );
}

/// Small label above a large value with a unit, e.g. "Frame rate / 60 fps".
pub fn metric(ui: &mut Ui, label: &str, value: &str, unit: &str, value_size: f32) -> Response {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let response = ui.label(RichText::new(label).size(SMALL).color(TEXT_MUTED));
        automation_id(&response, label);
        let response = ui.label(value_with_unit(value, unit, value_size));
        if unit.is_empty() {
            automation_id(&response, value);
        }
    })
    .response
}

pub fn value_with_unit(value: &str, unit: &str, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        value,
        0.0,
        TextFormat {
            font_id: FontId::new(size, mono_medium()),
            color: TEXT,
            valign: Align::BOTTOM,
            ..Default::default()
        },
    );
    if !unit.is_empty() {
        job.append(
            unit,
            4.0,
            TextFormat {
                font_id: FontId::new((size * 0.55).max(11.5), FontFamily::Proportional),
                color: TEXT_MUTED,
                valign: Align::BOTTOM,
                ..Default::default()
            },
        );
    }
    job
}

// ── Status ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    /// Solid with a halo: something is live.
    Live,
    /// A ring: ready and listening.
    Ready,
    /// Solid, no halo: idle.
    Idle,
}

pub fn status_dot(ui: &Ui, center: Pos2, color: Color32, radius: f32, style: Dot) {
    let painter = ui.painter();
    match style {
        Dot::Live => {
            painter.circle_filled(center, radius * 2.6, color.gamma_multiply(0.07));
            painter.circle_filled(center, radius * 1.8, color.gamma_multiply(0.16));
            painter.circle_filled(center, radius, color);
        }
        Dot::Ready => {
            painter.circle_stroke(center, radius, Stroke::new((radius * 0.36).max(1.4), color));
            painter.circle_filled(center, radius * 0.34, color);
        }
        Dot::Idle => {
            painter.circle_filled(center, radius, color);
        }
    }
}

/// Pill with a coloured label, e.g. "USB" or "Ready".
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::new(11.5, medium()),
        Color32::PLACEHOLDER,
    );
    let size = galley.size() + vec2(18.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    automation_id(&response, text);
    ui.painter().rect(
        rect,
        size.y / 2.0,
        mix(SURFACE, color, 12),
        Stroke::new(1.0, mix(SURFACE, color, 26)),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

/// Coloured notice with an icon, title, body and optional actions. It fades
/// and settles in when it first shows.
pub fn banner(
    ui: &mut Ui,
    color: Color32,
    icon: Icon,
    title: &str,
    body: &str,
    actions_width: f32,
    actions: impl FnOnce(&mut Ui),
) {
    let shown = appear(ui, ui.id().with(("banner", title)), 0.28);
    ui.add_space((1.0 - shown) * 6.0);
    ui.scope(|ui| {
        ui.set_opacity(shown);
        banner_frame(ui, color, icon, title, body, actions_width, actions);
    });
}

fn banner_frame(
    ui: &mut Ui,
    color: Color32,
    icon: Icon,
    title: &str,
    body: &str,
    actions_width: f32,
    actions: impl FnOnce(&mut Ui),
) {
    Frame::new()
        .fill(mix(SURFACE, color, 7))
        .stroke(Stroke::new(1.0, mix(SURFACE, color, 26)))
        .corner_radius(12.0)
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let width = ui.available_width();
            let text_width = (width - 26.0 - actions_width - 16.0).max(160.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (icon_rect, _) = ui.allocate_exact_size(vec2(26.0, 20.0), Sense::hover());
                icons::paint(
                    ui.painter(),
                    Rect::from_min_size(icon_rect.min + vec2(0.0, 1.0), vec2(16.0, 16.0)),
                    icon,
                    color,
                );
                let text = ui
                    .allocate_ui_with_layout(
                        vec2(text_width, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(text_width);
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.add(
                                Label::new(
                                    RichText::new(title)
                                        .family(medium())
                                        .size(BODY)
                                        .color(mix(color, TEXT, 35)),
                                )
                                .wrap(),
                            );
                            if !body.is_empty() {
                                ui.add(Label::new(muted(body)).wrap());
                            }
                        },
                    )
                    .response;
                ui.add_space(16.0);
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), text.rect.height().max(BUTTON_HEIGHT)),
                    Layout::right_to_left(Align::Center),
                    actions,
                );
            });
        });
}

/// Empty-state block: a circled icon, a title and a hint, centred.
pub fn empty_state(ui: &mut Ui, icon: Icon, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(10.0);
        let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
        ui.painter().circle(
            rect.center(),
            22.0,
            SURFACE_RAISED,
            Stroke::new(1.0, BORDER_STRONG),
        );
        icons::paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), vec2(18.0, 18.0)),
            icon,
            TEXT_MUTED,
        );
        ui.add_space(4.0);
        wrapped(ui, title, strong);
        wrapped(ui, hint, muted);
        ui.add_space(6.0);
    });
}

// ── Charts ──────────────────────────────────────────────────────────────────

/// Area chart with a soft gradient fill. `max_hint` keeps a stable scale.
pub fn area_chart(
    ui: &mut Ui,
    data: &[f32],
    height: f32,
    color: Color32,
    max_hint: f32,
    unit: &str,
) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let label_width = 38.0;
    let plot = Rect::from_min_max(
        pos2(rect.left() + label_width, rect.top() + 6.0),
        pos2(rect.right(), rect.bottom() - 6.0),
    );
    let step = nice_step(data.iter().copied().fold(max_hint, f32::max).max(0.001) / 4.0);
    let max = step * 4.0;

    for i in 0..=4 {
        let y = egui::lerp(plot.bottom()..=plot.top(), i as f32 / 4.0);
        painter.hline(
            plot.x_range(),
            y,
            Stroke::new(1.0, if i == 0 { BORDER_STRONG } else { BORDER }),
        );
        let value = step * i as f32;
        painter.text(
            pos2(rect.left(), y),
            egui::Align2::LEFT_CENTER,
            format_axis(value, unit),
            FontId::new(10.5, FontFamily::Monospace),
            TEXT_FAINT,
        );
    }

    if data.len() < 2 {
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            "Waiting for frames",
            FontId::new(SMALL, FontFamily::Proportional),
            TEXT_FAINT,
        );
        return;
    }
    paint_series(painter, plot, data, max, color, 1.8);
}

/// Compact trend line without axes.
pub fn sparkline(ui: &mut Ui, data: &[f32], height: f32, color: Color32, max_hint: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if data.len() < 2 || !ui.is_rect_visible(rect) {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 1.0,
            Stroke::new(1.0, BORDER),
        );
        return;
    }
    let max = data.iter().copied().fold(max_hint, f32::max).max(0.001) * 1.1;
    paint_series(ui.painter(), rect, data, max, color, 1.5);
}

fn paint_series(
    painter: &egui::Painter,
    plot: Rect,
    data: &[f32],
    max: f32,
    color: Color32,
    stroke: f32,
) {
    let n = data.len();
    let points: Vec<Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = egui::lerp(plot.left()..=plot.right(), i as f32 / (n - 1) as f32);
            let y = egui::lerp(plot.bottom()..=plot.top(), (v / max).clamp(0.0, 1.0));
            pos2(x, y)
        })
        .collect();

    let mut mesh = Mesh::default();
    let top = color.gamma_multiply(0.22);
    let bottom = color.gamma_multiply(0.0);
    for (i, point) in points.iter().enumerate() {
        mesh.colored_vertex(*point, top);
        mesh.colored_vertex(pos2(point.x, plot.bottom()), bottom);
        if i > 0 {
            let base = (i as u32) * 2;
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base + 1, base);
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.add(Shape::line(points.clone(), Stroke::new(stroke, color)));
    if let Some(last) = points.last() {
        painter.circle_filled(*last, stroke + 1.2, color);
    }
}

/// The smallest of 1, 2, 2.5 or 5 × 10ⁿ that is at least `value`.
pub(super) fn nice_step(value: f32) -> f32 {
    let magnitude = 10f32.powf(value.log10().floor());
    for step in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if value <= step * magnitude {
            return step * magnitude;
        }
    }
    10.0 * magnitude
}

fn format_axis(value: f32, unit: &str) -> String {
    if value == 0.0 {
        format!("0 {unit}")
    } else if value.fract().abs() > 0.01 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

/// Egui's combo box with the house chevron instead of a filled triangle.
pub fn combo(
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: &str,
    width: f32,
) -> egui::ComboBox {
    combo_rich(id, RichText::new(selected), width)
}

/// [`combo`] showing `selected` in its own font, e.g. a language's name.
pub fn combo_rich(
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: RichText,
    width: f32,
) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected.size(13.0).color(TEXT))
        .width(width)
        .height(360.0)
        .icon(|ui, rect, _visuals, open| {
            let color = if open { TEXT } else { TEXT_MUTED };
            icons::paint(
                ui.painter(),
                Rect::from_center_size(rect.center(), vec2(14.0, 14.0)),
                Icon::Chevron,
                color,
            );
        })
}

/// Names a [`combo`] in a row by the row's title as a screen reader reads
/// it, in English too, with the title's English as its AutomationId, and
/// gives it the choice it shows, in reading order, as its value. Call it
/// after `show_ui`, which describes the combo box.
pub fn name_combo(ui: &Ui, response: &Response, shown: &str) {
    let value = i18n::automation(shown).map_or_else(|| shown.to_string(), |names| names.name);
    let title = row_title(ui).as_deref().and_then(i18n::automation);
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_value(value);
        if let Some(title) = title {
            node.set_author_id(title.id);
            node.set_label(title.name);
        }
    });
}

/// [`automation_id`] with a given id: a language's own name.
pub fn automation_id_as(response: &Response, id: &str, shown: &str) {
    let name = i18n::automation(shown).map_or_else(|| shown.to_string(), |names| names.name);
    set_automation(response, id, &name);
}

/// Two cards side by side, stretched to the taller one's height.
pub fn card_pair(
    left_ui: &mut Ui,
    right_ui: &mut Ui,
    left: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    // Columns use a justified layout, which would also justify wrapped text.
    let plain = Layout::top_down(Align::Min);
    let mut a = card().begin(left_ui);
    a.content_ui.set_width(a.content_ui.available_width());
    a.content_ui.with_layout(plain, left);
    let mut b = card().begin(right_ui);
    b.content_ui.set_width(b.content_ui.available_width());
    b.content_ui.with_layout(plain, right);
    // Stretch both to the taller content. (`set_min_height` would reserve
    // the height again below the cursor, so extend the rects directly.)
    let bottom = a
        .content_ui
        .min_rect()
        .bottom()
        .max(b.content_ui.min_rect().bottom());
    for prepared in [&mut a, &mut b] {
        let rect = prepared.content_ui.min_rect();
        prepared
            .content_ui
            .expand_to_include_rect(Rect::from_min_max(
                rect.left_top(),
                pos2(rect.left(), bottom),
            ));
    }
    a.end(left_ui);
    b.end(right_ui);
}

// ── Choices ─────────────────────────────────────────────────────────────────

/// One of a few options, as a card: a radio mark, a title and a line under
/// it. Setup's choices.
pub fn choice_card(
    ui: &mut Ui,
    selected: bool,
    title: &str,
    description: &str,
    size: egui::Vec2,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, title));
    automation_id(&response, title);
    if ui.is_rect_visible(rect) {
        let hover =
            ui.ctx()
                .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
        let chosen = ui
            .ctx()
            .animate_bool_with_time(response.id.with("selected"), selected, 0.14);
        ui.painter().rect(
            rect,
            10.0,
            SURFACE
                .lerp_to_gamma(SURFACE_RAISED, hover)
                .lerp_to_gamma(mix(SURFACE, ACCENT, 7), chosen),
            Stroke::new(1.0, BORDER_STRONG.lerp_to_gamma(ACCENT, chosen)),
            StrokeKind::Inside,
        );
        focus_ring(ui, &response, rect, 10.0);
        // The radio mark.
        let mark = pos2(rect.left() + 22.0, rect.top() + 22.0);
        ui.painter().circle_stroke(
            mark,
            7.0,
            Stroke::new(1.5, TEXT_FAINT.lerp_to_gamma(ACCENT, chosen)),
        );
        if chosen > 0.0 {
            ui.painter()
                .circle_filled(mark, 3.5 * chosen, ACCENT.gamma_multiply(chosen));
        }
        let text_rect = Rect::from_min_max(
            pos2(rect.left() + 42.0, rect.top() + 13.0),
            pos2(rect.right() - 14.0, rect.bottom() - 10.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        child.spacing_mut().item_spacing.y = 2.0;
        wrapped(&mut child, title, strong);
        if !description.is_empty() {
            wrapped(&mut child, description, muted);
        }
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A compact option showing `text` in its own font: a language in setup.
/// `id` is its AutomationId, its name in English.
pub fn choice_tile(
    ui: &mut Ui,
    selected: bool,
    text: RichText,
    id: &str,
    size: egui::Vec2,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, text.text()));
    automation_id_as(&response, id, text.text());
    if ui.is_rect_visible(rect) {
        let hover =
            ui.ctx()
                .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
        let chosen = ui
            .ctx()
            .animate_bool_with_time(response.id.with("selected"), selected, 0.14);
        ui.painter().rect(
            rect,
            9.0,
            SURFACE
                .lerp_to_gamma(SURFACE_RAISED, hover)
                .lerp_to_gamma(mix(SURFACE, ACCENT, 7), chosen),
            Stroke::new(1.0, BORDER_STRONG.lerp_to_gamma(ACCENT, chosen)),
            StrokeKind::Inside,
        );
        focus_ring(ui, &response, rect, 9.0);
        let galley = WidgetText::from(text.size(13.5).color(TEXT)).into_galley(
            ui,
            Some(TextWrapMode::Truncate),
            rect.width() - 44.0,
            FontSelection::Default,
        );
        let pos = pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, TEXT);
        if chosen > 0.0 {
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    pos2(rect.right() - 18.0, rect.center().y),
                    vec2(14.0, 14.0),
                ),
                Icon::Check,
                ACCENT.gamma_multiply(chosen),
            );
        }
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}
