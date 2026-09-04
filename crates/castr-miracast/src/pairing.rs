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

/// Seven digits and the checksum digit, which is what `wpa_supplicant` and
/// Windows both expect.
pub const PIN_DIGITS: usize = 8;

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
    fn provisioning_with_no_pin_at_all_still_offers_one() {
        // Not expected - the group is always up first - but a source that can
        // never succeed is worse than a PIN that appears late.
        let mut p = Pairing::new();
        assert_eq!(p.provisioning(1234567), Show::Pin("12345670".into()));
    }
}
