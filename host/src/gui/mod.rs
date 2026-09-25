//! The host window: connection status, pairing, diagnostics and settings.
//!
//! Automation contract, shared with `scripts/win/Inspect-Host.ps1` and
//! `tests/gui_snapshots.rs`. Keep these names when changing copy:
//! - navigation buttons "Stream", "Performance" and "Settings";
//! - the "QR code" button, which opens a dialog labelled "QR code";
//! - on Stream, the "Pairing", "USB connection" and "PC audio" rows, and a
//!   "Connected iPad" card that lists "Decode rate" and "Repaired fragments"
//!   (or "Waiting for an iPad" when nobody is connected);
//! - on Settings, the "Encoder input" row.

mod icons;
mod page_performance;
mod page_settings;
mod page_stream;
mod theme;
mod widgets;

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, Frame, Margin, Rect, Sense, Stroke, StrokeKind};
use qrcode::{Color as QrModuleColor, QrCode};
use tracing::{info, warn};

use crate::autostart::{first_lan_ipv4, read_startup_registry, set_startup_registry};
use crate::capture::{enumerate_outputs, OutputInfo};
use crate::control::{CaptureTarget, GuiControl};
use crate::settings::SettingsFile;
use crate::stats::PIPELINE_STATS;
use crate::transport::link::{LinkId, PeerId};
use crate::transport::session::ClientInfo;
use eternal_wire::v2::control::ReceiverReport;

use icons::Icon;
use theme::*;
use widgets::Tone;

#[derive(PartialEq, Eq, Clone, Copy, Hash, Debug)]
enum Page {
    Stream,
    Performance,
    Settings,
}

struct StatsSnapshot {
    listen_addr: String,
    audio: crate::audio::AudioStats,
    capture_fps: f64,
    capture_frame_count: u64,
    capture_resolution: (u32, u32),
    encode_fps: f64,
    encode_time_us: u128,
    encode_frame_count: u64,
    nal_bytes_last: usize,
    bitrate_bps: u32,
    codec_name: String,
    using_software_fallback: bool,
    gpu_name: String,
    capture_display: String,
    transport_fps: f64,
    transport_bytes_sent: u64,
    transport_packets_sent: u64,
    transport_fragments_sent: u64,
    transport_retransmits: u64,
    usb_service_reachable: bool,
    usb_devices: usize,
    usb_link_state: String,
    usb_cabled_devices: usize,
    apple_devices_installed: bool,
    usb_frames_dropped: u64,
    target_addr: String,
    latency_ms: f64,
    bandwidth_mbps: f64,
    encode_time_history: Vec<f64>,
    pipeline_running: bool,
    uptime_secs: f64,
    mdns_active: bool,
}

impl StatsSnapshot {
    fn take() -> Self {
        let s = PIPELINE_STATS.lock();
        Self {
            listen_addr: s.listen_addr.clone(),
            audio: s.audio.clone(),
            capture_fps: s.capture_fps,
            capture_frame_count: s.capture_frame_count,
            capture_resolution: s.capture_resolution,
            encode_fps: s.encode_fps,
            encode_time_us: s.encode_time_us,
            encode_frame_count: s.encode_frame_count,
            nal_bytes_last: s.nal_bytes_last,
            bitrate_bps: s.bitrate_bps,
            codec_name: s.codec_name.clone(),
            using_software_fallback: s.using_software_fallback,
            gpu_name: s.gpu_name.clone(),
            capture_display: s.capture_display.clone(),
            transport_fps: s.transport_fps,
            transport_bytes_sent: s.transport_bytes_sent,
            transport_packets_sent: s.transport_packets_sent,
            transport_fragments_sent: s.transport_fragments_sent,
            transport_retransmits: s.transport_retransmits,
            usb_service_reachable: s.usb_service_reachable,
            usb_devices: s.usb_devices,
            usb_link_state: s.usb_link_state.clone(),
            usb_cabled_devices: s.usb_cabled_devices,
            apple_devices_installed: s.apple_devices_installed,
            usb_frames_dropped: s.usb_frames_dropped,
            target_addr: s.target_addr.clone(),
            latency_ms: s.latency_ms,
            bandwidth_mbps: s.bandwidth_mbps,
            encode_time_history: s.encode_time_history.iter().copied().collect(),
            pipeline_running: s.pipeline_running,
            uptime_secs: s.uptime_secs(),
            mdns_active: s.mdns_active,
        }
    }
}

