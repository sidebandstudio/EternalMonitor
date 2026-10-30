//! First-run setup: the language, Start with Windows, how the iPad connects,
//! what it shows, then pairing. It opens on a fresh install (no settings
//! file at launch) and from Settings > Run setup again. Each choice applies
//! at once through the same actions as Settings, so Next without a change,
//! or Skip setup, leaves today's defaults. Nothing the app detects (graphics
//! card, encoder) is asked, and diagnostics sharing is not a step.

use eframe::egui::{self, vec2, Color32, Frame, Margin, RichText, Stroke, Ui};

use super::fonts::name_family;
use super::icons::Icon;
use super::theme::*;
use super::widgets::{self, Tone};
use super::{
    AnalyzerApp, StatsSnapshot, CAPTURE_AUTO_LABEL, CAPTURE_VIRTUAL_LABEL, CAPTURE_VIRTUAL_SENTINEL,
};
use crate::i18n::{self, tr, trf, Lang};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Language,
    Startup,
    Connect,
    Show,
    Pair,
}

const STEPS: [Step; 5] = [
    Step::Language,
    Step::Startup,
    Step::Connect,
    Step::Show,
    Step::Pair,
];

/// How the iPad will reach this PC. Only USB needs anything more: Apple
/// Devices.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Link {
    WiFi,
    Usb,
    Both,
}

#[derive(Default)]
pub(super) struct Setup {
    step: usize,
    link: Option<Link>,
    /// Whether Apple Devices is installed, checked once USB is chosen.
    apple_devices: Option<bool>,
}

/// The dialog's content width. With its margins it fits the window at its
/// minimum size, 820 × 560.
const WIDTH: f32 = 560.0;
const BODY_HEIGHT: f32 = 286.0;

impl AnalyzerApp {
    pub(super) fn open_setup(&mut self) {
        self.setup = Some(Setup::default());
    }

