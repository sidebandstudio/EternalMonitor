//! The frame rate a software encoder holds. libx264 and libx265 encode on the
//! processor, and on a slow one a frame at the user's 120 fps can take
//! longer to encode than it lasts. The capture slot keeps only the newest
//! picture, so nothing queues in front of the encoder, but capture still
//! read back 120 pictures a second for it to skip most of them, and those
//! readbacks took processor time from the encoder. This holds capture to a
//! rate the encoder keeps up with, in steps, and raises it again once the
//! encoder has steady headroom. Hardware encoders are left as they are.
//!
//! The iPad is told the stream's rate as before (`StreamConfig.fps` is the
//! user's setting): it already takes a stream whose rate varies, as a still
//! desktop's does. The software session itself reopens at the rate capture
//! is held to (see [`encode_fps`]).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::info;

/// The rates capture steps down through, below the stream's own rate (the
/// user's setting or the iPad's, whichever is lower).
const STEPS: [u32; 5] = [120, 90, 60, 45, 30];
/// Capture is never held below this.
pub(crate) const FLOOR_FPS: u32 = 30;
/// The encoder's cost is averaged over this long before a rate changes.
const WINDOW: Duration = Duration::from_secs(1);
/// Fewer new pictures than this in a window say nothing about the encoder,
/// a still desktop, unless the encoder was busy most of the window (see
/// [`HOLDS`]): then they were few because each took long.
const MIN_FRAMES: u32 = 5;
/// A rate holds while a frame's conversion and encoding take at most this
/// share of its interval; capture and the transport need the rest.
const HOLDS: f64 = 0.9;
/// The next rate up is tried only while a frame would take at most this
/// share of that rate's interval. The gap to [`HOLDS`] keeps a cost near one
/// step's limit from going up and straight back down.
const HEADROOM: f64 = 0.7;
/// Headroom must last this long before the rate goes up a step.
const UP_AFTER: Duration = Duration::from_secs(5);
/// A step up that has to come down within this was one too many, as when
/// the higher capture rate itself slows the encoder down: the wait for the
/// next one doubles, up to [`UP_AFTER_MAX`].
const UP_FAILED_WITHIN: Duration = Duration::from_secs(30);
const UP_AFTER_MAX: Duration = Duration::from_secs(60);
/// Headroom is judged only while new pictures come at this share of the
/// rate capture is held to: a desktop that moves all the time.
const STEADY: f64 = 0.8;

/// A change of the rate capture is held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    pub from: u32,
    /// The stream's own rate when the limit ends.
    pub to: u32,
    /// The window's average conversion and encode time per frame.
    pub cost: Duration,
}

/// Holds capture to a software encoder's rate through `published`, the
/// shared `cpu_fps_limit` (0 for none), which only this writes.
#[derive(Debug)]
pub(crate) struct SoftwareRate {
    published: Arc<AtomicU32>,
    /// The encoder and session the measurements are of.
    of: Option<(String, Option<u32>)>,
    /// The rate capture is held to, while it is below the stream's.
    limit: Option<u32>,
    /// Since when, the summed cost and the frames of the current window.
    window: Option<(Instant, Duration, u32)>,
    headroom_since: Option<Instant>,
    up_after: Duration,
    raised_at: Option<Instant>,
}

impl SoftwareRate {
    /// No limit, published at once.
    pub(crate) fn new(published: Arc<AtomicU32>) -> Self {
        published.store(0, Ordering::Relaxed);
        Self {
            published,
            of: None,
            limit: None,
            window: None,
            headroom_since: None,
            up_after: UP_AFTER,
            raised_at: None,
        }
    }
}

/// libx264 and libx265: every encoder that is not a graphics card's.
pub(crate) fn is_software_encoder(encoder_name: &str) -> bool {
    matches!(encoder_name, "libx264" | "libx265")
}

/// The rates capture can run at under `ceiling`, highest first.
fn levels(ceiling: u32) -> impl Iterator<Item = u32> {
    std::iter::once(ceiling).chain(STEPS.into_iter().filter(move |&step| step < ceiling))
}

fn holds(cost: Duration, fps: u32, share: f64) -> bool {
    cost.as_secs_f64() * f64::from(fps.max(1)) <= share
}