/// The connected iPad, if any, read once per frame.
struct ClientSnapshot {
    info: Option<ClientInfo>,
    peer: Option<PeerId>,
    report: Option<ReceiverReport>,
}

impl ClientSnapshot {
    fn take(control: &GuiControl) -> Self {
        let session = control.shared.session.lock();
        Self {
            info: session.client_info(),
            peer: session.peer(),
            report: session.last_report(),
        }
    }

    fn link_name(&self) -> &'static str {
        if self
            .peer
            .is_some_and(|p| matches!(p.link, LinkId::Usb { .. }))
        {
            "USB"
        } else {
            "Wi-Fi"
        }
    }
}

/// What the header says about the stream as a whole.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Off,
    Ready,
    Streaming,
}

impl StreamState {
    fn of(snap: &StatsSnapshot, client: &ClientSnapshot) -> Self {
        if !snap.pipeline_running {
            StreamState::Off
        } else if client.info.is_some() {
            StreamState::Streaming
        } else {
            StreamState::Ready
        }
    }

    fn color(self) -> Color32 {
        match self {
            StreamState::Off => TEXT_FAINT,
            StreamState::Ready | StreamState::Streaming => ACCENT,
        }
    }

    fn dot(self) -> widgets::Dot {
        match self {
            StreamState::Off => widgets::Dot::Idle,
            StreamState::Ready => widgets::Dot::Ready,
            StreamState::Streaming => widgets::Dot::Live,
        }
    }
}

const CAPTURE_AUTO_LABEL: &str = "Main display (mirror)";
const CAPTURE_VIRTUAL_LABEL: &str = "Extended display (iPad)";
// The settings sentinel for the managed virtual display lives in control.rs
// (shared with the startup preload).
use crate::control::CAPTURE_VIRTUAL_SENTINEL;

/// Menu label for an enumerated output, e.g. `DISPLAY2 · 1920 × 1080 · primary`.
fn format_output_label(o: &OutputInfo) -> String {
    let short = o.device_name.rsplit('\\').next().unwrap_or(&o.device_name);
    let primary = if o.is_primary { " · primary" } else { "" };
    format!("{} · {} × {}{}", short, o.width, o.height, primary)
}

const ENCODER_AUTO_LABEL: &str = "Auto";
const ENCODER_CHOICES: &[(&str, &str)] = &[
    (ENCODER_AUTO_LABEL, ""),
    ("NVENC", "h264_nvenc"),
    ("AMF", "h264_amf"),
    ("QSV", "h264_qsv"),
    ("x264", "libx264"),
];

/// Samples of the live rates, kept by the window for its trend lines.
struct History {
    capture_fps: VecDeque<f32>,
    encode_fps: VecDeque<f32>,
    send_fps: VecDeque<f32>,
    bandwidth_mbps: VecDeque<f32>,
    last_sample: Option<Instant>,
}

const HISTORY_SAMPLES: usize = 240; // one minute at 4 Hz

impl History {
    fn new() -> Self {
        Self {
            capture_fps: VecDeque::with_capacity(HISTORY_SAMPLES),
            encode_fps: VecDeque::with_capacity(HISTORY_SAMPLES),
            send_fps: VecDeque::with_capacity(HISTORY_SAMPLES),
            bandwidth_mbps: VecDeque::with_capacity(HISTORY_SAMPLES),
            last_sample: None,
        }
    }

    fn sample(&mut self, snap: &StatsSnapshot, now: Instant) {
        if self
            .last_sample
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(250))
        {
            return;
        }
        self.last_sample = Some(now);
        for (series, value) in [
            (&mut self.capture_fps, snap.capture_fps),
            (&mut self.encode_fps, snap.encode_fps),
            (&mut self.send_fps, snap.transport_fps),
            (&mut self.bandwidth_mbps, snap.bandwidth_mbps),
        ] {
            if series.len() == HISTORY_SAMPLES {
                series.pop_front();
            }
            series.push_back(value as f32);
        }
    }
}

/// Which "copy" button last succeeded, to show a check mark briefly.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Copied {
    Address,
    Logs,
}

