//! The Stream page notice while another program holds the stream's port, and
//! what the status card says meanwhile.

use eframe::egui::Ui;

use super::icons::Icon;
use super::theme::WARNING;
use super::widgets::{self, GAP};
use super::AnalyzerApp;
use crate::startup::port::Holder;

impl AnalyzerApp {
    /// Goes away by itself once the port is free.
    pub(super) fn port_notice(&mut self, ui: &mut Ui) {
        let Some(holder) = crate::startup::port::holder() else {
            return;
        };
        let (title, body) = port_notice_text(&holder, crate::startup::port::starts_when_free());
        widgets::banner(ui, WARNING, Icon::Warning, &title, &body, 0.0, |_| {});
        ui.add_space(GAP - 10.0);
    }
}

/// What the notice says, naming the program when Windows does. `starts` is
/// whether the stream starts by itself once the port is free; after the user
/// clicked Stop, it says to click Start instead.
pub(super) fn port_notice_text(holder: &Holder, starts: bool) -> (String, String) {
    let port = holder.port;
    let then = if starts {
        "and EternalMonitor starts streaming by itself."
    } else {
        "then click Start streaming."
    };
    if holder.eternal_monitor {
        return (
            "Another copy of EternalMonitor is running".into(),
            format!("Only one copy can stream at a time, so iPads can't connect to this one. Quit the other copy (look for EternalMonitor in Task Manager if you can't see it), {then}"),
        );
    }
    match &holder.program {
        Some(program) => (
            format!("{program} is using EternalMonitor's network port"),
            format!("iPads can't connect while {program} uses port {port}. Quit {program}, {then}"),
        ),
        None => (
            "Another program is using EternalMonitor's network port".into(),
            format!(
                "iPads can't connect while it uses port {port}. Restart the PC to free it, {then}"
            ),
        ),
    }
}

/// The status card's title and subtitle while the stream is off only until
/// a held port is free (`starts`), instead of "Click Start streaming".
pub(super) fn waiting_status(off: bool, starts: bool) -> Option<(String, String)> {
    (off && starts).then(|| {
        (
            "Waiting for the stream port…".to_string(),
            "EternalMonitor starts by itself when it's free.".to_string(),
        )
    })
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
        let (title, body) = port_notice_text(&holder(Some("VLC media player"), false), true);
        assert_eq!(
            title,
            "VLC media player is using EternalMonitor's network port"
        );
        assert!(body.contains("port 9876"), "{body}");
        assert!(
            body.ends_with("Quit VLC media player, and EternalMonitor starts streaming by itself."),
            "{body}"
        );

        let (title, body) = port_notice_text(&holder(Some("EternalMonitor"), true), true);
        assert_eq!(title, "Another copy of EternalMonitor is running");
        assert!(body.contains("Quit the other copy"), "{body}");

        let (title, body) = port_notice_text(&holder(None, false), true);
        assert!(title.starts_with("Another program"), "{title}");
        assert!(body.contains("Restart the PC"), "{body}");
    }

    /// After Stop the stream does not start by itself, so nothing says it
    /// will.
    #[test]
    fn after_stop_the_notice_says_to_click_start() {
        for holder in [
            holder(Some("VLC media player"), false),
            holder(Some("EternalMonitor"), true),
            holder(None, false),
        ] {
            let (_, body) = port_notice_text(&holder, false);
            assert!(body.ends_with("then click Start streaming."), "{body}");
            assert!(!body.contains("by itself"), "{body}");
        }
    }

    #[test]
    fn the_status_card_waits_for_the_port_instead_of_asking_for_start() {
        let (title, subtitle) = waiting_status(true, true).expect("waiting");
        assert_eq!(title, "Waiting for the stream port…");
        assert_eq!(subtitle, "EternalMonitor starts by itself when it's free.");
        assert_eq!(waiting_status(true, false), None, "after Stop: Click Start");
        assert_eq!(
            waiting_status(false, true),
            None,
            "streaming: its own status"
        );
    }
}
