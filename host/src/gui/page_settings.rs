//! Settings, grouped by what they change.

use eframe::egui::{self, pos2, vec2, Color32, Rect, RichText, Sense, Ui};

use super::page_performance::page_header;
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{AnalyzerApp, StatsSnapshot, CAPTURE_VIRTUAL_SENTINEL};

impl AnalyzerApp {
    pub(super) fn settings_page(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        page_header(ui, "Settings", "Changes save automatically.", |_| {});
        self.restart_notice(ui);

        // ── Display ─────────────────────────────────────────────────────────
        widgets::section_label(ui, "Display");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut refresh = false;
            widgets::setting_row(
                ui,
                "How the iPad shows your PC",
                "Mirror my screen shows your main screen. Second screen adds the iPad as an extra screen while it is connected. You can also mirror one particular monitor.",
                300.0,
                |ui| {
                    refresh = widgets::icon_button(ui, super::icons::Icon::Refresh, "Refresh displays").clicked();
                    self.display_picker(ui, 250.0);
                },
            );
            if refresh {
                self.refresh_outputs();
            }
            widgets::divider(ui);
            let mut vdd_match = self.settings_vdd_match;
            let extending = self.settings_capture_display == CAPTURE_VIRTUAL_SENTINEL;
            widgets::setting_row(
                ui,
                "Match the iPad's resolution",
                vdd_match_description(extending),
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut vdd_match, "Match the second screen to the iPad's resolution");
                },
            );
            if vdd_match != self.settings_vdd_match {
                self.set_vdd_match(vdd_match);
            }
            widgets::divider(ui);
            let fps_env = self.overrides.target_fps.env();
            let mut fps = match fps_env {
                Some(_) => self.overrides.target_fps.value,
                None => self.settings_fps_target,
            };
            widgets::setting_row(
                ui,
                "Frame rate",
                &env_or(fps_env, "Pictures per second. Higher feels smoother but needs a faster network. The iPad can ask for less."),
                230.0,
                |ui| {
                    ui.add_enabled_ui(fps_env.is_none(), |ui| {
                        widgets::segmented(ui, &mut fps, &[(30, "30"), (60, "60"), (90, "90"), (120, "120")]);
                    });
                },
            );
            if self.overrides.target_fps.env().is_none() {
                self.set_fps(fps);
            }
            widgets::divider(ui);
            let mut bitrate = self.settings_bitrate_mbps;
            widgets::setting_row(
                ui,
                "Video quality limit",
                "Higher is sharper but needs a stronger network. The stream lowers it by itself when the network is busy.",
                290.0,
                |ui| {
                    ui.add_sized(
                        vec2(64.0, 24.0),
                        egui::Label::new(mono(format!("{bitrate:.0} Mbps"), 13.0)),
                    );
                    widgets::slider(ui, &mut bitrate, 4.0..=50.0, 1.0, 210.0, "Maximum bitrate");
                },
            );
            if bitrate != self.settings_bitrate_mbps {
                self.set_bitrate_mbps(bitrate);
            }
        });
        ui.add_space(GAP - 6.0);

        // ── Audio ───────────────────────────────────────────────────────────
        widgets::section_label(ui, "Audio");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let enabled = self
                .control
                .shared
                .audio_enabled
                .load(std::sync::atomic::Ordering::SeqCst);
            let mut audio = enabled;
            widgets::setting_row(
                ui,
                "Stream PC audio",
                "Plays this PC's sound on the iPad. Video keeps streaming if audio fails.",
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut audio, "Stream PC audio");
                },
            );
            if audio != enabled {
                self.set_stream_audio(audio);
            }
        });
        ui.add_space(GAP - 6.0);

        // ── Security ────────────────────────────────────────────────────────
        widgets::section_label(ui, "Security");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let required = self.control.shared.pairing.lock().required;
            let mut require = required;
            widgets::setting_row(
                ui,
                "Require pairing",
                "New iPads on Wi-Fi type the pairing code shown on the Stream page once. USB needs no code.",
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut require, "Require pairing");
                },
            );
            if require != required {
                self.set_require_pairing(require);
            }
            widgets::divider(ui);
            let mut forget = false;
            widgets::setting_row(
                ui,
                "Forget all iPads",
                "Creates a new pairing key. Every iPad has to pair again.",
                170.0,
                |ui| {
                    forget = widgets::button(ui, Tone::Danger, None, "Forget all iPads").clicked();
                },
            );
            if forget {
                self.forget_all_ipads();
            }
            if let Some(error) = &self.settings_error {
                ui.add(egui::Label::new(RichText::new(error).size(SMALL).color(DANGER)).wrap());
                ui.add_space(10.0);
            }
        });
        ui.add_space(GAP - 6.0);

        // ── General ─────────────────────────────────────────────────────────
        widgets::section_label(ui, "General");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if cfg!(windows) {
                let mut boot = self.settings_start_on_boot;
                widgets::setting_row(
                    ui,
                    "Start with Windows",
                    "Opens EternalMonitor when you sign in, so your iPad can connect right away.",
                    64.0,
                    |ui| {
                        widgets::toggle(ui, &mut boot, "Start on Windows startup");
                    },
                );
                if boot != self.settings_start_on_boot {
                    self.set_start_on_boot(boot);
                }
                widgets::divider(ui);
            }
            self.update_settings(ui);
        });
        ui.add_space(GAP - 6.0);

        widgets::section_label(ui, "Support diagnostics");
        widgets::list_card().show(ui, |ui| {
            let mut upload = crate::telemetry::enabled();
            widgets::setting_row(ui, "Automatically share diagnostics", "Sends device details, errors and stream statistics to EternalMonitor support. Includes PC names and network addresses. Screen content, audio and pairing credentials are excluded.", 64.0, |ui| { widgets::toggle(ui, &mut upload, "Automatically share diagnostics"); });
            if upload != crate::telemetry::enabled() { crate::telemetry::set_enabled(upload); }
            ui.label(muted(crate::telemetry::status()));
        });
        ui.add_space(GAP - 6.0);

        self.troubleshooting_section(ui, snap);
        ui.add_space(GAP - 6.0);

        // ── About ───────────────────────────────────────────────────────────
        widgets::section_label(ui, "About");
        widgets::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                if let Some((_, texture)) = &self.logo {
                    ui.painter().image(
                        texture.id(),
                        rect,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(heading("EternalMonitor"));
                    ui.label(muted(format!(
                        "Version {} · Built by Sideband Studio",
                        env!("CARGO_PKG_VERSION")
                    )));
                });
            });
            ui.add_space(10.0);
            let mut feedback = false;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                widgets::link(ui, "sideband.studio", "https://sideband.studio");
                widgets::link(
                    ui,
                    "Source code",
                    "https://github.com/sidebandstudio/EternalMonitor",
                );
                feedback =
                    widgets::link_button(ui, "Send feedback", super::icons::Icon::Mail).clicked();
                widgets::link(ui, "hello@sideband.studio", "mailto:hello@sideband.studio");
            });
            if feedback {
                self.open_feedback();
            }
        });
    }
}

/// The Video encoder choices in words a user knows: the maker of the
/// graphics card, or the processor.
pub(super) fn choice_label(label: &str) -> String {
    match label {
        "Auto" => "Automatic".into(),
        "NVENC" => "NVIDIA".into(),
        "AMF" => "AMD".into(),
        "QSV" => "Intel".into(),
        "x264" => "Software (processor)".into(),
        other => other.into(),
    }
}

/// A row's description, or which environment variable fixes its value.
pub(super) fn env_or(env: Option<&str>, description: &str) -> String {
    match env {
        Some(name) => format!("Set by {name} on this PC, which wins over this setting."),
        None => description.to_string(),
    }
}

/// What "Match the iPad's resolution" does, and when a change applies: the
/// driver reads the modes when the second screen starts, and restarting
/// the stream keeps the second screen that is already there.
pub(super) fn vdd_match_description(extending: bool) -> &'static str {
    if extending {
        "Makes the second screen exactly as sharp and smooth as the iPad's display. A change applies the next time the second screen starts, when the iPad connects again."
    } else {
        "Used by Second screen. Makes it exactly as sharp and smooth as the iPad's display. A change applies the next time the second screen starts."
    }
}
