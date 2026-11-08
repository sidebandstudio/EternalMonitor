//! Per-direction anti-replay window, after RFC 6479.
//!
//! A ring of 64-bit blocks covers [`WINDOW`] counters below the highest one
//! accepted so far, plus one spare block so advancing never clears a bit that
//! is still inside the window. A counter is accepted once, in any order, while
//! it is within [`WINDOW`] of the highest; anything older is dropped.
//!
//! Receivers use it in two steps, as RFC 4303 §3.4.3 does: [`check`] before
//! opening the AEAD (cheap, read-only), then [`check_and_mark`] after the
//! packet authenticated, under the same lock as the window. Only
//! authenticated packets ever move the window, and of two copies racing on
//! different threads only one passes the second step.
//!
//! [`check`]: ReplayWindow::check
//! [`check_and_mark`]: ReplayWindow::check_and_mark

/// Counters accepted below the highest seen: `top - counter < WINDOW`.
pub const WINDOW: u64 = 4096;

const BLOCK_BITS: u64 = 64;
const BLOCKS: usize = (WINDOW / BLOCK_BITS) as usize + 1;

#[derive(Clone)]
pub struct ReplayWindow {
    /// Highest counter marked so far, or `None` before the first.
    top: Option<u64>,
    blocks: [u64; BLOCKS],
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ReplayWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayWindow")
            .field("top", &self.top)
            .finish()
    }
}

impl ReplayWindow {
    pub const fn new() -> Self {
        Self {
            top: None,
            blocks: [0; BLOCKS],
        }
    }

    /// The highest counter accepted so far.
    pub fn top(&self) -> Option<u64> {
        self.top
    }

    /// True if `counter` would be accepted now. Changes nothing.
    pub fn check(&self, counter: u64) -> bool {
        let Some(top) = self.top else {
            return true;
        };
        if counter > top {
            return true;
        }
        if top - counter >= WINDOW {
            return false;
        }
        let (block, bit) = position(counter);
        self.blocks[block] & bit == 0
    }

    /// Accepts `counter` and records it, or returns false if it is too old or
    /// already recorded. Call only for authenticated packets.
    pub fn check_and_mark(&mut self, counter: u64) -> bool {
        if !self.check(counter) {
            return false;
        }
        match self.top {
            None => {
                self.blocks = [0; BLOCKS];
                self.top = Some(counter);
            }
            Some(top) if counter > top => {
                let current = top / BLOCK_BITS;
                let target = counter / BLOCK_BITS;
                let clear = (target - current).min(BLOCKS as u64);
                for step in 1..=clear {
                    self.blocks[((current + step) % BLOCKS as u64) as usize] = 0;
                }
                self.top = Some(counter);
            }
            Some(_) => {}
        }
        let (block, bit) = position(counter);
        self.blocks[block] |= bit;
        true
    }
}

fn position(counter: u64) -> (usize, u64) {
    (
        ((counter / BLOCK_BITS) % BLOCKS as u64) as usize,
        1u64 << (counter % BLOCK_BITS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The window's contract, stated without bitmaps.
    struct Model {
        seen: BTreeSet<u64>,
    }

    impl Model {
        fn would_accept(&self, counter: u64) -> bool {
            if let Some(&top) = self.seen.last() {
                if counter <= top && top - counter >= WINDOW {
                    return false;
                }
            }
            !self.seen.contains(&counter)
        }

        fn accept(&mut self, counter: u64) -> bool {
            self.would_accept(counter) && self.seen.insert(counter)
        }
    }

    #[test]
    fn accepts_once_in_any_order_inside_the_window() {
        let mut window = ReplayWindow::new();
        assert!(window.check_and_mark(10));
        assert!(!window.check_and_mark(10), "duplicate");
        assert!(window.check_and_mark(3), "late but inside");
        assert!(!window.check(3));
        assert!(window.check_and_mark(11));
        assert!(window.check_and_mark(0));
        assert_eq!(window.top(), Some(11));
    }

    #[test]
    fn edge_of_the_window_is_exactly_4096_wide() {
        let mut window = ReplayWindow::new();
        assert!(window.check_and_mark(5000));
        assert!(
            !window.check(5000 - WINDOW),
            "4096 below the top is outside"
        );
        assert!(
            window.check(5000 - WINDOW + 1),
            "4095 below the top is inside"
        );
        assert!(window.check_and_mark(5000 - WINDOW + 1));
        assert!(!window.check_and_mark(5000 - WINDOW + 1));
    }

    #[test]
    fn a_big_jump_forgets_everything_older() {
        let mut window = ReplayWindow::new();
        for counter in 0..100 {
            assert!(window.check_and_mark(counter));
        }
        assert!(window.check_and_mark(1_000_000));
        assert!(!window.check(99));
        // Bits from before the jump must not linger in reused blocks.
        for counter in (1_000_000 - WINDOW + 1)..1_000_000 {
            assert!(window.check(counter), "{counter}");
        }
    }

    #[test]
    fn check_alone_never_moves_the_window() {
        let mut window = ReplayWindow::new();
        assert!(window.check_and_mark(1));
        assert!(window.check(1_000_000));
        assert_eq!(window.top(), Some(1));
        assert!(window.check_and_mark(2));
    }

    #[test]
    fn matches_the_model_on_reordered_duplicated_streams() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..50 {
            let mut window = ReplayWindow::new();
            let mut model = Model {
                seen: BTreeSet::new(),
            };
            let mut base = 0u64;
            for _ in 0..20_000 {
                let r = next();
                let counter = match r % 10 {
                    0 => base.saturating_sub(r % 5000), // old or very old
                    1 => base + (r % 70_000),           // jump ahead
                    _ => base + (r % 64),               // small reorder
                };
                base = base.max(counter);
                assert_eq!(window.check(counter), model.would_accept(counter));
                assert_eq!(window.check_and_mark(counter), model.accept(counter));
            }
        }
    }

    #[test]
    fn racing_copies_pass_only_once() {
        use std::sync::{Arc, Barrier, Mutex};
        let window = Arc::new(Mutex::new(ReplayWindow::new()));
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let window = window.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut passed = 0;
                    barrier.wait();
                    for counter in 0..2000u64 {
                        if window.lock().unwrap().check(counter)
                            && window.lock().unwrap().check_and_mark(counter)
                        {
                            passed += 1;
                        }
                    }
                    passed
                })
            })
            .collect();
        let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 2000);
    }
}
