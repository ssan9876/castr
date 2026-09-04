//! The WPS PIN a Miracast source has to be told before it will join.
//!
//! Windows collects the PIN from the viewer *before* it puts anything on the
//! air: the box appears the moment the sink is picked from the list, and no
//! provisioning frame reaches us until a PIN has been typed. A PIN minted when
//! provisioning starts therefore arrives too late to be read - there is
//! nothing on the television when the viewer is asked for it, and pairing
//! cannot complete. One is minted as soon as the group is up instead, and held
//! until it is used or fails.
//!
//! Pure: no radio, no sockets, no platform. The randomness is injected so the
//! whole policy is testable, and so is the checksum the standard requires.

use std::time::{Duration, Instant};

/// Seven digits and the checksum digit, which is what `wpa_supplicant` and
/// Windows both expect.
pub const PIN_DIGITS: usize = 8;

/// The registration window a `WPS_PIN` opens - the specification calls it the
/// walk time - after which a group owner stops advertising that it will accept
/// an enrolment.
pub const WPS_WALK_TIME: Duration = Duration::from_secs(120);

/// How often the registrar is re-armed while a PIN is on the screen.
///
/// Arming it once, when the group comes up, makes the offer true for two
/// minutes and a lie thereafter: the PIN stays on the television and the sink
/// stays discoverable, but a source finds no network it can enrol with and
/// reports only that it could not connect. Nothing arrives here to explain it,
/// because nothing reaches the sink at all.
///
/// So it is re-armed on an interval comfortably inside the window, for as long
/// as the PIN is displayed.
pub const WPS_REARM: Duration = Duration::from_secs(45);

/// When the registrar was last armed, and whether it is due again.
#[derive(Debug, Default)]
pub struct WpsWindow {
    last: Option<Instant>,
}

impl WpsWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the registrar has never been armed, or has been left long
    /// enough that it is worth arming again.
    pub fn due(&self, now: Instant) -> bool {
        match self.last {
            None => true,
            Some(t) => now.duration_since(t) >= WPS_REARM,
        }
    }

    pub fn armed(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// Forgets the last arming, so the next check is due. Used when the PIN
    /// changes: the old registration is worthless.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// The checksum digit the WPS specification defines, as `wpa_supplicant`
/// implements it: the argument is the seven-digit value, and the digit this
/// returns is appended to it. Windows rejects a PIN whose checksum does not
/// match, so this is not decoration.
pub fn checksum(mut pin: u32) -> u32 {
    let mut accum = 0;
    while pin > 0 {
        accum += 3 * (pin % 10);
        pin /= 10;
        accum += pin % 10;
        pin /= 10;
    }
    (10 - accum % 10) % 10
}

/// A PIN built from `entropy`, which need only be unpredictable enough that a
/// person in the room cannot guess the next one; the PIN is shown on a screen
/// in that room, not used as a secret at a distance.
pub fn pin_from(entropy: u32) -> String {
    let value = entropy % 10_000_000;
    format!("{:07}{}", value, checksum(value))
}

/// Whether `pin` is a PIN a source will accept: eight digits, last one the
/// checksum of the first seven.
pub fn is_valid(pin: &str) -> bool {
    pin.len() == PIN_DIGITS
        && pin.bytes().all(|b| b.is_ascii_digit())
        && match (pin[..7].parse::<u32>(), pin[7..].parse::<u32>()) {
            (Ok(value), Ok(check)) => checksum(value) == check,
            _ => false,
        }
}

/// What the sink should do about the PIN after something happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Show {
    /// Put this PIN on the screen and arm the supplicant with it.
    Pin(String),
    /// Take the PIN off the screen: it is no longer wanted.
    Clear,
    /// Nothing to do; whatever is on the screen is still correct.
    Nothing,
}

/// The PIN currently offered to sources, and when it changes.
#[derive(Debug, Default)]
pub struct Pairing {
    pin: Option<String>,
}

impl Pairing {
    pub fn new() -> Self {
        Self::default()
    }

    /// The PIN on offer, if any.
    pub fn current(&self) -> Option<&str> {
        self.pin.as_deref()
    }

    /// The group is up and a source could pick us at any moment. Mints a PIN
    /// if there is not one already; an existing one is kept, so a group that
    /// comes back up does not change the digits a viewer is part way through
    /// typing.
    pub fn group_up(&mut self, entropy: u32) -> Show {
        match &self.pin {
            Some(_) => Show::Nothing,
            None => {
                let pin = pin_from(entropy);
                self.pin = Some(pin.clone());
                Show::Pin(pin)
            }
        }
    }

    /// A source is provisioning. The PIN it is about to send was read off the
    /// screen, so this must not change it - minting one here is what made
    /// pairing impossible.
    pub fn provisioning(&mut self, entropy: u32) -> Show {
        match &self.pin {
            Some(_) => Show::Nothing,
            // Only if we somehow have none: better a PIN that appears late
            // than a source that can never succeed.
            None => self.group_up(entropy),
        }
    }

