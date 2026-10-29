//! Settings > Troubleshooting: what a support reply asks a user to change,
//! in place of environment variables. Closed until clicked; every default is
//! Automatic, which is the app's normal behaviour.

use eframe::egui::{self, RichText, Ui};

use super::page_settings::{choice_label, env_or};
use super::renderer;
use super::theme::*;
use super::widgets::{self, Tone};
use super::{AnalyzerApp, StatsSnapshot, ENCODER_CHOICES};
use crate::encoder::input::EncoderInput;
use crate::gpu_preference::{Change, GpuFacts, Preference};
use crate::troubleshooting::{WindowDrawing, FULL_PACKET, SMALL_PACKET};

impl AnalyzerApp {
    pub(super) fn troubleshooting_section(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        widgets::section_label(ui, "Troubleshooting");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut toggle = false;
            widgets::setting_row(
                ui,
                "Troubleshooting options",
                "For when EternalMonitor doesn't work right, or support asks you to change something. Automatic suits almost every PC.",
                140.0,
                |ui| {
                    let label = if self.troubleshooting_open {
                        "Hide options"
                    } else {
                        "Show options"
                    };
                    toggle = widgets::button(ui, Tone::Secondary, None, label).clicked();
                },
            );
            if toggle {
                self.troubleshooting_open = !self.troubleshooting_open;
                if self.troubleshooting_open {
                    self.refresh_gpu_facts();
                }
            }
            if !self.troubleshooting_open {
                return;
            }
            widgets::divider(ui);
            self.graphics_card_row(ui);
            widgets::divider(ui);
            self.window_drawing_row(ui);
            widgets::divider(ui);
            self.video_rows(ui, snap);
            widgets::divider(ui);
            let mut reset = false;
            widgets::setting_row(
                ui,
                "Reset troubleshooting settings",
                "Puts every option above back to Automatic.",
                110.0,
                |ui| {
                    reset = widgets::button(ui, Tone::Secondary, None, "Reset").clicked();
                },
            );
            if reset {
                self.reset_troubleshooting();
            }
        });
    }

    fn graphics_card_row(&mut self, ui: &mut Ui) {
        let facts = self.control.shared.gpu_facts.lock().clone();
        let record = self.control.shared.gpu_switch.lock().clone();
        let current = crate::gpu_preference::preference_in(self.gpu_value.as_deref())
            .map_or(Some(Preference::Automatic), Preference::from_value);
        let shown = match &self.gpu_choice {
            Some(Change::Set(preference)) => Some(*preference),
            Some(Change::Restore(value)) => crate::gpu_preference::preference_in(value.as_deref())
                .map_or(Some(Preference::Automatic), Preference::from_value),
            None => current,
        };
        let mut picked = None;
        let choice = facts.as_ref().filter(|facts| facts.has_choice());
        widgets::setting_row(
            ui,
            "Graphics card for EternalMonitor",
            &graphics_card_description(facts.as_ref()),
            300.0,
            |ui| {
                let Some(facts) = choice else {
                    return;
                };
                let selected = shown.map_or_else(
                    || "Chosen in Windows Settings".to_string(),
                    |preference| gpu_option_label(facts, preference),
                );
                widgets::combo("graphics-card", &selected, 290.0).show_ui(ui, |ui| {
                    for preference in [
                        Preference::Automatic,
                        Preference::PowerSaving,
                        Preference::HighPerformance,
                    ] {
                        if ui
                            .selectable_label(
                                shown == Some(preference),
                                gpu_option_label(facts, preference),
                            )
                            .clicked()
                        {
                            picked = Some(preference);
                        }
                    }
                });
            },
        );
        if let Some(preference) = picked {
            self.choose_graphics_card(preference);
        }
        if let Some(facts) = &facts {
            for line in wiring_lines(facts) {
                ui.label(muted(line));
            }
        }
        if record.as_ref().is_some_and(|record| record.failed) {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "The last change didn't help: another app is probably choosing the card. See the message on the Stream page.",
                    )
                    .size(SMALL)
                    .color(WARNING),
                )
                .wrap(),
            );
        }
        if self.gpu_choice.is_some() {
            let mut restart = false;
            ui.horizontal(|ui| {
                ui.label(muted(
                    "Windows applies this when EternalMonitor starts again.",
                ));
                restart = widgets::button(
                    ui,
                    Tone::Primary,
                    Some(super::icons::Icon::Refresh),
                    "Restart EternalMonitor",
                )
                .clicked();
            });
            if restart {
                self.apply_graphics_card();
            }
        }
        self.gpu_error_line(ui);
        ui.add_space(8.0);
    }

    fn window_drawing_row(&mut self, ui: &mut Ui) {
        let env = self.overrides.window.env();
        let status = renderer::status().unwrap_or_default();
        let mut drawing = self.settings_window_drawing;
        widgets::setting_row(
            ui,
            "Window drawing",
            &env_or(
                env,
                "How EternalMonitor draws this window. Change it only if this window stays blank or the app closes as it starts. Applies at the next start.",
            ),
            220.0,
            |ui| {
                ui.add_enabled_ui(env.is_none(), |ui| {
                    widgets::combo("window-drawing", window_drawing_label(drawing), 210.0)
                        .show_ui(ui, |ui| {
                            for option in [
                                WindowDrawing::Automatic,
                                WindowDrawing::Dx12,
                                WindowDrawing::Vulkan,
                                WindowDrawing::Warp,
                            ] {
                                ui.selectable_value(
                                    &mut drawing,
                                    option,
                                    window_drawing_label(option),
                                );
                            }
                        });
                });
            },
        );
        self.set_window_drawing(drawing);
        for line in drawing_status_lines(&status, self.settings_window_drawing) {
            ui.label(muted(line));
        }
        let mut retry = false;
        let mut restart = false;
        if status.avoiding && env.is_none() {
            ui.horizontal(|ui| {
                ui.label(muted(
                    "After a problem when EternalMonitor started, it set a drawing method aside.",
                ));
                retry = widgets::link_button(ui, "Try all again", super::icons::Icon::Refresh)
                    .clicked();
            });
        }
        if env.is_none() && (self.settings_window_drawing != status.choice || status.reset_pending)
        {
            ui.horizontal(|ui| {
                ui.label(muted("Applies when EternalMonitor starts again."));
                restart = widgets::button(
                    ui,
                    Tone::Primary,
                    Some(super::icons::Icon::Refresh),
                    "Restart EternalMonitor",
                )
                .clicked();
            });
        }
        if retry {
            renderer::reset_avoidance();
        }
        if restart {
            self.restart_now(crate::relaunch::Reason::Settings);
        }
        ui.add_space(8.0);
    }

    fn video_rows(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        let encoder_env = self.overrides.encoder.env();
        let detected = super::friendly_codec(&snap.codec_name);
        let mut choice = self.settings_encoder_choice.clone();
        let shown = match encoder_env {
            Some(_) => ENCODER_CHOICES
                .iter()
                .find(|(_, ffmpeg)| Some(*ffmpeg) == self.overrides.encoder.value.as_deref())
                .map_or_else(
                    || self.overrides.encoder.value.clone().unwrap_or_default(),
                    |(label, _)| choice_label(label),
                ),
            None => choice_label(&choice),
        };
        widgets::setting_row(
            ui,
            "Video encoder",
            &env_or(
                encoder_env,
                &format!("Automatic uses your graphics card. Pick another only if the picture never appears or looks broken. In use now: {detected}."),
            ),
            200.0,
            |ui| {
                ui.add_enabled_ui(encoder_env.is_none(), |ui| {
                    widgets::combo("encoder-override", &shown, 190.0).show_ui(ui, |ui| {
                        for (label, _) in ENCODER_CHOICES {
                            if ui
                                .selectable_label(choice == *label, choice_label(label))
                                .clicked()
                            {
                                choice = (*label).to_string();
                            }
                        }
                    });
                });
            },
        );
        if choice != self.settings_encoder_choice {
            self.set_encoder_choice(&choice);
        }
        widgets::divider(ui);

        let input_env = self.overrides.encoder_input.env();
        let mut input = match input_env {
            Some(_) => self.overrides.encoder_input.value,
            None => self.settings_encoder_input,
        };
        widgets::setting_row(
            ui,
            "Encoder input",
            &env_or(
                input_env,
                "YUV420, the default, suits most PCs. Try another option only if the picture has wrong colours or never appears. Applies after a restart.",
            ),
            230.0,
            |ui| {
                ui.add_enabled_ui(input_env.is_none(), |ui| {
                    widgets::segmented(
                        ui,
                        &mut input,
                        &[
                            (EncoderInput::Auto, "Auto"),
                            (EncoderInput::Bgra, "BGRA"),
                            (EncoderInput::Yuv420, "YUV420"),
                        ],
                    );
                });
            },
        );
        if input_env.is_none() && input != self.settings_encoder_input {
            self.set_encoder_input(input);
        }
        widgets::divider(ui);

        let hevc_env = self.overrides.hevc.env();
        let mut hevc = match hevc_env {
            Some(_) => self.overrides.hevc.value,
            None => self.settings_hevc_enabled,
        };
        widgets::setting_row(
            ui,
            "Sharper video format (HEVC)",
            &env_or(
                hevc_env,
                "Sharper at the same quality limit on iPads and graphics cards that support it. Turn it off if the picture stutters or never appears.",
            ),
            64.0,
            |ui| {
                ui.add_enabled_ui(hevc_env.is_none(), |ui| {
                    widgets::toggle(ui, &mut hevc, "Prefer HEVC (H.265) when the iPad supports it");
                });
            },
        );
        if hevc_env.is_none() && hevc != self.settings_hevc_enabled {
            self.set_hevc(hevc);
        }
        widgets::divider(ui);

        let packet_env = self.overrides.max_dgram.env();
        let small_now = match packet_env {
            Some(_) => self.overrides.max_dgram.value < FULL_PACKET,
            None => self.settings_packet_size < u32::from(FULL_PACKET),
        };
        let mut small = small_now;
        widgets::setting_row(
            ui,
            "Smaller network packets",
            &env_or(
                packet_env,
                "Turn on if the picture breaks up over a VPN (such as Tailscale) or a phone hotspot.",
            ),
            64.0,
            |ui| {
                ui.add_enabled_ui(packet_env.is_none(), |ui| {
                    widgets::toggle(ui, &mut small, "Use smaller network packets");
                });
            },
        );
        if packet_env.is_none() && small != small_now {
            self.set_packet_size(u32::from(if small { SMALL_PACKET } else { FULL_PACKET }));
        }
    }
}