    pub(super) fn draw_setup(&mut self, ctx: &egui::Context, snap: &StatsSnapshot) {
        let Some(mut setup) = self.setup.take() else {
            return;
        };
        let step = STEPS[setup.step.min(STEPS.len() - 1)];
        let mut action = None;
        egui::Modal::new(egui::Id::new("setup"))
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
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 6.0;
                let (title, subtitle) = heading(step);
                widgets::overline_label(
                    ui,
                    &trf(
                        "Step {step} of {total}",
                        &[
                            ("step", &(setup.step + 1).to_string()),
                            ("total", &STEPS.len().to_string()),
                        ],
                    ),
                );
                let response = ui.label(
                    RichText::new(title)
                        .family(semibold())
                        .size(20.0)
                        .color(TEXT),
                );
                widgets::name_label(&response, title);
                widgets::wrapped(ui, subtitle, muted);
                ui.add_space(14.0);
                let top = ui.cursor().top();
                match step {
                    Step::Language => self.language_step(ui),
                    Step::Startup => self.startup_step(ui),
                    Step::Connect => self.connect_step(ui, &mut setup, snap),
                    Step::Show => self.show_step(ui),
                    Step::Pair => self.pair_step(ui, snap),
                }
                // Every step is as tall, so the buttons stay put.
                let used = ui.cursor().top() - top;
                ui.add_space((BODY_HEIGHT - used).max(0.0));
                ui.add_space(14.0);
                action = footer(ui, setup.step);
            });
        match action {
            Some(Footer::Skip) => {
                tracing::info!(step = ?step, "Setup skipped");
                return;
            }
            Some(Footer::Back) => setup.step = setup.step.saturating_sub(1),
            Some(Footer::Next) if setup.step + 1 < STEPS.len() => {
                setup.step += 1;
                if STEPS[setup.step] == Step::Pair {
                    super::refresh_listen_addr(snap);
                }
            }
            Some(Footer::Next) => {
                tracing::info!("Setup finished");
                return;
            }
            None => {}
        }
        self.setup = Some(setup);
    }

    fn language_step(&mut self, ui: &mut Ui) {
        let chosen = self.language_choice();
        let options: Vec<Option<Lang>> = std::iter::once(None).chain(Lang::ALL.map(Some)).collect();
        let gap = 8.0;
        let size = vec2((WIDTH - 2.0 * gap) / 3.0, 38.0);
        let mut choice = None;
        for row in options.chunks(3) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for option in row {
                    let (text, name) = match option {
                        None => (RichText::new(tr("System language")), "System language"),
                        Some(lang) => (
                            RichText::new(lang.native_name()).family(name_family(*lang)),
                            lang.native_name(),
                        ),
                    };
                    if widgets::choice_tile(ui, *option == chosen, text, name, size).clicked() {
                        choice = Some(option.map_or(i18n::SYSTEM, Lang::code));
                    }
                }
            });
            ui.add_space(gap - 6.0);
        }
        if let Some(choice) = choice {
            self.set_language(&ui.ctx().clone(), choice);
        }
    }

    fn startup_step(&mut self, ui: &mut Ui) {
        let mut boot = self.settings_start_on_boot;
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::setting_row(
                ui,
                tr("Start with Windows"),
                tr("Open EternalMonitor when you sign in to Windows, so your iPad can connect right away."),
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut boot, "Start on Windows startup");
                },
            );
        });
        if boot != self.settings_start_on_boot {
            self.set_start_on_boot(boot);
        }
    }

    fn connect_step(&mut self, ui: &mut Ui, setup: &mut Setup, snap: &StatsSnapshot) {
        let size = vec2(WIDTH, 58.0);
        for (link, title, description) in [
            (
                Link::WiFi,
                "Wi-Fi",
                tr("Your iPad and this PC are on the same local network."),
            ),
            (
                Link::Usb,
                "USB",
                tr("A cable between your iPad and this PC."),
            ),
            (
                Link::Both,
                tr("Both"),
                tr("Wi-Fi, and USB whenever the cable is plugged in."),
            ),
        ] {
            if widgets::choice_card(ui, setup.link == Some(link), title, description, size)
                .clicked()
            {
                setup.link = Some(link);
            }
        }
        if !matches!(setup.link, Some(Link::Usb | Link::Both)) {
            return;
        }
        // Apple's service answers only while Apple Devices runs; otherwise
        // ask Windows whether the app is there.
        let installed = *setup.apple_devices.get_or_insert_with(|| {
            snap.usb_service_reachable
                || crate::transport::apple_usb::probe().apple_devices_installed
        });
        ui.add_space(4.0);
        if installed {
            widgets::wrapped(
                ui,
                tr("Apple Devices is installed. Keep it open while you stream over USB."),
                muted,
            );
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.vertical(|ui| {
                ui.set_width(WIDTH - 220.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                widgets::wrapped(ui, tr("Apple Devices is not installed"), |text| {
                    RichText::new(text)
                        .family(medium())
                        .size(BODY)
                        .color(mix(WARNING, TEXT, 35))
                });
                widgets::wrapped(ui, tr("USB needs Apple's free Apple Devices app."), muted);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(
                    ui,
                    Tone::Secondary,
                    Some(Icon::External),
                    tr("Get Apple Devices"),
                )
                .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(
                        crate::transport::apple_usb::APPLE_DEVICES_STORE_URL,
                    ));
                }
            });
        });
    }

    fn show_step(&mut self, ui: &mut Ui) {
        let second = self.settings_capture_display == CAPTURE_VIRTUAL_SENTINEL;
        let gap = 10.0;
        let size = vec2((WIDTH - gap) / 2.0, 96.0);
        let mut choice = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (option, title, description) in [
                (
                    false,
                    tr(CAPTURE_AUTO_LABEL),
                    tr("The iPad shows the same picture as your main screen."),
                ),
                (
                    true,
                    tr(CAPTURE_VIRTUAL_LABEL),
                    tr("The iPad is an extra screen next to your main one."),
                ),
            ] {
                if widgets::choice_card(ui, second == option, title, description, size).clicked() {
                    choice = Some(option);
                }
            }
        });
        if let Some(second) = choice {
            let running = crate::stats::PIPELINE_STATS.lock().pipeline_running;
            self.set_second_screen(second, running);
        }
        ui.add_space(10.0);
        let enabled = self
            .control
            .shared
            .audio_enabled
            .load(std::sync::atomic::Ordering::SeqCst);
        let mut audio = enabled;
        widgets::list_card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::setting_row(
                ui,
                tr("Play PC audio"),
                tr("Hear this PC's sound on the iPad."),
                64.0,
                |ui| {
                    widgets::toggle(ui, &mut audio, "Play PC audio");
                },
            );
        });
        if audio != enabled {
            self.set_stream_audio(audio);
        }
    }

    fn pair_step(&mut self, ui: &mut Ui, snap: &StatsSnapshot) {
        let address = self.qr_address(snap);
        let (code, required) = {
            let pairing = self.control.shared.pairing.lock();
            (format!("{:06}", pairing.code()), pairing.required)
        };
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            self.qr_tile(ui, &address, 196.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(4.0);
                let open = tr("Open EternalMonitor on your iPad");
                widgets::wrapped(ui, open, strong);
                widgets::wrapped(
                    ui,
                    tr("Then tap this PC, or tap Scan QR code and point the camera at this code."),
                    muted,
                );
                ui.add_space(10.0);
                widgets::overline_label(ui, tr("Pairing code"));
                if required {
                    ui.label(mono(format!("{} {}", &code[..3], &code[3..]), 19.0));
                } else {
                    ui.label(muted(tr("Not needed while pairing is off.")));
                }
                ui.add_space(6.0);
                widgets::overline_label(ui, tr("Address"));
                ui.add(egui::Label::new(mono(address.clone(), 15.0)).truncate());
            });
        });
    }
}

/// Each step's title and the line under it.
fn heading(step: Step) -> (&'static str, &'static str) {
    match step {
        Step::Language => (
            tr("Welcome to EternalMonitor"),
            tr("Choose your language. You can change it later in Settings."),
        ),
        Step::Startup => (
            tr("Starting EternalMonitor"),
            tr("EternalMonitor runs in the background so your iPad can find this PC."),
        ),
        Step::Connect => (
            tr("How will your iPad connect?"),
            tr("Both work at any time. This only checks what USB needs."),
        ),
        Step::Show => (
            tr("What should the iPad show?"),
            tr("You can switch at any time on the Stream page."),
        ),
        Step::Pair => (
            tr("Connect your iPad"),
            tr("Pair it once with the QR code or the pairing code."),
        ),
    }
}

enum Footer {
    Skip,
    Back,
    Next,
}

fn footer(ui: &mut Ui, step: usize) -> Option<Footer> {
    let last = step + 1 == STEPS.len();
    let mut action = None;
    ui.horizontal(|ui| {
        if !last && widgets::button(ui, Tone::Ghost, None, tr("Skip setup")).clicked() {
            action = Some(Footer::Skip);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let next = if last { tr("Done") } else { tr("Next") };
            if widgets::button_ex(ui, Tone::Primary, None, next, true, 96.0).clicked() {
                action = Some(Footer::Next);
            }
            if step > 0 && widgets::button(ui, Tone::Secondary, None, tr("Back")).clicked() {
                action = Some(Footer::Back);
            }
        });
    });
    action
}
