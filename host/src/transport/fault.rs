//! Deterministic first-transmission faults for stream tests. Control traffic
//! and repair datagrams bypass this queue. Disabled in normal operation.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const QUEUE_LIMIT: usize = 4096;
const REORDER_WAIT: Duration = Duration::from_millis(1);

pub struct FaultInjector {
    drop_rate: f64,
    reorder_rate: f64,
    jitter: Duration,
    rng: u64,
    order: u64,
    queue: BTreeMap<(Instant, u64), Vec<u8>>,
    held: Option<(Instant, Vec<u8>)>,
}

impl FaultInjector {
    pub fn new(drop_rate: f64, reorder_rate: f64, jitter: Duration) -> Self {
        Self {
            drop_rate,
            reorder_rate,
            jitter,
            rng: 0x9E37_79B9_7F4A_7C15,
            order: 0,
            queue: BTreeMap::new(),
            held: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.drop_rate > 0.0 || self.reorder_rate > 0.0 || !self.jitter.is_zero()
    }

    pub fn clear(&mut self) {
        self.queue.clear();
        self.held = None;
    }

    fn random(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    fn schedule(&mut self, datagram: Vec<u8>, now: Instant) {
        if self.queue.len() >= QUEUE_LIMIT {
            return;
        }
        let delay = self.jitter.mul_f64(self.random());
        self.order = self.order.wrapping_add(1);
        self.queue.insert((now + delay, self.order), datagram);
    }

    pub fn push(&mut self, datagram: &[u8], now: Instant) -> Vec<Vec<u8>> {
        let mut ready = self.drain_due(now);
        if self.drop_rate > 0.0 && self.random() < self.drop_rate {
            return ready;
        }
        if let Some((_, previous)) = self.held.take() {
            self.schedule(datagram.to_vec(), now);
            self.schedule(previous, now);
        } else if self.reorder_rate > 0.0 && self.random() < self.reorder_rate {
            self.held = Some((now + REORDER_WAIT, datagram.to_vec()));
        } else {
            self.schedule(datagram.to_vec(), now);
        }
        ready.extend(self.drain_due(now));
        ready
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.queue
            .first_key_value()
            .map(|(key, _)| key.0)
            .into_iter()
            .chain(self.held.as_ref().map(|(at, _)| *at))
            .min()
    }

    pub fn drain_due(&mut self, now: Instant) -> Vec<Vec<u8>> {
        if self.held.as_ref().is_some_and(|(at, _)| *at <= now) {
            let (at, datagram) = self.held.take().unwrap();
            self.schedule(datagram, at);
        }
        let mut result = Vec::new();
        while self
            .queue
            .first_key_value()
            .is_some_and(|(key, _)| key.0 <= now)
        {
            result.push(self.queue.pop_first().unwrap().1);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_and_adjacent_reorder_are_deterministic() {
        let now = Instant::now();
        let mut drop = FaultInjector::new(1.0, 0.0, Duration::ZERO);
        assert!(drop.push(&[1], now).is_empty());
        assert!(drop.next_deadline().is_none());
        let mut swap = FaultInjector::new(0.0, 1.0, Duration::ZERO);
        assert!(swap.push(&[1], now).is_empty());
        assert_eq!(swap.push(&[2], now), [vec![2], vec![1]]);
        assert!(swap.push(&[3], now).is_empty());
        assert_eq!(swap.drain_due(now + REORDER_WAIT), [vec![3]]);
    }

    #[test]
    fn jitter_stays_within_bound_and_reproduces_the_seed() {
        let now = Instant::now();
        let max = Duration::from_millis(12);
        let mut a = FaultInjector::new(0.05, 0.02, max);
        let mut b = FaultInjector::new(0.05, 0.02, max);
        for value in 0..200u8 {
            assert_eq!(a.push(&[value], now), b.push(&[value], now));
            assert_eq!(a.next_deadline(), b.next_deadline());
        }
        assert!(!a.queue.is_empty());
        assert!(a.queue.keys().all(|(at, _)| *at >= now && *at <= now + max));
        let first = a.drain_due(now + max + REORDER_WAIT);
        assert_eq!(first, b.drain_due(now + max + REORDER_WAIT));
        assert!(!first.is_empty());
        assert!(a.next_deadline().is_none());
    }

    #[test]
    fn queue_is_bounded_and_clear_discards_old_session_datagrams() {
        let now = Instant::now();
        let mut injector = FaultInjector::new(0.0, 0.0, Duration::from_secs(1));
        for _ in 0..QUEUE_LIMIT + 100 {
            injector.push(&[1], now);
        }
        assert_eq!(injector.queue.len(), QUEUE_LIMIT);
        injector.clear();
        assert!(injector.next_deadline().is_none());
    }
}
