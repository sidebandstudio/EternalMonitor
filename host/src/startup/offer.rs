//! A failure message offers a newer version when there is one: "Download it
//! now?" opens the download page. The check starts as soon as a failure is
//! known, beside the cleanup, and the message waits for it at most
//! [`FAILURE_CHECK`] after it started, then offers the saved answer. One
//! check per process, whatever the number of messages.

#[cfg(windows)]
use std::sync::{Condvar, Mutex, OnceLock};
#[cfg(windows)]
use std::time::Instant;

#[cfg(windows)]
use crate::updates::FAILURE_CHECK;

#[cfg(windows)]
struct Check {
    /// One deadline for everything: the saved state, the name lookup and
    /// the website's answer.
    deadline: Instant,
    answer: Mutex<Answer>,
    finished: Condvar,
}

#[cfg(windows)]
#[derive(Default)]
struct Answer {
    /// The version to offer as known so far: the saved release's once it is
    /// read, then the website's.
    version: Option<String>,
    /// The website answered, or the check gave up on it.
    done: bool,
}

#[cfg(windows)]
static CHECK: OnceLock<Check> = OnceLock::new();
/// In the failure reporter: the launch whose failure it reports, for the
/// upload event.
#[cfg(windows)]
static REPORTING_FOR: OnceLock<String> = OnceLock::new();

/// The failure reporter reports for this launch.
#[cfg(windows)]
pub(super) fn reporting_for(launch_id: &str) {
    let _ = REPORTING_FOR.set(launch_id.to_string());
}

/// Other platforms show no failure message.
#[cfg(not(windows))]
pub(super) fn begin() {}

/// Starts the check unless it started already, or no message will show.
#[cfg(windows)]
pub(super) fn begin() {
    if !super::dialogs_wanted() {
        return;
    }
    let mut first = false;
    let check = CHECK.get_or_init(|| {
        first = true;
        Check {
            deadline: Instant::now() + FAILURE_CHECK,
            answer: Mutex::new(Answer::default()),
            finished: Condvar::new(),
        }
    });
    if !first {
        return;
    }
    let deadline = check.deadline;
    let spawned = std::thread::Builder::new()
        .name("failure-offer".into())
        .spawn(move || {
            let lookup = crate::updates::failure_lookup();
            note(lookup.saved_offer().version, false);
            let offer = lookup.finish(deadline);
            // The message first: a slow disk must not hold it past the deadline.
            let line = offer.describe();
            note(offer.version, true);
            record(&line);
        });
    if spawned.is_err() {
        note(None, true);
        record("Update offer on the failure message: not offered source=none fetch=failed");
    }
}

#[cfg(windows)]
fn note(version: Option<String>, done: bool) {
    if let Some(check) = CHECK.get() {
        if let Ok(mut answer) = check.answer.lock() {
            *answer = Answer { version, done };
        }
        check.finished.notify_all();
    }
}

/// The version to offer: waits for the check, starting it if nothing did,
/// until its deadline, then takes the saved answer.
#[cfg(windows)]
pub(super) fn wait() -> Option<String> {
    begin();
    let check = CHECK.get()?;
    let mut answer = check.answer.lock().ok()?;
    while !answer.done {
        let left = check.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        answer = check.finished.wait_timeout(answer, left).ok()?.0;
    }
    answer.version.clone()
}

/// The line goes to the session log and the upload outbox. It names
/// versions only.
#[cfg(windows)]
fn record(line: &str) {
    let _ = crate::logging::append_now(&super::timestamped(" INFO", line));
    let _ = match REPORTING_FOR.get() {
        Some(launch) if launch == "-" => Ok(()),
        Some(launch) => crate::telemetry::queue_for_launch(launch, 2, line, "info"),
        None => crate::telemetry::queue_now(line, "info"),
    };
}

/// The message's last paragraph when a newer version is available.
#[cfg(any(windows, test))]
fn question(version: &str) -> String {
    format!("A newer version of EternalMonitor ({version}) is available. Download it now?")
}

/// The message with the offer added, when there is one.
#[cfg(any(windows, test))]
fn body_with_offer(body: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("{body}\n\n{}", question(version)),
        None => body.to_string(),
    }
}

/// A failure message box. With a newer version it asks Yes or No, and Yes
/// opens the download page. `style` adds the caller's flags.
#[cfg(windows)]
pub(super) fn show_box(
    title: &str,
    body: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONERROR, MB_OK, MB_YESNO,
    };
    let version = wait();
    let buttons = if version.is_some() { MB_YESNO } else { MB_OK };
    let text = body_with_offer(body, version.as_deref());
    let answer = unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            buttons | MB_ICONERROR | style,
        )
    };
    if version.is_some() && answer == IDYES {
        open_download_page();
    }
}

#[cfg(windows)]
fn open_download_page() {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let opened = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(crate::updates::WEBSITE_DOWNLOAD),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    // Above 32 means it started.
    let line = if opened.0 as usize > 32 {
        "Download page opened from the failure message"
    } else {
        "Could not open the download page from the failure message"
    };
    let _ = crate::logging::append_now(&super::timestamped(" INFO", line));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offer_is_the_message_s_last_question() {
        let body = "Windows error 0xC0000005 in atio6axx.dll\n\nThe details are saved.";
        assert_eq!(body_with_offer(body, None), body);
        let offered = body_with_offer(body, Some("0.3.5"));
        assert!(offered.starts_with(body), "{offered}");
        assert!(
            offered.ends_with(
                "\n\nA newer version of EternalMonitor (0.3.5) is available. Download it now?"
            ),
            "{offered}"
        );
    }
}
