use castr_proto::{Mode, Stats};

pub const MIN_BITRATE: u32 = 1_000_000;
const CUT_GUARD_US: u64 = 500_000;
const CLEAN_RAISE_US: u64 = 1_000_000;
const FLOOR_STEP_DOWN_US: u64 = 2_000_000;
const CLEAN_STEP_UP_US: u64 = 5_000_000;
/// Game mode: a decode queue that stays deep with no packet loss means the
/// receiver cannot decode this resolution in real time; bitrate cuts will not
/// help, so step the resolution down after this long.
const QUEUE_STEP_DOWN_US: u64 = 1_000_000;
const MAX_STEP_UP_WAIT_US: u64 = 60_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub bitrate_bps: u32,
    pub resolution: Resolution,
}

pub struct BitrateController {
    ceiling: u32,
    bitrate: u32,
    ladder: Vec<Resolution>,
    rung: usize,
    mode: Mode,
    last_cut_us: Option<u64>,
    clean_since_us: Option<u64>,
    at_floor_since_us: Option<u64>,
    last_raise_us: Option<u64>,
    last_step_up_us: Option<u64>,
    queue_high_since_us: Option<u64>,
    /// Clean time required before stepping resolution back up. Doubles every
    /// time a rung proves too heavy for the receiver's decoder, so a slow
    /// receiver is not bounced between resolutions every few seconds.
    step_up_wait_us: u64,
}

