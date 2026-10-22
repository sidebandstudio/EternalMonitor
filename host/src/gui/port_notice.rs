//! The Stream page notice while another program holds the stream's port.

use eframe::egui::Ui;

use super::icons::Icon;
use super::theme::WARNING;
use super::widgets::{self, GAP};
use super::AnalyzerApp;
use crate::startup::port::Holder;

impl AnalyzerApp {
    /// Goes away by itself once the port is free and the stream starts.
    pub(super) fn port_notice(&mut self, ui: &mut Ui) {
        let Some(holder) = crate::startup::port::holder() else {
            return;
        };
        let (title, body) = port_notice_text(&holder);
        widgets::banner(ui, WARNING, Icon::Warning, &title, &body, 0.0, |_| {});
        ui.add_space(GAP - 10.0);
    }
}

/// What the notice says, naming the program when Windows does.
pub(super) fn port_notice_text(holder: &Holder) -> (String, String) {
    let port = holder.port;
    if holder.eternal_monitor {
        return (
            "Another copy of EternalMonitor is running".into(),
            "Only one copy can stream at a time, so iPads can't connect to this one. Quit the other copy (look for EternalMonitor in Task Manager if you can't see it), and this one starts streaming by itself.".into(),
        );
    }
    match &holder.program {
        Some(program) => (
            format!("{program} is using EternalMonitor's network port"),
            format!("iPads can't connect while {program} uses port {port}. Quit {program}, and EternalMonitor starts streaming by itself."),
        ),
        None => (
            "Another program is using EternalMonitor's network port".into(),
            format!("iPads can't connect while it uses port {port}. Restart the PC to free it, and EternalMonitor starts streaming by itself."),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(program: Option<&str>, eternal_monitor: bool) -> Holder {
        Holder {
            port: 9876,
            program: program.map(str::to_string),
            eternal_monitor,
        }
    }

    #[test]
    fn the_port_notice_names_the_program_and_says_what_to_do() {
        let (title, body) = port_notice_text(&holder(Some("VLC media player"), false));
        assert_eq!(
            title,
            "VLC media player is using EternalMonitor's network port"
        );
        assert!(body.contains("port 9876"), "{body}");
        assert!(body.contains("Quit VLC media player"), "{body}");

        let (title, body) = port_notice_text(&holder(Some("EternalMonitor"), true));
        assert_eq!(title, "Another copy of EternalMonitor is running");
        assert!(body.contains("Quit the other copy"), "{body}");

        let (title, body) = port_notice_text(&holder(None, false));
        assert!(title.starts_with("Another program"), "{title}");
        assert!(body.contains("Restart the PC"), "{body}");
    }
}