pub struct AnalyzerApp {
    control: GuiControl,
    page: Page,
    settings_bitrate_mbps: f32,
    settings_fps_target: u32,
    settings_encoder_input: crate::encoder::input::EncoderInput,
    settings_check_updates: bool,
    available_update: crate::updates::AvailableUpdate,
    update_dismissed: bool,
    settings_packet_size: u32,
    /// Last settings error worth showing (the autostart registry write or a
    /// pairing reset).
    settings_error: Option<String>,
    settings_start_on_boot: bool,
    settings_hevc_enabled: bool,
    settings_vdd_match: bool,
    /// Set when a high-frequency control (the bitrate slider) changed; the
    /// settings file is written once the value has been stable for 800 ms.
    settings_dirty_at: Option<Instant>,
    settings_encoder_choice: String, // display label, e.g. "Auto" or "NVENC"
    /// DXGI `DeviceName` of the chosen capture display; empty string means auto (primary).
    settings_capture_display: String,
    /// Cached enumerated outputs for the capture-display picker; refreshed on demand.
    available_outputs: Vec<OutputInfo>,
    /// A change that only takes effect when the stream restarts.
    restart_pending: bool,
    show_qr_modal: bool,
    qr_cache: Option<(String, QrCode)>,
    copied: Option<(Copied, Instant)>,
    history: History,
    logo: Option<(f32, egui::TextureHandle)>,
}

impl AnalyzerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, control: GuiControl) -> Self {
        theme::install(&cc.egui_ctx);

        // Load persisted settings first; values fall back to the live runtime state when the
        // file is missing or unreadable.
        let persisted = SettingsFile::load();

        let runtime_bitrate_mbps =
            control.shared.bitrate_bps.load(Ordering::SeqCst) as f32 / 1_000_000.0;
        let bitrate_mbps = if persisted.bitrate_mbps > 0.0 {
            control.shared.bitrate_bps.store(
                (persisted.bitrate_mbps * 1_000_000.0).round() as u32,
                Ordering::SeqCst,
            );
            PIPELINE_STATS
                .lock()
                .set_bitrate((persisted.bitrate_mbps * 1_000_000.0).round() as u32);
            persisted.bitrate_mbps
        } else {
            runtime_bitrate_mbps
        };

        let fps_target = if [30, 60, 90, 120].contains(&persisted.target_fps) {
            persisted.target_fps
        } else {
            control.shared.target_fps.load(Ordering::SeqCst)
        };
        control
            .shared
            .target_fps
            .store(fps_target, Ordering::SeqCst);

        let encoder_choice = if let Some(name) = persisted.encoder_override.as_deref() {
            *control.shared.encoder_override.lock() = Some(name.to_string());
            ENCODER_CHOICES
                .iter()
                .find(|(_, ffmpeg)| *ffmpeg == name)
                .map(|(label, _)| label.to_string())
                .unwrap_or_else(|| ENCODER_AUTO_LABEL.to_string())
        } else {
            ENCODER_AUTO_LABEL.to_string()
        };

        // Apply the persisted capture display into SharedControl so the next stream restart
        // honors it (same model as encoder_override — the first pipeline run started before
        // the GUI loaded settings).
        let settings_capture_display = match persisted.capture_display.as_deref() {
            Some(s) if s == CAPTURE_VIRTUAL_SENTINEL => {
                *control.shared.capture_target.lock() = CaptureTarget::VirtualExtended;
                CAPTURE_VIRTUAL_SENTINEL.to_string()
            }
            Some(name) if !name.is_empty() => {
                *control.shared.capture_target.lock() = CaptureTarget::Output(name.to_string());
                name.to_string()
            }
            _ => {
                *control.shared.capture_target.lock() = CaptureTarget::PrimaryAuto;
                String::new()
            }
        };
        let available_outputs = enumerate_outputs();

        // Same model as encoder_override: push the persisted preference into
        // SharedControl so the encoder honours it without a restart.
        control
            .shared
            .hevc_enabled
            .store(persisted.hevc_enabled, Ordering::SeqCst);
        control
            .shared
            .audio_enabled
            .store(persisted.stream_audio, Ordering::SeqCst);

        let start_on_boot = if persisted.start_on_boot != read_startup_registry() {
            // Persisted state disagrees with the registry — trust the registry as ground truth.
            read_startup_registry()
        } else {
            persisted.start_on_boot
        };

        // If autostart is enabled, refresh the stored HKCU\Run path to the *current* exe location.
        // After an installer upgrade or a move, the old path would silently fail to launch on boot.
        if start_on_boot {
            if let Err(error) = set_startup_registry(true) {
                warn!(error = %error, "Failed to refresh startup registry path on launch");
            }
        }

