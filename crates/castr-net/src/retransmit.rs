use bytes::Bytes;
use castr_proto::Nack;
use std::collections::VecDeque;

struct Sent {
    frame_number: u32,
    // No longer read by `lookup` now that the sender has no delta-repair
    // rule of its own, but `record`'s callers still supply it, and it costs
    // nothing to keep for the log and for any future policy.
    #[allow(dead_code)]
    keyframe: bool,
    sent_at_us: u64,
    fragments: Vec<Bytes>,
}

pub struct RetransmitBuffer {
    max_age_us: u64,
    frames: VecDeque<Sent>,
}

impl RetransmitBuffer {
    pub fn new(max_age_us: u64) -> Self {
        Self {
            max_age_us,
            frames: VecDeque::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn prune(&mut self, now_us: u64) {
        while let Some(front) = self.frames.front() {
            if now_us.saturating_sub(front.sent_at_us) > self.max_age_us {
                self.frames.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn record(
        &mut self,
        frame_number: u32,
        keyframe: bool,
        fragments: Vec<Bytes>,
        sent_at_us: u64,
    ) {
        self.frames.push_back(Sent {
            frame_number,
            keyframe,
            sent_at_us,
            fragments,
        });
        self.prune(sent_at_us);
    }

    /// Fragments to resend for this NACK, or empty if the frame is unknown or
    /// has aged out.
    ///
    /// There is deliberately no rule here about deltas: the receiver decides
    /// whether a repair can still arrive in time, because it is the only side
    /// that knows its own playout deadline, its mode and the round trip. A
    /// second opinion here could only ever contradict it.
    pub fn lookup(&mut self, nack: &Nack, now_us: u64) -> Vec<Bytes> {
        self.prune(now_us);
        let Some(sent) = self
            .frames
            .iter()
            .find(|s| s.frame_number == nack.frame_number)
        else {
            return Vec::new();
        };
        nack.missing
            .iter()
            .filter_map(|&i| sent.fragments.get(i as usize).cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use castr_proto::Nack;

    fn frags(n: usize) -> Vec<Bytes> {
        (0..n).map(|i| Bytes::from(vec![i as u8; 10])).collect()
    }

    #[test]
    fn keyframe_fragments_are_resent_within_max_age() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(10, true, frags(4), 0);
        let out = b.lookup(
            &Nack {
                frame_number: 10,
                missing: vec![1, 3],
            },
            400_000,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0][0], 1);
        assert_eq!(out[1][0], 3);
    }

    #[test]
    fn delta_fragments_are_resent_for_as_long_as_they_are_held() {
        // The receiver decides whether a repair is worth having: it is the
        // only side that knows its playout deadline. The sender's job is to
        // still have the fragment.
        let mut b = RetransmitBuffer::new(500_000);
        b.record(10, false, frags(4), 0);
        let nack = Nack {
            frame_number: 10,
            missing: vec![2],
        };
        let out = b.lookup(&nack, 100_000);
        assert_eq!(out.len(), 1, "a 100 ms old delta is still resent");
    }

    #[test]
    fn fragments_are_dropped_once_they_age_out() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(10, false, frags(4), 0);
        let nack = Nack {
            frame_number: 10,
            missing: vec![2],
        };
        assert!(b.lookup(&nack, 600_000).is_empty(), "past the retention window");
    }

    #[test]
    fn expired_frames_are_pruned() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(1, true, frags(1), 0);
        b.record(2, true, frags(1), 100_000);
        assert!(b
            .lookup(
                &Nack {
                    frame_number: 1,
                    missing: vec![0]
                },
                500_001,
            )
            .is_empty());
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn unknown_frame_or_index_yields_nothing() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(1, true, frags(2), 0);
        assert!(b
            .lookup(
                &Nack {
                    frame_number: 9,
                    missing: vec![0]
                },
                0,
            )
            .is_empty());
        assert!(b
            .lookup(
                &Nack {
                    frame_number: 1,
                    missing: vec![5]
                },
                0,
            )
            .is_empty());
    }
}
