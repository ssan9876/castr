use castr_proto::{frame_newer_or_eq, CompleteFrame, Mode};
use std::collections::VecDeque;

const QUALITY_DELAY_US: u64 = 150_000;

pub struct JitterBuffer {
    mode: Mode,
    interval_us: u64,
    frames: VecDeque<CompleteFrame>,
    last_popped: Option<u32>,
    base_us: Option<i64>,
    dropped: u32,
}

impl JitterBuffer {
    pub fn new(mode: Mode, frame_interval_us: u64) -> Self {
        Self {
            mode,
            interval_us: frame_interval_us,
            frames: VecDeque::new(),
            last_popped: None,
            base_us: None,
            dropped: 0,
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.flush();
    }
    pub fn flush(&mut self) {
        self.frames.clear();
        self.base_us = None;
    }
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
    pub fn dropped(&mut self) -> u32 {
        std::mem::take(&mut self.dropped)
    }

    pub fn push(&mut self, frame: CompleteFrame, now_us: u64) {
        if let Some(last) = self.last_popped {
            if !frame_newer_or_eq(frame.frame_number, last) || frame.frame_number == last {
                self.dropped += 1;
                return;
            }
        }
        if self.base_us.is_none() {
            self.base_us =
                Some(now_us as i64 - frame.timestamp_us as i64 + QUALITY_DELAY_US as i64);
        }
        let pos = self
            .frames
            .iter()
            .position(|f| frame_newer_or_eq(f.frame_number, frame.frame_number));
        match pos {
            Some(i) if self.frames[i].frame_number == frame.frame_number => {}
            Some(i) => self.frames.insert(i, frame),
            None => self.frames.push_back(frame),
        }
    }

    /// frames[0..=idx] are candidates. Return the newest keyframe among them if any, else idx.
    fn choose(&self, idx: usize) -> usize {
        (0..=idx)
            .rev()
            .find(|&i| self.frames[i].keyframe)
            .unwrap_or(idx)
    }

    /// Pop frames[idx], dropping everything before it.
    fn take(&mut self, idx: usize) -> CompleteFrame {
        for _ in 0..idx {
            self.frames.pop_front();
            self.dropped += 1;
        }
        let f = self.frames.pop_front().unwrap();
        self.last_popped = Some(f.frame_number);
        f
    }

    pub fn pop(&mut self, now_us: u64) -> Option<CompleteFrame> {
        if self.frames.is_empty() {
            return None;
        }
        match self.mode {
            Mode::Game => {
                let idx = self.choose(self.frames.len() - 1);
                Some(self.take(idx))
            }
            Mode::Quality => {
                let base = self.base_us.expect("base set on push");
                let due_at = |f: &CompleteFrame| f.timestamp_us as i64 + base;
                if due_at(&self.frames[0]) > now_us as i64 {
                    return None;
                }
                let mut last_due = 0;
                while last_due + 1 < self.frames.len()
                    && due_at(&self.frames[last_due + 1]) <= now_us as i64
                {
                    last_due += 1;
                }
                let lateness = now_us as i64 - due_at(&self.frames[0]);
                let idx = if last_due > 0 && lateness > self.interval_us as i64 {
                    self.choose(last_due)
                } else {
                    0
                };
                Some(self.take(idx))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use castr_proto::STREAM_VIDEO;

    fn f(n: u32, ts: u64, key: bool) -> CompleteFrame {
        CompleteFrame {
            stream: STREAM_VIDEO,
            frame_number: n,
            timestamp_us: ts,
            keyframe: key,
            data: vec![n as u8],
        }
    }
    const IV: u64 = 33_333;

    #[test]
    fn game_returns_newest_and_drops_older_deltas() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(1, 0, false), 0);
        j.push(f(2, IV, false), 0);
        j.push(f(3, 2 * IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 3);
        assert_eq!(j.depth(), 0);
        assert_eq!(j.dropped(), 2);
        assert!(j.pop(0).is_none());
    }

    #[test]
    fn game_returns_keyframe_first_then_newest_delta() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(1, 0, false), 0);
        j.push(f(2, IV, true), 0);
        j.push(f(3, 2 * IV, false), 0);
        j.push(f(4, 3 * IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 1);
        assert_eq!(j.depth(), 2);
        assert_eq!(j.pop(0).unwrap().frame_number, 4);
        assert_eq!(j.dropped(), 1);
        assert!(j.pop(0).is_none());
    }

    #[test]
    fn game_discards_frames_older_than_last_popped() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(5, 0, true), 0);
        j.pop(0);
        j.push(f(4, 0, false), 0);
        j.push(f(5, 0, false), 0);
        assert_eq!(j.depth(), 0);
        assert_eq!(j.dropped(), 2);
    }

    #[test]
    fn quality_holds_frames_until_150ms_after_first_arrival() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 1_000_000, true), 0);
        j.push(f(2, 1_000_000 + IV, false), 0);
        assert!(j.pop(149_999).is_none());
        assert_eq!(j.pop(150_000).unwrap().frame_number, 1);
        assert!(j.pop(150_000 + IV - 1).is_none());
        assert_eq!(j.pop(150_000 + IV).unwrap().frame_number, 2);
    }

    #[test]
    fn quality_skips_frame_more_than_one_interval_late_when_newer_is_due() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        assert_eq!(j.pop(150_000).unwrap().frame_number, 1);
        j.push(f(2, IV, false), 150_000 + IV);
        j.push(f(3, 2 * IV, false), 150_000 + 2 * IV);
        let r = j.pop(150_000 + 3 * IV + 1).unwrap();
        assert_eq!(r.frame_number, 3);
        assert_eq!(j.dropped(), 1);
    }

    #[test]
    fn quality_keeps_late_keyframe() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        j.pop(150_000);
        j.push(f(2, IV, true), 150_000 + IV);
        j.push(f(3, 2 * IV, false), 150_000 + 2 * IV);
        let now = 150_000 + 3 * IV + 1;
        assert_eq!(j.pop(now).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 0);
        assert_eq!(j.pop(now).unwrap().frame_number, 3);
    }

    #[test]
    fn quality_one_interval_late_is_not_skipped() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        j.pop(150_000);
        j.push(f(2, IV, false), 150_000 + IV);
        j.push(f(3, 2 * IV, false), 150_000 + 2 * IV);
        assert_eq!(j.pop(150_000 + 2 * IV).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 0);
    }

    #[test]
    fn set_mode_flushes_and_resets_base() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        j.set_mode(Mode::Game);
        assert_eq!(j.depth(), 0);
        j.push(f(2, 0, true), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 2);
    }

    #[test]
    fn wrapping_order_is_respected() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(u32::MAX, 0, false), 0);
        j.push(f(0, IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 0);
        assert_eq!(j.dropped(), 1);
    }
}