    /// Provisioning failed. A PIN that has been tried and rejected is worth
    /// nothing, and a viewer who mistyped needs to see that something changed,
    /// so the next attempt gets fresh digits.
    pub fn failed(&mut self, entropy: u32) -> Show {
        self.pin = None;
        self.group_up(entropy)
    }

    /// A source joined. Nobody needs to type anything now.
    pub fn joined(&mut self) -> Show {
        match self.pin.take() {
            Some(_) => Show::Clear,
            None => Show::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_eight_digits_with_a_valid_checksum() {
        for entropy in [0, 1, 12345, 9_999_999, u32::MAX, 7_654_321] {
            let pin = pin_from(entropy);
            assert_eq!(pin.len(), PIN_DIGITS, "{pin}");
            assert!(pin.bytes().all(|b| b.is_ascii_digit()), "{pin}");
            assert!(is_valid(&pin), "{pin}");
        }
    }

    #[test]
    fn the_checksum_matches_the_standards_worked_example() {
        // 12345670 and 87654325 are the two PINs the specification and
        // wpa_supplicant both publish as examples.
        assert_eq!(pin_from(1234567), "12345670");
        assert_eq!(pin_from(8765432), "87654325");
        assert!(is_valid("12345670"));
        assert!(is_valid("87654325"));
        assert!(!is_valid("12345671"), "a wrong checksum must be rejected");
    }

    #[test]
    fn a_pin_that_is_not_eight_digits_is_not_valid() {
        assert!(!is_valid(""));
        assert!(!is_valid("1234567"));
        assert!(!is_valid("123456700"));
        assert!(!is_valid("1234567a"));
    }

    #[test]
    fn a_pin_exists_as_soon_as_the_group_is_up() {
        // The whole point: something readable is on the screen before any
        // source asks for it.
        let mut p = Pairing::new();
        assert_eq!(p.group_up(1234567), Show::Pin("12345670".into()));
        assert_eq!(p.current(), Some("12345670"));
    }

    #[test]
    fn provisioning_keeps_the_pin_the_viewer_is_reading() {
        let mut p = Pairing::new();
        p.group_up(1234567);
        // A different entropy value, to catch a mint that ignores the state.
        assert_eq!(p.provisioning(7_654_321), Show::Nothing);
        assert_eq!(p.current(), Some("12345670"));
    }

    #[test]
    fn a_group_coming_back_up_does_not_change_the_digits() {
        let mut p = Pairing::new();
        p.group_up(1234567);
        assert_eq!(p.group_up(7_654_321), Show::Nothing);
        assert_eq!(p.current(), Some("12345670"));
    }

    #[test]
    fn a_failure_mints_fresh_digits() {
        let mut p = Pairing::new();
        p.group_up(1234567);
        let after = p.failed(7_654_321);
        assert_eq!(after, Show::Pin(pin_from(7_654_321)));
        assert_ne!(p.current(), Some("12345670"));
    }

    #[test]
    fn joining_takes_the_pin_off_the_screen_once() {
        let mut p = Pairing::new();
        p.group_up(1234567);
        assert_eq!(p.joined(), Show::Clear);
        assert_eq!(p.current(), None);
        assert_eq!(p.joined(), Show::Nothing, "nothing left to clear");
    }

    #[test]
    fn the_registrar_is_rearmed_well_inside_the_walk_time() {
        // A PIN registered once goes stale after the walk time, and from then
        // on the sink is discoverable but impossible to pair with - which is
        // exactly what a source reports as "could not connect".
        assert!(
            WPS_REARM * 2 < WPS_WALK_TIME,
            "re-arming must leave room for a missed interval"
        );
    }

    #[test]
    fn a_window_that_was_never_armed_is_due() {
        assert!(WpsWindow::new().due(Instant::now()));
    }

    #[test]
    fn a_freshly_armed_window_is_not_due_again_at_once() {
        let now = Instant::now();
        let mut w = WpsWindow::new();
        w.armed(now);
        assert!(!w.due(now));
        assert!(!w.due(now + WPS_REARM - Duration::from_secs(1)));
        assert!(w.due(now + WPS_REARM));
    }

    #[test]
    fn a_reset_window_is_due_immediately() {
        // A new PIN makes the old registration worthless.
        let now = Instant::now();
        let mut w = WpsWindow::new();
        w.armed(now);
        w.reset();
        assert!(w.due(now));
    }

    #[test]
    fn provisioning_with_no_pin_at_all_still_offers_one() {
        // Not expected - the group is always up first - but a source that can
        // never succeed is worse than a PIN that appears late.
        let mut p = Pairing::new();
        assert_eq!(p.provisioning(1234567), Show::Pin("12345670".into()));
    }
}