        let available_update = std::sync::Arc::new(parking_lot::Mutex::new(None));
        if persisted.check_for_updates {
            crate::updates::start(available_update.clone());
        }
        let encoder_input = *control.shared.encoder_input.lock();
        Self {
            settings_encoder_input: encoder_input,
            settings_check_updates: persisted.check_for_updates,
            available_update,
            update_dismissed: false,
            settings_packet_size: control.shared.max_dgram.load(Ordering::SeqCst),
            control,
            page: Page::Stream,
            settings_bitrate_mbps: bitrate_mbps,
            settings_fps_target: fps_target,
            settings_error: None,
            settings_start_on_boot: start_on_boot,
            settings_hevc_enabled: persisted.hevc_enabled,
            settings_vdd_match: persisted.vdd_match_resolution,
            settings_dirty_at: None,
            settings_encoder_choice: encoder_choice,
            settings_capture_display,
            available_outputs,
            restart_pending: false,
            show_qr_modal: false,
            qr_cache: None,
            copied: None,
            history: History::new(),
            logo: None,
        }
    }

    fn persist_settings(&self) {
        let encoder_override = ENCODER_CHOICES
            .iter()
            .find(|(label, _)| *label == self.settings_encoder_choice)
            .and_then(|(_, ffmpeg)| {
                if ffmpeg.is_empty() {
                    None
                } else {
                    Some((*ffmpeg).to_string())
                }
            });
        let pairing = self.control.shared.pairing.lock();
        let file = SettingsFile {
            bitrate_mbps: self.settings_bitrate_mbps,
            target_fps: self.settings_fps_target,
            max_dgram: self.settings_packet_size as u16,
            // v2 has no manual target; the field stays in the file only so
            // older settings.json still parse.
            target_ip: None,
            encoder_override,
            encoder_input: self.settings_encoder_input,
            check_for_updates: self.settings_check_updates,
            capture_display: if self.settings_capture_display.is_empty() {
                None
            } else {
                Some(self.settings_capture_display.clone())
            },
            hevc_enabled: self.settings_hevc_enabled,
            stream_audio: self.control.shared.audio_enabled.load(Ordering::SeqCst),
            vdd_match_resolution: self.settings_vdd_match,
            start_on_boot: self.settings_start_on_boot,
            require_pairing: pairing.required,
            auth_token_hex: crate::pairing::token_hex(&pairing.token()),
        };
        file.save();
    }
}

// ── Settings actions ────────────────────────────────────────────────────────
// Each applies the value live where the pipeline allows it, then persists.

impl AnalyzerApp {
    fn set_bitrate_mbps(&mut self, mbps: f32) {
        self.settings_bitrate_mbps = mbps;
        let bitrate_bps = (mbps * 1_000_000.0).round() as u32;
        self.control
            .shared
            .bitrate_bps
            .store(bitrate_bps, Ordering::SeqCst);
        PIPELINE_STATS.lock().set_bitrate(bitrate_bps);
        // Live value applies immediately; the file write is debounced
        // (a slider drag emits dozens of change events per second).
        self.settings_dirty_at = Some(Instant::now());
    }

    fn set_fps(&mut self, fps: u32) {
        if self.settings_fps_target == fps {
            return;
        }
        self.settings_fps_target = fps;
        self.control.shared.target_fps.store(fps, Ordering::SeqCst);
        info!(target_fps = fps, "Capture target FPS updated from GUI");
        self.persist_settings();
    }

    fn set_packet_size(&mut self, bytes: u32) {
        self.settings_packet_size = bytes;
        self.control.shared.max_dgram.store(bytes, Ordering::SeqCst);
        self.settings_dirty_at = Some(Instant::now());
    }

    fn set_encoder_input(&mut self, input: crate::encoder::input::EncoderInput) {
        if self.settings_encoder_input == input {
            return;
        }
        self.settings_encoder_input = input;
        *self.control.shared.encoder_input.lock() = input;
        self.persist_settings();
        self.restart_pending = true;
    }

    fn set_encoder_choice(&mut self, label: &str) {
        if self.settings_encoder_choice == label {
            return;
        }
        self.settings_encoder_choice = label.to_string();
        let ffmpeg_name = ENCODER_CHOICES
            .iter()
            .find(|(choice, _)| *choice == label)
            .map(|(_, ffmpeg)| (*ffmpeg).to_string());
        *self.control.shared.encoder_override.lock() = ffmpeg_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        info!(
            encoder = self.settings_encoder_choice,
            "Encoder override set — takes effect on next stream restart"
        );
        self.persist_settings();
        self.restart_pending = true;
    }

