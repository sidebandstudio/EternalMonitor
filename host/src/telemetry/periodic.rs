//! Upload-side summaries of the periodic stats lines. A streaming host logs a
//! `Receiver report` twice a second, `Audio stream stats` every second and a
//! `Support stream summary` every 10 s. Uploaded as they are, they were most
//! of the backend's rows and pushed everything else out of its retention.
//! The upload gets one `Periodic stats summary` per session per [`WINDOW`]
//! instead; the session log and "Copy logs" keep every line.
//!
//! A window closes when it has run its length, when another session starts,
//! and when its session ends (its `session_summary` line, or a stream summary
//! without it), so the end of a stream is summarized at once. Quitting and
//! in-process fatal reports write the open window. A native crash skips
//! both; the session log keeps those lines.

use std::time::{Duration, Instant};

/// Longest a summary of a session covers.
pub const WINDOW: Duration = Duration::from_secs(60);
/// Longest a summary covers while no session runs and only the stream
/// summary is logged, every 10 s.
pub const IDLE_WINDOW: Duration = Duration::from_secs(600);
/// The line that starts a session. The window before it is closed first, so
/// one summary never spans two sessions.
const SESSION_START: &str = "Client session established";
/// Counted in the summary, and uploaded as they are.
const KEYFRAME_REQUEST: &str = "Keyframe request received";

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
            ("frags_received", Counter),
            ("frags_lost", Counter),
            ("rtt_ms_x10", Gauge),
            ("latency_ms_x10", Gauge),
            ("audio_lost", Counter),
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
    keyframe_requests: u32,
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

/// `Some(7)` and `7` are the same session; `None` and `-` are no session.
fn session_value(value: &str) -> Option<String> {
    let value = value
        .strip_prefix("Some(")
        .map_or(value, |v| v.trim_end_matches(')'));
    (!matches!(value, "None" | "-" | "")).then(|| value.to_owned())
}

