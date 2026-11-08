//! Decides when the pointer goes to an iPad that draws it (see
//! `capture::pointer`). Pure logic with an injected clock; the transport
//! loop feeds it the latest image and position and sends what it returns.
//!
//! - A position goes at most once a frame budget, the newest one, so a
//!   1000 Hz mouse costs no more than the video would. The budgets are a
//!   fixed grid from the session's first send, not counted from the last
//!   send: a send that ran late would otherwise delay every later one by
//!   as much. One that waits for the next budget goes the moment it starts.
//! - The last position goes once more [`REPEAT_AFTER`] later, and every
//!   heartbeat, so a lost one is replaced without the pointer moving.
//! - An image goes when it changes, when a session starts, with every IDR
//!   the session asks for, and again when a report the iPad sent
//!   [`SHAPE_RESEND_AFTER`] or more after it went still names another one.
//! - An image's chunks resume where the link last refused one (the USB
//!   tunnel's control queue holds 64), [`SHAPE_RETRY`] later. It counts as
//!   sent once every chunk was accepted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{PointerState, POINTER_VISIBLE};

use crate::capture::pointer::{Position, Shape};

pub const REPEAT_AFTER: Duration = Duration::from_millis(100);
/// Long enough for the image to reach the iPad before a report it sends,
/// so a report already on its way doesn't send the image twice.
pub const SHAPE_RESEND_AFTER: Duration = Duration::from_millis(250);
/// How long a refused image chunk waits before it is offered again.
pub const SHAPE_RETRY: Duration = Duration::from_millis(5);

#[derive(Debug, PartialEq, Eq)]
pub enum Send {
    /// The image, from this chunk on: report what went with
    /// [`PointerSender::shape_progress`].
    Shape(Arc<Shape>, usize),
    State(PointerState),
}

#[derive(Default)]
pub struct PointerSender {
    session_id: Option<u32>,
    /// The last state sent, without its time, and when.
    sent: Option<(PointerState, Instant)>,
    /// Where the grid of frame budgets starts.
    epoch: Option<Instant>,
    repeat_at: Option<Instant>,
    /// The image sent last, whole, and when.
    shape_sent: Option<(u32, Instant)>,
    /// The image being sent, and its next chunk.
    pending: Option<(Arc<Shape>, usize)>,
    /// A chunk was refused: offer the image again from then.
    retry_at: Option<Instant>,
    shape_due: bool,
    /// The image the iPad's last report said it holds, and when it came.
    client_shape: Option<(u32, Instant)>,
}

impl PointerSender {
    /// The session the pointer goes to, or `None`. A new one gets
    /// everything again.
    pub fn serve(&mut self, session_id: Option<u32>) {
        if self.session_id != session_id {
            *self = Self {
                session_id,
                shape_due: true,
                ..Self::default()
            };
        }
    }

    pub fn session_id(&self) -> Option<u32> {
        self.session_id
    }

    /// The session's IDR: send the image with it, and the position.
    pub fn keyframe(&mut self, now: Instant) {
        self.shape_due = true;
        self.refresh(now);
    }

    /// Send the current position again at the next poll.
    pub fn refresh(&mut self, now: Instant) {
        self.repeat_at = Some(now);
    }

    pub fn report(&mut self, pointer_shape_id: u32, now: Instant) {
        self.client_shape = Some((pointer_shape_id, now));
    }

