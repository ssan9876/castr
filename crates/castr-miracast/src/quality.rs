//! Loss numbers in, a bitrate ceiling out.
//!
//! A 2.4 GHz link that is losing packets does not recover by being asked for
//! more data. Falling is instant and rising is slow on purpose: that asymmetry
//! is what stops the request oscillating on a link that is marginal rather
//! than broken.

use std::time::{Duration, Instant};

/// Three rungs. More would be noise at 720p30 on a 2.4 GHz radio.
pub const LADDER: [u32; 3] = [8000, 4000, 2000];
/// A second is bad at five losses. At 720p30 a frame is roughly 24 datagrams,
/// so this sits well above the single-packet noise floor and well below a
/// visibly damaged second.
const BAD_SECOND: u64 = 5;
/// Clean seconds needed per step back up.
const CLEAN_PER_STEP: u32 = 10;
/// The ladder acts at most once per second, whatever the caller does.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub struct BitrateLadder {
    /// Index into `LADDER`; 0 is the top.
    rung: usize,
    clean: u32,
    last_loss: u64,
    last_sample: Option<Instant>,
}

impl Default for BitrateLadder {
    fn default() -> Self {
        Self::new()
    }
}

impl BitrateLadder {
    pub fn new() -> Self {
        Self {
            rung: 0,
            clean: 0,
            last_loss: 0,
            last_sample: None,
        }
    }

    pub fn current_kbps(&self) -> u32 {
        LADDER[self.rung]
    }

    /// Feeds the cumulative loss counter. Returns the new ceiling when it
    /// changed and the source must be told, and `None` otherwise.
    pub fn sample(&mut self, cumulative_loss: u64, now: Instant) -> Option<u32> {
        if let Some(t) = self.last_sample {
            if now.saturating_duration_since(t) < SAMPLE_INTERVAL {
                return None;
            }
        }
        // The very first call has no `last_sample` to compare against, so it
        // always bypasses the rate limit above. Intentional: there is nothing
        // to rate-limit against yet.
        self.last_sample = Some(now);
        let delta = cumulative_loss.saturating_sub(self.last_loss);
        self.last_loss = cumulative_loss;

        if delta >= BAD_SECOND {
            self.clean = 0;
            let floor = LADDER.len() - 1;
            if self.rung == floor {
                return None;
            }
            self.rung = floor;
            return Some(self.current_kbps());
        }
        self.clean += 1;
        if self.clean >= CLEAN_PER_STEP && self.rung > 0 {
            self.clean = 0;
            self.rung -= 1;
            return Some(self.current_kbps());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn one_bad_second_goes_straight_to_the_floor() {
        let mut l = BitrateLadder::new();
        let t0 = Instant::now();
        assert_eq!(l.current_kbps(), 8000);
        assert_eq!(l.sample(5, t0 + Duration::from_secs(1)), Some(2000));
        assert_eq!(l.current_kbps(), 2000);
    }

    #[test]
    fn a_quiet_second_asks_for_nothing() {
        let mut l = BitrateLadder::new();
        let t0 = Instant::now();
        assert_eq!(l.sample(4, t0 + Duration::from_secs(1)), None, "under the threshold");
        assert_eq!(l.current_kbps(), 8000);
    }

    #[test]
    fn ten_clean_seconds_buy_one_step_back_up() {
        let mut l = BitrateLadder::new();
        let mut t = Instant::now();
        t += Duration::from_secs(1);
        assert_eq!(l.sample(9, t), Some(2000));
        // Nine clean seconds are not enough.
        for _ in 0..9 {
            t += Duration::from_secs(1);
            assert_eq!(l.sample(9, t), None);
        }
        t += Duration::from_secs(1);
        assert_eq!(l.sample(9, t), Some(4000), "the tenth clean second");
        for _ in 0..9 {
            t += Duration::from_secs(1);
            assert_eq!(l.sample(9, t), None);
        }
        t += Duration::from_secs(1);
        assert_eq!(l.sample(9, t), Some(8000), "back to the top");
    }

    #[test]
    fn a_flapping_link_does_not_oscillate() {
        let mut l = BitrateLadder::new();
        let mut t = Instant::now();
        let mut loss = 0;
        // Alternating bad and clean seconds: the clean ones never accumulate to
        // ten, so after the first drop nothing further is ever requested.
        let mut requests = Vec::new();
        for i in 0..40 {
            t += Duration::from_secs(1);
            if i % 2 == 0 {
                loss += 5;
            }
            if let Some(k) = l.sample(loss, t) {
                requests.push(k);
            }
        }
        assert_eq!(requests, vec![2000], "one drop, no climb, no flapping");
    }

    #[test]
    fn samples_closer_than_a_second_are_ignored() {
        let mut l = BitrateLadder::new();
        let t0 = Instant::now();
        // Seed `last_sample`; the first-ever call bypasses the rate limit
        // itself, so this one proves nothing about the gate.
        assert_eq!(l.sample(0, t0 + Duration::from_millis(100)), None);
        // A loss well above the bad-second threshold would trigger a drop on
        // its own, so only the time gate can explain a `None` here.
        assert_eq!(l.sample(99, t0 + Duration::from_millis(200)), None,
                   "a burst inside one second is still one second");
    }

    #[test]
    fn the_floor_is_never_requested_twice() {
        let mut l = BitrateLadder::new();
        let mut t = Instant::now();
        t += Duration::from_secs(1);
        assert_eq!(l.sample(10, t), Some(2000));
        t += Duration::from_secs(1);
        assert_eq!(l.sample(20, t), None, "already at the floor");
    }
}