    fn set_capture_display(&mut self, display: String) {
        if self.settings_capture_display == display {
            return;
        }
        self.settings_capture_display = display;
        *self.control.shared.capture_target.lock() = if self.settings_capture_display.is_empty() {
            CaptureTarget::PrimaryAuto
        } else if self.settings_capture_display == CAPTURE_VIRTUAL_SENTINEL {
            CaptureTarget::VirtualExtended
        } else {
            CaptureTarget::Output(self.settings_capture_display.clone())
        };
        info!(
            display = %self.settings_capture_display,
            "Capture display set — takes effect on next stream restart"
        );
        self.persist_settings();
        self.restart_pending = true;
    }

    fn refresh_outputs(&mut self) {
        self.available_outputs = enumerate_outputs();
        info!(
            count = self.available_outputs.len(),
            "Re-enumerated display outputs"
        );
    }

    fn set_vdd_match(&mut self, enabled: bool) {
        self.settings_vdd_match = enabled;
        info!(
            enabled,
            "VDD resolution match changed — applies next time the extended display starts"
        );
        self.persist_settings();
    }

    fn set_hevc(&mut self, enabled: bool) {
        self.settings_hevc_enabled = enabled;
        self.control
            .shared
            .hevc_enabled
            .store(enabled, Ordering::SeqCst);
        info!(
            enabled,
            "HEVC preference changed — applies at the next frame"
        );
        self.persist_settings();
    }

    fn set_stream_audio(&mut self, enabled: bool) {
        self.control
            .shared
            .audio_enabled
            .store(enabled, Ordering::SeqCst);
        self.persist_settings();
    }

    fn set_start_on_boot(&mut self, enabled: bool) {
        match set_startup_registry(enabled) {
            Ok(()) => {
                self.settings_start_on_boot = enabled;
                self.settings_error = None;
                self.persist_settings();
            }
            Err(error) => self.settings_error = Some(error),
        }
    }

    fn set_check_updates(&mut self, enabled: bool) {
        self.settings_check_updates = enabled;
        if enabled {
            crate::updates::start(self.available_update.clone());
        }
        self.persist_settings();
    }

    fn set_require_pairing(&mut self, required: bool) {
        self.control.shared.pairing.lock().required = required;
        self.persist_settings();
    }

    fn new_pairing_code(&mut self) {
        let result = self.control.shared.pairing.lock().rotate_code();
        if let Err(error) = result {
            self.settings_error = Some(format!("Could not create a pairing code: {error}"));
        }
    }

    fn forget_all_ipads(&mut self) {
        let result = self.control.shared.pairing.lock().regenerate_token();
        match result {
            Ok(()) => self.persist_settings(),
            Err(error) => self.settings_error = Some(format!("Could not reset pairing: {error}")),
        }
    }

    fn restart_stream(&mut self) {
        self.restart_pending = false;
        self.control.request_restart();
    }

    fn copy(&mut self, ctx: &egui::Context, what: Copied, text: String) {
        ctx.copy_text(text);
        self.copied = Some((what, Instant::now()));
    }

    fn recently_copied(&self, what: Copied) -> bool {
        self.copied
            .is_some_and(|(w, at)| w == what && at.elapsed() < Duration::from_millis(1600))
    }
}

impl eframe::App for AnalyzerApp {
    fn on_exit(&mut self) {
        if self.settings_dirty_at.take().is_some() {
            self.persist_settings();
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        CANVAS.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(dirty_at) = self.settings_dirty_at {
            if dirty_at.elapsed() >= Duration::from_millis(800) {
                self.settings_dirty_at = None;
                self.persist_settings();
            }
        }
        // (A debounced write still pending at window close is flushed by on_exit.)
        // Live numbers refresh five times a second. Input and animations
        // repaint on their own, and a quieter window leaves the GPU to the
        // encoder.
        ctx.request_repaint_after(Duration::from_millis(200));

        let snap = StatsSnapshot::take();
        let client = ClientSnapshot::take(&self.control);
        self.history.sample(&snap, Instant::now());
        if !snap.pipeline_running {
            // A stopped pipeline starts with the new settings anyway.
            self.restart_pending = false;
        }

        self.draw_sidebar(ui, &snap, &client);

        egui::CentralPanel::default()
            .frame(Frame::new().fill(CANVAS))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(self.page)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::new()
                            .inner_margin(Margin {
                                left: 32,
                                right: 32,
                                top: 28,
                                bottom: 36,
                            })
                            .show(ui, |ui| {
                                // Each page fades and settles in when opened.
                                let shown =
                                    widgets::appear(ui, egui::Id::new(("page", self.page)), 0.22);
                                ui.add_space((1.0 - shown) * 10.0);
                                ui.set_opacity(shown);
                                ui.set_max_width(ui.available_width().min(980.0));
                                ui.spacing_mut().item_spacing.y = 10.0;
                                match self.page {
                                    Page::Stream => self.stream_page(ui, &snap, &client),
                                    Page::Performance => self.performance_page(ui, &snap),
                                    Page::Settings => self.settings_page(ui, &snap),
                                }
                            });
                    });
            });

        if self.show_qr_modal {
            self.draw_qr_modal(&ctx, &snap);
        }
    }
}