/// A card's name without trademark marks, for buttons and short lines.
pub(super) fn short_gpu_name(name: &str) -> String {
    let name = name
        .replace("(TM)", "")
        .replace("(R)", "")
        .replace("(tm)", "");
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = name
        .strip_suffix(" Graphics")
        .or_else(|| name.strip_suffix(" GPU"))
        .unwrap_or(&name)
        .trim();
    trimmed.to_string()
}

/// One graphics card choice, naming the card it selects on this PC.
pub(super) fn gpu_option_label(facts: &GpuFacts, preference: Preference) -> String {
    let named = |label: &str| match facts.card_for(preference) {
        Some(card) => format!("{label} ({})", short_gpu_name(&card.name)),
        None => label.to_string(),
    };
    match preference {
        Preference::Automatic => "Automatic (Windows decides)".to_string(),
        Preference::PowerSaving => named("Power saving"),
        Preference::HighPerformance => named("High performance"),
    }
}

pub(super) fn graphics_card_description(facts: Option<&GpuFacts>) -> String {
    match facts {
        Some(facts) if facts.has_choice() => "Which card Windows runs EternalMonitor on. On a laptop, pick the card your screen is connected to.".to_string(),
        Some(facts) if facts.adapters.len() > 1 => "Windows can't say for certain which card each choice picks on this PC. Choose one in Windows Settings > System > Display > Graphics.".to_string(),
        Some(facts) => match facts.running_on() {
            Some(card) => format!(
                "This PC has one graphics card, the {}, so there is nothing to choose.",
                card.name
            ),
            None => "Windows didn't list a graphics card.".to_string(),
        },
        None => "Windows lists the graphics cards here.".to_string(),
    }
}

