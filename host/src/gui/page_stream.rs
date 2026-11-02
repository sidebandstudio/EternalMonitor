//! Stream: is it working, and how does an iPad connect?

use eframe::egui::{self, vec2, Align, Layout, RichText, Sense, Ui};

use super::icons::Icon;
use super::theme::*;
use super::widgets::{self, Tone, GAP};
use super::{
    value_or_unknown, AnalyzerApp, ClientSnapshot, Copied, StatsSnapshot, StreamState,
    CAPTURE_AUTO_LABEL, CAPTURE_VIRTUAL_LABEL, CAPTURE_VIRTUAL_SENTINEL, ENCODER_AUTO_LABEL,
};
use crate::capture::recovery::CaptureBlock;
use crate::control::VddStatus;
use crate::i18n::{tr, trf};
use crate::vdd::VddTaskHealth;
use crate::vdd_device::VddProblem;

impl AnalyzerApp {
    pub(super) fn stream_page(
        &mut self,
        ui: &mut Ui,
        snap: &StatsSnapshot,
        client: &ClientSnapshot,
    ) {
        self.notices(ui, snap, client);
        self.ipad_update_notice(ui, client);
        self.feedback_ask_card(ui, client);
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
    fn notices(&mut self, ui: &mut Ui, snap: &StatsSnapshot, client: &ClientSnapshot) {
        self.port_notice(ui);
        if let Some((title, body)) = usb_cable_notice(
            snap.usb_service_reachable,
            snap.usb_cabled_devices,
            snap.apple_devices_installed,
            client.on_usb(),
        ) {
            widgets::banner(ui, WARNING, Icon::Plug, title, body, 170.0, |ui| {
                apple_devices_action(ui, snap.apple_devices_installed);
            });
            ui.add_space(GAP - 10.0);
        }
        if snap.using_software_fallback {
            // Choices made while troubleshooting are the usual reason a
            // working graphics card is skipped, so offer to undo them first.
            let customized = self.settings_encoder_choice != ENCODER_AUTO_LABEL
                || self.settings_encoder_input != crate::encoder::input::EncoderInput::default();
            let mut reset = false;
            let mut restart = false;
            widgets::banner(
                ui,
                WARNING,
                Icon::Warning,
                SOFTWARE_FALLBACK_TITLE,
                &software_fallback_body(customized, snap.nvidia_driver_outdated),
                if customized { 230.0 } else { 110.0 },
                |ui| {
                    if customized {
                        reset =
                            widgets::button(ui, Tone::Primary, None, "Use recommended settings")
                                .clicked();
                    } else {
                        restart =
                            widgets::button(ui, Tone::Secondary, Some(Icon::Refresh), "Restart")
                                .clicked();
                    }
                },
            );
            if reset {
                self.reset_video_settings();
            }
            if restart {
                self.restart_stream();
            }
            ui.add_space(GAP - 10.0);
        }
        #[allow(unused_mut)] // Debug builds may simulate a problem below.
        let (mut capture_block, mut vdd_status, mut vdd_health, mut vdd_problem) = (
            *self.control.shared.capture_block.lock(),
            *self.control.shared.vdd_status.lock(),
            crate::vdd::task_health(),
            *self.control.shared.vdd_problem.lock(),
        );
        #[cfg(debug_assertions)]
        simulate_problem(
            &mut capture_block,
            &mut vdd_status,
            &mut vdd_health,
            &mut vdd_problem,
        );
        if let Some((icon, title, body)) =
            capture_block_notice(capture_block, snap.pipeline_running)
        {
            if capture_block == Some(CaptureBlock::UnsupportedOnThisGpu) {
                self.graphics_card_notice(ui, icon, title, body);
            } else {
                widgets::banner(ui, WARNING, icon, title, body, 0.0, |_| {});
            }
            ui.add_space(GAP - 10.0);
        }
        // The user asked for the extended (virtual) display but it could not be brought up,
        // so capture silently fell back to the primary monitor. Say so.
        let vdd_failure = crate::capture::second_screen::remembered_failure(&self.control.shared);
        if let Some(mut notice) =
            second_screen_notice(vdd_status, vdd_health, vdd_problem, vdd_failure)
        {
            add_later_retry(
                &mut notice,
                crate::capture::second_screen::retries_later(&self.control.shared),
            );
            let mut copy_logs = false;
            let mut retry = false;
            widgets::banner(
                ui,
                WARNING,
                Icon::Warning,
                SECOND_SCREEN_FAILED_TITLE,
                &notice.body,
                110.0 * (1 + notice.retry as u8 + notice.installer as u8) as f32,
                |ui| {
                    copy_logs =
                        widgets::button(ui, Tone::Ghost, Some(Icon::Logs), "Copy logs").clicked();
                    if notice.retry {
                        retry =
                            widgets::button(ui, Tone::Secondary, Some(Icon::Refresh), "Try again")
                                .clicked();
                    }
                    if notice.installer {
                        widgets::link(ui, "Download installer", INSTALLER_URL);
                    }
                },
            );
            if retry {
                self.restart_stream();
            }
            if copy_logs {
                if let Some(text) = crate::logging::session_log_text() {
                    self.copy(ui.ctx(), Copied::Logs, text);
                }
            }
            ui.add_space(GAP - 10.0);
        }
        self.restart_notice(ui);
    }

    /// Windows won't share the screen from the card EternalMonitor runs on:
    /// name the card the screen is connected to and offer to move the app
    /// onto it, or, when that did not help, say what overrides it.
    fn graphics_card_notice(&mut self, ui: &mut Ui, icon: Icon, title: &str, body: &str) {
        let diagnosis = self.control.shared.gpu_diagnosis.lock().clone();
        let record = self.control.shared.gpu_switch.lock().clone();
        match gpu_notice(diagnosis.as_ref(), record.as_ref()) {
            GpuNotice::Plain => widgets::banner(ui, WARNING, icon, title, body, 0.0, |_| {}),
            GpuNotice::Choose { gpu } => widgets::banner(
                ui,
                WARNING,
                icon,
                title,
                &gpu_choice_body(&gpu),
                0.0,
                |_| {},
            ),
            GpuNotice::Switch {
                gpu,
                app_gpu,
                preference,
            } => {
                let label = gpu_switch_button(&gpu);
                let mut clicked = false;
                widgets::banner(
                    ui,
                    WARNING,
                    icon,
                    title,
                    &gpu_switch_body(&gpu, &app_gpu),
                    button_width(&label),
                    |ui| {
                        clicked = widgets::button(ui, Tone::Primary, None, &label).clicked();
                    },
                );
                if clicked {
                    self.switch_graphics_card(preference, &gpu);
                }
            }
            GpuNotice::Overridden { gpu, app_gpu } => {
                let (mut restart, mut undo) = (false, false);
                widgets::banner(
                    ui,
                    WARNING,
                    Icon::Warning,
                    GPU_OVERRIDDEN_TITLE,
                    &gpu_overridden_body(&gpu, app_gpu.as_deref()),
                    330.0,
                    |ui| {
                        undo =
                            widgets::button(ui, Tone::Secondary, None, "Undo the change").clicked();
                        restart = widgets::button(
                            ui,
                            Tone::Primary,
                            Some(Icon::Refresh),
                            "Restart EternalMonitor",
                        )
                        .clicked();
                    },
                );
                if undo {
                    self.undo_graphics_switch();
                } else if restart {
                    // The user changed the card in another app.
                    self.restart_now(crate::relaunch::Reason::Plain);
                }
            }
        }
        self.gpu_error_line(ui);
    }

    /// Why the graphics card setting wasn't saved, or its restart didn't
    /// start, when it wasn't.
    pub(super) fn gpu_error_line(&self, ui: &mut Ui) {
        if let Some(error) = &self.gpu_error {
            ui.add(
                egui::Label::new(
                    RichText::new(gpu_error_text(error))
                        .size(SMALL)
                        .color(DANGER),
                )
                .wrap(),
            );
        }
        if matches!(
            self.restart_refused,
            Some(crate::relaunch::Reason::GraphicsCard | crate::relaunch::Reason::Plain)
        ) {
            self.handoff_line(ui, crate::handoff::Handoff::Update);
        }
    }

    /// Where the user asked for an update or a restart: why it waits for
    /// `refused_by`, which hands over.
    pub(super) fn handoff_line(&self, ui: &mut Ui, refused_by: crate::handoff::Handoff) {
        ui.add(
            egui::Label::new(
                RichText::new(refused_by.refusal())
                    .size(SMALL)
                    .color(WARNING),
            )
            .wrap(),
        );
    }

    /// "Restart EternalMonitor" on every page: while the new copy starts,
    /// and, when it never said it runs, why this window stays, with Quit.
    pub(super) fn restart_banner(&mut self, ui: &mut Ui) {
        if self.restart.busy() {
            widgets::banner(
                ui,
                ACCENT,
                Icon::Refresh,
                "Restarting EternalMonitor",
                "EternalMonitor opens again in a moment.",
                0.0,
                |_| {},
            );
            return;
        }
        let Some(reason) = self.restart.failed() else {
            return;
        };
        let mut quit = false;
        widgets::banner(
            ui,
            DANGER,
            Icon::Warning,
            crate::relaunch::RESTART_FAILED_TITLE,
            &crate::relaunch::restart_failed_body(reason),
            button_width("Quit EternalMonitor"),
            |ui| {
                quit = widgets::button(ui, Tone::Primary, None, "Quit EternalMonitor").clicked();
            },
        );
        if quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
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
            "These display and video changes apply when the stream restarts. The iPad reconnects by itself.",
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
        let quality = self
            .quality
            .update(client.session_id, client.report.as_ref());
        let device = client
            .info
            .as_ref()
            .map(|i| i.device_name.clone())
            .unwrap_or_default();
        let (title, subtitle) = match state {
            StreamState::Off => (
                tr("Streaming is off").to_string(),
                tr("Click Start streaming so your iPad can connect.").to_string(),
            ),
            StreamState::Ready => (
                tr("Ready for your iPad").to_string(),
                "Open EternalMonitor on your iPad and tap this PC, or plug in a USB cable."
                    .to_string(),
            ),
            StreamState::Streaming => {
                let (w, h) = snap.capture_resolution;
                let mode = if self.settings_capture_display == CAPTURE_VIRTUAL_SENTINEL
                    && *self.control.shared.vdd_status.lock() == VddStatus::Active
                {
                    tr(CAPTURE_VIRTUAL_LABEL)
                } else {
                    tr("Mirroring")
                };
                let mut parts = vec![client.link_name().to_string(), mode.to_string()];
                if w > 0 && h > 0 {
                    parts.push(format!("{w} × {h}"));
                }
                parts.push(format!("{} fps", self.control.shared.effective_fps()));
                (
                    trf("Streaming to {device}", &[("device", &device)]),
                    parts.join("  ·  "),
                )
            }
            StreamState::Paused => (
                trf("Streaming to {device} is paused", &[("device", &device)]),
                "Windows stopped screen capture. See the message above.".to_string(),
            ),
        };
        // Off only until another program lets go of the stream port.
        let waiting = crate::startup::port::held().is_some();
        let (title, subtitle) =
            super::port_notice::waiting_status(state == StreamState::Off, waiting)
                .unwrap_or((title, subtitle));

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

                    // Room for the buttons drawn below, as wide as their
                    // labels in this language, and never less than before.
                    let spacing = ui.spacing().item_spacing.x;
                    let buttons = [
                        (snap.pipeline_running || waiting, Icon::Stop, tr("Stop")),
                        (snap.pipeline_running, Icon::Refresh, tr("Restart")),
                        (
                            !snap.pipeline_running && !waiting,
                            Icon::Play,
                            tr("Start streaming"),
                        ),
                    ];
                    let measured: f32 = buttons
                        .into_iter()
                        .filter(|&(shown, _, _)| shown)
                        .map(|(_, icon, label)| {
                            widgets::button_width(ui, Some(icon), label) + spacing
                        })
                        .sum();
                    let buttons_width = measured.max(if state == StreamState::Off {
                        170.0
                    } else {
                        210.0
                    });
                    // A narrow window cuts the text short, never under the
                    // buttons: Russian at the minimum size leaves about 130.
                    let text_width = (ui.available_width() - buttons_width).max(0.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_width, 48.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.add_space(3.0);
                            widgets::truncated(ui, &title, |text| {
                                RichText::new(text)
                                    .family(semibold())
                                    .size(18.0)
                                    .color(TEXT)
                            });
                            widgets::truncated(ui, &subtitle, muted);
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if snap.pipeline_running || waiting {
                            // While waiting for the port, Stop cancels the
                            // start that would follow.
                            if widgets::button(ui, Tone::Danger, Some(Icon::Stop), tr("Stop"))
                                .clicked()
                            {
                                action = Some(HeaderAction::Stop);
                            }
                        }
                        if snap.pipeline_running {
                            if widgets::button(
                                ui,
                                Tone::Secondary,
                                Some(Icon::Refresh),
                                tr("Restart"),
                            )
                            .on_hover_text(
                                "Restart capture and encoding. The iPad reconnects on its own.",
                            )
                            .clicked()
                            {
                                action = Some(HeaderAction::Restart);
                            }
                        } else if !waiting
                            && widgets::button(
                                ui,
                                Tone::Primary,
                                Some(Icon::Play),
                                tr("Start streaming"),
                            )
                            .clicked()
                        {
                            action = Some(HeaderAction::Start);
                        }
                    });
                },
            );

            ui.add_space(14.0);
            widgets::divider(ui);
            ui.add_space(12.0);
            self.screen_mode_row(ui, snap.pipeline_running);

            if state == StreamState::Streaming {
                ui.add_space(12.0);
                widgets::divider(ui);
                ui.add_space(14.0);
                let latency = client
                    .report
                    .as_ref()
                    .filter(|r| r.e2e_latency_ms_x10 > 0)
                    .map(|r| format!("{:.0}", f64::from(r.e2e_latency_ms_x10) / 10.0))
                    .unwrap_or_else(|| format!("{:.0}", snap.latency_ms));
                ui.columns(3, |cols| {
                    widgets::metric(
                        &mut cols[0],
                        tr("Frame rate"),
                        &format!("{:.0}", snap.capture_fps),
                        "fps",
                        22.0,
                    )
                    .on_hover_text("Pictures per second. 60 looks smooth.");
                    widgets::metric(&mut cols[1], tr("Delay"), &latency, "ms", 22.0)
                        .on_hover_text("Time from this PC's screen to the picture on the iPad.");
                    widgets::metric(&mut cols[2], tr("Connection"), tr(quality), "", 22.0)
                        .on_hover_text(
                            "How well the picture is getting through. Performance has the details.",
                        );
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

    /// Mirror or second screen, named as on the iPad. Always above the fold:
    /// people who never scroll still find it.
    fn screen_mode_row(&mut self, ui: &mut Ui, running: bool) {
        let current = self.settings_capture_display.clone();
        let mut second = current == CAPTURE_VIRTUAL_SENTINEL;
        let vdd = *self.control.shared.vdd_status.lock();
        let output = (!current.is_empty() && !second)
            .then(|| current.rsplit('\\').next().unwrap_or(&current).to_string());
        let mut arrange = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let label = tr("Use the iPad as");
            let response = ui.label(RichText::new(label).family(medium()).color(TEXT));
            widgets::automation_id(&response, label);
            // A particular monitor (chosen in Settings) is still a mirror.
            widgets::segmented(
                ui,
                &mut second,
                &[
                    (false, tr(CAPTURE_AUTO_LABEL)),
                    (true, tr(CAPTURE_VIRTUAL_LABEL)),
                ],
            );
            if second && vdd == VddStatus::Active {
                arrange = widgets::button(ui, Tone::Ghost, None, tr("Arrange screens"))
                    .on_hover_text(
                        "Opens Windows Settings at Display, where you drag the screens into place.",
                    )
                    .clicked();
            }
        });
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(muted(screen_mode_text(
                &current,
                output.as_deref(),
                vdd,
                running,
            )))
            .wrap(),
        );
        if arrange {
            super::open_display_settings();
        }
        if second != (current == CAPTURE_VIRTUAL_SENTINEL) {
            self.set_second_screen(second, running);
        }
    }

    fn connect_card(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        // detect_local_ip() reports "unknown:<port>" when no LAN adapter is up.
        let online = !snap.listen_addr.is_empty() && !snap.listen_addr.starts_with("unknown");
        let mut open_qr = false;
        widgets::card_header(ui, tr("Connect an iPad"), |ui| {
            open_qr = widgets::button_ex(
                ui,
                Tone::Secondary,
                Some(Icon::Qr),
                tr("QR code"),
                online,
                0.0,
            )
            .clicked();
        });
        widgets::paragraph(
            ui,
            muted("On your iPad, open EternalMonitor and tap this PC, or tap Scan QR code. Over a USB cable no code is needed."),
        );
        ui.add_space(8.0);

        let (code, pairing_required, paused) = {
            let pairing = self.control.shared.pairing.lock();
            (
                format!("{:06}", pairing.code()),
                pairing.required,
                pairing.code_pairing_paused(std::time::Instant::now()),
            )
        };
        let code = format!("{} {}", &code[..3], &code[3..]);
        let mut copy = false;
        let mut rotate = false;
        let mut resume = false;
        let copied = self.recently_copied(Copied::Address);
        if online {
            value_block(ui, tr("Address"), &snap.listen_addr, |ui| {
                let (icon, tip) = if copied {
                    (Icon::Check, tr("Copied"))
                } else {
                    (Icon::Copy, tr("Copy address"))
                };
                copy = widgets::icon_button(ui, icon, tip).clicked();
            });
            if let Some(home) = home_address(&self.address_hint, &snap.listen_addr) {
                widgets::paragraph(
                    ui,
                    muted(format!(
                        "If that doesn't connect and your iPad is on the same network as this PC, use {home} as the PC address."
                    )),
                );
            }
        } else {
            widgets::overline_label(ui, tr("Address"));
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
        if let (true, Some(left)) = (pairing_required, paused) {
            widgets::overline_label(ui, tr("Pairing code"));
            widgets::paragraph(
                ui,
                RichText::new(code_pairing_paused_text(left))
                    .size(SMALL)
                    .color(WARNING),
            );
            resume =
                widgets::button(ui, Tone::Secondary, None, tr("Resume code pairing")).clicked();
        } else if pairing_required {
            value_block(ui, tr("Pairing code"), &code, |ui| {
                rotate = widgets::icon_button(ui, Icon::Refresh, tr("New code")).clicked();
            });
        } else {
            widgets::overline_label(ui, tr("Pairing code"));
            let note = tr("Not needed while pairing is off.");
            let response = ui.label(muted(note));
            widgets::automation_id(&response, note);
        }
        if copy && online {
            self.copy(ui.ctx(), Copied::Address, snap.listen_addr.clone());
        }
        if rotate {
            self.new_pairing_code();
        }
        if resume {
            self.resume_code_pairing();
        }
        if open_qr {
            self.open_qr();
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
                tr("Pairing"),
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
                snap.usb_cabled_devices,
                snap.apple_devices_installed,
            );
            let usb_color = if usb.available { ACCENT } else { TEXT_MUTED };
            let (usb_text, usb_badge) = (usb.text, usb.badge);
            widgets::icon_row(
                ui,
                Icon::Plug,
                usb_color,
                tr("USB connection"),
                &usb_text,
                if snap.usb_service_reachable {
                    110.0
                } else {
                    170.0
                },
                |ui| {
                    if snap.usb_service_reachable {
                        widgets::badge(ui, tr(usb_badge), usb_color);
                    } else {
                        apple_devices_action(ui, snap.apple_devices_installed);
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
                tr("PC audio"),
                &audio_text,
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut audio_on, "Stream PC audio");
                },
            );
            if audio_on != enabled {
                self.set_stream_audio(audio_on);
            }
        });
    }

    /// Mirror / extend / specific monitor. Shared with Settings.
    pub(super) fn display_picker(&mut self, ui: &mut Ui, width: f32) {
        let outputs = self.available_outputs.clone();
        let current = self.settings_capture_display.clone();
        let selected_text = if current.is_empty() {
            tr(CAPTURE_AUTO_LABEL).to_string()
        } else if current == CAPTURE_VIRTUAL_SENTINEL {
            tr(CAPTURE_VIRTUAL_LABEL).to_string()
        } else if let Some(o) = outputs.iter().find(|o| o.device_name == current) {
            super::format_output_label(o)
        } else {
            trf(
                "{display} (not connected)",
                &[("display", current.rsplit('\\').next().unwrap_or(&current))],
            )
        };
        let mut choice = current.clone();
        let combo = widgets::combo(ui.id().with("capture-display"), &selected_text, width).show_ui(
            ui,
            |ui| {
                ui.set_min_width(width);
                let auto = ui.selectable_label(choice.is_empty(), tr(CAPTURE_AUTO_LABEL));
                widgets::automation_id(&auto, tr(CAPTURE_AUTO_LABEL));
                if auto.clicked() {
                    choice = String::new();
                }
                // Managed virtual display — created on demand, removed when not in use.
                let virtual_display = ui.selectable_label(
                    choice == CAPTURE_VIRTUAL_SENTINEL,
                    tr(CAPTURE_VIRTUAL_LABEL),
                );
                widgets::automation_id(&virtual_display, tr(CAPTURE_VIRTUAL_LABEL));
                if virtual_display.clicked() {
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
            },
        );
        widgets::name_combo(ui, &combo.response, &selected_text);
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

pub(super) const SOFTWARE_FALLBACK_TITLE: &str =
    "Video is using the processor instead of the graphics card";

/// What the CPU fallback notice says: undo custom settings first, say the
/// NVIDIA driver is too old when that is what the graphics card refused, or
/// suggest a driver update.
pub(super) fn software_fallback_body(customized: bool, nvidia_driver_outdated: bool) -> String {
    if customized {
        "The graphics card could not encode video with the options chosen in Settings, so the picture may lag and the PC may run hot. Go back to the recommended options first.".to_string()
    } else if nvidia_driver_outdated {
        format!(
            "The NVIDIA graphics driver on this PC is too old for the graphics card to encode video, so the picture may lag and the PC may run hot. Update the NVIDIA driver to version {} or newer, if your graphics card supports it, then click Restart. Older cards keep using the processor.",
            crate::encoder::NVENC_MIN_DRIVER
        )
    } else {
        "The picture may lag and the PC may run hot. Update your graphics driver from NVIDIA, AMD or Intel, then click Restart.".to_string()
    }
}
pub(super) const SECOND_SCREEN_FAILED_TITLE: &str =
    "The second screen couldn't start, so your main screen is mirrored";
const INSTALLER_URL: &str = "https://eternalmonitor.dev/download.html";

/// Debug builds only, for screenshots: `ETERNAL_SIMULATE_PROBLEM` names one
/// problem for the Stream page to show without it happening.
#[cfg(debug_assertions)]
fn simulate_problem(
    block: &mut Option<CaptureBlock>,
    vdd: &mut VddStatus,
    health: &mut VddTaskHealth,
    problem: &mut Option<VddProblem>,
) {
    let Ok(name) = std::env::var("ETERNAL_SIMULATE_PROBLEM") else {
        return;
    };
    match name.as_str() {
        "secure-desktop" => *block = Some(CaptureBlock::SecureDesktop),
        "display-changing" => *block = Some(CaptureBlock::DisplayChanging),
        "no-display" => *block = Some(CaptureBlock::NoDisplay),
        "unsupported-gpu" => *block = Some(CaptureBlock::UnsupportedOnThisGpu),
        "too-many-capture-apps" => *block = Some(CaptureBlock::TooManyCaptureApps),
        "session-disconnected" => *block = Some(CaptureBlock::SessionDisconnected),
        "graphics-driver-stuck" => *block = Some(CaptureBlock::DriverNotResponding),
        "driver-not-responding" => {
            (*vdd, *health) = (VddStatus::TaskFailed, VddTaskHealth::NotResponding)
        }
        "driver-tasks-missing" => {
            (*vdd, *health) = (VddStatus::TaskFailed, VddTaskHealth::TasksMissing)
        }
        "driver-not-installed" => {
            (*vdd, *problem) = (VddStatus::TaskFailed, Some(VddProblem::NotInstalled))
        }
        "driver-signature" => {
            (*vdd, *problem) = (VddStatus::TaskFailed, Some(VddProblem::SignatureBlocked))
        }
        "driver-error" => {
            (*vdd, *problem) = (
                VddStatus::TaskFailed,
                Some(VddProblem::DriverError(Some(43))),
            )
        }
        "display-did-not-appear" => {
            (*vdd, *problem) = (VddStatus::TaskFailed, Some(VddProblem::NoDisplay))
        }
        "display-not-extended" => {
            (*vdd, *problem) = (VddStatus::Failed, Some(VddProblem::NotExtended))
        }
        _ => {}
    }
}

/// What the second screen notice says and offers.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SecondScreenNotice {
    pub body: String,
    pub retry: bool,
    pub installer: bool,
}

/// The notice for a second screen that could not start, by what the display
/// driver tasks and its device report, and why the start failed (the
/// `second_screen_fallback` reason). `None` unless the second screen failed.
pub(super) fn second_screen_notice(
    status: VddStatus,
    health: VddTaskHealth,
    problem: Option<VddProblem>,
    failure: Option<&str>,
) -> Option<SecondScreenNotice> {
    use VddStatus::{Failed, TaskFailed};
    let notice = |body: &str, retry, installer| {
        Some(SecondScreenNotice {
            body: body.to_string(),
            retry,
            installer,
        })
    };
    match (status, health, problem) {
        (Failed | TaskFailed, VddTaskHealth::NotResponding, _) => notice(
            "Windows' display driver stopped responding. Restart the PC to use the second screen.",
            false,
            false,
        ),
        (Failed | TaskFailed, VddTaskHealth::TasksMissing, _) => notice(
            "The second screen isn't installed. Download EternalMonitor-Setup.exe and run it again to add it, then click Try again.",
            true,
            true,
        ),
        (Failed | TaskFailed, VddTaskHealth::Ok, Some(VddProblem::NotInstalled)) => notice(
            "The second screen's display driver isn't installed. Download EternalMonitor-Setup.exe and run it again to reinstall it, then click Try again.",
            true,
            true,
        ),
        (Failed | TaskFailed, VddTaskHealth::Ok, Some(VddProblem::SignatureBlocked)) => notice(
            "Windows couldn't verify the display driver's signature, so it won't start it (Windows code 52). Run EternalMonitor-Setup.exe again while connected to the internet to reinstall the driver, then click Try again. You don't need to turn off Secure Boot or Memory Integrity.",
            true,
            true,
        ),
        (Failed | TaskFailed, VddTaskHealth::Ok, Some(VddProblem::DriverError(code))) => {
            let code = code.map_or(String::new(), |code| format!(" (Windows code {code})"));
            notice(
                &format!("The second screen's display driver stopped with an error{code}. Restart the PC, then run EternalMonitor-Setup.exe again to repair it."),
                true,
                true,
            )
        }
        (Failed | TaskFailed, VddTaskHealth::Ok, Some(VddProblem::NoDisplay)) => notice(
            "The display driver is running, but Windows didn't add the second screen. Click Try again. If it keeps happening, restart the PC.",
            true,
            false,
        ),
        (Failed | TaskFailed, VddTaskHealth::Ok, Some(VddProblem::NotExtended)) => notice(
            "Windows added the second screen but kept your desktop off it. Click Try again, and while the second screen turns on, press Windows key + P and choose Extend, or choose Extend these displays in Settings > System > Display. EternalMonitor waits 30 seconds, and Windows remembers the choice.",
            true,
            false,
        ),
        (TaskFailed, VddTaskHealth::Ok, None) => notice(
            "Part of the EternalMonitor install is missing or blocked. Download EternalMonitor-Setup.exe and run it again to repair it, then click Try again.",
            true,
            true,
        ),
        // Nothing was tried: the update manifest names this PC's hardware.
        (Failed, VddTaskHealth::Ok, None) if failure == Some("hardware_rule") => notice(
            "On PCs like this one the second screen often doesn't start, so the iPad shows your main screen instead. Click Try again to use the second screen.",
            true,
            false,
        ),
        // It attached, so it was not late: it showed no picture.
        (Failed, VddTaskHealth::Ok, None)
            if matches!(failure, Some("no_frames" | "duplication_failed")) =>
        {
            notice(
                "Windows added the second screen, but EternalMonitor couldn't get a picture from it, so the iPad shows your main screen instead. Click Try again. If it keeps happening, restart the PC.",
                true,
                false,
            )
        }
        (Failed, VddTaskHealth::Ok, None) => notice(
            "Windows didn't add the second screen in time. Click Try again. If it keeps happening, run EternalMonitor-Setup.exe again to repair the display driver.",
            true,
            false,
        ),
        _ => None,
    }
}

/// Say that the host tries the second screen again by itself, after a
/// failure it remembers for this PC.
pub(super) fn add_later_retry(notice: &mut SecondScreenNotice, retries_later: bool) {
    if retries_later && notice.retry {
        notice
            .body
            .push_str(" EternalMonitor also tries again by itself later.");
    }
}

/// What the notice says when Windows won't share the screen from the card
/// EternalMonitor runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GpuNotice {
    /// The card could not be found: the general Windows Settings steps.
    Plain,
    /// One click moves EternalMonitor onto the screen's card.
    Switch {
        gpu: String,
        app_gpu: String,
        preference: crate::gpu_preference::Preference,
    },
    /// No preference certainly selects the card: how to pick it by hand.
    Choose { gpu: String },
    /// EternalMonitor switched and Windows still refuses: another app picks
    /// the card. Offered instead of the switch, so the app never reopens
    /// in a loop.
    Overridden {
        gpu: String,
        app_gpu: Option<String>,
    },
}

