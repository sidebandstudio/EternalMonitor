//! Upload-side summaries of the periodic stats lines. A streaming host logs a
//! `Receiver report` twice a second, `Audio stream stats` every second and a
//! `Support stream summary` every 10 s. Uploaded as they are, they were most
//! of the backend's rows and pushed everything else out of its retention.
//! The upload gets one `Periodic stats summary` per session per [`WINDOW`]
//! instead; the session log and "Copy logs" keep every line.

use std::time::{Duration, Instant};

/// Longest a summary covers.
pub const WINDOW: Duration = Duration::from_secs(60);
/// The line that starts a session. The window before it is closed first, so
/// one summary never spans two sessions.
const SESSION_START: &str = "Client session established";

#[derive(Clone, Copy)]
enum Field {
    /// Summarized as `min/avg/max`.
    Gauge,
    /// Cumulative; summarized as its last value and the increase in this
    /// window, `last(+increase)`.
    Counter,
    /// The last value, as logged.
    Last,
}
use Field::{Counter, Gauge, Last};

struct Kind {
    message: &'static str,
    prefix: &'static str,
    fields: &'static [(&'static str, Field)],
}

/// The audio device name is left out: it can carry the owner's name.
const KINDS: [Kind; 3] = [
    Kind {
        message: "Receiver report",
        prefix: "rr",
        fields: &[
            ("epoch", Last),
            ("complete", Counter),
            ("dropped", Counter),
            ("repaired", Counter),
            ("nacks", Counter),
            ("jitter_us", Gauge),
            ("decode_fps_x10", Gauge),
            ("decode_depth", Gauge),
            ("retransmits", Counter),
        ],
    },
    Kind {
        message: "Audio stream stats",
        prefix: "audio",
        fields: &[
            ("kbps", Gauge),
            ("packets_per_sec", Gauge),
            ("quiet_packets", Counter),
            ("dropped", Counter),
            ("capture_queue_dropped", Counter),
            ("stale_dropped", Counter),
            ("oversized_dropped", Counter),
            ("send_failed", Counter),
            ("late_seqs", Last),
            ("failed_seqs", Last),
            ("audio_stream", Last),
            ("wire_stream", Last),
        ],
    },
    Kind {
        message: "Support stream summary",
        prefix: "stream",
        fields: &[
            ("running", Last),
            ("captured", Counter),
            ("encoded", Counter),
            ("sent", Counter),
            ("capture_fps", Gauge),
            ("encode_fps", Gauge),
            ("send_fps", Gauge),
            ("encode_us", Gauge),
            ("codec", Last),
            ("cpu_fallback", Last),
            ("resolution", Last),
            ("bitrate", Gauge),
            ("target_fps", Last),
            ("max_dgram", Last),
            ("input", Last),
            ("hevc", Last),
            ("retransmits", Counter),
            ("usb_drops", Counter),
            ("usb_state", Last),
            ("usb_devices", Last),
            ("capture_heartbeat_age_ms", Gauge),
            ("encode_heartbeat_age_ms", Gauge),
        ],
    },
];

#[derive(Default)]
struct Value {
    min: f64,
    max: f64,
    sum: f64,
    samples: u32,
    /// Counter increase in this window, and the last text seen.
    increase: f64,
    last: String,
}

#[derive(Default)]
struct KindState {
    lines: u32,
    values: Vec<Value>,
    /// Each counter's last value, kept across windows so the first line of
    /// a window still counts what happened since the previous one.
    previous: Vec<Option<f64>>,
}

#[derive(Default)]
pub struct Periodic {
    started: Option<Instant>,
    first_stamp: String,
    last_stamp: String,
    session: Option<String>,
    kinds: [KindState; 3],
}

/// `name=value` pairs after the message. A value runs to the next ` name=`,
/// so `codec=H.264 (NVENC)` and `resolution=(2560, 1600)` stay whole.
fn fields(text: &str) -> Vec<(&str, &str)> {
    static NAME: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r" ([a-z][a-z0-9_]*)=").expect("field pattern")
    });
    let names: Vec<_> = NAME.captures_iter(text).collect();
    names
        .iter()
        .enumerate()
        .map(|(index, captures)| {
            let whole = captures.get(0).expect("match");
            let end = names
                .get(index + 1)
                .map_or(text.len(), |next| next.get(0).expect("match").start());
            (
                captures.get(1).expect("name").as_str(),
                &text[whole.end()..end],
            )
        })
        .collect()
}

/// `Some(7)` and `7` are the same session; `None` is no session.
fn session_value(value: &str) -> Option<String> {
    let value = value
        .strip_prefix("Some(")
        .map_or(value, |v| v.trim_end_matches(')'));
    (value != "None" && !value.is_empty()).then(|| value.to_owned())
}