fn session_field(text: &str) -> Option<String> {
    fields(text)
        .into_iter()
        .find_map(|(name, value)| (name == "session_id").then(|| session_value(value)))
        .flatten()
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
        // A session's summary ends its stream: its window goes first. Other
        // summaries (a refused HELLO) leave the window open.
        if let Some(summary) = super::session_summary(line) {
            if self.session.is_some() && session_field(summary) == self.session {
                out.extend(self.end_session(now));
            }
            out.push(line.to_owned());
            return out;
        }
        let Some((stamp, rest)) = crate::log_repeat::split_stamp(line) else {
            out.push(line.to_owned());
            return out;
        };
        let Some(message) = rest.trim_start().strip_prefix("INFO ") else {
            out.push(line.to_owned());
            return out;
        };
        if message.starts_with(SESSION_START) {
            out.extend(self.end_session(now));
            self.session = session_field(message);
            out.push(line.to_owned());
            return out;
        }
        if message.starts_with(KEYFRAME_REQUEST) {
            self.keyframe_requests += 1;
            out.push(line.to_owned());
            return out;
        }
        // A count of identical lines is news (a frozen stream): it uploads
        // as it is.
        let repeated = message.ends_with(']') && message.contains(" [repeated ");
        let Some((index, text)) = KINDS.iter().enumerate().find_map(|(index, kind)| {
            let text = message.strip_prefix(kind.message)?;
            (!repeated && (text.is_empty() || text.starts_with(' '))).then_some((index, text))
        }) else {
            out.push(line.to_owned());
            return out;
        };
        let fields = fields(text);
        if let Some((_, value)) = fields.iter().find(|(name, _)| *name == "session_id") {
            let session = session_value(value);
            // A disconnect (`None`) ends the session too.
            if session != self.session {
                out.extend(self.end_session(now));
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

    /// How long the open window runs: a minute with a session or its
    /// reports, longer while only the idle stream summary is logged.
    fn length(&self) -> Duration {
        let reports = self.kinds[0].lines + self.kinds[1].lines;
        if self.session.is_some() || reports > 0 {
            WINDOW
        } else {
            IDLE_WINDOW
        }
    }

    /// The summary of a window that has run its length, if any.
    pub fn flush_due(&mut self, now: Instant) -> Vec<String> {
        match self.started {
            Some(started) if now.duration_since(started) >= self.length() => self.flush_at(now),
            _ => Vec::new(),
        }
    }

    /// Closes the window and forgets the session and its counters, so the
    /// next session's first report is its own baseline.
    fn end_session(&mut self, now: Instant) -> Vec<String> {
        let out = self.flush_at(now);
        for kind in &mut self.kinds {
            kind.previous.clear();
        }
        self.session = None;
        out
    }

    /// Whether a window holds lines not yet summarized.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.started.is_some()
    }

    /// How many stats lines the open window holds.
    #[cfg(test)]
    pub fn lines(&self) -> u32 {
        self.kinds.iter().map(|kind| kind.lines).sum()
    }

    /// The summary of the open window, for a session's end and shutdown.
    pub fn flush(&mut self) -> Vec<String> {
        self.flush_at(Instant::now())
    }

    fn flush_at(&mut self, now: Instant) -> Vec<String> {
        let length = self.length();
        let requests = std::mem::take(&mut self.keyframe_requests);
        let Some(started) = self.started.take() else {
            return Vec::new();
        };
        let mut line = format!(
            "{}  INFO Periodic stats summary window_s={} from={} session_id={} \
             keyframe_requests={requests}",
            self.last_stamp,
            now.duration_since(started).as_secs().min(length.as_secs()),
            self.first_stamp,
            self.session.as_deref().unwrap_or("None"),
        );
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
            "2026-10-29T17:{:02}:{:02}.000000Z",
            second / 60 % 60,
            second % 60
        )
    }

    fn report(second: u64, complete: u32, dropped: u32, jitter: u32) -> String {
        format!(
            "{}  INFO Receiver report epoch=1 complete={complete} dropped={dropped} repaired=0 \
             nacks=0 jitter_us={jitter} decode_fps_x10=600 decode_depth=0 retransmits=0 \
             frags_received={} frags_lost={dropped} rtt_ms_x10={} latency_ms_x10=0 audio_lost=0",
            stamp(second),
            complete * 4,
            jitter / 10,
        )
    }

    fn summary(session: u32, second: u64) -> String {
        format!(
            "{}  INFO Support stream summary session_id=Some({session}) running=true \
             captured=100 encoded=98 sent=0 capture_fps=59.94 encode_fps=59.9 send_fps=59.9 \
             encode_us=2100 codec=H.264 (NVENC) cpu_fallback=false resolution=(2560, 1600) \
             bitrate=15000000 target_fps=60 max_dgram=1400 input=Yuv420 hevc=false \
             retransmits=0 usb_drops=0 usb_state=Waiting for an iPad usb_devices=0 \
             capture_heartbeat_age_ms=16 encode_heartbeat_age_ms=17",
            stamp(second)
        )
    }

    fn audio(second: u64, quiet: u64) -> String {
        format!(
            "{}  INFO Audio stream stats device=Speakers (Maria's AirPods) kbps=96.0 \
             packets_per_sec=50.0 quiet_packets={quiet} dropped=0 capture_queue_dropped=0 \
             stale_dropped=0 oversized_dropped=0 send_failed=0 late_seqs= failed_seqs= \
             audio_stream=1 wire_stream=7/1",
            stamp(second)
        )
    }

    fn session_summary_line(session: &str, second: u64) -> String {
        format!(
            "{}  INFO session_summary v=1 role=pc session_id={session} nonce=00000000e2e00001 \
             transport=wifi mode=mirror stage=steady outcome=user_ended failure=none \
             fallback=none end=goodbye ttff_ms=120 duration_ms=90000 os=win:26200.9457 hw=pc \
             gpu=NVIDIA hybrid=false",
            stamp(second)
        )
    }

    #[test]
    fn a_minute_of_reports_becomes_one_line_with_min_avg_max() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        let mut out = periodic.push(&summary(7, 0), start);
        for (i, (complete, dropped, jitter)) in [(10, 0, 100), (40, 2, 300), (70, 2, 200)]
            .into_iter()
            .enumerate()
        {
            let second = i as u64 + 1;
            out.extend(periodic.push(
                &report(second, complete, dropped, jitter),
                start + Duration::from_secs(second),
            ));
        }
        out.extend(periodic.push(&audio(4, 10), start + Duration::from_secs(4)));
        out.extend(periodic.push(&audio(5, 25), start + Duration::from_secs(5)));
        assert!(out.is_empty(), "{out:?}");
        assert!(periodic
            .flush_due(start + WINDOW - Duration::from_millis(1))
            .is_empty());
        let closed = periodic.flush_due(start + WINDOW);
        assert_eq!(closed.len(), 1);
        let line = &closed[0];
        assert!(
            line.starts_with(&format!(
                "{}  INFO Periodic stats summary window_s=60 from={} session_id=7 \
                 keyframe_requests=0 rr_lines=3",
                stamp(5),
                stamp(0)
            )),
            "{line}"
        );
        // The first report is the baseline: 60 frames and 2 drops since it.
        assert!(
            line.contains(" rr_complete=70(+60) rr_dropped=2(+2) "),
            "{line}"
        );
        assert!(line.contains(" rr_jitter_us=100/200/300 "), "{line}");
        assert!(line.contains(" rr_rtt_ms_x10=10/20/30 "), "{line}");
        assert!(line.contains(" rr_frags_lost=2(+2) "), "{line}");
        assert!(line.contains(" rr_decode_fps_x10=600/600/600 "), "{line}");
        assert!(
            line.contains(" audio_lines=2 audio_kbps=96/96/96 "),
            "{line}"
        );
        assert!(line.contains(" audio_quiet_packets=25(+15) "), "{line}");
        assert!(!line.contains("AirPods"), "no audio device name: {line}");
        assert!(line.contains(" stream_lines=1 "), "{line}");
        // The encoder-hold and first-frame-gate reading survives.
        assert!(
            line.contains(" stream_captured=100(+0) stream_encoded=98(+0) stream_sent=0(+0) "),
            "{line}"
        );
        assert!(line.contains(" stream_codec=H.264 (NVENC) "), "{line}");
        assert!(line.contains(" stream_bitrate=15000000/15000000/15000000 "));
        assert!(line.contains(" stream_usb_drops=0(+0) "), "{line}");
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
    fn keyframe_requests_are_counted_and_still_uploaded() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        periodic.push(&summary(7, 0), start);
        let request = format!(
            "{}  INFO Keyframe request received reason=DecoderError",
            stamp(1)
        );
        for _ in 0..3 {
            assert_eq!(periodic.push(&request, start), vec![request.clone()]);
        }
        let line = periodic.flush().remove(0);
        assert!(line.contains(" keyframe_requests=3 "), "{line}");
        periodic.push(&summary(7, 2), start);
        let line = periodic.flush().remove(0);
        assert!(line.contains(" keyframe_requests=0 "), "{line}");
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
    fn the_end_of_a_stream_sends_its_partial_minute_first() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        let established = format!("{}  INFO Client session established session_id=9", stamp(0));
        assert_eq!(periodic.push(&established, start), vec![established]);
        periodic.push(&report(1, 10, 0, 1), start + Duration::from_secs(1));
        periodic.push(&report(2, 30, 0, 1), start + Duration::from_secs(2));
        // A refused HELLO's summary is not this stream's end.
        let refused = session_summary_line("-", 3);
        assert_eq!(
            periodic.push(&refused, start + Duration::from_secs(3)),
            vec![refused]
        );
        assert!(periodic.is_open());
        let end = session_summary_line("9", 20);
        let out = periodic.push(&end, start + Duration::from_secs(20));
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(
            out[0].contains("window_s=19 from=") && out[0].contains(" session_id=9 "),
            "{}",
            out[0]
        );
        assert!(out[0].contains(" rr_lines=2 rr_epoch=1 rr_complete=30(+20) "));
        assert_eq!(out[1], end, "the session summary is never changed");
        assert!(!periodic.is_open());
        assert!(periodic.flush().is_empty());
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
        assert!(out[0].contains("session_id=1 "), "{}", out[0]);
        assert!(out[0].contains(" rr_lines=1 "), "{}", out[0]);
        assert_eq!(out[1], established);
        // A summary for another session also closes the window.
        periodic.push(&summary(2, 3), start + Duration::from_secs(3));
        let out = periodic.push(&summary(3, 4), start + Duration::from_secs(4));
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("session_id=2 "), "{}", out[0]);
        assert!(out[0].contains(" rr_lines=0 "), "{}", out[0]);
    }

    #[test]
    fn other_lines_levels_and_repeat_counts_pass_through() {
        let mut periodic = Periodic::default();
        let now = Instant::now();
        for line in [
            format!("{}  WARN Receiver report epoch=1", stamp(0)),
            format!("{}  INFO Receiver reports are late", stamp(0)),
            format!("{} ERROR Capture loop exited with error", stamp(0)),
            format!(
                "{}  INFO Receiver report epoch=1 complete=5 [repeated 59 more times from {} to {}]",
                stamp(30),
                stamp(0),
                stamp(30)
            ),
            format!("{}  INFO second_screen_fallback=no_frames", stamp(0)),
            session_summary_line("4", 0),
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
            if tenth % 100 == 0 {
                lines.push(summary(7, second));
            }
            if tenth % 5 == 0 {
                lines.push(report(second, tenth as u32, 0, 200));
            }
            if tenth % 10 == 0 {
                lines.push(audio(second, tenth));
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

    /// An idle host logs only its stream summary, with no session: one line
    /// every ten minutes.
    #[test]
    fn an_idle_hour_uploads_six_summary_rows() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        let idle = |second| summary(7, second).replace("session_id=Some(7)", "session_id=None");
        let mut uploaded = Vec::new();
        for second in (0..3600).step_by(10) {
            let now = start + Duration::from_secs(second);
            uploaded.extend(periodic.push(&idle(second), now));
            uploaded.extend(periodic.flush_due(now));
        }
        uploaded.extend(periodic.flush());
        assert_eq!(uploaded.len(), 6, "{uploaded:?}");
        assert!(uploaded[0].contains(" window_s=600 "), "{}", uploaded[0]);
        assert!(uploaded[0].contains(" stream_lines=60 "), "{}", uploaded[0]);
    }

    /// Two sessions in turn, each with its own windows and counters.
    #[test]
    fn sessions_in_turn_get_their_own_summaries() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        let mut out = Vec::new();
        for (session, offset) in [(1u32, 0u64), (2, 90)] {
            out.extend(periodic.push(
                &format!(
                    "{}  INFO Client session established session_id={session}",
                    stamp(offset)
                ),
                start + Duration::from_secs(offset),
            ));
            for half in 0..140u64 {
                let second = offset + half / 2;
                let now = start + Duration::from_millis(offset * 1000 + half * 500);
                out.extend(periodic.push(&report(second, half as u32 * 15, 0, 100), now));
                out.extend(periodic.flush_due(now));
            }
            let end = session_summary_line(&session.to_string(), offset + 70);
            out.extend(periodic.push(&end, start + Duration::from_secs(offset + 70)));
        }
        let summaries: Vec<_> = out
            .iter()
            .filter(|line| line.contains("Periodic stats summary"))
            .collect();
        assert_eq!(summaries.len(), 4, "{summaries:#?}");
        for (summary, (session, lines)) in
            summaries
                .iter()
                .zip([("1", 120), ("1", 20), ("2", 120), ("2", 20)])
        {
            assert!(
                summary.contains(&format!(" session_id={session} ")),
                "{summary}"
            );
            assert!(
                summary.contains(&format!(" rr_lines={lines} ")),
                "{summary}"
            );
        }
        // Session 2's first report is its own baseline.
        assert!(
            summaries[2].contains(" rr_complete=1785(+1785) "),
            "{}",
            summaries[2]
        );
    }

    /// Time between sessions carries no session id, and its counters do not
    /// continue the session before.
    #[test]
    fn a_disconnect_ends_the_session() {
        let mut periodic = Periodic::default();
        let start = Instant::now();
        periodic.push(&summary(7, 0), start);
        periodic.push(&report(0, 100, 0, 1), start);
        let idle = summary(7, 10).replace("session_id=Some(7)", "session_id=None");
        let out = periodic.push(&idle, start + Duration::from_secs(10));
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains(" session_id=7 "), "{}", out[0]);
        assert!(out[0].contains(" rr_lines=1 "), "{}", out[0]);
        periodic.push(&report(11, 120, 0, 1), start + Duration::from_secs(11));
        let line = periodic.flush().remove(0);
        assert!(line.contains(" session_id=None "), "{line}");
        assert!(line.contains(" rr_complete=120(+0) "), "{line}");
    }
}