pub(super) fn gpu_notice(
    diagnosis: Option<&crate::gpu_preference::Diagnosis>,
    record: Option<&crate::gpu_preference::SwitchRecord>,
) -> GpuNotice {
    if let Some(record) = record.filter(|record| record.failed) {
        return GpuNotice::Overridden {
            gpu: record.gpu.clone(),
            app_gpu: diagnosis.map(|d| d.app_gpu.clone()),
        };
    }
    match diagnosis {
        Some(d) => match d.preference {
            Some(preference) => GpuNotice::Switch {
                gpu: d.screen_gpu.clone(),
                app_gpu: d.app_gpu.clone(),
                preference,
            },
            // Already on that card: switching can't help.
            None if d.screen_gpu == d.app_gpu => GpuNotice::Plain,
            None => GpuNotice::Choose {
                gpu: d.screen_gpu.clone(),
            },
        },
        None => GpuNotice::Plain,
    }
}

pub(super) fn gpu_switch_button(gpu: &str) -> String {
    format!(
        "Use the {}",
        super::page_troubleshooting::short_gpu_name(gpu)
    )
}

/// Room for a button with `label` in a banner.
fn button_width(label: &str) -> f32 {
    (label.chars().count() as f32 * 7.5 + 40.0).clamp(140.0, 320.0)
}