// ── Sidebar ─────────────────────────────────────────────────────────────────

impl AnalyzerApp {
    fn draw_sidebar(&mut self, ui: &mut egui::Ui, snap: &StatsSnapshot, client: &ClientSnapshot) {
        egui::Panel::left("sidebar")
            .exact_size(224.0)
            .resizable(false)
            .show_separator_line(true)
            .frame(Frame::new().fill(SIDEBAR).inner_margin(Margin {
                left: 14,
                right: 14,
                top: 18,
                bottom: 16,
            }))
            .show(ui, |ui| {
                self.draw_brand(ui);
                ui.add_space(26.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                let stream = self.nav_item(ui, Page::Stream, Icon::Display, "Stream");
                let performance =
                    self.nav_item(ui, Page::Performance, Icon::Activity, "Performance");
                let settings = self.nav_item(ui, Page::Settings, Icon::Sliders, "Settings");
                // A lime bar marks the open page and slides to the one clicked.
                let active = match self.page {
                    Page::Stream => stream,
                    Page::Performance => performance,
                    Page::Settings => settings,
                };
                let y = ui.ctx().animate_value_with_time(
                    egui::Id::new("nav-active-bar"),
                    active.center().y,
                    0.18,
                );
                ui.painter().rect_filled(
                    Rect::from_center_size(pos2(active.left() + 2.5, y), vec2(3.0, 16.0)),
                    1.5,
                    ACCENT,
                );

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.label(faint(format!("Version {}", env!("CARGO_PKG_VERSION"))));
                    ui.add_space(6.0);
                    sidebar_status(ui, snap, client);
                });
            });
    }

    fn draw_brand(&mut self, ui: &mut egui::Ui) {
        const LOGO: f32 = 30.0;
        let ppp = ui.ctx().pixels_per_point();
        if self.logo.as_ref().is_none_or(|(scale, _)| *scale != ppp) {
            let pixels = (LOGO * ppp).round().max(1.0) as usize;
            self.logo = logo_texture(ui.ctx(), pixels).map(|texture| (ppp, texture));
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let (rect, _) = ui.allocate_exact_size(vec2(LOGO, LOGO), Sense::hover());
            if let Some((_, texture)) = &self.logo {
                ui.painter().image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            ui.label(
                egui::RichText::new("EternalMonitor")
                    .family(semibold())
                    .size(15.5)
                    .color(TEXT),
            );
        });
    }

    /// One sidebar entry. Returns its rectangle for the active-page bar.
    fn nav_item(&mut self, ui: &mut egui::Ui, page: Page, icon: Icon, label: &str) -> Rect {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        if response.clicked() {
            self.page = page;
        }
        let active = self.page == page;
        // Hover and selection fade in and out instead of switching.
        let hover = ui.ctx().animate_bool_with_time(
            response.id.with("hover"),
            response.hovered() && !active,
            0.12,
        );
        let selected = ui
            .ctx()
            .animate_bool_with_time(response.id.with("active"), active, 0.18);
        let painter = ui.painter();
        painter.rect(
            rect,
            8.0,
            SIDEBAR
                .lerp_to_gamma(SURFACE, hover)
                .lerp_to_gamma(SURFACE_RAISED, selected),
            Stroke::new(1.0, BORDER.gamma_multiply(selected)),
            StrokeKind::Inside,
        );
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
            TEXT_FAINT
                .lerp_to_gamma(TEXT_MUTED, hover)
                .lerp_to_gamma(ACCENT, selected),
        );
        painter.text(
            pos2(rect.left() + 38.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::new(13.5, medium()),
            TEXT_MUTED.lerp_to_gamma(TEXT, hover.max(selected)),
        );
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
        rect
    }
}

