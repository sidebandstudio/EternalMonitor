//! Stream: is it working, and how does an iPad connect?

use eframe::egui::{self, vec2, Align, Layout, RichText, Sense, Ui};

use super::icons::Icon;
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{
    friendly_codec, value_or_unknown, AnalyzerApp, ClientSnapshot, Copied, StatsSnapshot,
    StreamState, CAPTURE_AUTO_LABEL, CAPTURE_VIRTUAL_LABEL, CAPTURE_VIRTUAL_SENTINEL,
};
use crate::control::VddStatus;

impl AnalyzerApp {
    pub(super) fn stream_page(
        &mut self,
        ui: &mut Ui,
        snap: &StatsSnapshot,
        client: &ClientSnapshot,
    ) {
        self.notices(ui, snap);
        self.status_header(ui, snap, client);
        ui.add_space(GAP - 10.0);
        let wide = ui.available_width() >= 640.0;
        if wide {
            ui.columns(2, |cols| {
                let (left, right) = cols.split_at_mut(1);
                widgets::card_pair(
                    &mut left[0],
                    &mut right[0],
                    |ui| self.connect_card(ui, snap),
                    |ui| client_card(ui, client, snap.pipeline_running),
                );
            });
        } else {
            widgets::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.connect_card(ui, snap);
            });
            ui.add_space(GAP - 10.0);
            widgets::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                client_card(ui, client, snap.pipeline_running);
            });
        }
        ui.add_space(GAP - 10.0);
        self.status_rows(ui, snap);
    }

    /// Update, restart and fault notices, most urgent first.
    fn notices(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        if snap.using_software_fallback {
            widgets::banner(
                ui,
                WARNING,
                Icon::Warning,
                "Hardware encoder unavailable. Encoding on the CPU (libx264).",
                "Expect higher latency and heavy CPU load. Update your NVIDIA, AMD or Intel graphics driver, then restart the stream.",
                0.0,
                |_| {},
            );
            ui.add_space(GAP - 10.0);
        }
        // The user asked for the extended (virtual) display but it could not be brought up,
        // so capture silently fell back to the primary monitor. Say so.
        let vdd_status = *self.control.shared.vdd_status.lock();
        if matches!(vdd_status, VddStatus::Failed | VddStatus::TaskFailed) {
            let body = if vdd_status == VddStatus::TaskFailed {
                "The display task could not run. Reinstall EternalMonitor-Setup.exe to repair its permissions, then restart the stream. Copy the logs to see the Windows error."
            } else {
                "The display task ran, but no display appeared in time. Check Windows display settings and the driver, then restart the stream."
            };
            let mut copy_logs = false;
            widgets::banner(
                ui,
                WARNING,
                Icon::Warning,
                "Extended display unavailable. Mirroring the main screen instead.",
                body,
                110.0,
                |ui| {
                    copy_logs = widgets::button(ui, Tone::Secondary, Some(Icon::Logs), "Copy logs")
                        .clicked();
                },
            );
            if copy_logs {
                if let Some(text) = crate::logging::session_log_text() {
                    self.copy(ui.ctx(), Copied::Logs, text);
                }
            }
            ui.add_space(GAP - 10.0);
        }
        self.restart_notice(ui);
        if self.settings_check_updates && !self.update_dismissed {
            let update = self.available_update.lock().clone();
            if let Some(version) = update {
                let mut dismiss = false;
                widgets::banner(
                    ui,
                    ACCENT,
                    Icon::Download,
                    &format!("EternalMonitor v{version} is available"),
                    "Update the PC and the iPad app together. Both need the same version.",
                    210.0,
                    |ui| {
                        dismiss = widgets::button(ui, Tone::Ghost, None, "Dismiss").clicked();
                        widgets::link(
                            ui,
                            "View release",
                            &format!(
                                "https://github.com/whoisaldo/EternalMonitor/releases/tag/v{version}"
                            ),
                        );
                    },
                );
                if dismiss {
                    self.update_dismissed = true;
                }
                ui.add_space(GAP - 10.0);
            }
        }
    }

    pub(super) fn restart_notice(&mut self, ui: &mut Ui) {
        if !self.restart_pending {
            return;
        }
        let mut restart = false;
        widgets::banner(
            ui,
            ACCENT,
            Icon::Refresh,
            "Restart the stream to apply your changes",
            "Display and encoder changes take effect when the stream restarts. The iPad reconnects on its own.",
            140.0,
            |ui| {
                restart = widgets::button(ui, Tone::Primary, None, "Restart now").clicked();
            },
        );
        if restart {
            self.restart_stream();
        }
        ui.add_space(GAP - 10.0);
    }

    fn status_header(&mut self, ui: &mut Ui, snap: &StatsSnapshot, client: &ClientSnapshot) {
        let state = StreamState::of(snap, client);
        let device = client
            .info
            .as_ref()
            .map(|i| i.device_name.clone())
            .unwrap_or_default();
        let (title, subtitle) = match state {
            StreamState::Off => (
                "Streaming is off".to_string(),
                "Start streaming so your iPad can connect.".to_string(),
            ),
            StreamState::Ready => (
                "Ready for your iPad".to_string(),
                "Open EternalMonitor on your iPad and choose this PC.".to_string(),
            ),
            StreamState::Streaming => {
                let (w, h) = snap.capture_resolution;
                let mut parts = vec![client.link_name().to_string()];
                if w > 0 && h > 0 {
                    parts.push(format!("{w} × {h}"));
                }
                parts.push(format!("{} fps", self.control.shared.effective_fps()));
                parts.push(friendly_codec(&snap.codec_name));
                (format!("Streaming to {device}"), parts.join("  ·  "))
            }
        };

        widgets::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let width = ui.available_width();
            let mut action = None;
            ui.allocate_ui_with_layout(
                vec2(width, 48.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let (glyph, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    let color = state.color();
                    ui.painter().circle(
                        glyph.center(),
                        22.0,
                        mix(
                            SURFACE,
                            color,
                            if state == StreamState::Off { 6 } else { 10 },
                        ),
                        egui::Stroke::new(1.0, mix(SURFACE, color, 24)),
                    );
                    widgets::status_dot(ui, glyph.center(), color, 6.0, state.dot());

                    let buttons_width = if state == StreamState::Off {
                        170.0
                    } else {
                        210.0
                    };
                    let text_width = (ui.available_width() - buttons_width).max(160.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_width, 48.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.add_space(3.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(title)
                                        .family(semibold())
                                        .size(18.0)
                                        .color(TEXT),
                                )
                                .truncate(),
                            );
                            ui.add(egui::Label::new(muted(subtitle)).truncate());
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if snap.pipeline_running {
                            if widgets::button(ui, Tone::Danger, Some(Icon::Stop), "Stop").clicked()
                            {
                                action = Some(HeaderAction::Stop);
                            }
                            if widgets::button(ui, Tone::Secondary, Some(Icon::Refresh), "Restart")
                                .on_hover_text(
                                    "Restart capture and encoding. The iPad reconnects on its own.",
                                )
                                .clicked()
                            {
                                action = Some(HeaderAction::Restart);
                            }
                        } else if widgets::button(
                            ui,
                            Tone::Primary,
                            Some(Icon::Play),
                            "Start streaming",
                        )
                        .clicked()
                        {
                            action = Some(HeaderAction::Start);
                        }
                    });
                },
            );

            if state == StreamState::Streaming {
                ui.add_space(14.0);
                widgets::divider(ui);
                ui.add_space(14.0);
                let latency = client
                    .report
                    .as_ref()
                    .filter(|r| r.e2e_latency_ms_x10 > 0)
                    .map(|r| format!("{:.0}", f64::from(r.e2e_latency_ms_x10) / 10.0))
                    .unwrap_or_else(|| format!("{:.0}", snap.latency_ms));
                ui.columns(4, |cols| {
                    widgets::metric(
                        &mut cols[0],
                        "Frame rate",
                        &format!("{:.0}", snap.capture_fps),
                        "fps",
                        22.0,
                    )
                    .on_hover_text("Frames captured per second on this PC.");
                    widgets::metric(&mut cols[1], "Latency", &latency, "ms", 22.0)
                        .on_hover_text("Time from capture on this PC to the picture on the iPad.");
                    widgets::metric(
                        &mut cols[2],
                        "Bitrate",
                        &format!("{:.1}", snap.bandwidth_mbps),
                        "Mbps",
                        22.0,
                    )
                    .on_hover_text("Video sent to the iPad each second. It adapts to the network.");
                    widgets::metric(
                        &mut cols[3],
                        "Encode time",
                        &format!("{:.1}", snap.encode_time_us as f64 / 1000.0),
                        "ms",
                        22.0,
                    )
                    .on_hover_text("Time the encoder takes to compress each frame.");
                });
            }

            match action {
                Some(HeaderAction::Stop) => self.control.request_stop(),
                Some(HeaderAction::Start) => self.control.request_start(),
                Some(HeaderAction::Restart) => self.restart_stream(),
                None => {}
            }
        });
    }

    fn connect_card(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        // detect_local_ip() reports "unknown:<port>" when no LAN adapter is up.
        let online = !snap.listen_addr.is_empty() && !snap.listen_addr.starts_with("unknown");
        let mut open_qr = false;
        widgets::card_header(ui, "Connect an iPad", |ui| {
            open_qr =
                widgets::button_ex(ui, Tone::Secondary, Some(Icon::Qr), "QR code", online, 0.0)
                    .clicked();
        });
        widgets::paragraph(
            ui,
            muted("On your iPad, open EternalMonitor and choose this PC, or tap Scan QR. USB connections pair automatically."),
        );
        ui.add_space(8.0);

        let (code, pairing_required) = {
            let pairing = self.control.shared.pairing.lock();
            (format!("{:06}", pairing.code()), pairing.required)
        };
        let code = format!("{} {}", &code[..3], &code[3..]);
        let mut copy = false;
        let mut rotate = false;
        let copied = self.recently_copied(Copied::Address);
        if online {
            value_block(ui, "Address", &snap.listen_addr, |ui| {
                let (icon, tip) = if copied {
                    (Icon::Check, "Copied")
                } else {
                    (Icon::Copy, "Copy address")
                };
                copy = widgets::icon_button(ui, icon, tip).clicked();
            });
        } else {
            ui.label(overline("Address"));
            widgets::paragraph(
                ui,
                RichText::new(
                    "No network found. Connect this PC to Wi-Fi or Ethernet, or use a USB cable.",
                )
                .size(SMALL)
                .color(WARNING),
            );
        }
        ui.add_space(4.0);
        if pairing_required {
            value_block(ui, "Pairing code", &code, |ui| {
                rotate = widgets::icon_button(ui, Icon::Refresh, "New code").clicked();
            });
        } else {
            ui.label(overline("Pairing code"));
            ui.label(muted("Not needed while pairing is off."));
        }
        if copy && online {
            self.copy(ui.ctx(), Copied::Address, snap.listen_addr.clone());
        }
        if rotate {
            self.new_pairing_code();
        }
        if open_qr {
            self.open_qr(snap);
        }
        if let Some(error) = &self.settings_error {
            ui.add(egui::Label::new(RichText::new(error).size(SMALL).color(DANGER)).wrap());
        }
    }

    fn status_rows(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());

            // Pairing
            let required = self.control.shared.pairing.lock().required;
            let mut require = required;
            widgets::icon_row(
                ui,
                Icon::Lock,
                if required { ACCENT } else { TEXT_MUTED },
                "Pairing",
                if required {
                    "New iPads on Wi-Fi enter the pairing code once. USB needs no code."
                } else {
                    "Off. Any iPad on your network can connect without a code."
                },
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut require, "Require pairing");
                },
            );
            if require != required {
                self.set_require_pairing(require);
            }
            widgets::divider(ui);

            // USB
            let usb = usb_summary(
                snap.usb_service_reachable,
                snap.usb_devices,
                &snap.usb_link_state,
            );
            let usb_color = if usb.available { ACCENT } else { TEXT_MUTED };
            let (usb_text, usb_badge) = (usb.text, usb.badge);
            widgets::icon_row(
                ui,
                Icon::Plug,
                usb_color,
                "USB connection",
                &usb_text,
                if snap.usb_service_reachable {
                    110.0
                } else {
                    150.0
                },
                |ui| {
                    if snap.usb_service_reachable {
                        widgets::badge(ui, usb_badge, usb_color);
                    } else {
                        widgets::link(
                            ui,
                            "Get Apple Devices",
                            "https://apps.microsoft.com/detail/9np83lwlpz9k",
                        );
                    }
                },
            );
            widgets::divider(ui);

            // Audio
            let enabled = self
                .control
                .shared
                .audio_enabled
                .load(std::sync::atomic::Ordering::SeqCst);
            let mut audio_on = enabled;
            let (audio_text, audio_color) = audio_summary(&snap.audio, enabled);
            widgets::icon_row(
                ui,
                Icon::Speaker,
                audio_color,
                "PC audio",
                &audio_text,
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut audio_on, "Stream PC audio");
                },
            );
            if audio_on != enabled {
                self.set_stream_audio(audio_on);
            }
            widgets::divider(ui);

            // Display
            let description = match self.settings_capture_display.as_str() {
                "" => "Mirrors your main screen on the iPad.".to_string(),
                s if s == CAPTURE_VIRTUAL_SENTINEL => match *self.control.shared.vdd_status.lock() {
                    VddStatus::Active => {
                        "Extended display is on. Drag windows past the edge of your main screen."
                            .to_string()
                    }
                    VddStatus::Failed | VddStatus::TaskFailed => {
                        "The extended display didn't start, so the main screen is mirrored."
                            .to_string()
                    }
                    _ => "Adds a second screen for the iPad. It appears when an iPad connects."
                        .to_string(),
                },
                name => format!("Mirrors {}.", name.rsplit('\\').next().unwrap_or(name)),
            };
            widgets::icon_row(
                ui,
                Icon::Display,
                TEXT_MUTED,
                "Display",
                &description,
                250.0,
                |ui| {
                    self.display_picker(ui, 250.0);
                },
            );
        });
    }

    /// Mirror / extend / specific monitor. Shared with Settings.
    pub(super) fn display_picker(&mut self, ui: &mut Ui, width: f32) {
        let outputs = self.available_outputs.clone();
        let current = self.settings_capture_display.clone();
        let selected_text = if current.is_empty() {
            CAPTURE_AUTO_LABEL.to_string()
        } else if current == CAPTURE_VIRTUAL_SENTINEL {
            CAPTURE_VIRTUAL_LABEL.to_string()
        } else if let Some(o) = outputs.iter().find(|o| o.device_name == current) {
            super::format_output_label(o)
        } else {
            format!(
                "{} (not connected)",
                current.rsplit('\\').next().unwrap_or(&current)
            )
        };
        let mut choice = current.clone();
        widgets::combo(ui.id().with("capture-display"), &selected_text, width).show_ui(ui, |ui| {
            ui.set_min_width(width);
            if ui
                .selectable_label(choice.is_empty(), CAPTURE_AUTO_LABEL)
                .clicked()
            {
                choice = String::new();
            }
            // Managed virtual display — created on demand, removed when not in use.
            if ui
                .selectable_label(choice == CAPTURE_VIRTUAL_SENTINEL, CAPTURE_VIRTUAL_LABEL)
                .clicked()
            {
                choice = CAPTURE_VIRTUAL_SENTINEL.to_string();
            }
            if !outputs.is_empty() {
                ui.separator();
            }
            for o in &outputs {
                if ui
                    .selectable_label(choice == o.device_name, super::format_output_label(o))
                    .clicked()
                {
                    choice = o.device_name.clone();
                }
            }
        });
        if choice != current {
            self.set_capture_display(choice);
        }
    }
}