pub(super) fn gpu_switch_body(gpu: &str, app_gpu: &str) -> String {
    format!(
        "Your screen is connected to the {gpu}, but EternalMonitor is running on the {app_gpu}. Click the button and EternalMonitor restarts on the right card. You can change this later in Settings > Troubleshooting."
    )
}

/// A graphics card setting that couldn't be saved: nothing else changed.
pub(super) fn gpu_error_text(error: &str) -> String {
    if error == crate::gpu_preference::SETTINGS_NOT_SAVED {
        return error.into();
    }
    format!("Couldn't save the graphics card setting ({error}). Nothing was changed.")
}

pub(super) fn gpu_choice_body(gpu: &str) -> String {
    format!(
        "Your screen is connected to the {gpu}. Open Windows Settings > System > Display > Graphics, choose EternalMonitor, and pick the {gpu}. Then quit EternalMonitor and open it again."
    )
}

pub(super) const GPU_OVERRIDDEN_TITLE: &str =
    "Another app is choosing EternalMonitor's graphics card";

/// After a switch that did not help. `app_gpu` is the card EternalMonitor
/// still runs on, which says whose control app is in the way.
pub(super) fn gpu_overridden_body(gpu: &str, app_gpu: Option<&str>) -> String {
    let nvidia = |name: &str| name.to_ascii_uppercase().contains("NVIDIA");
    let setting = if nvidia(gpu) {
        "\"High-performance NVIDIA processor\""
    } else {
        "\"Let Windows decide\" (or \"Auto-select\") or \"Integrated graphics\""
    };
    let other_is_nvidia = nvidia(gpu) || app_gpu.is_none_or(nvidia);
    if other_is_nvidia {
        format!(
            "Your screen is connected to the {gpu}, but the NVIDIA app or NVIDIA Control Panel probably keeps EternalMonitor on another card. Open it, find EternalMonitor in the program settings, and set its graphics processor to {setting}. Then click Restart EternalMonitor."
        )
    } else {
        format!(
            "Your screen is connected to the {gpu}, but your graphics card's own app (such as AMD Software) probably keeps EternalMonitor on another card. Set EternalMonitor there to use the {gpu}, then click Restart EternalMonitor."
        )
    }
}

