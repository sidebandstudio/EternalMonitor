//! Collapses bursts of identical log lines. A retry loop that fails the same
//! way thousands of times a second (desktop duplication on the lock screen,
//! sends to a closed USB tunnel) must not evict the rest of the session log or
//! fill the support upload outbox. The first line of a burst is kept verbatim
//! and a single summary line carries the count and the first/last timestamps.

use std::time::{Duration, Instant};

/// Identical lines within this window of the first one are counted, not written.
pub const REPEAT_WINDOW: Duration = Duration::from_secs(30);
/// Distinct repeating messages tracked at once; the oldest is summarized first.
const MAX_RUNS: usize = 32;

struct Run {
    /// Line without its timestamp: level, message and fields.
    key: String,
    started: Instant,
    first_stamp: String,
    last_stamp: String,
    suppressed: u64,
}

impl Run {
    fn summary(&self) -> Option<String> {
        (self.suppressed > 0).then(|| {
            format!(
                "{}{} [repeated {} more times from {} to {}]",
                self.last_stamp, self.key, self.suppressed, self.first_stamp, self.last_stamp
            )
        })
    }
}

#[derive(Default)]
pub struct RepeatCollapser {
    runs: Vec<Run>,
}

/// `2026-10-21T03:10:19.347613Z  WARN message` splits into the timestamp and
/// the rest. Continuation lines (no leading timestamp) are never collapsed.
fn split_stamp(line: &str) -> Option<(&str, &str)> {
    let end = line.find(' ')?;
    let (stamp, rest) = line.split_at(end);
    let looks_like_stamp = stamp.len() >= 20
        && stamp.as_bytes()[..4].iter().all(u8::is_ascii_digit)
        && stamp.as_bytes()[4] == b'-'
        && stamp.contains('T');
    let has_level = ["TRACE ", "DEBUG ", "INFO ", "WARN ", "ERROR "]
        .iter()
        .any(|level| rest.trim_start().starts_with(level));
    (looks_like_stamp && has_level).then_some((stamp, rest))
}

impl RepeatCollapser {
    /// Returns the lines to write for `line`, in order: summaries of bursts
    /// whose window closed, then `line` itself unless it repeats an open burst.
    pub fn push(&mut self, line: &str, now: Instant) -> Vec<String> {
        let mut out = self.flush_due(now);
        let Some((stamp, rest)) = split_stamp(line) else {
            out.push(line.to_owned());
            return out;
        };
        if let Some(run) = self.runs.iter_mut().find(|run| run.key == rest) {
            run.suppressed += 1;
            run.last_stamp = stamp.to_owned();
            return out;
        }
        if self.runs.len() >= MAX_RUNS {
            let oldest = self.runs.remove(0);
            out.extend(oldest.summary());
        }
        self.runs.push(Run {
            key: rest.to_owned(),
            started: now,
            first_stamp: stamp.to_owned(),
            last_stamp: stamp.to_owned(),
            suppressed: 0,
        });
        out.push(line.to_owned());
        out
    }

    /// Summaries for bursts older than the window. Called on every line, so a
    /// burst is reported no later than the next line logged after it ends.
    pub fn flush_due(&mut self, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        self.runs.retain(|run| {
            if now.duration_since(run.started) < REPEAT_WINDOW {
                return true;
            }
            out.extend(run.summary());
            false
        });
        out
    }

    /// Summaries for every open burst, for shutdown.
    pub fn flush_all(&mut self) -> Vec<String> {
        self.runs
            .drain(..)
            .filter_map(|run| run.summary())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(second: u32, message: &str) -> String {
        format!("2026-10-21T03:10:{second:02}.000000Z  WARN {message}")
    }

    #[test]
    fn a_burst_writes_one_line_and_one_summary() {
        let mut collapser = RepeatCollapser::default();
        let start = Instant::now();
        let message = "Desktop duplication access lost — reinitializing";
        let mut written = Vec::new();
        for i in 0..1000u32 {
            written.extend(collapser.push(
                &line(i % 30, message),
                start + Duration::from_millis(u64::from(i)),
            ));
        }
        assert_eq!(written, vec![line(0, message)]);

        // The next unrelated line after the window carries the summary first.
        let later = start + REPEAT_WINDOW + Duration::from_secs(1);
        let written = collapser.push(&line(59, "Support stream summary"), later);
        assert_eq!(written.len(), 2);
        assert_eq!(
            written[0],
            format!(
                "2026-10-21T03:10:09.000000Z  WARN {message} [repeated 999 more times \
                 from 2026-10-21T03:10:00.000000Z to 2026-10-21T03:10:09.000000Z]"
            )
        );
        assert!(
            written[0].contains(" WARN "),
            "level survives for telemetry"
        );
        assert_eq!(written[1], line(59, "Support stream summary"));
    }

    #[test]
    fn a_repeat_after_the_window_is_written_again() {
        let mut collapser = RepeatCollapser::default();
        let start = Instant::now();
        assert_eq!(collapser.push(&line(0, "x"), start).len(), 1);
        let later = start + REPEAT_WINDOW;
        // No suppressed repeats: no summary, and the line is written again.
        assert_eq!(collapser.push(&line(30, "x"), later), vec![line(30, "x")]);
    }

    #[test]
    fn interleaved_messages_are_tracked_separately() {
        let mut collapser = RepeatCollapser::default();
        let now = Instant::now();
        let mut written = Vec::new();
        for _ in 0..5 {
            written.extend(collapser.push(&line(1, "a"), now));
            written.extend(collapser.push(&line(1, "b"), now));
        }
        assert_eq!(written, vec![line(1, "a"), line(1, "b")]);
        let summaries = collapser.flush_all();
        assert_eq!(summaries.len(), 2);
        assert!(summaries[0].contains("repeated 4 more times"));
    }

    #[test]
    fn continuation_lines_and_new_fields_are_never_collapsed() {
        let mut collapser = RepeatCollapser::default();
        let now = Instant::now();
        assert_eq!(collapser.push(" stdout=", now), vec![" stdout="]);
        assert_eq!(collapser.push(" stdout=", now), vec![" stdout="]);
        assert_eq!(collapser.push(&line(1, "silent for 3005ms"), now).len(), 1);
        assert_eq!(collapser.push(&line(1, "silent for 3006ms"), now).len(), 1);
    }

    #[test]
    fn the_run_table_is_bounded() {
        let mut collapser = RepeatCollapser::default();
        let now = Instant::now();
        for i in 0..MAX_RUNS {
            collapser.push(&line(1, &format!("m{i}")), now);
            collapser.push(&line(2, &format!("m{i}")), now);
        }
        let written = collapser.push(&line(3, "overflow"), now);
        assert_eq!(written.len(), 2, "oldest burst summarized to make room");
        assert!(written[0].contains("m0 [repeated 1 more times"));
    }
}