fn sidebar_status(ui: &mut egui::Ui, snap: &StatsSnapshot, client: &ClientSnapshot) {
    let state = StreamState::of(snap, client);
    let (title, detail) = match state {
        StreamState::Off => ("Stopped".to_string(), "Streaming is off".to_string()),
        StreamState::Ready => ("Ready".to_string(), "Waiting for an iPad".to_string()),
        StreamState::Streaming => (
            "Streaming".to_string(),
            format!(
                "{} · {}",
                client
                    .info
                    .as_ref()
                    .map(|i| i.device_name.as_str())
                    .unwrap_or("iPad"),
                client.link_name()
            ),
        ),
    };
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(10.0)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let (dot, _) = ui.allocate_exact_size(vec2(10.0, 30.0), Sense::hover());
                widgets::status_dot(ui, dot.center(), state.color(), 4.0, state.dot());
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(strong(title));
                    ui.add(egui::Label::new(faint(detail)).truncate());
                });
            });
        });
}

/// The window logo, downsampled to exactly `pixels` so it stays crisp.
fn logo_texture(ctx: &egui::Context, pixels: usize) -> Option<egui::TextureHandle> {
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon_256.png")).ok()?;
    let (w, h) = (icon.width as usize, icon.height as usize);
    let mut out = vec![0u8; pixels * pixels * 4];
    let scale = w as f32 / pixels as f32;
    for y in 0..pixels {
        for x in 0..pixels {
            // Box filter over the source footprint, in premultiplied alpha.
            let (x0, x1) = (
                (x as f32 * scale) as usize,
                (((x + 1) as f32 * scale).ceil() as usize).min(w),
            );
            let (y0, y1) = (
                (y as f32 * scale * h as f32 / w as f32) as usize,
                ((((y + 1) as f32 * scale * h as f32 / w as f32).ceil()) as usize).min(h),
            );
            let mut acc = [0f32; 4];
            let mut n = 0f32;
            for sy in y0..y1.max(y0 + 1) {
                for sx in x0..x1.max(x0 + 1) {
                    let i = (sy.min(h - 1) * w + sx.min(w - 1)) * 4;
                    let a = icon.rgba[i + 3] as f32 / 255.0;
                    acc[0] += icon.rgba[i] as f32 * a;
                    acc[1] += icon.rgba[i + 1] as f32 * a;
                    acc[2] += icon.rgba[i + 2] as f32 * a;
                    acc[3] += a;
                    n += 1.0;
                }
            }
            let o = (y * pixels + x) * 4;
            out[o] = (acc[0] / n).round() as u8;
            out[o + 1] = (acc[1] / n).round() as u8;
            out[o + 2] = (acc[2] / n).round() as u8;
            out[o + 3] = (acc[3] / n * 255.0).round() as u8;
        }
    }
    let image = egui::ColorImage::from_rgba_premultiplied([pixels, pixels], &out);
    Some(ctx.load_texture("eternal-logo", image, egui::TextureOptions::LINEAR))
}

// ── QR dialog ───────────────────────────────────────────────────────────────

impl AnalyzerApp {
    fn open_qr(&mut self, snap: &StatsSnapshot) {
        // Re-resolve the LAN address on open — DHCP renews and interface
        // changes would otherwise leave the QR encoding a stale IP from
        // process start.
        if let Some(port) = snap
            .listen_addr
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
        {
            PIPELINE_STATS.lock().set_listen_addr(detect_local_ip(port));
        }
        self.show_qr_modal = true;
    }