/// The notice while Windows keeps the screen from EternalMonitor. It goes
/// away when the picture comes back or the stream stops.
pub(super) fn capture_block_notice(
    block: Option<CaptureBlock>,
    running: bool,
) -> Option<(Icon, &'static str, &'static str)> {
    if !running {
        return None;
    }
    Some(match block? {
        CaptureBlock::SecureDesktop => (
            Icon::Lock,
            "Your PC is showing the lock screen or a permission prompt",
            "The iPad keeps the last picture. Your desktop comes back on the iPad when you unlock the PC or answer the prompt.",
        ),
        CaptureBlock::DisplayChanging => (
            Icon::Display,
            "Windows is changing the display",
            "This happens when the resolution changes or a full-screen game or app starts. The picture comes back by itself when it's done.",
        ),
        CaptureBlock::NoDisplay => (
            Icon::Display,
            "No display is on",
            "Open the laptop lid or turn a display on, and the picture comes back by itself. To keep streaming with the lid closed, choose Second screen.",
        ),
        CaptureBlock::UnsupportedOnThisGpu => (
            Icon::Display,
            "Windows won't share this screen from the graphics card EternalMonitor uses",
            "Open Settings > System > Display > Graphics, choose EternalMonitor, and pick the graphics card your screen is connected to (on a laptop, usually Power saving). Then quit EternalMonitor and open it again. Windows applies this choice only when the app opens.",
        ),
        CaptureBlock::TooManyCaptureApps => (
            Icon::Warning,
            "Another app is recording or sharing your screen",
            "Windows lets only a few apps do this at once. Close the screen recorder or meeting app, and the picture comes back by itself.",
        ),
        CaptureBlock::SessionDisconnected => (
            Icon::Lock,
            "This Windows session is disconnected",
            "This happens when Remote Desktop disconnects. Sign in at the PC itself to stream again.",
        ),
        CaptureBlock::DriverNotResponding => (
            Icon::Warning,
            "The graphics driver isn't responding",
            "The iPad keeps the last picture, and EternalMonitor waits for the driver. The picture comes back by itself when it responds. If it doesn't, restart the PC.",
        ),
    })
}

