//! Visual language shared with the iPad app and the website: near-black
//! surfaces, a single lime accent taken from the logo, and Geist type.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText,
    Shadow, Stroke, TextStyle,
};

// ── Surfaces ────────────────────────────────────────────────────────────────
pub const CANVAS: Color32 = Color32::from_rgb(10, 10, 11);
pub const SIDEBAR: Color32 = Color32::from_rgb(14, 14, 16);
pub const SURFACE: Color32 = Color32::from_rgb(18, 18, 20);
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(26, 26, 29);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(33, 33, 37);
pub const BORDER: Color32 = Color32::from_rgb(35, 35, 40);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(52, 52, 59);

// ── Text ────────────────────────────────────────────────────────────────────
pub const TEXT: Color32 = Color32::from_rgb(244, 244, 245);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(161, 161, 170);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(113, 113, 122);

// ── Accent (the logo's lime) and states ─────────────────────────────────────
pub const ACCENT: Color32 = Color32::from_rgb(232, 255, 71);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(241, 255, 138);
pub const ACCENT_PRESSED: Color32 = Color32::from_rgb(208, 232, 40);
pub const ON_ACCENT: Color32 = Color32::from_rgb(12, 13, 4);
pub const WARNING: Color32 = Color32::from_rgb(251, 191, 36);
pub const DANGER: Color32 = Color32::from_rgb(248, 113, 113);

/// `color` laid over `base` at `percent` opacity, as an opaque color.
pub const fn mix(base: Color32, color: Color32, percent: u32) -> Color32 {
    Color32::from_rgb(
        channel(base.r(), color.r(), percent),
        channel(base.g(), color.g(), percent),
        channel(base.b(), color.b(), percent),
    )
}

const fn channel(base: u8, color: u8, percent: u32) -> u8 {
    ((base as u32 * (100 - percent) + color as u32 * percent + 50) / 100) as u8
}

/// A state color as a quiet card fill.
pub const fn tint(color: Color32) -> Color32 {
    mix(SURFACE, color, 9)
}

/// A state color as a hairline border.
pub const fn tint_border(color: Color32) -> Color32 {
    mix(SURFACE, color, 28)
}

// ── Type ────────────────────────────────────────────────────────────────────
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn mono_medium() -> FontFamily {
    FontFamily::Name("mono-medium".into())
}

pub const TITLE: f32 = 22.0;
pub const HEADING: f32 = 15.0;
pub const BODY: f32 = 13.5;
pub const SMALL: f32 = 12.5;
pub const CAPTION: f32 = 11.5;

pub fn title(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .family(semibold())
        .size(TITLE)
        .color(TEXT)
}

pub fn heading(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .family(semibold())
        .size(HEADING)
        .color(TEXT)
}

pub fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text).family(medium()).size(BODY).color(TEXT)
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).size(SMALL).color(TEXT_MUTED)
}

pub fn faint(text: impl Into<String>) -> RichText {
    RichText::new(text).size(CAPTION).color(TEXT_FAINT)
}

/// Small uppercase label above a group or value.
pub fn overline(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .family(medium())
        .size(10.5)
        .extra_letter_spacing(0.9)
        .color(TEXT_FAINT)
}

pub fn mono(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text)
        .family(mono_medium())
        .size(size)
        .color(TEXT)
}

// ── Setup ───────────────────────────────────────────────────────────────────

/// Install fonts and the dark style. Called once from the app constructor.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(apply_style);
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let faces: [(&str, &'static [u8]); 5] = [
        (
            "geist",
            include_bytes!("../../assets/fonts/Geist-Regular.ttf"),
        ),
        (
            "geist-medium",
            include_bytes!("../../assets/fonts/Geist-Medium.ttf"),
        ),
        (
            "geist-semibold",
            include_bytes!("../../assets/fonts/Geist-SemiBold.ttf"),
        ),
        (
            "geist-mono",
            include_bytes!("../../assets/fonts/GeistMono-Regular.ttf"),
        ),
        (
            "geist-mono-medium",
            include_bytes!("../../assets/fonts/GeistMono-Medium.ttf"),
        ),
    ];
    for (name, bytes) in faces {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }

    // Keep egui's bundled faces behind Geist so device names in other
    // scripts, symbols and emoji still render.
    let fallback_sans = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let fallback_mono = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let with_fallback = |face: &str, fallback: &[String]| {
        std::iter::once(face.to_string())
            .chain(fallback.iter().cloned())
            .collect::<Vec<_>>()
    };
    fonts.families.insert(
        FontFamily::Proportional,
        with_fallback("geist", &fallback_sans),
    );
    fonts.families.insert(
        FontFamily::Monospace,
        with_fallback("geist-mono", &fallback_mono),
    );
    fonts
        .families
        .insert(medium(), with_fallback("geist-medium", &fallback_sans));
    fonts
        .families
        .insert(semibold(), with_fallback("geist-semibold", &fallback_sans));
    fonts.families.insert(
        mono_medium(),
        with_fallback("geist-mono-medium", &fallback_mono),
    );
    fonts
}

fn apply_style(style: &mut egui::Style) {
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(CAPTION, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(BODY, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(BODY, medium())),
        (TextStyle::Heading, FontId::new(TITLE, semibold())),
        (
            TextStyle::Monospace,
            FontId::new(BODY, FontFamily::Monospace),
        ),
    ]
    .into();

    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.35;

    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(8.0, 8.0);
    spacing.button_padding = egui::vec2(12.0, 6.0);
    spacing.interact_size = egui::vec2(32.0, 32.0);
    spacing.combo_height = 320.0;
    spacing.menu_margin = Margin::same(6);
    spacing.window_margin = Margin::same(20);
    spacing.scroll = egui::style::ScrollStyle {
        floating: true,
        bar_width: 8.0,
        floating_width: 3.0,
        floating_allocated_width: 0.0,
        bar_inner_margin: 3.0,
        bar_outer_margin: 3.0,
        foreground_color: false,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.0,
        dormant_handle_opacity: 0.0,
        active_handle_opacity: 0.55,
        interact_handle_opacity: 0.8,
        ..egui::style::ScrollStyle::floating()
    };

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = None;
    v.panel_fill = CANVAS;
    v.window_fill = SURFACE_RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER_STRONG);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 18],
        blur: 48,
        spread: 0,
        color: Color32::from_black_alpha(150),
    };
    v.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.extreme_bg_color = CANVAS;
    v.faint_bg_color = SURFACE;
    v.code_bg_color = SURFACE_RAISED;
    v.text_edit_bg_color = Some(CANVAS);
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARNING;
    v.error_fg_color = DANGER;
    v.selection.bg_fill = mix(SURFACE_RAISED, ACCENT, 16);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.text_cursor.stroke = Stroke::new(1.5, ACCENT);
    v.slider_trailing_fill = true;
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    v.button_frame = true;
    v.disabled_alpha = 0.45;

    let radius = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = SURFACE_RAISED;
    w.inactive.weak_bg_fill = SURFACE_RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = SURFACE_HOVER;
    w.hovered.weak_bg_fill = SURFACE_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, mix(BORDER_STRONG, TEXT, 12));
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = SURFACE_HOVER;
    w.active.weak_bg_fill = SURFACE_HOVER;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = SURFACE_HOVER;
    w.open.weak_bg_fill = SURFACE_HOVER;
    w.open.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.corner_radius = radius;
}
