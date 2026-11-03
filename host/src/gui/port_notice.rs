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
/// itself, and the status card says the stream is waiting. None too while
/// Windows has refused the port only a few times with no program on it.
pub(super) fn port_notice_text(held: &Held) -> Option<(String, String)> {
    let port = held.port;
    let then = "EternalMonitor starts streaming as soon as the port is free.";
    Some(match &held.by {
        HeldBy::OtherCopy => (
            "Another copy of EternalMonitor is running".into(),
            "Only one copy can stream at a time, so iPads can't connect to this one. Quit the other copy (look for EternalMonitor in Task Manager if you can't see it), and this one starts streaming by itself.".into(),
        ),
        HeldBy::Program(_) | HeldBy::Windows | HeldBy::WindowsService => {
            let program = name(&held.by)?;
            (
                format!("{program} is using EternalMonitor's network port"),
                format!("iPads can't connect while {program} uses port {port}. Quit {program}. {then}"),
            )
        }
        HeldBy::Unknown => (
            "Another program is using EternalMonitor's network port".into(),
            format!("iPads can't connect while another program uses port {port}. Quit that program or restart the PC. {then}"),
        ),
        HeldBy::Blocked { ipv6, .. } => {
            let mut body = "A firewall, security tool or reserved port range can do this.".to_string();
            if let Some(program) = ipv6.as_deref().and_then(name) {
                body += &format!(" {program} also uses this port over IPv6.");
            }
            (
                format!("Windows is blocking EternalMonitor from using port {port}"),
                body,
            )
        }
        // Checked again before anything is said.
        HeldBy::ThisCopy | HeldBy::Refused => return None,
    })
}

/// What the notice calls a program on the port, when Windows says.
fn name(by: &HeldBy) -> Option<&str> {
    Some(match by {
        HeldBy::Program(program) => program,
        HeldBy::Windows => "Windows",
        HeldBy::WindowsService => "A Windows service",
        HeldBy::OtherCopy => "Another copy of EternalMonitor",
        _ => return None,
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
        assert_eq!(port_notice_text(&held(HeldBy::Refused)), None);
    }

    /// Windows refuses the port and no program has it: no "another program"
    /// advice.
    #[test]
    fn a_blocked_port_says_windows_blocks_it() {
        let blocked = |ipv6: Option<HeldBy>| {
            port_notice_text(&held(HeldBy::Blocked {
                code: Some(10013),
                ipv6: ipv6.map(Box::new),
            }))
            .unwrap()
        };
        let (title, body) = blocked(None);
        assert_eq!(
            title,
            "Windows is blocking EternalMonitor from using port 9876"
        );
        assert_eq!(
            body,
            "A firewall, security tool or reserved port range can do this."
        );
        assert_eq!(blocked(Some(HeldBy::Unknown)).1, body, "nobody to name");

        // A program on the port over IPv6 only is named as a hint.
        let (title, body) = blocked(Some(HeldBy::Program("Steam".into())));
        assert_eq!(
            title,
            "Windows is blocking EternalMonitor from using port 9876"
        );
        assert_eq!(
            body,
            "A firewall, security tool or reserved port range can do this. Steam also uses this port over IPv6."
        );
    }

    /// Windows' own processes keep their plain names.
    #[test]
    fn windows_and_its_services_are_named_plainly() {
        let (title, _) = port_notice_text(&held(HeldBy::Windows)).unwrap();
        assert_eq!(title, "Windows is using EternalMonitor's network port");
        let (title, _) = port_notice_text(&held(HeldBy::WindowsService)).unwrap();
        assert_eq!(
            title,
            "A Windows service is using EternalMonitor's network port"
        );
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