/// What the Stream page says under the mirror / second screen choice.
pub(super) fn screen_mode_text(
    capture: &str,
    output: Option<&str>,
    vdd: VddStatus,
    running: bool,
) -> String {
    let restart = if running {
        " Switching restarts the stream, and the iPad reconnects by itself."
    } else {
        ""
    };
    let what = if capture == CAPTURE_VIRTUAL_SENTINEL {
        match vdd {
            VddStatus::Active => "The iPad is an extra screen. Drag windows past the edge of your main screen to move them onto it.",
            VddStatus::Failed | VddStatus::TaskFailed => {
                "The second screen didn't start, so your main screen is mirrored. See the message above."
            }
            _ => "The iPad becomes an extra screen next to your main one as soon as it connects.",
        }
        .to_string()
    } else if let Some(name) = output {
        format!(
            "The iPad shows the same picture as {name}. Choose a different monitor in Settings."
        )
    } else {
        "The iPad shows the same picture as your main screen.".to_string()
    };
    what + restart
}

/// The home network's address, without the port (the iPad has its own Port
/// field), when the shown address is a VPN's or virtual switch's and the
/// home network's is known.
fn home_address(
    hints: &crate::network::HintCache,
    listen_addr: &str,
) -> Option<std::net::Ipv4Addr> {
    let (ip, _port) = listen_addr.rsplit_once(':')?;
    hints.get(ip.parse().ok()?)
}