/// The rate the session for `encoder_name` opens at. x264's and x265's
/// zero-latency rate control spends `bitrate / fps` on each frame whatever
/// the timestamps say, so a software session encodes at the rate capture is
/// held to: at 45 fps captured into a session opened at 120, it sent 389 of
/// 1,000 kbps. With no limit this is the stream's rate, as before. Hardware
/// sessions always open at the stream's rate.
pub(crate) fn encode_fps(encoder_name: &str, shared: &crate::control::SharedControl) -> u32 {
    if is_software_encoder(encoder_name) {
        shared.capture_fps()
    } else {
        shared.effective_fps()
    }
}

impl SoftwareRate {
    /// The rate capture runs at under `ceiling`.
    pub(crate) fn rate(&self, ceiling: u32) -> u32 {
        self.limit
            .filter(|&limit| limit < ceiling)
            .unwrap_or(ceiling)
    }

    /// A frame the software encoder converted and encoded from a new
    /// picture in `cost`. `ceiling` is the stream's rate. Returns a change
    /// of the rate capture is held to.
    pub(crate) fn encoded(&mut self, cost: Duration, ceiling: u32, now: Instant) -> Option<Step> {
        let current = self.rate(ceiling);
        if self.limit.is_some_and(|limit| limit >= ceiling) {
            self.limit = None;
        }
        let (start, sum, frames) = self.window.get_or_insert((now, Duration::ZERO, 0));
        *sum += cost;
        *frames += 1;
        let start = *start;
        let elapsed = now.saturating_duration_since(start);
        if elapsed < WINDOW {
            return None;
        }
        let (_, sum, frames) = self.window.take().expect("filled above");
        // Few frames from an encoder busy all the window: each took long.
        let busy = sum.as_secs_f64() >= HOLDS * elapsed.as_secs_f64();
        if frames < MIN_FRAMES && !busy {
            self.headroom_since = None;
            return None;
        }
        let cost = sum / frames;
        if self
            .raised_at
            .is_some_and(|at| now.saturating_duration_since(at) >= UP_FAILED_WITHIN)
        {
            // The last step up held.
            self.raised_at = None;
            self.up_after = UP_AFTER;
        }
        if !holds(cost, current, HOLDS) {
            self.headroom_since = None;
            let to = levels(ceiling)
                .find(|&fps| fps < current && holds(cost, fps, HOLDS))
                .unwrap_or(FLOOR_FPS.min(ceiling));
            if to >= current {
                return None;
            }
            if self.raised_at.take().is_some() {
                self.up_after = (self.up_after * 2).min(UP_AFTER_MAX);
            }
            self.limit = Some(to);
            return Some(Step {
                from: current,
                to,
                cost,
            });
        }
        if self.limit.is_none() || frames < MIN_FRAMES {
            self.headroom_since = None;
            return None;
        }
        let next = levels(ceiling)
            .filter(|&fps| fps > current)
            .min()
            .expect("a limit is below the ceiling");
        let steady = f64::from(frames) >= STEADY * f64::from(current) * elapsed.as_secs_f64();
        if !(steady && holds(cost, next, HEADROOM)) {
            self.headroom_since = None;
            return None;
        }
        let since = *self.headroom_since.get_or_insert(start);
        if now.saturating_duration_since(since) < self.up_after {
            return None;
        }
        self.headroom_since = None;
        self.raised_at = Some(now);
        self.limit = (next < ceiling).then_some(next);
        Some(Step {
            from: current,
            to: next,
            cost,
        })
    }

    /// Forgets the measurements and ends any limit.
    fn reset(&mut self) {
        *self = Self::new(Arc::clone(&self.published));
    }

