//! Settings, grouped by what they change.

use eframe::egui::{self, pos2, vec2, Align, Color32, Layout, Rect, RichText, Sense, Ui};

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
                "Display to stream",
                "Mirror a monitor, or extend Windows onto the iPad with a virtual display that exists only while streaming.",
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
                if extending {
                    "Sizes the extended display to the iPad's screen and refresh rate."
                } else {
                    "Used when extending. Sizes the virtual display to the iPad's screen."
                },
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut vdd_match, "Match extended display to the iPad's resolution");
                },
            );
            if vdd_match != self.settings_vdd_match {
                self.set_vdd_match(vdd_match);
            }
        });
        ui.add_space(GAP - 6.0);

        // ── Video quality ───────────────────────────────────────────────────
        widgets::section_label(ui, "Video quality");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut fps = self.settings_fps_target;
            widgets::setting_row(
                ui,
                "Frame rate",
                "The iPad can ask for less. Higher rates feel smoother and use more bandwidth.",
                230.0,
                |ui| {
                    widgets::segmented(ui, &mut fps, &[(30, "30"), (60, "60"), (90, "90"), (120, "120")]);
                },
            );
            self.set_fps(fps);
            widgets::divider(ui);

            let mut bitrate = self.settings_bitrate_mbps;
            widgets::setting_row(
                ui,
                "Maximum bitrate",
                "The stream adapts below this ceiling when the network is busy.",
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
                "Prefer HEVC (H.265)",
                "Experimental. Sharper at the same bitrate when the iPad supports it. Falls back to H.264 on GPUs without an HEVC encoder.",
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut hevc, "Prefer HEVC (H.265) when the iPad supports it");
                },
            );
            if hevc != self.settings_hevc_enabled {
                self.set_hevc(hevc);
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

        // ── Network ─────────────────────────────────────────────────────────
        widgets::section_label(ui, "Network");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut packet = self.settings_packet_size;
            widgets::setting_row(
                ui,
                "Packet size",
                "Use 1200 when connecting over Tailscale or another VPN. 1400 suits most home networks.",
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
        });
        ui.add_space(GAP - 6.0);

        // ── Encoder ─────────────────────────────────────────────────────────
        widgets::section_label(ui, "Encoder");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let detected = super::friendly_codec(&snap.codec_name);
            let mut choice = self.settings_encoder_choice.clone();
            widgets::setting_row(
                ui,
                "Encoder",
                &format!("Auto picks the best hardware encoder. In use now: {detected}."),
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
                "Pixel format handed to the encoder. Applies after the stream restarts.",
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

        // ── Security ────────────────────────────────────────────────────────
        widgets::section_label(ui, "Security");
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let required = self.control.shared.pairing.lock().required;
            let mut require = required;
            widgets::setting_row(
                ui,
                "Require pairing",
                "New iPads on Wi-Fi must enter the code shown on the Stream page. USB needs no code.",
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
            let mut updates = self.settings_check_updates;
            widgets::setting_row(
                ui,
                "Check for updates",
                "Looks for a new release on GitHub once a day.",
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut updates, "Check for updates daily");
                },
            );
            if updates != self.settings_check_updates {
                self.set_check_updates(updates);
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
                        "Version {} · Built by Ali Younes (@whoisaldo)",
                        env!("CARGO_PKG_VERSION")
                    )));
                });
            });
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                widgets::link(ui, "github.com/whoisaldo", "https://github.com/whoisaldo");
                widgets::link(ui, "Source code", "https://github.com/sidebandstudio/EternalMonitor");
                widgets::link(ui, "aldo@sideband.studio", "mailto:aldo@sideband.studio");
            });
            ui.add_space(6.0);
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                ui.add(
                    egui::Label::new(faint(
                        "Set in Geist and Geist Mono by Vercel, used under the SIL Open Font License 1.1.",
                    ))
                    .wrap(),
                );
            });
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
