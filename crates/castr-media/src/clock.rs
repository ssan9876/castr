const STALE_US: u64 = 200_000;

#[derive(Default)]
pub struct AvClock {
    /// (sender ts, receiver now) at the last audio update.
    audio_anchor: Option<(u64, u64)>,
    /// receiver_now - sender_ts learned when video runs without audio.
    video_offset: Option<i64>,
}

impl AvClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn audio_played(&mut self, ts_us: u64, now_us: u64) {
        self.audio_anchor = Some((ts_us, now_us));
        self.video_offset = Some(now_us as i64 - ts_us as i64);
    }

    pub fn audio_stale(&self, now_us: u64) -> bool {
        match self.audio_anchor {
            Some((_, at)) => now_us.saturating_sub(at) >= STALE_US,
            None => true,
        }
    }

    pub fn presented_ts(&self, now_us: u64) -> Option<u64> {
        match (self.audio_anchor, self.video_offset) {
            (Some((ts, at)), _) if !self.audio_stale(now_us) => {
                Some(ts + now_us.saturating_sub(at))
            }
            (_, Some(off)) => Some((now_us as i64 - off).max(0) as u64),
            _ => None,
        }
    }

    pub fn video_due(&mut self, frame_ts_us: u64, now_us: u64) -> bool {
        if self.video_offset.is_none() {
            self.video_offset = Some(now_us as i64 - frame_ts_us as i64);
            return true;
        }
        self.presented_ts(now_us)
            .map(|p| p >= frame_ts_us)
            .unwrap_or(true)
    }

    pub fn drift_ratio(&self, buffered_audio_us: u64, target_us: u64) -> f64 {
        let err = buffered_audio_us as f64 - target_us as f64;
        (1.0 + err / 40_000_000.0).clamp(0.995, 1.005)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_follows_audio_when_audio_flowing() {
        let mut c = AvClock::new();
        c.audio_played(1_000_000, 50_000);
        assert_eq!(c.presented_ts(50_000), Some(1_000_000));
        assert_eq!(c.presented_ts(60_000), Some(1_010_000));
        assert!(!c.video_due(1_020_000, 60_000));
        assert!(c.video_due(1_020_000, 70_000));
    }

    #[test]
    fn audio_stale_after_200ms() {
        let mut c = AvClock::new();
        c.audio_played(0, 0);
        assert!(!c.audio_stale(199_999));
        assert!(c.audio_stale(200_000));
        assert!(AvClock::new().audio_stale(0));
    }

    #[test]
    fn video_without_audio_uses_learned_offset() {
        let mut c = AvClock::new();
        assert!(c.video_due(5_000_000, 100));
        assert!(!c.video_due(5_010_000, 5_000));
        assert!(c.video_due(5_010_000, 10_100));
    }

    #[test]
    fn stale_audio_falls_back_to_last_known_delta() {
        let mut c = AvClock::new();
        c.audio_played(1_000_000, 50_000);
        assert!(c.video_due(1_250_000, 300_000));
        assert!(!c.video_due(1_300_000, 300_000));
        assert!(c.video_due(1_300_000, 350_000));
    }

    #[test]
    fn drift_ratio_is_bounded_and_directional() {
        let c = AvClock::new();
        assert_eq!(c.drift_ratio(40_000, 40_000), 1.0);
        let fast = c.drift_ratio(200_000, 40_000);
        let slow = c.drift_ratio(0, 40_000);
        assert!(fast > 1.0 && fast <= 1.005);
        assert!(slow < 1.0 && slow >= 0.995);
    }
}