impl BitrateController {
    pub fn new(ceiling_bps: u32, initial_bps: u32, native: Resolution, mode: Mode) -> Self {
        let mut ladder = vec![native];
        // Rungs keep the native aspect ratio (a 1920x802 desktop becomes 1280x534,
        // not a squashed 1280x720); heights are rounded to even for 4:2:0.
        for width in [1280u32, 960] {
            if width < native.width {
                let height = (width * native.height / native.width) & !1;
                ladder.push(Resolution { width, height });
            }
        }
        Self {
            ceiling: ceiling_bps,
            bitrate: initial_bps.clamp(MIN_BITRATE, ceiling_bps),
            ladder,
            rung: 0,
            mode,
            last_cut_us: None,
            clean_since_us: None,
            at_floor_since_us: None,
            last_raise_us: None,
            last_step_up_us: None,
            queue_high_since_us: None,
            step_up_wait_us: CLEAN_STEP_UP_US,
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if mode == Mode::Quality {
            self.rung = 0;
        }
    }

    pub fn current(&self) -> Decision {
        Decision {
            bitrate_bps: self.bitrate,
            resolution: self.ladder[self.rung],
        }
    }

    pub fn on_stats(&mut self, stats: &Stats, now_us: u64) -> Option<Decision> {
        let before = self.current();
        let total = stats.fragments_lost + stats.fragments_received;
        let loss = if total == 0 {
            0.0
        } else {
            stats.fragments_lost as f64 / total as f64
        };
        let clean = loss < 0.005 && stats.decode_queue_depth <= 1;
        let cut_allowed = self
            .last_cut_us
            .is_none_or(|t| now_us.saturating_sub(t) >= CUT_GUARD_US);

        if loss > 0.02 && cut_allowed {
            self.bitrate = ((self.bitrate as f64 * 0.7) as u32).max(MIN_BITRATE);
            self.last_cut_us = Some(now_us);
        } else if stats.decode_queue_depth > 3 && cut_allowed {
            self.bitrate = ((self.bitrate as f64 * 0.85) as u32).max(MIN_BITRATE);
            self.last_cut_us = Some(now_us);
        }

        if clean {
            let since = *self.clean_since_us.get_or_insert(now_us);
            let raise_ref = self.last_raise_us.unwrap_or(since);
            if now_us.saturating_sub(raise_ref) >= CLEAN_RAISE_US && self.bitrate < self.ceiling {
                self.bitrate = (self.bitrate + self.ceiling / 20).min(self.ceiling);
                self.last_raise_us = Some(now_us);
            }
        } else {
            self.clean_since_us = None;
            self.last_raise_us = None;
        }

        if self.mode == Mode::Game {
            // Decoder too slow for this resolution (deep queue, clean network).
            if stats.decode_queue_depth > 3 && loss < 0.005 {
                let since = *self.queue_high_since_us.get_or_insert(now_us);
                if now_us.saturating_sub(since) >= QUEUE_STEP_DOWN_US
                    && self.rung + 1 < self.ladder.len()
                {
                    self.rung += 1;
                    self.queue_high_since_us = Some(now_us);
                    self.step_up_wait_us = (self.step_up_wait_us * 2).min(MAX_STEP_UP_WAIT_US);
                }
            } else {
                self.queue_high_since_us = None;
            }
            if self.bitrate == MIN_BITRATE {
                let since = *self.at_floor_since_us.get_or_insert(now_us);
                if now_us.saturating_sub(since) >= FLOOR_STEP_DOWN_US
                    && self.rung + 1 < self.ladder.len()
                {
                    self.rung += 1;
                    self.at_floor_since_us = Some(now_us);
                }
            } else {
                self.at_floor_since_us = None;
            }
            if let Some(since) = self.clean_since_us {
                let step_ref = self.last_step_up_us.map_or(since, |t| t.max(since));
                if now_us.saturating_sub(step_ref) >= self.step_up_wait_us && self.rung > 0 {
                    self.rung -= 1;
                    self.last_step_up_us = Some(now_us);
                }
            }
        }

        let after = self.current();
        (after != before).then_some(after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NATIVE: Resolution = Resolution {
        width: 1920,
        height: 1080,
    };
    fn st(lost: u32, received: u32, queue: u32) -> Stats {
        Stats {
            frames_received: 3,
            frames_dropped: 0,
            fragments_lost: lost,
            fragments_received: received,
            decode_queue_depth: queue,
            interval_ms: 100,
        }
    }
    fn ctl() -> BitrateController {
        BitrateController::new(10_000_000, 5_000_000, NATIVE, Mode::Game)
    }

    #[test]
    fn loss_over_two_percent_cuts_30_percent_once_per_500ms() {
        let mut c = ctl();
        let d = c.on_stats(&st(3, 97, 0), 0).unwrap();
        assert_eq!(d.bitrate_bps, 3_500_000);
        assert!(c.on_stats(&st(3, 97, 0), 100_000).is_none());
        assert!(c.on_stats(&st(3, 97, 0), 499_999).is_none());
        assert_eq!(
            c.on_stats(&st(3, 97, 0), 500_000).unwrap().bitrate_bps,
            2_450_000
        );
    }

    #[test]
    fn loss_at_two_percent_is_not_a_cut() {
        let mut c = ctl();
        assert!(c.on_stats(&st(2, 98, 0), 0).is_none());
    }

    #[test]
    fn deep_decode_queue_cuts_15_percent() {
        let mut c = ctl();
        assert_eq!(
            c.on_stats(&st(0, 100, 4), 0).unwrap().bitrate_bps,
            4_250_000
        );
        assert!(c.on_stats(&st(0, 100, 3), 600_000).is_none());
    }

    #[test]
    fn deep_queue_without_loss_steps_resolution_down_after_one_second() {
        let mut c = ctl();
        let mut t = 0;
        let mut res = c.current().resolution;
        while t < 1_000_000 {
            if let Some(d) = c.on_stats(&st(0, 100, 5), t) {
                res = d.resolution;
            }
            t += 100_000;
        }
        assert_eq!(res, NATIVE, "no step before a full second");
        let d = c.on_stats(&st(0, 100, 5), 1_000_000).unwrap();
        assert_eq!(d.resolution, Resolution { width: 1280, height: 720 });
        // A clean tick resets the timer.
        c.on_stats(&st(0, 100, 1), 1_100_000);
        let d = c.on_stats(&st(0, 100, 5), 2_000_000);
        assert!(d.is_none_or(|d| d.resolution.width == 1280));
    }

    #[test]
    fn queue_driven_step_down_doubles_the_clean_time_needed_to_step_up() {
        let mut c = ctl();
        // One second of deep queue: step down, and step-up now needs 10 s clean.
        for i in 0..=10 {
            c.on_stats(&st(0, 100, 5), i * 100_000);
        }
        assert_eq!(c.current().resolution.width, 1280);
        let mut t = 1_100_000;
        let mut width = 1280;
        while t < 1_100_000 + 9_900_000 {
            if let Some(d) = c.on_stats(&st(0, 100, 0), t) {
                width = d.resolution.width;
            }
            t += 100_000;
        }
        assert_eq!(width, 1280, "stepped up before 10 s clean");
        while t <= 1_100_000 + 10_100_000 {
            if let Some(d) = c.on_stats(&st(0, 100, 0), t) {
                width = d.resolution.width;
            }
            t += 100_000;
        }
        assert_eq!(width, 1920);
    }

    #[test]
    fn ladder_keeps_the_native_aspect_ratio() {
        let c = BitrateController::new(
            10_000_000,
            5_000_000,
            Resolution { width: 1920, height: 802 },
            Mode::Game,
        );
        assert_eq!(c.ladder[1], Resolution { width: 1280, height: 534 });
        assert_eq!(c.ladder[2], Resolution { width: 960, height: 400 });
    }

    #[test]
    fn one_second_clean_adds_5_percent_of_ceiling() {
        let mut c = ctl();
        for i in 0..10 {
            assert!(
                c.on_stats(&st(0, 100, 0), i * 100_000).is_none(),
                "tick {i}"
            );
        }
        assert_eq!(
            c.on_stats(&st(0, 100, 1), 1_000_000).unwrap().bitrate_bps,
            5_500_000
        );
        assert!(c.on_stats(&st(0, 100, 0), 1_100_000).is_none());
    }

    #[test]
    fn dirty_interval_resets_clean_timer() {
        let mut c = ctl();
        for i in 0..9 {
            c.on_stats(&st(0, 100, 0), i * 100_000);
        }
        c.on_stats(&st(1, 99, 0), 900_000);
        assert!(c.on_stats(&st(0, 100, 0), 1_000_000).is_none());
    }

    #[test]
    fn clamps_to_floor_and_ceiling() {
        let mut c = BitrateController::new(10_000_000, 1_200_000, NATIVE, Mode::Quality);
        assert_eq!(
            c.on_stats(&st(50, 50, 0), 0).unwrap().bitrate_bps,
            MIN_BITRATE
        );
        let mut c = BitrateController::new(10_000_000, 9_800_000, NATIVE, Mode::Quality);
        let mut t = 0;
        let mut last = None;
        for _ in 0..=10 {
            last = c.on_stats(&st(0, 100, 0), t).or(last);
            t += 100_000;
        }
        assert_eq!(last.unwrap().bitrate_bps, 10_000_000);
    }

    #[test]
    fn game_mode_steps_resolution_down_after_2s_at_floor_and_up_after_5s_clean() {
        let mut c = BitrateController::new(10_000_000, MIN_BITRATE, NATIVE, Mode::Game);
        let mut t = 0;
        let mut last = c.current();
        for _ in 0..21 {
            if let Some(d) = c.on_stats(&st(5, 95, 0), t) {
                last = d;
            }
            t += 100_000;
        }
        assert_eq!(
            last.resolution,
            Resolution {
                width: 1280,
                height: 720
            }
        );
        assert_eq!(last.bitrate_bps, MIN_BITRATE);
        for _ in 0..51 {
            if let Some(d) = c.on_stats(&st(0, 100, 0), t) {
                last = d;
            }
            t += 100_000;
        }
        assert_eq!(last.resolution, NATIVE);
    }

    #[test]
    fn quality_mode_never_changes_resolution() {
        let mut c = BitrateController::new(10_000_000, MIN_BITRATE, NATIVE, Mode::Quality);
        let mut t = 0;
        for _ in 0..30 {
            c.on_stats(&st(5, 95, 0), t);
            t += 100_000;
        }
        assert_eq!(c.current().resolution, NATIVE);
    }

    #[test]
    fn small_native_skips_larger_rungs() {
        let small = Resolution {
            width: 1280,
            height: 720,
        };
        let mut c = BitrateController::new(10_000_000, MIN_BITRATE, small, Mode::Game);
        let mut t = 0;
        let mut last = c.current();
        for _ in 0..21 {
            if let Some(d) = c.on_stats(&st(5, 95, 0), t) {
                last = d;
            }
            t += 100_000;
        }
        assert_eq!(
            last.resolution,
            Resolution {
                width: 960,
                height: 540
            }
        );
    }
}