    /// What to send now, the image before the position that names it.
    pub fn poll(
        &mut self,
        now: Instant,
        budget: Duration,
        (shape, position): (Option<Arc<Shape>>, Option<Position>),
    ) -> Vec<Send> {
        let mut out = Vec::new();
        if self.session_id.is_none() {
            return out;
        }
        if let Some(shape) = shape.as_ref() {
            let changed = self.shape_sent.is_none_or(|(id, _)| id != shape.id);
            let lagging = match (self.client_shape, self.shape_sent) {
                (Some((held, reported)), Some((_, sent))) => {
                    held != shape.id && reported >= sent + SHAPE_RESEND_AFTER
                }
                _ => false,
            };
            if changed || lagging || self.shape_due {
                self.shape_due = false;
                // Sending it already: carry on from where it got to.
                if self
                    .pending
                    .as_ref()
                    .is_none_or(|(sending, _)| sending.id != shape.id)
                {
                    self.pending = Some((Arc::clone(shape), 0));
                    self.retry_at = None;
                }
            }
        }
        if let Some((sending, next)) = self.pending.as_ref() {
            if self.retry_at.is_none_or(|at| now >= at) {
                out.push(Send::Shape(Arc::clone(sending), *next));
            }
        }
        let Some(position) = position else {
            return out;
        };
        let state = PointerState {
            flags: if position.visible { POINTER_VISIBLE } else { 0 },
            shape_id: shape.as_ref().map_or(0, |shape| shape.id),
            x: position.x,
            y: position.y,
            host_time_us: 0,
        };
        let epoch = *self.epoch.get_or_insert(now);
        let slot =
            |at: Instant| at.saturating_duration_since(epoch).as_nanos() / budget.as_nanos().max(1);
        let send = match self.sent {
            None => true,
            Some((sent, at)) if sent != state => slot(now) > slot(at),
            Some((_, at)) => self.repeat_at.is_some_and(|due| now >= due) && slot(now) > slot(at),
        };
        if send {
            // A repeat follows a change, not another repeat.
            let changed = self.sent.is_none_or(|(sent, _)| sent != state);
            self.repeat_at = changed.then_some(now + REPEAT_AFTER);
            self.sent = Some((state, now));
            out.push(Send::State(PointerState {
                host_time_us: position.host_time_us,
                ..state
            }));
        }
        out
    }

    /// The transport sent the pending image's chunks up to `next` of
    /// `total`; `refused` when the link refused chunk `next`. The image is
    /// sent once every chunk was.
    pub fn shape_progress(
        &mut self,
        id: u32,
        next: usize,
        total: usize,
        refused: bool,
        now: Instant,
    ) {
        let Some((sending, at)) = self.pending.as_mut() else {
            return;
        };
        if sending.id != id {
            return;
        }
        if next >= total {
            self.pending = None;
            self.retry_at = None;
            self.shape_sent = Some((id, now));
            return;
        }
        *at = next;
        self.retry_at = refused.then_some(now + SHAPE_RETRY);
    }

