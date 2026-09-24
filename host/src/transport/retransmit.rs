//! Bounded, per-session storage of the exact first-transmission datagrams.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eternal_wire::v2::media::MEDIA_FLAG_RETRANSMIT;

const FRAME_LIMIT: usize = 96;
const BYTE_LIMIT: usize = 12 * 1024 * 1024;
// The iPad requests each fragment once, retries once after its RTT plus 5 ms,
// and once more after a WiFi stall. A longer interval would swallow the retry
// that replaces a lost resend.
const RESEND_INTERVAL: Duration = Duration::from_millis(5);

struct StoredFrame {
    epoch: u32,
    seq: u32,
    datagrams: Vec<Vec<u8>>,
    last_resend: Vec<Option<Instant>>,
    bytes: usize,
}

pub struct RetransmitRing {
    frames: VecDeque<StoredFrame>,
    bytes: usize,
    frame_limit: usize,
    byte_limit: usize,
}

impl Default for RetransmitRing {
    fn default() -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            frame_limit: FRAME_LIMIT,
            byte_limit: BYTE_LIMIT,
        }
    }
}

impl RetransmitRing {
    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    /// Takes fragments in their original index order, before fault injection.
    pub fn insert(&mut self, epoch: u32, seq: u32, datagrams: Vec<Vec<u8>>) {
        let bytes = datagrams.iter().map(Vec::len).sum::<usize>();
        if datagrams.is_empty() || bytes > self.byte_limit {
            return;
        }
        if let Some(index) = self
            .frames
            .iter()
            .position(|f| f.epoch == epoch && f.seq == seq)
        {
            self.bytes -= self.frames.remove(index).unwrap().bytes;
        }
        while self.frames.len() >= self.frame_limit || self.bytes + bytes > self.byte_limit {
            let Some(frame) = self.frames.pop_front() else {
                break;
            };
            self.bytes -= frame.bytes;
        }
        self.bytes += bytes;
        self.frames.push_back(StoredFrame {
            epoch,
            seq,
            last_resend: vec![None; datagrams.len()],
            datagrams,
            bytes,
        });
    }

    /// Ignores expired frames, conflicting fragment counts, and rate-limited
    /// indices. Only byte 4 changes; the saved originals are never modified.
    pub fn resend(
        &mut self,
        epoch: u32,
        seq: u32,
        frag_count: u16,
        missing: &[u16],
        now: Instant,
    ) -> Vec<Vec<u8>> {
        let Some(frame) = self
            .frames
            .iter_mut()
            .find(|f| f.epoch == epoch && f.seq == seq)
        else {
            return Vec::new();
        };
        if frame.datagrams.len() != usize::from(frag_count) {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(missing.len().min(64));
        for &index in missing.iter().take(64) {
            let index = usize::from(index);
            let Some(original) = frame.datagrams.get(index) else {
                continue;
            };
            if frame.last_resend[index]
                .is_some_and(|last| now.saturating_duration_since(last) < RESEND_INTERVAL)
            {
                continue;
            }
            let mut datagram = original.clone();
            if let Some(flags) = datagram.get_mut(4) {
                *flags |= MEDIA_FLAG_RETRANSMIT;
                frame.last_resend[index] = Some(now);
                result.push(datagram);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::media::{MediaHeader, MEDIA_HEADER_SIZE};

    fn frame(epoch: u32, seq: u32, count: u16) -> Vec<Vec<u8>> {
        (0..count)
            .map(|index| {
                let mut bytes = vec![0xAA; MEDIA_HEADER_SIZE + 4];
                MediaHeader {
                    session_id: 7,
                    stream_epoch: epoch,
                    frame_seq: seq,
                    frag_index: index,
                    frag_count: count,
                    is_keyframe: true,
                    is_retransmit: false,
                    capture_ts_us: 1234,
                    payload_len: 4,
                }
                .encode_into(&mut bytes);
                bytes
            })
            .collect()
    }

    #[test]
    fn lookup_changes_only_the_retransmit_flag_and_preserves_saved_bytes() {
        let original = frame(3, 42, 3);
        let mut ring = RetransmitRing::default();
        ring.insert(3, 42, original.clone());
        let resent = ring.resend(3, 42, 3, &[0, 2], Instant::now());
        assert_eq!(resent.len(), 2);
        for (mut bytes, index) in resent.into_iter().zip([0, 2]) {
            assert!(MediaHeader::decode(&bytes).unwrap().0.is_retransmit);
            bytes[4] &= !MEDIA_FLAG_RETRANSMIT;
            assert_eq!(bytes, original[index]);
        }
        assert_eq!(ring.frames[0].datagrams, original);
    }

    #[test]
    fn evicts_by_count_and_bytes() {
        let now = Instant::now();
        let mut ring = RetransmitRing::default();
        for seq in 1..=97 {
            ring.insert(1, seq, frame(1, seq, 1));
        }
        assert_eq!(ring.frames.len(), 96);
        assert!(ring.resend(1, 1, 1, &[0], now).is_empty());
        assert_eq!(ring.resend(1, 2, 1, &[0], now).len(), 1);

        let mut small = RetransmitRing {
            byte_limit: 3 * (MEDIA_HEADER_SIZE + 4),
            ..Default::default()
        };
        small.insert(1, 1, frame(1, 1, 2));
        small.insert(1, 2, frame(1, 2, 2));
        assert_eq!(small.frames.len(), 1);
        assert_eq!(small.bytes, 2 * (MEDIA_HEADER_SIZE + 4));
        assert!(small.resend(1, 1, 2, &[0], now).is_empty());
        small.insert(1, 3, frame(1, 3, 4));
        assert_eq!(
            small.frames.len(),
            1,
            "oversized frame must not displace useful history"
        );
    }

    #[test]
    fn enforces_count_epoch_and_per_fragment_five_ms_limit() {
        let mut ring = RetransmitRing::default();
        ring.insert(1, 7, frame(1, 7, 2));
        let now = Instant::now();
        assert!(ring.resend(2, 7, 2, &[0], now).is_empty());
        assert!(ring.resend(1, 7, 3, &[0], now).is_empty());
        assert!(ring.resend(1, 7, 2, &[2], now).is_empty());
        assert_eq!(ring.resend(1, 7, 2, &[0, 0], now).len(), 1);
        assert!(ring
            .resend(1, 7, 2, &[0], now + Duration::from_millis(4))
            .is_empty());
        assert_eq!(ring.resend(1, 7, 2, &[1], now).len(), 1);
        assert_eq!(
            ring.resend(1, 7, 2, &[0], now + Duration::from_millis(5))
                .len(),
            1
        );
        ring.clear();
        assert_eq!(ring.bytes, 0);
        assert!(ring.resend(1, 7, 2, &[0], now).is_empty());
    }
}
