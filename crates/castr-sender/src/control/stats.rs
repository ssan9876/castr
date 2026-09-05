//! What a running cast is sending, and how it is reported.
//!
//! Pure: every method takes the time it should use rather than reading the
//! clock, so the whole of it is testable without a cast.
//!
//! Every number here is **sent-side**. Wi-Fi Display is source-authoritative
//! and has no back-channel of receiver statistics, so nothing in this module
//! knows what arrived — only what was written to the socket. `keepalive_age`
//! is the single exception and the only thing that says anything at all about
//! the far end.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Matches the cadence of the Pi receiver's `perf:` line, so a cast watched
/// from both ends can be compared directly.
pub const WINDOW: Duration = Duration::from_secs(5);

/// The context a snapshot needs that the counters do not know.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub display: String,
    pub address: String,
    /// `None` until M4 has chosen one.
    pub mode: Option<String>,
    /// What the display advertised it can carry, when the radio read one.
    pub ceiling_mbps: Option<u16>,
    /// When the display was last heard from; `None` before its first reply.
    pub last_heard: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    video_units: u64,
    audio_units: u64,
    repeated_frames: u64,
    datagrams: u64,
    bytes: u64,
    /// Recent sends, for the windowed rate. Pruned on every push, so it holds
    /// one window's worth and no more.
    window: VecDeque<(Instant, u64)>,
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    /// One encoded access unit muxed. `repeated` when the desktop did not
    /// change and the last frame was sent again.
    pub fn video(&mut self, repeated: bool) {
        self.video_units += 1;
        if repeated {
            self.repeated_frames += 1;
        }
    }

    pub fn audio(&mut self) {
        self.audio_units += 1;
    }

    pub fn sent(&mut self, datagrams: u64, bytes: u64, now: Instant) {
        self.datagrams += datagrams;
        self.bytes += bytes;
        self.window.push_back((now, bytes));
        while let Some(&(t, _)) = self.window.front() {
            if now.duration_since(t) > WINDOW {
                self.window.pop_front();
            } else {
                break;
            }
        }
    }

    /// Throughput over the last [`WINDOW`], or over the whole cast if it is
    /// younger than that.
    ///
    /// Dividing by a full window during the first seconds would report a rate
    /// well under the truth, which is the moment someone is most likely to be
    /// staring at it wondering whether anything is happening.
    pub fn mbps(&self, now: Instant, elapsed: Duration) -> f64 {
        let bytes: u64 = self
            .window
            .iter()
            .filter(|(t, _)| now.duration_since(*t) <= WINDOW)
            .map(|(_, b)| b)
            .sum();
        let over = elapsed.min(WINDOW).max(Duration::from_millis(100));
        bytes as f64 * 8.0 / over.as_secs_f64() / 1e6
    }

    pub fn snapshot(&self, now: Instant, started: Instant, ctx: &Context) -> Snapshot {
        let elapsed = now.saturating_duration_since(started);
        Snapshot {
            display: ctx.display.clone(),
            address: ctx.address.clone(),
            mode: ctx.mode.clone(),
            ceiling_mbps: ctx.ceiling_mbps,
            mbps: self.mbps(now, elapsed),
            video_units: self.video_units,
            audio_units: self.audio_units,
            datagrams: self.datagrams,
            bytes: self.bytes,
            repeated_frames: self.repeated_frames,
            elapsed_s: elapsed.as_secs(),
            keepalive_age_s: ctx
                .last_heard
                .map(|t| now.saturating_duration_since(t).as_secs()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub display: String,
    pub address: String,
    pub mode: Option<String>,
    pub ceiling_mbps: Option<u16>,
    pub mbps: f64,
    pub video_units: u64,
    pub audio_units: u64,
    pub datagrams: u64,
    pub bytes: u64,
    pub repeated_frames: u64,
    pub elapsed_s: u64,
    pub keepalive_age_s: Option<u64>,
}

/// A value with no useful reading yet.
const UNKNOWN: &str = "-";

impl Snapshot {
    /// Tab-separated `key=value`, one line.
    ///
    /// Tabs and not spaces because a display name legitimately contains
    /// spaces — the television in range is named `75" Crystal UHD` — and a
    /// value that can swallow the next field is a parser bug waiting to be
    /// written.
    pub fn to_fields(&self) -> String {
        let mut f: Vec<String> = Vec::new();
        let mut put = |k: &str, v: String| f.push(format!("{k}={}", sanitise(&v)));
        put("display", self.display.clone());
        put("address", self.address.clone());
        put("mode", self.mode.clone().unwrap_or_else(|| UNKNOWN.into()));
        put(
            "ceiling_mbps",
            self.ceiling_mbps
                .map_or_else(|| UNKNOWN.into(), |c| c.to_string()),
        );
        put("mbps", format!("{:.1}", self.mbps));
        put("video_units", self.video_units.to_string());
        put("audio_units", self.audio_units.to_string());
        put("datagrams", self.datagrams.to_string());
        put("bytes", self.bytes.to_string());
        put("repeated_frames", self.repeated_frames.to_string());
        put("elapsed_s", self.elapsed_s.to_string());
        put(
            "keepalive_age_s",
            self.keepalive_age_s
                .map_or_else(|| UNKNOWN.into(), |s| s.to_string()),
        );
        f.join("\t")
    }
}

/// A field value can never contain the separator or end the line.
fn sanitise(v: &str) -> String {
    v.replace(['\t', '\r', '\n'], " ")
}

/// Splits a status body back into pairs, for printing. Deliberately not a
/// typed parse: the client renders what it is given, so a field added here
/// needs no matching change there.
pub fn fields(body: &str) -> Vec<(&str, &str)> {
    body.split('\t')
        .filter_map(|f| f.split_once('='))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            display: "DietPi".into(),
            address: "192.168.173.1:7236".into(),
            mode: Some("1280x720@30".into()),
            ceiling_mbps: Some(10),
            last_heard: None,
        }
    }

    #[test]
    fn counters_count() {
        let t0 = Instant::now();
        let mut s = Stats::new();
        s.video(false);
        s.video(true);
        s.video(true);
        s.audio();
        s.sent(7, 9212, t0);
        let snap = s.snapshot(t0, t0, &ctx());
        assert_eq!(snap.video_units, 3);
        assert_eq!(snap.repeated_frames, 2);
        assert_eq!(snap.audio_units, 1);
        assert_eq!(snap.datagrams, 7);
        assert_eq!(snap.bytes, 9212);
    }

    #[test]
    fn throughput_is_measured_over_the_window() {
        let t0 = Instant::now();
        let mut s = Stats::new();
        // 1 MB a second for 5 s: 5 MB is 40 Mbit, over 5 s that is 8 Mbps.
        for i in 0..5 {
            s.sent(1, 1_000_000, t0 + Duration::from_secs(i));
        }
        let now = t0 + Duration::from_secs(5);
        assert!((s.mbps(now, Duration::from_secs(5)) - 8.0).abs() < 0.5);
    }

    #[test]
    fn a_young_cast_is_not_averaged_over_a_window_it_has_not_lived() {
        let t0 = Instant::now();
        let mut s = Stats::new();
        // 1 Mbit in the first second is 1 Mbps, not 0.2.
        s.sent(1, 125_000, t0);
        let now = t0 + Duration::from_secs(1);
        assert!((s.mbps(now, Duration::from_secs(1)) - 1.0).abs() < 0.01);
    }

    #[test]
    fn sends_older_than_the_window_stop_counting() {
        let t0 = Instant::now();
        let mut s = Stats::new();
        s.sent(1, 10_000_000, t0);
        // Ten seconds later that burst is long gone from the rate, though the
        // total still remembers it.
        let now = t0 + Duration::from_secs(10);
        s.sent(1, 0, now);
        assert_eq!(s.mbps(now, Duration::from_secs(10)), 0.0);
        assert_eq!(s.snapshot(now, t0, &ctx()).bytes, 10_000_000);
    }

    #[test]
    fn the_window_does_not_grow_without_bound() {
        let t0 = Instant::now();
        let mut s = Stats::new();
        for i in 0..10_000u64 {
            s.sent(1, 1000, t0 + Duration::from_millis(i));
        }
        // Ten seconds of sends, a five second window: about half, never all.
        assert!(s.window.len() < 6000, "window held {}", s.window.len());
    }

    #[test]
    fn keepalive_age_is_absent_until_the_display_answers() {
        let t0 = Instant::now();
        let s = Stats::new();
        assert_eq!(s.snapshot(t0, t0, &ctx()).keepalive_age_s, None);

        let heard = Context {
            last_heard: Some(t0),
            ..ctx()
        };
        let now = t0 + Duration::from_secs(3);
        assert_eq!(s.snapshot(now, t0, &heard).keepalive_age_s, Some(3));
    }

    #[test]
    fn a_snapshot_renders_every_field() {
        let t0 = Instant::now();
        let snap = Stats::new().snapshot(t0, t0, &ctx());
        let body = snap.to_fields();
        let got = fields(&body);
        assert_eq!(got.len(), 12);
        assert_eq!(got.iter().find(|(k, _)| *k == "display").unwrap().1, "DietPi");
        assert_eq!(
            got.iter().find(|(k, _)| *k == "mode").unwrap().1,
            "1280x720@30"
        );
    }

    #[test]
    fn what_is_not_known_yet_renders_as_unknown_rather_than_zero() {
        // A zero ceiling would read as "the display can carry nothing", which
        // is a different and alarming claim from "it never said".
        let t0 = Instant::now();
        let bare = Context {
            mode: None,
            ceiling_mbps: None,
            ..ctx()
        };
        let body = Stats::new().snapshot(t0, t0, &bare).to_fields();
        let got = fields(&body);
        assert_eq!(got.iter().find(|(k, _)| *k == "mode").unwrap().1, "-");
        assert_eq!(
            got.iter().find(|(k, _)| *k == "ceiling_mbps").unwrap().1,
            "-"
        );
        assert_eq!(
            got.iter().find(|(k, _)| *k == "keepalive_age_s").unwrap().1,
            "-"
        );
    }

    #[test]
    fn a_display_name_with_spaces_stays_one_field() {
        let t0 = Instant::now();
        let c = Context {
            display: "75\" Crystal UHD".into(),
            ..ctx()
        };
        let body = Stats::new().snapshot(t0, t0, &c).to_fields();
        let got = fields(&body);
        assert_eq!(got.len(), 12);
        assert_eq!(
            got.iter().find(|(k, _)| *k == "display").unwrap().1,
            "75\" Crystal UHD"
        );
    }

    #[test]
    fn a_name_containing_the_separator_cannot_forge_a_field() {
        let t0 = Instant::now();
        let c = Context {
            display: "evil\tmbps=999".into(),
            ..ctx()
        };
        let body = Stats::new().snapshot(t0, t0, &c).to_fields();
        let got = fields(&body);
        assert_eq!(got.len(), 12);
        assert_eq!(
            got.iter().find(|(k, _)| *k == "mbps").unwrap().1.parse::<f64>(),
            Ok(0.0)
        );
    }
}