    /// When [`Self::poll`] next has something to send without new input:
    /// the start of the next budget, for a newer position that waits for
    /// it, or a repeat.
    pub fn next_poll(
        &self,
        budget: Duration,
        latest: Option<Position>,
        shape_id: Option<u32>,
    ) -> Option<Instant> {
        [
            self.budget_end(budget, latest, shape_id),
            self.repeat_due(budget),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The start of the next budget, while a newer position waits for it.
    /// At most one budget away, and later than the one before: the loop
    /// waits for it on a high-resolution timer.
    pub fn budget_end(
        &self,
        budget: Duration,
        latest: Option<Position>,
        shape_id: Option<u32>,
    ) -> Option<Instant> {
        let (sent, _) = self.sent?;
        let waiting = latest.is_some_and(|position| {
            (
                position.visible,
                position.x,
                position.y,
                shape_id.unwrap_or(0),
            ) != (
                sent.flags & POINTER_VISIBLE != 0,
                sent.x,
                sent.y,
                sent.shape_id,
            )
        });
        waiting.then(|| self.next_slot(budget)).flatten()
    }

    /// When the last position goes again, no earlier than the next budget,
    /// or a refused image chunk is offered again.
    pub fn repeat_due(&self, budget: Duration) -> Option<Instant> {
        let repeat = self
            .repeat_at
            .and_then(|due| Some(due.max(self.next_slot(budget)?)));
        let retry = self.pending.as_ref().and(self.retry_at);
        [repeat, retry].into_iter().flatten().min()
    }

    fn next_slot(&self, budget: Duration) -> Option<Instant> {
        self.session_id?;
        let (_, at) = self.sent?;
        let epoch = self.epoch?;
        let budget = budget.max(Duration::from_nanos(1));
        let slot = at.saturating_duration_since(epoch).as_nanos() / budget.as_nanos();
        Some(epoch + budget * (slot as u32 + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::pointer::PointerImage;

    const BUDGET: Duration = Duration::from_micros(8_333);

    fn shape(id: u32) -> Arc<Shape> {
        Arc::new(Shape {
            id,
            image: PointerImage {
                source_type: 2,
                flags: 0,
                width: 1,
                height: 1,
                hot: (0, 0),
                bytes: vec![0; 4],
            },
        })
    }

    fn at(x: i32) -> Position {
        Position {
            visible: true,
            x,
            y: 0,
            host_time_us: x as u64,
        }
    }

    /// Polls with a link that takes every chunk.
    fn poll_all(
        sender: &mut PointerSender,
        now: Instant,
        budget: Duration,
        latest: (Option<Arc<Shape>>, Option<Position>),
    ) -> Vec<Send> {
        let sent = sender.poll(now, budget, latest);
        for send in &sent {
            if let Send::Shape(shape, _) = send {
                sender.shape_progress(shape.id, 1, 1, false, now);
            }
        }
        sent
    }

    fn states(sent: &[Send]) -> Vec<i32> {
        sent.iter()
            .filter_map(|send| match send {
                Send::State(state) => Some(state.x),
                Send::Shape(..) => None,
            })
            .collect()
    }

    /// Windows' updates at irregular times: a burst 1 ms apart, then gaps of
    /// 30 to 60 ms, as the #183 measurements fed the host.
    #[test]
    fn positions_go_at_most_once_a_budget_newest_first_and_none_is_lost_at_the_end() {
        let start = Instant::now();
        let mut sender = PointerSender::default();
        sender.serve(Some(1));
        let mut updates = Vec::new();
        let mut t = 0u64;
        let mut x = 0;
        for gap in [30, 45, 60, 38] {
            for _ in 0..4 {
                updates.push((t, x));
                t += 1;
                x += 1;
            }
            t += gap;
        }
        // Poll at every update and every millisecond between, as the loop's
        // wakeups would.
        let mut sent: Vec<(u64, i32)> = Vec::new();
        let mut latest = None;
        let mut last_end: Option<Instant> = None;
        let mut next = updates.iter().peekable();
        for ms in 0..=t + 200 {
            while next.peek().is_some_and(|(at_ms, _)| *at_ms == ms) {
                latest = Some(at(next.next().unwrap().1));
            }
            let now = start + Duration::from_millis(ms);
            for x in states(&poll_all(
                &mut sender,
                now,
                BUDGET,
                (Some(shape(1)), latest),
            )) {
                sent.push((ms, x));
            }
            // What the loop's high-resolution timer waits for: within one
            // budget, and never earlier than the end it waited for before.
            if let Some(end) = sender.budget_end(BUDGET, latest, Some(1)) {
                assert!(end > now && end <= now + BUDGET, "{ms} ms");
                assert!(last_end.is_none_or(|last| end >= last), "{ms} ms");
                last_end = Some(end);
            }
        }
        let slot = |ms: u64| Duration::from_millis(ms).as_nanos() / BUDGET.as_nanos();
        for pair in sent.windows(2) {
            assert!(
                slot(pair[1].0) > slot(pair[0].0),
                "two sends in one budget: {pair:?}"
            );
        }
        // Each burst's last position goes once its budget ends, not at the
        // next burst; the burst's first went at once.
        for burst in 0..4 {
            let first = updates[burst * 4];
            let last = updates[burst * 4 + 3];
            assert!(
                sent.contains(&first),
                "{first:?} not sent at once: {sent:?}"
            );
            let went = sent.iter().find(|s| s.1 == last.1).expect("burst end sent");
            assert!(went.0 - last.0 <= 9, "{last:?} waited until {went:?}");
        }
        // The very last position goes once more after REPEAT_AFTER, then
        // nothing until a heartbeat refresh.
        let final_x = updates.last().unwrap().1;
        assert_eq!(
            sent.iter().filter(|s| s.1 == final_x).count(),
            2,
            "{sent:?}"
        );
    }

    /// A mouse reporting about once a budget, a little early or late each
    /// time (a 125 Hz mouse at 120 fps). A late send must not delay the
    /// sends after it: counted from the last send, every later position
    /// waited for the lag of the latest one.
    #[test]
    fn positions_about_once_a_budget_go_without_a_growing_lag() {
        let start = Instant::now();
        let mut sender = PointerSender::default();
        sender.serve(Some(1));
        let mut seed = 0x2545_F491u32;
        let mut delays = Vec::new();
        let period_us = BUDGET.as_micros() as u64;
        for k in 0..600u64 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let arrived = k * period_us + u64::from(seed % 1500);
            let latest = Some(at(k as i32));
            // Poll at arrival, then every 100 us until it went.
            let mut t = arrived;
            loop {
                let now = start + Duration::from_micros(t);
                if states(&poll_all(
                    &mut sender,
                    now,
                    BUDGET,
                    (Some(shape(1)), latest),
                ))
                .contains(&(k as i32))
                {
                    delays.push(t - arrived);
                    break;
                }
                t += 100;
                assert!(t < arrived + 2 * period_us, "position {k} never went");
            }
        }
        delays.sort_unstable();
        let mean = delays.iter().sum::<u64>() / delays.len() as u64;
        let p90 = delays[delays.len() * 9 / 10];
        assert!(mean < 1_000 && p90 < 2_000, "mean {mean} us, p90 {p90} us");
    }

    #[test]
    fn a_still_pointer_sends_nothing_until_a_refresh() {
        let start = Instant::now();
        let mut sender = PointerSender::default();
        sender.serve(Some(1));
        let latest = (Some(shape(1)), Some(at(5)));
        assert_eq!(
            poll_all(&mut sender, start, BUDGET, latest.clone()).len(),
            2
        );
        assert_eq!(
            states(&poll_all(
                &mut sender,
                start + REPEAT_AFTER,
                BUDGET,
                latest.clone()
            )),
            [5]
        );
        for ms in 101..2000 {
            let now = start + Duration::from_millis(ms);
            assert!(
                poll_all(&mut sender, now, BUDGET, latest.clone()).is_empty(),
                "{ms} ms"
            );
            assert_eq!(sender.next_poll(BUDGET, latest.1, Some(1)), None);
        }
        let now = start + Duration::from_secs(2);
        sender.refresh(now);
        assert_eq!(sender.next_poll(BUDGET, latest.1, Some(1)), Some(now));
        assert_eq!(states(&poll_all(&mut sender, now, BUDGET, latest)), [5]);
    }

    #[test]
    fn hiding_the_pointer_is_a_change_that_goes() {
        let start = Instant::now();
        let mut sender = PointerSender::default();
        sender.serve(Some(1));
        poll_all(&mut sender, start, BUDGET, (None, Some(at(5))));
        let hidden = Position {
            visible: false,
            ..at(5)
        };
        assert_eq!(
            sender.next_poll(BUDGET, Some(hidden), None),
            Some(start + BUDGET)
        );
        let sent = poll_all(&mut sender, start + BUDGET, BUDGET, (None, Some(hidden)));
        assert!(matches!(sent[..], [Send::State(state)] if state.flags & POINTER_VISIBLE == 0));
    }

    #[test]
    fn images_go_on_change_session_start_keyframes_and_when_a_report_lags() {
        let start = Instant::now();
        let mut sender = PointerSender::default();
        let shapes = |sent: &[Send]| {
            sent.iter()
                .filter_map(|send| match send {
                    Send::Shape(shape, _) => Some(shape.id),
                    Send::State(_) => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(
            sender
                .poll(start, BUDGET, (Some(shape(1)), None))
                .is_empty(),
            "no session"
        );
        sender.serve(Some(7));
        assert_eq!(
            shapes(&poll_all(
                &mut sender,
                start,
                BUDGET,
                (Some(shape(1)), None)
            )),
            [1]
        );
        assert!(sender
            .poll(start, BUDGET, (Some(shape(1)), None))
            .is_empty());
        assert_eq!(
            shapes(&poll_all(
                &mut sender,
                start,
                BUDGET,
                (Some(shape(2)), None)
            )),
            [2]
        );
        // A report sent before image 2 could arrive names image 1: no resend.
        let soon = start + SHAPE_RESEND_AFTER - Duration::from_millis(1);
        sender.report(1, soon);
        assert!(poll_all(&mut sender, soon, BUDGET, (Some(shape(2)), None)).is_empty());
        // A later one still names it: image 2 was lost on the way.
        let later = start + SHAPE_RESEND_AFTER;
        sender.report(1, later);
        assert_eq!(
            shapes(&poll_all(
                &mut sender,
                later,
                BUDGET,
                (Some(shape(2)), None)
            )),
            [2]
        );
        assert!(
            sender
                .poll(later, BUDGET, (Some(shape(2)), None))
                .is_empty(),
            "once a report"
        );
        let next = later + Duration::from_millis(500);
        sender.report(2, next);
        assert!(poll_all(&mut sender, next, BUDGET, (Some(shape(2)), None)).is_empty());
        let much_later = later + Duration::from_secs(5);
        assert!(sender
            .poll(much_later, BUDGET, (Some(shape(2)), None))
            .is_empty());
        sender.keyframe(much_later);
        assert_eq!(
            shapes(&poll_all(
                &mut sender,
                much_later,
                BUDGET,
                (Some(shape(2)), None)
            )),
            [2]
        );
        // A new session gets the image and position again.
        sender.serve(Some(8));
        let sent = poll_all(
            &mut sender,
            much_later,
            BUDGET,
            (Some(shape(2)), Some(at(1))),
        );
        assert_eq!(shapes(&sent), [2]);
        assert_eq!(states(&sent), [1]);
        sender.serve(None);
        assert!(sender
            .poll(much_later, BUDGET, (Some(shape(3)), Some(at(2))))
            .is_empty());
    }

    /// Review r2: over USB the tunnel's control queue takes 64 datagrams and
    /// refuses the rest. An image of more chunks than that resumes where the
    /// link refused it, also when it is due again meanwhile, and counts as
    /// sent only once every chunk went.
    #[test]
    fn a_refused_image_resumes_where_it_stopped_and_is_sent_only_when_whole() {
        let start = Instant::now();
        let mut sender = PointerSender::default();
        sender.serve(Some(1));
        let total = 194;
        let big = shape(1);
        let offered = |sent: &[Send]| {
            sent.iter().find_map(|send| match send {
                Send::Shape(shape, next) => Some((shape.id, *next)),
                Send::State(_) => None,
            })
        };
        assert_eq!(
            offered(&sender.poll(start, BUDGET, (Some(Arc::clone(&big)), None))),
            Some((1, 0))
        );
        // The queue takes 64, then refuses.
        sender.shape_progress(1, 64, total, true, start);
        assert_eq!(sender.repeat_due(BUDGET), Some(start + SHAPE_RETRY));
        let soon = start + SHAPE_RETRY / 2;
        assert_eq!(
            offered(&sender.poll(soon, BUDGET, (Some(Arc::clone(&big)), None))),
            None,
            "waits"
        );
        // A keyframe asks for it again: it carries on, it doesn't restart.
        sender.keyframe(soon);
        let retry = start + SHAPE_RETRY;
        assert_eq!(
            offered(&sender.poll(retry, BUDGET, (Some(Arc::clone(&big)), None))),
            Some((1, 64))
        );
        sender.shape_progress(1, 128, total, true, retry);
        // Not sent yet: a report naming nothing doesn't resend from 0.
        sender.report(0, retry + SHAPE_RESEND_AFTER);
        let later = retry + SHAPE_RETRY;
        assert_eq!(
            offered(&sender.poll(later, BUDGET, (Some(Arc::clone(&big)), None))),
            Some((1, 128))
        );
        sender.shape_progress(1, total, total, false, later);
        assert_eq!(
            offered(&sender.poll(later, BUDGET, (Some(Arc::clone(&big)), None))),
            None,
            "whole"
        );
        assert_eq!(sender.repeat_due(BUDGET), None);
        // A newer image replaces one still going.
        let next = later + SHAPE_RETRY;
        assert_eq!(
            offered(&sender.poll(next, BUDGET, (Some(shape(2)), None))),
            Some((2, 0))
        );
        sender.shape_progress(2, 10, total, true, next);
        assert_eq!(
            offered(&sender.poll(next + SHAPE_RETRY, BUDGET, (Some(shape(3)), None))),
            Some((3, 0))
        );
    }
}