    fn draw_qr_modal(&mut self, ctx: &egui::Context, snap: &StatsSnapshot) {
        let listen_addr = if snap.listen_addr.is_empty() {
            self.control.shared.target_addr.lock().to_string()
        } else {
            snap.listen_addr.clone()
        };
        let url = format!(
            "eternaldisplay://{}?t={}",
            listen_addr,
            crate::pairing::token_hex(&self.control.shared.pairing.lock().token())
        );

        // Cache the encoded QR matrix until the URL changes.
        if self
            .qr_cache
            .as_ref()
            .map(|(cached_url, _)| cached_url != &url)
            .unwrap_or(true)
        {
            match QrCode::new(url.as_bytes()) {
                Ok(code) => self.qr_cache = Some((url.clone(), code)),
                Err(error) => {
                    warn!(error = %error, "Failed to encode QR code");
                    self.qr_cache = None;
                }
            }
        }

        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("qr-dialog"))
            .backdrop_color(Color32::from_black_alpha(175))
            .frame(
                Frame::new()
                    .fill(SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, BORDER_STRONG))
                    .corner_radius(18.0)
                    .inner_margin(Margin::same(28))
                    .shadow(egui::Shadow {
                        offset: [0, 24],
                        blur: 64,
                        spread: 0,
                        color: Color32::from_black_alpha(170),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.vertical_centered(|ui| {
                    ui.label(overline("QR code"));
                    ui.label(
                        egui::RichText::new("Scan to connect")
                            .family(semibold())
                            .size(20.0)
                            .color(TEXT),
                    );
                    ui.add(
                        egui::Label::new(muted(
                            "On your iPad, open EternalMonitor, tap Scan QR and point it at this code.",
                        ))
                        .wrap(),
                    );
                    ui.add_space(12.0);
                    let tile = 244.0;
                    let (rect, _) = ui.allocate_exact_size(vec2(tile, tile), Sense::hover());
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, 14.0, Color32::WHITE);
                    match self.qr_cache.as_ref().map(|(_, code)| code) {
                        Some(code) => {
                            let width = code.width();
                            let modules = code.to_colors();
                            let quiet = 16.0_f32;
                            let module = ((tile - 2.0 * quiet) / width as f32).floor().max(1.0);
                            let offset = (tile - module * width as f32) / 2.0;
                            let origin = rect.min + vec2(offset, offset);
                            for y in 0..width {
                                for x in 0..width {
                                    if matches!(modules[y * width + x], QrModuleColor::Dark) {
                                        let cell = Rect::from_min_size(
                                            origin + vec2(x as f32 * module, y as f32 * module),
                                            vec2(module, module),
                                        );
                                        painter.rect_filled(cell, 0.0, Color32::BLACK);
                                    }
                                }
                            }
                        }
                        None => {
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "The QR code could not be created",
                                egui::FontId::new(SMALL, egui::FontFamily::Proportional),
                                DANGER,
                            );
                        }
                    }
                    ui.add_space(12.0);
                    ui.label(mono(listen_addr.clone(), 14.0));
                    ui.add(
                        egui::Label::new(faint(
                            "The code carries this PC's pairing key. Only show it to iPads you trust.",
                        ))
                        .wrap(),
                    );
                    ui.add_space(14.0);
                    let width = ui.available_width();
                    if widgets::button_ex(ui, Tone::Primary, None, "Done", true, width).clicked() {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            self.show_qr_modal = false;
        }
    }
}

// ── Formatting ──────────────────────────────────────────────────────────────

fn format_bytes(bytes: u64) -> String {
    if bytes > 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes > 1_000_000 {
        format!("{:.2} MB", bytes as f64 / 1_000_000.0)
    } else if bytes > 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_uptime(uptime_secs: f64) -> String {
    let total = uptime_secs.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m {s:02}s")
    }
}

/// `h264_nvenc` → `H.264 · NVENC`, `libx264` → `H.264 · CPU`.
fn friendly_codec(name: &str) -> String {
    if name.is_empty() {
        return "Detecting encoder".into();
    }
    let lower = name.to_ascii_lowercase();
    let codec = if lower.contains("hevc") || lower.contains("265") {
        "HEVC"
    } else {
        "H.264"
    };
    let engine = if lower.contains("nvenc") {
        "NVENC"
    } else if lower.contains("amf") {
        "AMF"
    } else if lower.contains("qsv") {
        "Quick Sync"
    } else if lower.contains("videotoolbox") {
        "VideoToolbox"
    } else if lower.starts_with("lib") {
        "CPU"
    } else {
        return name.to_string();
    };
    format!("{codec} · {engine}")
}

fn value_or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "Unknown"
    } else {
        value
    }
}

pub fn detect_local_ip(listen_port: u16) -> String {
    // Primary: ask the OS which local interface would route toward a public address. This is the
    // happy path on any machine with a normal default route.
    if let Ok(addr) = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| {
        s.connect("8.8.8.8:80")?;
        s.local_addr()
    }) {
        let ip = addr.ip();
        if !ip.is_unspecified() && !ip.is_loopback() {
            return format!("{}:{}", ip, listen_port);
        }
    }

    // Fallback: no default route (isolated LAN / no internet / blocked) — enumerate local
    // adapters and use the first real LAN IPv4 so the displayed address and QR still work.
    if let Some(ip) = first_lan_ipv4() {
        return format!("{}:{}", ip, listen_port);
    }

    format!("unknown:{listen_port}")
}

/// Launch the GUI window. Blocks the calling thread.
pub fn run_gui(control: GuiControl) -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon_256.png"))
        .expect("embedded icon PNG is valid");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EternalMonitor")
            .with_inner_size([1000.0, 640.0])
            .with_min_inner_size([820.0, 560.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    eframe::run_native(
        "EternalMonitor",
        options,
        Box::new(|cc| Ok(Box::new(AnalyzerApp::new(cc, control)))),
    )
}

#[cfg(test)]
mod tests;