enum HeaderAction {
    Start,
    Stop,
    Restart,
}

/// Overline label, a large monospaced value and trailing controls.
fn value_block(ui: &mut Ui, label: &str, value: &str, controls: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(overline(label));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let max_text = (ui.available_width() - 40.0).max(60.0);
            ui.scope(|ui| {
                ui.set_max_width(max_text);
                ui.add(egui::Label::new(mono(value, 19.0)).truncate());
            });
            controls(ui);
        });
    });
}

fn client_card(ui: &mut Ui, client: &ClientSnapshot, streaming: bool) {
    let link = client.link_name();
    widgets::card_header(ui, "Connected iPad", |ui| {
        if client.info.is_some() {
            widgets::badge(ui, link, ACCENT);
        }
    });
    let Some(info) = &client.info else {
        if streaming {
            widgets::empty_state(
                ui,
                Icon::Tablet,
                "Waiting for an iPad",
                "It appears here as soon as it connects.",
            );
        } else {
            widgets::empty_state(
                ui,
                Icon::Tablet,
                "No iPad connected",
                "Start streaming, then connect from your iPad.",
            );
        }
        return;
    };
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let (rect, _) = ui.allocate_exact_size(vec2(18.0, 36.0), Sense::hover());
        super::icons::paint(
            ui.painter(),
            egui::Rect::from_center_size(rect.center(), vec2(18.0, 18.0)),
            Icon::Tablet,
            TEXT_MUTED,
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add(egui::Label::new(strong(info.device_name.clone())).truncate());
            let (w, h) = info.screen_px;
            ui.label(faint(format!("{w} × {h} · {} Hz", info.refresh_hz)));
        });
    });
    ui.add_space(6.0);
    let Some(r) = client.report.as_ref() else {
        ui.label(muted("Waiting for the first measurements from the iPad."));
        return;
    };
    let total = f64::from(r.frags_received) + f64::from(r.frags_repaired) + f64::from(r.frags_lost);
    let percent = |part: u32| {
        if total > 0.0 {
            100.0 * f64::from(part) / total
        } else {
            0.0
        }
    };
    let latency = if r.e2e_latency_ms_x10 == 0 {
        "Measuring".to_string()
    } else {
        format!("{:.1} ms", f64::from(r.e2e_latency_ms_x10) / 10.0)
    };
    let rows = [
        (
            "Round-trip time",
            format!("{:.1} ms", f64::from(r.rtt_ms_x10) / 10.0),
        ),
        ("End-to-end latency", latency),
        (
            "Decode rate",
            format!("{:.1} fps", f64::from(r.decode_fps_x10) / 10.0),
        ),
        ("Unrecovered loss", format!("{:.2}%", percent(r.frags_lost))),
        (
            "Repaired fragments",
            format!("{:.2}%", percent(r.frags_repaired)),
        ),
        ("Audio buffer", format!("{} ms", r.audio_buffer_ms)),
    ];
    ui.columns(2, |cols| {
        for (i, (label, value)) in rows.iter().enumerate() {
            let ui = &mut cols[i % 2];
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(*label).size(CAPTION).color(TEXT_FAINT));
            ui.label(mono(value.clone(), 15.0));
            ui.add_space(8.0);
        }
    });
}