fn number(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

impl Periodic {
    /// The lines to upload for `line`, in order: the summary of a window it
    /// closes, then `line` itself unless it is summarized.
    pub fn push(&mut self, line: &str, now: Instant) -> Vec<String> {
        let mut out = self.flush_due(now);
        let Some((stamp, rest)) = crate::log_repeat::split_stamp(line) else {
            out.push(line.to_owned());
            return out;
        };
        let Some(message) = rest.trim_start().strip_prefix("INFO ") else {
            out.push(line.to_owned());
            return out;
        };
        if message.starts_with(SESSION_START) {
            out.extend(self.flush());
            self.session = fields(message)
                .into_iter()
                .find_map(|(name, value)| (name == "session_id").then(|| session_value(value)))
                .flatten();
            out.push(line.to_owned());
            return out;
        }
        let Some((index, text)) = KINDS.iter().enumerate().find_map(|(index, kind)| {
            let text = message.strip_prefix(kind.message)?;
            (text.is_empty() || text.starts_with(' ')).then_some((index, text))
        }) else {
            out.push(line.to_owned());
            return out;
        };
        let fields = fields(text);
        if let Some((_, value)) = fields.iter().find(|(name, _)| *name == "session_id") {
            let session = session_value(value);
            if session.is_some() && self.session.is_some() && session != self.session {
                out.extend(self.flush());
            }
            if session.is_some() {
                self.session = session;
            }
        }
        if self.started.is_none() {
            self.started = Some(now);
            self.first_stamp = stamp.to_owned();
        }
        self.last_stamp = stamp.to_owned();
        let kind = &KINDS[index];
        let state = &mut self.kinds[index];
        if state.values.is_empty() {
            state.values = kind.fields.iter().map(|_| Value::default()).collect();
        }
        state.previous.resize(kind.fields.len(), None);
        state.lines += 1;
        for (slot, (name, field)) in kind.fields.iter().enumerate() {
            let Some((_, text)) = fields.iter().find(|(found, _)| found == name) else {
                continue;
            };
            let value = &mut state.values[slot];
            value.last = (*text).to_owned();
            let Ok(number) = text.parse::<f64>() else {
                continue;
            };
            match field {
                Gauge => {
                    if value.samples == 0 {
                        (value.min, value.max) = (number, number);
                    }
                    value.min = value.min.min(number);
                    value.max = value.max.max(number);
                    value.sum += number;
                    value.samples += 1;
                }
                // A smaller value is a counter that started again from 0.
                Counter => {
                    value.increase += match state.previous[slot] {
                        Some(previous) if number >= previous => number - previous,
                        Some(_) => number,
                        None => 0.0,
                    };
                    state.previous[slot] = Some(number);
                }
                Last => {}
            }
        }
        out
    }

    /// The summary of a window that has run for [`WINDOW`], if any.
    pub fn flush_due(&mut self, now: Instant) -> Vec<String> {
        match self.started {
            Some(started) if now.duration_since(started) >= WINDOW => self.flush_at(now),
            _ => Vec::new(),
        }
    }

    /// The summary of the open window, for a session start and shutdown.
    pub fn flush(&mut self) -> Vec<String> {
        self.flush_at(Instant::now())
    }

    fn flush_at(&mut self, now: Instant) -> Vec<String> {
        let Some(started) = self.started.take() else {
            return Vec::new();
        };
        let mut line = format!(
            "{}  INFO Periodic stats summary window_s={} from={}",
            self.last_stamp,
            now.duration_since(started).as_secs().min(WINDOW.as_secs()),
            self.first_stamp,
        );
        line.push_str(&format!(
            " session_id={}",
            self.session.as_deref().unwrap_or("None")
        ));
        for (kind, state) in KINDS.iter().zip(&mut self.kinds) {
            line.push_str(&format!(" {}_lines={}", kind.prefix, state.lines));
            for ((name, field), value) in kind.fields.iter().zip(&state.values) {
                if value.last.is_empty() {
                    continue;
                }
                let key = if name.starts_with(kind.prefix) {
                    (*name).to_owned()
                } else {
                    format!("{}_{name}", kind.prefix)
                };
                let text = match field {
                    Gauge if value.samples > 0 => format!(
                        "{}/{}/{}",
                        number(value.min),
                        number(value.sum / f64::from(value.samples)),
                        number(value.max)
                    ),
                    Counter if value.last.parse::<f64>().is_ok() => {
                        format!("{}(+{})", value.last, number(value.increase))
                    }
                    _ => value.last.clone(),
                };
                line.push_str(&format!(" {key}={text}"));
            }
            state.lines = 0;
            state.values.clear();
        }
        vec![line]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(second: u64) -> String {
        format!(
            "2026-10-22T17:{:02}:{:02}.000000Z",
            second / 60 % 60,
            second % 60
        )
    }

    fn report(second: u64, complete: u32, dropped: u32, jitter: u32) -> String {
        format!(
            "{}  INFO Receiver report epoch=1 complete={complete} dropped={dropped} repaired=0 \
             nacks=0 jitter_us={jitter} decode_fps_x10=600 decode_depth=0 retransmits=0",
            stamp(second)
        )
    }

    fn summary(session: u32, second: u64) -> String {
        format!(
            "{}  INFO Support stream summary session_id=Some({session}) running=true \
             captured=100 encoded=100 sent=100 capture_fps=59.94 encode_fps=59.9 send_fps=59.9 \
             encode_us=2100 codec=H.264 (NVENC) cpu_fallback=false resolution=(2560, 1600) \
             bitrate=15000000 target_fps=60 max_dgram=1400 input=Yuv420 hevc=false \
             retransmits=0 usb_drops=0 usb_state=Waiting for an iPad usb_devices=0 \
             capture_heartbeat_age_ms=16 encode_heartbeat_age_ms=17",
            stamp(second)
        )
    }

    #[test]
    fn a_window_of_reports_becomes_one_line_with_min_avg_max() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        let mut out = Vec::new();
        for (i, (complete, dropped, jitter)) in [(10, 0, 100), (40, 2, 300), (70, 2, 200)]
            .into_iter()
            .enumerate()
        {
            out.extend(periodic.push(
                &report(i as u64, complete, dropped, jitter),
                start + Duration::from_secs(i as u64),
            ));
        }
        out.extend(periodic.push(&summary(7, 3), start + Duration::from_secs(3)));
        assert!(out.is_empty(), "{out:?}");
        let closed = periodic.flush_due(start + WINDOW);
        assert_eq!(closed.len(), 1);
        let line = &closed[0];
        assert!(line.starts_with(&format!(
            "{}  INFO Periodic stats summary window_s=60 from={} session_id=7 rr_lines=3",
            stamp(3),
            stamp(0)
        )));
        // The first report is the baseline: 60 frames and 2 drops since it.
        assert!(
            line.contains(" rr_complete=70(+60) rr_dropped=2(+2) "),
            "{line}"
        );
        assert!(line.contains(" rr_jitter_us=100/200/300 "), "{line}");
        assert!(line.contains(" audio_lines=0 stream_lines=1 "), "{line}");
        assert!(line.contains(" stream_codec=H.264 (NVENC) "), "{line}");
        assert!(
            line.contains(" stream_capture_fps=59.9/59.9/59.9 "),
            "{line}"
        );
        assert!(
            line.contains(" stream_usb_state=Waiting for an iPad "),
            "{line}"
        );
        assert!(periodic.flush().is_empty(), "the window was closed");
    }

    #[test]
    fn counters_carry_across_windows_and_survive_a_restart() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        periodic.push(&report(0, 100, 0, 1), start);
        periodic.flush();
        periodic.push(&report(1, 130, 0, 1), start + Duration::from_secs(1));
        // A new epoch counts from 0 again.
        periodic.push(&report(2, 5, 0, 1), start + Duration::from_secs(2));
        let line = periodic.flush().remove(0);
        assert!(line.contains(" rr_complete=5(+35) "), "{line}");
    }

    #[test]
    fn a_new_session_closes_the_window_first() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        periodic.push(&summary(1, 0), start);
        periodic.push(&report(1, 10, 0, 1), start);
        let established = format!("{}  INFO Client session established session_id=2", stamp(2));
        let out = periodic.push(&established, start + Duration::from_secs(2));
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("session_id=1 rr_lines=1"), "{}", out[0]);
        assert_eq!(out[1], established);
        // A summary for another session also closes the window.
        periodic.push(&summary(2, 3), start + Duration::from_secs(3));
        let out = periodic.push(&summary(3, 4), start + Duration::from_secs(4));
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("session_id=2 rr_lines=0"), "{}", out[0]);
    }

    #[test]
    fn other_lines_and_other_levels_pass_through() {
        let mut periodic = Periodic::default();
        let now = Instant::now();
        for line in [
            format!("{}  WARN Receiver report epoch=1", stamp(0)),
            format!("{}  INFO Receiver reports are late", stamp(0)),
            format!("{} ERROR Capture loop exited with error", stamp(0)),
            " continuation".to_owned(),
            String::new(),
        ] {
            assert_eq!(periodic.push(&line, now), vec![line]);
        }
        assert!(periodic.flush().is_empty());
    }

    /// One hour of a host streaming at the real rates, through the upload
    /// path. Before: every line. After: one line a minute.
    #[test]
    fn a_streaming_hour_uploads_sixty_summary_rows() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        let (mut logged, mut uploaded) = (0, 0);
        for tenth in 0..36_000u64 {
            let now = start + Duration::from_millis(tenth * 100);
            let second = tenth / 10;
            let mut lines = Vec::new();
            if tenth % 5 == 0 {
                lines.push(report(second, tenth as u32, 0, 200));
            }
            if tenth % 10 == 0 {
                lines.push(format!(
                    "{}  INFO Audio stream stats device=Speakers kbps=96.0 packets_per_sec=50.0 \
                     quiet_packets={tenth} dropped=0 capture_queue_dropped=0 stale_dropped=0 \
                     oversized_dropped=0 send_failed=0 late_seqs= failed_seqs= audio_stream=1 \
                     wire_stream=7/1",
                    stamp(second)
                ));
            }
            if tenth % 100 == 0 {
                lines.push(summary(7, second));
            }
            for line in lines {
                logged += 1;
                uploaded += periodic.push(&line, now).len();
            }
            uploaded += periodic.flush_due(now).len();
        }
        uploaded += periodic.flush().len();
        assert_eq!(logged, 7200 + 3600 + 360);
        assert_eq!(uploaded, 60);
    }
}