    /// After each frame the open session encoded from a new picture, in
    /// `session` (the viewer's, if any): holds capture to the rate a
    /// software encoder keeps up with. A hardware session, another encoder
    /// or another session starts again without a limit.
    pub(crate) fn note_encoded(
        &mut self,
        encoder_name: &str,
        session: Option<u32>,
        cost: Duration,
        ceiling: u32,
        now: Instant,
    ) {
        if self
            .of
            .as_ref()
            .is_none_or(|(encoder, of)| encoder != encoder_name || *of != session)
        {
            self.reset();
            self.of = Some((encoder_name.to_string(), session));
        }
        if !is_software_encoder(encoder_name) {
            return;
        }
        let step = self.encoded(cost, ceiling, now);
        // Also when a lower stream rate ended the limit without a step.
        self.published
            .store(self.limit.unwrap_or(0), Ordering::Relaxed);
        let Some(step) = step else {
            return;
        };
        let encode_us = step.cost.as_micros() as u64;
        if step.to < step.from {
            info!(
                from = step.from,
                to = step.to,
                encode_us,
                encoder = encoder_name,
                "The software encoder cannot hold the frame rate; capturing fewer frames per second"
            );
        } else {
            info!(
                from = step.from,
                to = step.to,
                encode_us,
                encoder = encoder_name,
                "The software encoder has headroom again; capturing more frames per second"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{frame_slot, RawFrame, Taken};
    use crate::control::SharedControl;
    use std::sync::Arc;

    const MS: Duration = Duration::from_millis(1);

    /// What a simulated stream did.
    #[derive(Default)]
    struct Run {
        /// (seconds into the run, capture's rate) at each change.
        rates: Vec<(f64, u32)>,
        /// Frame numbers given to the encoder.
        submitted: Vec<u64>,
        captured: u64,
        /// Longest wait of a picture in front of the encoder, and the frame
        /// interval it was captured at.
        worst_wait: Duration,
        worst_wait_interval: Duration,
        /// Capture's rate at the end.
        last: u32,
    }

    impl Run {
        fn rate_at(&self, second: f64) -> u32 {
            self.rates
                .iter()
                .rev()
                .find(|(at, _)| *at <= second)
                .map(|(_, fps)| *fps)
                .expect("a rate from the start")
        }

        fn changes(&self) -> usize {
            self.rates.len() - 1
        }
    }

    /// A stream on a simulated clock: capture at `shared.capture_fps()`
    /// publishes into the real capture slot, a fake encoder named `encoder`
    /// takes each picture as soon as it is free and spends `cost(second,
    /// capture rate)` on it. `limiter: false` is the stream without any
    /// limit, as before this change.
    fn simulate(
        shared: &SharedControl,
        encoder: &str,
        seconds: f64,
        limiter: bool,
        cost: impl Fn(f64, u32) -> Duration,
    ) -> Run {
        let (producer, consumer) = frame_slot();
        let epoch = Instant::now();
        let end = epoch + Duration::from_secs_f64(seconds);
        let mut rate = SoftwareRate::new(Arc::clone(&shared.cpu_fps_limit));
        let mut run = Run::default();
        let mut next_capture = epoch;
        let mut encoder_free = epoch;
        // The capture interval of each published frame, by frame number.
        let mut intervals = std::collections::HashMap::new();
        let mut kept = None;
        loop {
            let capture_fps = shared.capture_fps();
            if run.rates.last().is_none_or(|&(_, fps)| fps != capture_fps) {
                let at = next_capture.min(encoder_free).duration_since(epoch);
                run.rates.push((at.as_secs_f64(), capture_fps));
            }
            // The next event: a capture, or the encoder taking a picture.
            if next_capture <= encoder_free {
                let now = next_capture;
                if now >= end {
                    break;
                }
                run.captured += 1;
                let interval = crate::capture::frame_budget_for(capture_fps);
                intervals.insert(run.captured, interval);
                producer.publish(RawFrame {
                    frame_number: run.captured,
                    timestamp: now,
                    data: Arc::new(vec![0; 4]),
                    width: 1,
                    height: 1,
                    cached: false,
                });
                next_capture = now + interval;
                continue;
            }
            let now = encoder_free;
            match consumer.take_within(Duration::ZERO, &mut kept) {
                Taken::Frame(frame) => {
                    let waited = now.duration_since(frame.timestamp);
                    if waited > run.worst_wait {
                        run.worst_wait = waited;
                        run.worst_wait_interval = intervals[&frame.frame_number];
                    }
                    run.submitted.push(frame.frame_number);
                    let spent = cost(now.duration_since(epoch).as_secs_f64(), capture_fps);
                    encoder_free = now + spent;
                    if limiter {
                        rate.note_encoded(
                            encoder,
                            Some(1),
                            spent,
                            shared.effective_fps(),
                            encoder_free,
                        );
                    }
                }
                // Idle until the next capture.
                _ => encoder_free = next_capture,
            }
        }
        run.last = shared.capture_fps();
        run
    }

    fn shared_at(fps: u32) -> SharedControl {
        let shared = SharedControl::new(9876, 15_000_000);
        shared.target_fps.store(fps, Ordering::SeqCst);
        shared
    }

    /// The field case: libx264 at 120 fps on a processor that takes 20 ms a
    /// frame. Capture comes down to 45 fps, which the encoder holds, and no
    /// picture waits in front of it longer than a frame. Once frames cost
    /// 6 ms, capture goes back up a step at a time to 90 fps and stays
    /// there: 120 fps would want 6 ms to be at most 70% of its 8.3 ms.
    #[test]
    fn a_slow_software_encoder_holds_capture_to_a_rate_it_keeps_up_with() {
        let shared = shared_at(120);
        let run = simulate(&shared, "libx264", 60.0, true, |second, _| {
            if second < 20.0 {
                20 * MS
            } else {
                6 * MS
            }
        });
        assert_eq!(run.rate_at(0.0), 120);
        assert_eq!(run.rate_at(1.1), 45, "{:?}", run.rates);
        assert_eq!(run.rate_at(19.9), 45, "{:?}", run.rates);
        // At 45 fps the encoder takes every picture capture reads back.
        let at_45 = run
            .submitted
            .iter()
            .filter(|&&n| (200..800).contains(&n))
            .count();
        assert_eq!(at_45, 600, "every picture at 45 fps is encoded");
        assert!(
            run.worst_wait <= run.worst_wait_interval,
            "a picture waited {:?} in front of the encoder, at a {:?} frame",
            run.worst_wait,
            run.worst_wait_interval
        );
        // Back up a step after 5 s of headroom each, then no more changes.
        let rates: Vec<u32> = run.rates.iter().map(|&(_, fps)| fps).collect();
        assert_eq!(rates, [120, 45, 60, 90], "{:?}", run.rates);
        assert!(
            run.rate_at(28.0) == 60 && run.rate_at(35.0) == 90,
            "{:?}",
            run.rates
        );
        assert_eq!(run.last, 90);
    }

    /// The base had no limit: at 120 fps and 20 ms a frame the encoder
    /// skipped five pictures of six, which capture had read back for it.
    #[test]
    fn without_the_limit_capture_reads_back_pictures_the_encoder_skips() {
        let shared = shared_at(120);
        let run = simulate(&shared, "libx264", 10.0, false, |_, _| 20 * MS);
        assert_eq!(run.changes(), 0);
        assert!(run.captured >= 1199, "{}", run.captured);
        assert!(run.submitted.len() <= 501, "{}", run.submitted.len());
    }

    /// When the higher rate itself slows the encoder (capture's readbacks
    /// take its processor time), a step up comes back down, and each such
    /// try waits twice as long as the one before.
    #[test]
    fn a_cost_that_rises_with_the_rate_does_not_go_up_and_down_every_few_seconds() {
        let shared = shared_at(120);
        let run = simulate(&shared, "libx264", 300.0, true, |_, fps| match fps {
            // Holds 60 with room for 90, but not 90 itself.
            120 => 12 * MS,
            90 => 10500 * Duration::from_micros(1),
            _ => 7 * MS,
        });
        let ups: Vec<f64> = run
            .rates
            .windows(2)
            .filter(|pair| pair[1].1 > pair[0].1)
            .map(|pair| pair[1].0)
            .collect();
        assert!(ups.len() <= 8, "{:?}", run.rates);
        let gaps: Vec<f64> = ups.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(
            gaps.windows(2).all(|pair| pair[1] >= pair[0] - 0.1),
            "the waits grow: {gaps:?}"
        );
        assert!(
            run.rates.iter().all(|&(_, fps)| fps == 120 || fps <= 90),
            "{:?}",
            run.rates
        );
        // Most of the time at 60, which holds.
        let at_60 = (0..300)
            .filter(|&s| run.rate_at(f64::from(s)) == 60)
            .count();
        assert!(at_60 >= 270, "{at_60} s at 60 fps: {:?}", run.rates);
    }

    /// Never above the user's setting or the iPad's rate, never below 30.
    #[test]
    fn the_limit_stays_between_30_fps_and_the_streams_rate() {
        // Plenty of headroom at the user's 60: no limit, no step up.
        let shared = shared_at(60);
        let run = simulate(&shared, "libx264", 20.0, true, |_, _| MS);
        assert_eq!(run.changes(), 0);
        assert_eq!(run.last, 60);

        // The iPad asks for 90: capture starts there, steps down from it,
        // and comes back up only to it.
        let shared = shared_at(120);
        shared.client_preferred_fps.store(90, Ordering::SeqCst);
        let run = simulate(&shared, "libx264", 40.0, true, |second, _| {
            if second < 10.0 {
                14 * MS
            } else {
                MS
            }
        });
        let rates: Vec<u32> = run.rates.iter().map(|&(_, fps)| fps).collect();
        assert_eq!(rates, [90, 60, 90], "{:?}", run.rates);

        // Slower than even 30 fps: capture stays at 30.
        let shared = shared_at(120);
        let run = simulate(&shared, "libx265", 20.0, true, |_, _| 100 * MS);
        let rates: Vec<u32> = run.rates.iter().map(|&(_, fps)| fps).collect();
        assert_eq!(rates, [120, 30], "{:?}", run.rates);
        assert!(run.worst_wait <= run.worst_wait_interval);

        // A user's 30 fps is never lowered.
        let shared = shared_at(30);
        let run = simulate(&shared, "libx264", 10.0, true, |_, _| 100 * MS);
        assert_eq!(run.changes(), 0);
        assert_eq!(run.last, 30);
    }

    /// A hardware encoder keeps the stream's rate however long it takes, and
    /// gets the very frames it got before this change.
    #[test]
    fn hardware_encoders_are_paced_as_before() {
        for encoder in ["h264_nvenc", "h264_amf", "h264_qsv", "hevc_nvenc"] {
            let cost = |second: f64, _| if second < 5.0 { 20 * MS } else { MS };
            let base = simulate(&shared_at(120), encoder, 15.0, false, cost);
            let shared = shared_at(120);
            let run = simulate(&shared, encoder, 15.0, true, cost);
            assert_eq!(run.changes(), 0, "{encoder}");
            assert_eq!(run.captured, base.captured, "{encoder}");
            assert_eq!(run.submitted, base.submitted, "{encoder}");
            assert_eq!(shared.cpu_fps_limit.load(Ordering::SeqCst), 0);
        }
    }

    /// Frames of `cost` every `cost` (an encoder busy all the time) from
    /// `at` for `seconds`, for `encoder` in `session` at `ceiling`.
    fn busy(
        rate: &mut SoftwareRate,
        (encoder, session, ceiling): (&str, Option<u32>, u32),
        cost: Duration,
        at: &mut Instant,
        seconds: u32,
    ) {
        let end = *at + seconds * 1000 * MS;
        while *at < end {
            *at += cost;
            rate.note_encoded(encoder, session, cost, ceiling, *at);
        }
    }

    /// The review's sequence: held to 45, the user picks 30, then 120
    /// again with cheap frames. Capture goes back to 120 with the
    /// controller, instead of staying at 45.
    #[test]
    fn a_limit_a_lower_setting_ended_ends_in_capture_too() {
        let shared = shared_at(120);
        let mut rate = SoftwareRate::new(Arc::clone(&shared.cpu_fps_limit));
        let mut at = Instant::now();
        busy(&mut rate, ("libx264", Some(1), 120), 20 * MS, &mut at, 3);
        assert_eq!(shared.capture_fps(), 45);
        shared.target_fps.store(30, Ordering::SeqCst);
        busy(&mut rate, ("libx264", Some(1), 30), MS, &mut at, 3);
        assert_eq!(shared.capture_fps(), 30);
        shared.target_fps.store(120, Ordering::SeqCst);
        busy(&mut rate, ("libx264", Some(1), 120), MS, &mut at, 3);
        assert_eq!(rate.rate(120), 120);
        assert_eq!(shared.cpu_fps_limit.load(Ordering::SeqCst), 0);
        assert_eq!(shared.capture_fps(), 120);
    }

    /// A hardware session after a limited software one, another software
    /// encoder or another session: capture goes back to the stream's rate
    /// at the first frame, before any measurement.
    #[test]
    fn a_hardware_encoder_another_encoder_or_session_starts_without_a_limit() {
        let shared = shared_at(120);
        let limited = |shared: &SharedControl| {
            let mut rate = SoftwareRate::new(Arc::clone(&shared.cpu_fps_limit));
            let mut at = Instant::now();
            busy(&mut rate, ("libx264", Some(1), 120), 20 * MS, &mut at, 2);
            assert_eq!(shared.capture_fps(), 45);
            (rate, at)
        };
        for (encoder, session) in [
            ("h264_nvenc", Some(1)),
            ("h264_amf", Some(1)),
            ("libx265", Some(1)),
            ("libx264", Some(2)),
            ("libx264", None),
        ] {
            let (mut rate, at) = limited(&shared);
            rate.note_encoded(encoder, session, MS, 120, at + MS);
            assert_eq!(shared.cpu_fps_limit.load(Ordering::SeqCst), 0, "{encoder}");
            assert_eq!(shared.capture_fps(), 120, "{encoder} {session:?}");
        }
        // However long the hardware encoder takes.
        let (mut rate, mut at) = limited(&shared);
        busy(
            &mut rate,
            ("h264_nvenc", Some(1), 120),
            50 * MS,
            &mut at,
            10,
        );
        assert_eq!(shared.capture_fps(), 120);
    }

    /// An encoder so slow that a second holds only a few frames still
    /// limits capture: 500 ms and 100 ms a frame, busy all the time.
    #[test]
    fn an_encoder_too_slow_for_five_frames_a_second_still_limits_capture() {
        for cost in [500 * MS, 100 * MS] {
            let shared = shared_at(120);
            let run = simulate(&shared, "libx264", 60.0, true, |_, _| cost);
            let rates: Vec<u32> = run.rates.iter().map(|&(_, fps)| fps).collect();
            assert_eq!(rates, [120, 30], "{cost:?}: {:?}", run.rates);
            assert!(run.rate_at(2.1) == 30, "{cost:?}: {:?}", run.rates);
            assert_eq!(shared.cpu_fps_limit.load(Ordering::SeqCst), 30);
        }
    }

    /// A still desktop, whose few pictures are cheap, is no headroom.
    #[test]
    fn a_still_desktop_does_not_raise_or_lower_the_rate() {
        let mut rate = SoftwareRate::new(Arc::default());
        let start = Instant::now();
        let mut at = start;
        for _ in 0..60 {
            at += 22 * MS;
            rate.encoded(20 * MS, 120, at);
        }
        assert_eq!(rate.rate(120), 45);
        // A picture every 250 ms for a minute, a millisecond each.
        for _ in 0..240 {
            at += 250 * MS;
            assert_eq!(rate.encoded(MS, 120, at), None);
        }
        assert_eq!(rate.rate(120), 45);
        // Nor do fewer than five a second lower it while the encoder is
        // mostly idle, though 40 ms is more than 90% of a 45 fps frame.
        for _ in 0..150 {
            at += 400 * MS;
            assert_eq!(rate.encoded(40 * MS, 120, at), None);
        }
        assert_eq!(rate.rate(120), 45);
    }

    /// The summary every 10 s says what capture is held to.
    #[test]
    fn the_stream_summary_says_when_the_software_encoder_limits_the_rate() {
        struct Sink(Arc<parking_lot::Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let summary = |shared: &SharedControl| {
            let out = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
            let writer = Arc::clone(&out);
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(move || Sink(Arc::clone(&writer)))
                .finish();
            crate::logging::prepare_log_capture();
            tracing::subscriber::with_default(subscriber, || {
                crate::diagnostics::log_stream_summary(shared)
            });
            let text = String::from_utf8(out.lock().clone()).unwrap();
            text
        };
        let shared = shared_at(120);
        let line = summary(&shared);
        assert!(line.contains("Support stream summary"), "{line}");
        assert!(line.contains(" target_fps=120 cpu_fps_limit=0 "), "{line}");
        shared.cpu_fps_limit.store(45, Ordering::SeqCst);
        let line = summary(&shared);
        assert!(line.contains(" target_fps=120 cpu_fps_limit=45 "), "{line}");
    }
}
