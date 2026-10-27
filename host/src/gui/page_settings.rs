//! Settings, grouped by what they change.

use eframe::egui::{self, pos2, vec2, Color32, Rect, RichText, Sense, Ui};

use super::page_performance::page_header;
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{AnalyzerApp, StatsSnapshot, CAPTURE_VIRTUAL_SENTINEL, ENCODER_CHOICES};
use crate::encoder::input::EncoderInput;

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
            let mut fps = self.settings_fps_target;
            widgets::setting_row(
                ui,
                "Frame rate",
                "Pictures per second. Higher feels smoother but needs a faster network. The iPad can ask for less.",
                230.0,
                |ui| {
                    widgets::segmented(ui, &mut fps, &[(30, "30"), (60, "60"), (90, "90"), (120, "120")]);
                },
            );
            self.set_fps(fps);
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

        // ── Advanced ────────────────────────────────────────────────────────
        // Troubleshooting options. The defaults suit almost every PC.
        widgets::section_label(ui, "Advanced");
        ui.label(muted("Most people never need these. Change them only if the picture looks wrong or a support guide asks you to."));
        ui.add_space(4.0);
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
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
            widgets::divider(ui);

            let mut hevc = self.settings_hevc_enabled;
            widgets::setting_row(
                ui,
                "Sharper video format (HEVC)",
                "Experimental. Sharper at the same quality limit on iPads and graphics cards that support it; others keep the standard format.",
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut hevc, "Prefer HEVC (H.265) when the iPad supports it");
                },
            );
            if hevc != self.settings_hevc_enabled {
                self.set_hevc(hevc);
            }
            widgets::divider(ui);
            let mut packet = self.settings_packet_size;
            widgets::setting_row(
                ui,
                "Packet size",
                "Leave at 1400 on a home network. Use 1200 only when connecting through a VPN such as Tailscale.",
                150.0,
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut packet)
                            .range(576..=1400)
                            .speed(4.0)
                            .suffix(" bytes"),
                    );
                },
            );
            if packet != self.settings_packet_size {
                self.set_packet_size(packet);
            }
            widgets::divider(ui);
            let detected = super::friendly_codec(&snap.codec_name);
            let mut choice = self.settings_encoder_choice.clone();
            widgets::setting_row(
                ui,
                "Video encoder",
                &format!("Automatic uses your graphics card when it can. Change this only if the picture is missing or broken. In use now: {detected}."),
                200.0,
                |ui| {
                    widgets::combo("encoder-override", &choice_label(&choice), 190.0).show_ui(
                        ui,
                        |ui| {
                            for (label, _) in ENCODER_CHOICES {
                                if ui
                                    .selectable_label(choice == *label, choice_label(label))
                                    .clicked()
                                {
                                    choice = (*label).to_string();
                                }
                            }
                        },
                    );
                },
            );
            if choice != self.settings_encoder_choice {
                self.set_encoder_choice(&choice);
            }
            widgets::divider(ui);

            let mut input = self.settings_encoder_input;
            widgets::setting_row(
                ui,
                "Encoder input",
                "YUV420, the default, suits most PCs. Try another option only if the picture has wrong colours or never appears. Applies after a restart.",
                230.0,
                |ui| {
                    widgets::segmented(
                        ui,
                        &mut input,
                        &[
                            (EncoderInput::Auto, "Auto"),
                            (EncoderInput::Bgra, "BGRA"),
                            (EncoderInput::Yuv420, "YUV420"),
                        ],
                    );
                },
            );
            if input != self.settings_encoder_input {
                self.set_encoder_input(input);
            }
        });
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

fn choice_label(label: &str) -> String {
    match label {
        "Auto" => "Automatic".into(),
        "NVENC" => "NVIDIA NVENC".into(),
        "AMF" => "AMD AMF".into(),
        "QSV" => "Intel Quick Sync".into(),
        "x264" => "x264 (CPU)".into(),
        other => other.into(),
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