pub(super) struct UsbSummary {
    pub text: String,
    pub badge: &'static str,
    pub available: bool,
}

/// What the USB row says. The install hint appears only when Apple's device
/// service cannot be reached.
pub(super) fn usb_summary(reachable: bool, devices: usize, link_state: &str) -> UsbSummary {
    if !reachable {
        return UsbSummary {
            text: "Install the Apple Devices app from the Microsoft Store (or iTunes) to use USB."
                .into(),
            badge: "Unavailable",
            available: false,
        };
    }
    let (text, badge) = match link_state {
        "Connected" => (
            "An iPad is streaming over the cable.".to_string(),
            "Connected",
        ),
        "Waiting for the iPad app" => (
            "iPad plugged in. Open EternalMonitor on it to connect.".to_string(),
            "Ready",
        ),
        "Waiting for an iPad" => (
            "Plug in your iPad to connect over USB.".to_string(),
            "Ready",
        ),
        "Checking Apple device service" => (
            "Checking for Apple's device service…".to_string(),
            "Checking",
        ),
        other => {
            let plugged = match devices {
                0 => "no iPad plugged in".to_string(),
                1 => "1 device plugged in".to_string(),
                n => format!("{n} devices plugged in"),
            };
            (format!("{other} · {plugged}"), "Ready")
        }
    };
    UsbSummary {
        text,
        badge,
        available: true,
    }
}

/// What the audio row says, and its tint. An audio failure never claims the
/// video stopped.
pub(super) fn audio_summary(
    audio: &crate::audio::AudioStats,
    enabled: bool,
) -> (String, egui::Color32) {
    if let Some(error) = &audio.error {
        return (
            format!("Audio unavailable: {error}. Video continues. Turn Stream PC audio off and on to retry."),
            WARNING,
        );
    }
    if !enabled {
        return (
            "Off. The iPad plays no sound from this PC.".into(),
            TEXT_MUTED,
        );
    }
    if audio.status == "Streaming" {
        let device = if audio.device.is_empty() {
            "PC audio"
        } else {
            audio.device.as_str()
        };
        return (
            format!(
                "Streaming {device} · Opus {:.0} kbps · {:.0} packets/s",
                audio.kbps, audio.packets_per_sec
            ),
            ACCENT,
        );
    }
    if audio.device.is_empty() {
        (value_or_unknown(&audio.status).to_string(), TEXT_MUTED)
    } else {
        (format!("{} · {}", audio.status, audio.device), TEXT_MUTED)
    }
}
