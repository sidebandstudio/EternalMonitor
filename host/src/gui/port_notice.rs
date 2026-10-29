//! The Stream page notice while another program holds the stream's port, and
//! what the status card says meanwhile.

use eframe::egui::Ui;

use super::icons::Icon;
use super::theme::WARNING;
use super::widgets::{self, GAP};
use super::AnalyzerApp;
use crate::startup::port::{Held, HeldBy};

impl AnalyzerApp {
    /// Goes away by itself once the port is free.
    pub(super) fn port_notice(&mut self, ui: &mut Ui) {
        let Some((title, body)) =
            crate::startup::port::held().and_then(|held| port_notice_text(&held))
        else {
            return;
        };
        widgets::banner(ui, WARNING, Icon::Warning, &title, &body, 0.0, |_| {});
        ui.add_space(GAP - 10.0);
    }
}

/// What the notice says, naming the program when Windows does. None while
/// this copy's own previous stream still holds the port: it lets go by
/// itself, and the status card says the stream is waiting.
pub(super) fn port_notice_text(held: &Held) -> Option<(String, String)> {
    let port = held.port;
    let then = "EternalMonitor starts streaming as soon as the port is free.";
    Some(match &held.by {
        HeldBy::OtherCopy => (
            "Another copy of EternalMonitor is running".into(),
            "Only one copy can stream at a time, so iPads can't connect to this one. Quit the other copy (look for EternalMonitor in Task Manager if you can't see it), and this one starts streaming by itself.".into(),
        ),
        HeldBy::Program(program) => (
            format!("{program} is using EternalMonitor's network port"),
            format!("iPads can't connect while {program} uses port {port}. Quit {program}. {then}"),
        ),
        HeldBy::Unknown => (
            "Another program is using EternalMonitor's network port".into(),
            format!("iPads can't connect while another program uses port {port}. Quit that program or restart the PC. {then}"),
        ),
        HeldBy::ThisCopy => return None,
    })
}

/// The status card's title and subtitle while the stream is off only until
/// the port is free, instead of "Click Start streaming".
pub(super) fn waiting_status(off: bool, waiting: bool) -> Option<(String, String)> {
    (off && waiting).then(|| {
        (
            "Waiting for the stream port…".to_string(),
            "EternalMonitor starts by itself when it's free.".to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(by: HeldBy) -> Held {
        Held { port: 9876, by }
    }

    #[test]
    fn the_port_notice_names_the_program_and_says_what_to_do() {
        let (title, body) =
            port_notice_text(&held(HeldBy::Program("VLC media player".into()))).unwrap();
        assert_eq!(
            title,
            "VLC media player is using EternalMonitor's network port"
        );
        assert_eq!(
            body,
            "iPads can't connect while VLC media player uses port 9876. Quit VLC media player. EternalMonitor starts streaming as soon as the port is free."
        );
    }

    /// Another copy is named as such, not by its file or as some program.
    #[test]
    fn another_copy_is_named_plainly() {
        let (title, body) = port_notice_text(&held(HeldBy::OtherCopy)).unwrap();
        assert_eq!(title, "Another copy of EternalMonitor is running");
        assert!(body.contains("Quit the other copy"), "{body}");
        assert!(!body.contains("port"), "{body}");
    }

    #[test]
    fn an_unknown_holder_gets_a_generic_notice() {
        let (title, body) = port_notice_text(&held(HeldBy::Unknown)).unwrap();
        assert_eq!(
            title,
            "Another program is using EternalMonitor's network port"
        );
        assert!(body.contains("port 9876"), "{body}");
        assert!(body.contains("restart the PC"), "{body}");
    }

    #[test]
    fn this_copy_s_previous_stream_has_no_notice() {
        assert_eq!(port_notice_text(&held(HeldBy::ThisCopy)), None);
    }

    #[test]
    fn the_status_card_waits_for_the_port_instead_of_asking_for_start() {
        let (title, subtitle) = waiting_status(true, true).expect("waiting");
        assert_eq!(title, "Waiting for the stream port…");
        assert_eq!(subtitle, "EternalMonitor starts by itself when it's free.");
        assert_eq!(waiting_status(true, false), None, "stopped: Click Start");
        assert_eq!(
            waiting_status(false, true),
            None,
            "streaming: its own status"
        );
    }
}