/// Which card runs the app now and which card each screen is connected to.
pub(super) fn wiring_lines(facts: &GpuFacts) -> Vec<String> {
    let mut lines = Vec::new();
    if facts.adapters.len() < 2 {
        return lines;
    }
    if let Some(card) = facts.running_on() {
        lines.push(format!("EternalMonitor runs on the {} now.", card.name));
    }
    for (screen, _) in &facts.screens {
        if let Some(card) = facts.wired_to(screen) {
            let short = screen.rsplit('\\').next().unwrap_or(screen);
            lines.push(format!("{short} is connected to the {}.", card.name));
        }
    }
    lines
}

pub(super) fn window_drawing_label(drawing: WindowDrawing) -> &'static str {
    match drawing {
        WindowDrawing::Automatic => "Automatic",
        WindowDrawing::Dx12 => "DirectX 12",
        WindowDrawing::Vulkan => "Vulkan",
        WindowDrawing::Warp => "Software (slower)",
    }
}

/// What the window draws with now and why, under Window drawing.
pub(super) fn drawing_status_lines(status: &renderer::Status, saved: WindowDrawing) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(backend) = &status.backend {
        lines.push(format!("Drawing with {backend} now."));
    }
    if status.chosen_failed && saved == status.choice {
        lines.push(format!(
            "{} didn't start last time, so EternalMonitor chose automatically.",
            window_drawing_label(status.choice)
        ));
    }
    lines
}
