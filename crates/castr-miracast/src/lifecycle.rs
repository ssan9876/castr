//! When the peer goes away, what happens to the group and the screen.
//!
//! The group's lifetime is the service's lifetime, not the session's. A peer
//! that drops for a moment must find everything exactly as it left it: same
//! group, same credentials, and for thirty seconds the same screen.

use std::time::{Duration, Instant};

/// How long the screen stays with a peer that vanished. Long enough to cover a
/// radio blip; short enough that a room is not stuck looking at a dead cast.
pub const HOLD: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Group up, credentials valid, listening.
    Advertising,
    Streaming,
    /// The peer vanished. The group and the screen are still theirs.
    Holding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An RTSP connection was accepted. The caller has already taken the
    /// display from the arbiter if it needed to.
    Connected,
    /// Reason reported to the user elsewhere, through `SinkOut::Ended`; the
    /// machine has no use for it.
    Ended,
    /// The radio itself failed; nothing about the group can be trusted.
    RadioError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    AcquireDisplay,
    ReleaseDisplay,
    ShowReconnecting,
    ClearOverlay,
    RebuildGroup,
}

pub struct Lifecycle {
    phase: Phase,
    holding_since: Option<Instant>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: Phase::Advertising,
            holding_since: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// True while the display is ours, which is both Streaming and Holding.
    fn holds_display(&self) -> bool {
        matches!(self.phase, Phase::Streaming | Phase::Holding)
    }

    pub fn on(&mut self, e: Event, now: Instant) -> Vec<Action> {
        match (self.phase, e) {
            (Phase::Advertising, Event::Connected) => {
                self.phase = Phase::Streaming;
                vec![Action::AcquireDisplay, Action::ClearOverlay]
            }
            (Phase::Holding, Event::Connected) => {
                // The display was never released, so it is not re-acquired.
                self.phase = Phase::Streaming;
                self.holding_since = None;
                vec![Action::ClearOverlay]
            }
            (Phase::Streaming, Event::Ended) => {
                self.phase = Phase::Holding;
                self.holding_since = Some(now);
                vec![Action::ShowReconnecting]
            }
            (_, Event::RadioError) => {
                let held = self.holds_display();
                self.phase = Phase::Advertising;
                self.holding_since = None;
                let mut out = vec![Action::RebuildGroup];
                if held {
                    out.insert(0, Action::ReleaseDisplay);
                    out.push(Action::ClearOverlay);
                }
                out
            }
            _ => Vec::new(),
        }
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let Some(since) = self.holding_since else {
            return Vec::new();
        };
        if now.saturating_duration_since(since) < HOLD {
            return Vec::new();
        }
        self.phase = Phase::Advertising;
        self.holding_since = None;
        vec![Action::ReleaseDisplay, Action::ClearOverlay]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_new_session_takes_the_display() {
        let mut l = Lifecycle::new();
        assert_eq!(l.phase(), Phase::Advertising);
        let out = l.on(Event::Connected, Instant::now());
        assert_eq!(l.phase(), Phase::Streaming);
        assert!(out.contains(&Action::AcquireDisplay), "{out:?}");
    }

    #[test]
    fn a_blip_resumes_without_releasing_the_display() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        let lost = l.on(Event::Ended, t0);
        assert_eq!(l.phase(), Phase::Holding);
        assert!(
            !lost.iter().any(|a| matches!(a, Action::ReleaseDisplay)),
            "the screen stays theirs: {lost:?}"
        );
        assert!(lost.contains(&Action::ShowReconnecting), "{lost:?}");

        let back = l.on(Event::Connected, t0 + Duration::from_secs(3));
        assert_eq!(l.phase(), Phase::Streaming);
        assert!(back.contains(&Action::ClearOverlay), "{back:?}");
        assert!(
            !back.iter().any(|a| matches!(a, Action::AcquireDisplay)),
            "it was never released, so it is not re-acquired: {back:?}"
        );
    }

    #[test]
    fn the_hold_expires_after_thirty_seconds() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        l.on(Event::Ended, t0);
        assert!(l.tick(t0 + Duration::from_secs(29)).is_empty(), "still holding");
        let expired = l.tick(t0 + Duration::from_secs(31));
        assert_eq!(l.phase(), Phase::Advertising);
        assert!(expired.contains(&Action::ReleaseDisplay), "{expired:?}");
        assert!(expired.contains(&Action::ClearOverlay), "{expired:?}");
    }

    #[test]
    fn the_hold_expires_only_once() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        l.on(Event::Ended, t0);
        l.tick(t0 + Duration::from_secs(31));
        assert!(l.tick(t0 + Duration::from_secs(32)).is_empty());
        assert!(l.tick(t0 + Duration::from_secs(90)).is_empty());
    }

    #[test]
    fn a_radio_error_rebuilds_the_group_and_gives_the_screen_back() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        let out = l.on(Event::RadioError, t0);
        assert_eq!(l.phase(), Phase::Advertising);
        assert!(out.contains(&Action::ReleaseDisplay), "{out:?}");
        assert!(out.contains(&Action::RebuildGroup), "{out:?}");
    }

    #[test]
    fn a_radio_error_while_advertising_still_rebuilds() {
        let mut l = Lifecycle::new();
        let out = l.on(Event::RadioError, Instant::now());
        assert!(out.contains(&Action::RebuildGroup), "{out:?}");
        assert!(
            !out.contains(&Action::ReleaseDisplay),
            "nothing was held, so nothing is released: {out:?}"
        );
    }

    #[test]
    fn a_second_connection_while_streaming_changes_nothing() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        let again = l.on(Event::Connected, t0);
        assert_eq!(l.phase(), Phase::Streaming);
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn an_end_while_advertising_is_ignored() {
        let mut l = Lifecycle::new();
        let out = l.on(Event::Ended, Instant::now());
        assert_eq!(l.phase(), Phase::Advertising);
        assert!(out.is_empty(), "{out:?}");
    }
}
