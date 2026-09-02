use castr_proto::{frame_newer_or_eq, CompleteFrame, Mode};
use std::collections::VecDeque;

const QUALITY_DELAY_US: u64 = 150_000;
/// Game mode: frames a slow decoder may fall behind before the current GOP is
/// abandoned and a keyframe requested (about 1 s at 30 fps). This is the last
/// resort; the queue depth is reported to the sender, which lowers resolution
/// long before this trips.
const GAME_MAX_DEPTH: usize = 30;
/// Quality mode: how late the head frame may be before the GOP is abandoned.
const QUALITY_MAX_LATE_US: u64 = 500_000;
/// How long to hold frames behind a missing predecessor before treating it as
/// lost. NACK repair of a keyframe takes one RTT plus the 20 ms NACK tick.
const GAP_WAIT_US: u64 = 150_000;
/// Frames held while waiting for a keyframe or a gap, before the oldest go.
const WAIT_MAX_DEPTH: usize = 120;

pub struct JitterBuffer {
    mode: Mode,
    interval_us: u64,
    frames: VecDeque<CompleteFrame>,
    last_popped: Option<u32>,
    base_us: Option<i64>,
    dropped: u32,
    /// The decoder has no usable reference (start of stream, a decode error, or an
    /// abandoned GOP): hold deltas and hand out the next keyframe as soon as it
    /// arrives, regardless of mode timing.
    need_keyframe: bool,
    /// When the head frame's predecessor first went missing. Frame numbers are
    /// contiguous per stream, so a gap means the predecessor is still being
    /// repaired (NACK) or is lost; we wait a little before giving up on the GOP.
    gap_since: Option<u64>,
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
            need_keyframe: true,
            gap_since: None,
        }
    }

    /// The decoder lost its reference (a decode error).
    pub fn require_keyframe(&mut self) {
        self.need_keyframe = true;
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.flush();
    }
    /// Discard everything queued. The reference chain is broken by that, so the
    /// next frame out is a keyframe.
    pub fn flush(&mut self) {
        self.frames.clear();
        self.base_us = None;
        self.need_keyframe = true;
        self.gap_since = None;
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

    pub fn keyframe_needed(&self) -> bool {
        self.need_keyframe
    }

    /// Index of the newest keyframe among frames[0..=idx], if any.
    fn newest_keyframe(&self, idx: usize) -> Option<usize> {
        (0..=idx).rev().find(|&i| self.frames[i].keyframe)
    }

    /// Too far behind and no keyframe to jump to: throw the GOP away and wait for
    /// the IDR the network loop will request. Skipping a delta frame instead
    /// would only produce a "reference lost" decode error.
    fn give_up_on_gop(&mut self) {
        self.dropped += self.frames.len() as u32;
        self.frames.clear();
        self.need_keyframe = true;
    }

    /// Pop frames[idx], dropping everything before it.
    fn take(&mut self, idx: usize) -> CompleteFrame {
        for _ in 0..idx {
            self.frames.pop_front();
            self.dropped += 1;
        }
        let f = self.frames.pop_front().unwrap();
        self.last_popped = Some(f.frame_number);
        self.gap_since = None;
        f
    }

    /// Bound the queue while we are holding frames for something that may never
    /// come, dropping the oldest.
    fn trim_waiting(&mut self) {
        while self.frames.len() > WAIT_MAX_DEPTH {
            self.frames.pop_front();
            self.dropped += 1;
        }
    }

    pub fn pop(&mut self, now_us: u64) -> Option<CompleteFrame> {
        if self.frames.is_empty() {
            return None;
        }
        if self.need_keyframe {
            // Hold deltas rather than dropping them: a keyframe that is still being
            // repaired sorts in front of them, and they play on from it.
            return match self.newest_keyframe(self.frames.len() - 1) {
                Some(i) => {
                    self.need_keyframe = false;
                    Some(self.take(i))
                }
                None => {
                    self.trim_waiting();
                    None
                }
            };
        }
        // Frame numbers are contiguous, so a head frame that does not follow the
        // last one out is missing its reference. Wait for it (NACK repair), then
        // give up on the GOP. A keyframe at the head never needs a predecessor.
        if let Some(last) = self.last_popped {
            let head = &self.frames[0];
            if !head.keyframe && head.frame_number != last.wrapping_add(1) {
                if let Some(k) = self.newest_keyframe(self.frames.len() - 1) {
                    return Some(self.take(k));
                }
                let since = *self.gap_since.get_or_insert(now_us);
                if now_us.saturating_sub(since) > GAP_WAIT_US {
                    self.give_up_on_gop();
                } else {
                    self.trim_waiting();
                }
                return None;
            }
        }
        // Delta frames reference the frame before them, so the only frame we may
        // ever jump forward to is a keyframe; otherwise frames go out in order.
        match self.mode {
            Mode::Game => {
                if let Some(k) = self.newest_keyframe(self.frames.len() - 1) {
                    return Some(self.take(k));
                }
                if self.frames.len() > GAME_MAX_DEPTH {
                    self.give_up_on_gop();
                    return None;
                }
                Some(self.take(0))
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
                if last_due > 0 && lateness > self.interval_us as i64 {
                    if let Some(k) = self.newest_keyframe(last_due) {
                        return Some(self.take(k));
                    }
                    if lateness > QUALITY_MAX_LATE_US as i64 {
                        self.give_up_on_gop();
                        return None;
                    }
                }
                Some(self.take(0))
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

    /// Fresh buffer with keyframe 0 already delivered, so tests start mid-stream.
    fn started(mode: Mode) -> JitterBuffer {
        let mut j = JitterBuffer::new(mode, IV);
        j.push(f(0, 0, true), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 0);
        j
    }

    #[test]
    fn stream_starts_with_a_keyframe_holding_earlier_deltas() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        assert!(j.keyframe_needed());
        j.push(f(1, IV, false), IV);
        assert!(j.pop(IV).is_none());
        assert_eq!(j.dropped(), 0);
        // The keyframe was slow to reassemble and sorts in front of the delta.
        j.push(f(0, 0, true), 2 * IV);
        assert_eq!(j.pop(2 * IV).unwrap().frame_number, 0);
        assert!(!j.keyframe_needed());
        assert_eq!(j.pop(2 * IV).unwrap().frame_number, 1);
    }

    #[test]
    fn require_keyframe_holds_deltas_until_a_keyframe_arrives() {
        let mut j = started(Mode::Game);
        j.require_keyframe();
        j.push(f(1, IV, false), IV);
        j.push(f(2, 2 * IV, false), 2 * IV);
        assert!(j.pop(2 * IV).is_none());
        assert_eq!(j.dropped(), 0);
        j.push(f(3, 3 * IV, true), 3 * IV);
        j.push(f(4, 4 * IV, false), 4 * IV);
        assert_eq!(j.pop(4 * IV).unwrap().frame_number, 3);
        assert_eq!(j.dropped(), 2);
        // Requirement cleared: the delta after the keyframe flows normally.
        assert_eq!(j.pop(4 * IV).unwrap().frame_number, 4);
        assert_eq!(j.dropped(), 0);
    }

    #[test]
    fn require_keyframe_in_quality_mode_ignores_the_playout_delay() {
        let mut j = started(Mode::Quality);
        j.require_keyframe();
        j.push(f(1, IV, true), IV);
        assert_eq!(j.pop(IV).unwrap().frame_number, 1);
    }

    #[test]
    fn gap_waits_for_the_missing_predecessor() {
        let mut j = started(Mode::Game);
        // Frame 1 (a keyframe being NACK-repaired) is missing; 2 arrived first.
        j.push(f(2, 2 * IV, false), 2 * IV);
        assert!(j.pop(2 * IV).is_none());
        assert!(j.pop(2 * IV + GAP_WAIT_US / 2).is_none());
        assert!(!j.keyframe_needed());
        j.push(f(1, IV, true), 2 * IV + GAP_WAIT_US / 2);
        assert_eq!(j.pop(2 * IV + GAP_WAIT_US / 2).unwrap().frame_number, 1);
        assert_eq!(j.pop(2 * IV + GAP_WAIT_US / 2).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 0);
    }

    #[test]
    fn gap_gives_up_on_the_gop_after_the_wait() {
        let mut j = started(Mode::Game);
        j.push(f(2, 2 * IV, false), 2 * IV);
        assert!(j.pop(2 * IV).is_none());
        assert!(j.pop(2 * IV + GAP_WAIT_US + 1).is_none());
        assert!(j.keyframe_needed());
        assert_eq!(j.dropped(), 1);
    }

    #[test]
    fn gap_jumps_to_a_later_keyframe_immediately() {
        let mut j = started(Mode::Game);
        j.push(f(2, 2 * IV, false), 2 * IV);
        j.push(f(3, 3 * IV, true), 3 * IV);
        assert_eq!(j.pop(3 * IV).unwrap().frame_number, 3);
        assert_eq!(j.dropped(), 1);
    }

    #[test]
    fn game_never_skips_a_delta_frame() {
        let mut j = started(Mode::Game);
        j.push(f(1, IV, false), 0);
        j.push(f(2, 2 * IV, false), 0);
        j.push(f(3, 3 * IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 1);
        assert_eq!(j.pop(0).unwrap().frame_number, 2);
        assert_eq!(j.pop(0).unwrap().frame_number, 3);
        assert_eq!(j.dropped(), 0);
        assert!(j.pop(0).is_none());
    }

    #[test]
    fn game_jumps_to_newest_keyframe_then_plays_deltas_in_order() {
        let mut j = started(Mode::Game);
        j.push(f(1, IV, false), 0);
        j.push(f(2, 2 * IV, true), 0);
        j.push(f(3, 3 * IV, false), 0);
        j.push(f(4, 4 * IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 1);
        assert_eq!(j.depth(), 2);
        assert_eq!(j.pop(0).unwrap().frame_number, 3);
        assert_eq!(j.pop(0).unwrap().frame_number, 4);
        assert_eq!(j.dropped(), 0);
        assert!(j.pop(0).is_none());
    }

    #[test]
    fn game_abandons_gop_and_asks_for_keyframe_when_too_far_behind() {
        let mut j = started(Mode::Game);
        let n_frames = GAME_MAX_DEPTH as u32 + 1;
        for n in 1..=n_frames {
            j.push(f(n, n as u64 * IV, false), 0);
        }
        assert!(j.pop(0).is_none());
        assert_eq!(j.dropped(), n_frames);
        assert!(j.keyframe_needed());
        // Deltas are held, then discarded when the requested keyframe shows up.
        j.push(f(20, 20 * IV, false), 0);
        assert!(j.pop(0).is_none());
        j.push(f(21, 21 * IV, true), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 21);
        assert_eq!(j.dropped(), 1);
        assert!(!j.keyframe_needed());
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
        // The opening keyframe goes straight out; the playout clock is anchored
        // to its arrival and the deltas after it follow the schedule.
        assert_eq!(j.pop(0).unwrap().frame_number, 1);
        assert!(j.pop(150_000 + IV - 1).is_none());
        assert_eq!(j.pop(150_000 + IV).unwrap().frame_number, 2);
    }

    #[test]
    fn quality_late_deltas_still_play_in_order() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        assert_eq!(j.pop(150_000).unwrap().frame_number, 1);
        j.push(f(2, IV, false), 150_000 + IV);
        j.push(f(3, 2 * IV, false), 150_000 + 2 * IV);
        let now = 150_000 + 3 * IV + 1;
        assert_eq!(j.pop(now).unwrap().frame_number, 2);
        assert_eq!(j.pop(now).unwrap().frame_number, 3);
        assert_eq!(j.dropped(), 0);
    }

    #[test]
    fn quality_skips_to_a_late_keyframe_and_abandons_gop_when_hopelessly_late() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        assert_eq!(j.pop(150_000).unwrap().frame_number, 1);
        j.push(f(2, IV, false), 150_000 + IV);
        j.push(f(3, 2 * IV, true), 150_000 + 2 * IV);
        assert_eq!(j.pop(150_000 + 3 * IV + 1).unwrap().frame_number, 3);
        assert_eq!(j.dropped(), 1);
        j.push(f(4, 3 * IV, false), 150_000 + 3 * IV);
        j.push(f(5, 4 * IV, false), 150_000 + 4 * IV);
        assert!(j.pop(150_000 + 3 * IV + QUALITY_MAX_LATE_US + 1).is_none());
        assert_eq!(j.dropped(), 2);
        assert!(j.keyframe_needed());
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
        // Frame 0 follows u32::MAX; it must sort after it and count as contiguous.
        j.push(f(0, IV, false), 0);
        j.push(f(u32::MAX, 0, true), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, u32::MAX);
        assert_eq!(j.pop(0).unwrap().frame_number, 0);
        assert_eq!(j.dropped(), 0);
    }
}
