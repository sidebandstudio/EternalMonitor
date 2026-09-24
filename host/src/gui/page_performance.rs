//! Performance: live rates, encode timing and session totals.

use std::collections::VecDeque;

use eframe::egui::{self, vec2, Align, Layout, RichText, Ui};

use super::icons::Icon;
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{
    format_bytes, format_uptime, friendly_codec, value_or_unknown, AnalyzerApp, Copied,
    StatsSnapshot,
};

impl AnalyzerApp {
    pub(super) fn performance_page(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        let mut copy_logs = false;
        // Ask the bounded in-memory buffer; the session file grows all
        // session and is read only when the user copies it.
        let has_logs = crate::logging::recent_log_text(1).is_some();
        let copied = self.recently_copied(Copied::Logs);
        page_header(
            ui,
            "Performance",
            "Live measurements from capture, encoding and the network on this PC.",
            |ui| {
                let label = if copied { "Copied" } else { "Copy logs" };
                let icon = if copied { Icon::Check } else { Icon::Logs };
                let response =
                    widgets::button_ex(ui, Tone::Secondary, Some(icon), label, has_logs, 0.0);
                copy_logs = response.clicked();
                if has_logs {
                    response.on_hover_text(format!(
                        "Copies the full session log from {}",
                        crate::logging::session_log_path().display()
                    ));
                } else {
                    response.on_hover_text("No log lines have been captured yet.");
                }
            },
        );
        if copy_logs {
            if let Some(text) = crate::logging::session_log_text() {
                self.copy(ui.ctx(), Copied::Logs, text);
            }
        }

        let tiles: [(&str, String, &str, &VecDeque<f32>, f32); 4] = [
            (
                "Capture",
                format!("{:.0}", snap.capture_fps),
                "fps",
                &self.history.capture_fps,
                60.0,
            ),
            (
                "Encode",
                format!("{:.0}", snap.encode_fps),
                "fps",
                &self.history.encode_fps,
                60.0,
            ),
            (
                "Send",
                format!("{:.0}", snap.transport_fps),
                "fps",
                &self.history.send_fps,
                60.0,
            ),
            (
                "Bandwidth",
                format!("{:.1}", snap.bandwidth_mbps),
                "Mbps",
                &self.history.bandwidth_mbps,
                10.0,
            ),
        ];
        let columns = if ui.available_width() >= 700.0 { 4 } else { 2 };
        for chunk in tiles.chunks(columns) {
            ui.columns(columns, |cols| {
                for (col, (label, value, unit, series, max)) in cols.iter_mut().zip(chunk) {
                    widgets::card()
                        .inner_margin(egui::Margin::same(16))
                        .show(col, |ui| {
                            ui.set_width(ui.available_width());
                            widgets::metric(ui, label, value, unit, 24.0);
                            ui.add_space(6.0);
                            let data: Vec<f32> = series.iter().copied().collect();
                            widgets::sparkline(ui, &data, 34.0, ACCENT, *max);
                        });
                }
            });
            ui.add_space(GAP - 10.0);
        }

        widgets::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let data: Vec<f32> = snap.encode_time_history.iter().map(|v| *v as f32).collect();
            let summary = if data.is_empty() {
                String::new()
            } else {
                let avg = data.iter().sum::<f32>() / data.len() as f32;
                let peak = data.iter().copied().fold(0.0_f32, f32::max);
                format!("avg {avg:.1} ms  ·  peak {peak:.1} ms")
            };
            widgets::card_header(ui, "Encode time", |ui| {
                ui.label(RichText::new(summary).family(egui::FontFamily::Monospace).size(12.0).color(TEXT_MUTED));
            });
            ui.label(muted("Milliseconds to encode each frame. Lower is better; under 8 ms keeps up with 120 fps."));
            ui.add_space(8.0);
            widgets::area_chart(ui, &data, 170.0, ACCENT, 4.0, "ms");
        });
        ui.add_space(GAP - 10.0);

        let (w, h) = snap.capture_resolution;
        let encoder_rows = [
            ("GPU", value_or_unknown(&snap.gpu_name).to_string()),
            ("Encoder", friendly_codec(&snap.codec_name)),
            ("Codec name", value_or_unknown(&snap.codec_name).to_string()),
            (
                "Resolution",
                if w > 0 {
                    format!("{w} × {h}")
                } else {
                    "Unknown".into()
                },
            ),
            (
                "Capture display",
                value_or_unknown(&snap.capture_display).to_string(),
            ),
            (
                "Target bitrate",
                format!("{:.1} Mbps", snap.bitrate_bps as f64 / 1_000_000.0),
            ),
            ("Last frame size", format_bytes(snap.nal_bytes_last as u64)),
        ];
        let session_rows = [
            ("Uptime", format_uptime(snap.uptime_secs)),
            ("Frames captured", snap.capture_frame_count.to_string()),
            ("Frames encoded", snap.encode_frame_count.to_string()),
            ("Data sent", format_bytes(snap.transport_bytes_sent)),
            ("Packets sent", snap.transport_packets_sent.to_string()),
            ("Fragments sent", snap.transport_fragments_sent.to_string()),
            ("Retransmitted", snap.transport_retransmits.to_string()),
            ("USB frames dropped", snap.usb_frames_dropped.to_string()),
            ("Target", value_or_unknown(&snap.target_addr).to_string()),
            (
                "Local discovery",
                if snap.mdns_active {
                    "Active".into()
                } else {
                    "Inactive".into()
                },
            ),
        ];
        let show = |ui: &mut Ui, title: &str, rows: &[(&str, String)]| {
            widgets::card_header(ui, title, |_| {});
            ui.add_space(2.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            for (label, value) in rows {
                widgets::stat_line(ui, label, value);
            }
        };
        if ui.available_width() >= 640.0 {
            ui.columns(2, |cols| {
                let (left, right) = cols.split_at_mut(1);
                widgets::card_pair(
                    &mut left[0],
                    &mut right[0],
                    |ui| show(ui, "Encoder", &encoder_rows),
                    |ui| show(ui, "Session", &session_rows),
                );
            });
        } else {
            widgets::card().show(ui, |ui| show(ui, "Encoder", &encoder_rows));
            ui.add_space(GAP - 10.0);
            widgets::card().show(ui, |ui| show(ui, "Session", &session_rows));
        }
    }
}

/// Page title, subtitle and right-aligned actions.
pub(super) fn page_header(ui: &mut Ui, title: &str, subtitle: &str, actions: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(width, 52.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(super::theme::title(title));
                ui.label(muted(subtitle));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), actions);
        },
    );
    ui.add_space(8.0);
}