/// Why the code is gone from the Stream page, for how long, and what
/// still works.
pub(super) fn code_pairing_paused_text(left: std::time::Duration) -> String {
    let minutes = left.as_secs().div_ceil(60).max(1);
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    format!(
        "Too many wrong codes were typed, so code pairing is paused for {minutes} more {unit}. The QR code and USB still work. Resuming gives a new code."
    )
}

/// Overline label, a large monospaced value and trailing controls.
fn value_block(ui: &mut Ui, label: &str, value: &str, controls: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        widgets::overline_label(ui, label);
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
    widgets::card_header(ui, tr("Connected iPad"), |ui| {
        if client.info.is_some() {
            widgets::badge(ui, link, ACCENT);
        }
    });
    let Some(info) = &client.info else {
        if streaming {
            widgets::empty_state(
                ui,
                Icon::Tablet,
                tr("Waiting for an iPad"),
                "It appears here as soon as it connects. If the iPad says this PC didn't answer, put both on the same Wi-Fi, or plug in a USB cable.",
            );
        } else {
            widgets::empty_state(
                ui,
                Icon::Tablet,
                tr("No iPad connected"),
                tr("Start streaming, then connect from your iPad."),
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

/// Opens Apple Devices when it is installed, otherwise links to its Store page.
fn apple_devices_action(ui: &mut Ui, installed: bool) {
    if installed {
        if widgets::button(ui, Tone::Primary, None, tr("Open Apple Devices")).clicked() {
            crate::transport::apple_usb::open_apple_devices();
        }
    } else {
        widgets::link(
            ui,
            tr("Get Apple Devices"),
            crate::transport::apple_usb::APPLE_DEVICES_STORE_URL,
        );
    }
}

/// The Stream page notice for an iPad on the cable that Windows cannot reach
/// because Apple Devices is not running or not installed. A session streaming
/// over USB proves the cable works, so it silences the notice.
pub(super) fn usb_cable_notice(
    reachable: bool,
    cabled: usize,
    installed: bool,
    streaming_over_usb: bool,
) -> Option<(&'static str, &'static str)> {
    if reachable || cabled == 0 || streaming_over_usb {
        return None;
    }
    Some(if installed {
        (
            "Your iPad is plugged in, but Apple Devices is not running",
            "EternalMonitor reaches an iPad over the USB cable through Apple Devices. Open it and leave it running; it can stay minimized. Wi-Fi works without it.",
        )
    } else {
        (
            "Your iPad is plugged in, but Apple Devices is not installed",
            "EternalMonitor needs Apple's free Apple Devices app to stream over the USB cable. Install it from the Microsoft Store, open it, and leave it running. Wi-Fi works without it.",
        )
    })
}

/// What the USB row says. The Apple Devices hint appears only when Apple's
/// device service cannot be reached.
pub(super) fn usb_summary(
    reachable: bool,
    devices: usize,
    link_state: &str,
    cabled: usize,
    installed: bool,
) -> UsbSummary {
    if !reachable {
        let text = match (cabled > 0, installed) {
            (true, true) => "iPad plugged in. Open Apple Devices and keep it running to stream over the cable.",
            (true, false) => "iPad plugged in, but Apple Devices is not installed. Get it from the Microsoft Store to use USB.",
            (false, true) => "Open Apple Devices to use USB. It must stay running while you stream.",
            (false, false) => "Install the Apple Devices app from the Microsoft Store to use USB.",
        };
        return UsbSummary {
            text: text.into(),
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
    // The audio stage's idle status names the protocol, not the device.
    if audio.status == "Waiting for client" {
        return ("Starts when an iPad connects.".into(), TEXT_MUTED);
    }
    if audio.device.is_empty() {
        (value_or_unknown(&audio.status).to_string(), TEXT_MUTED)
    } else {
        (format!("{} · {}", audio.status, audio.device), TEXT_MUTED)
    }
}
