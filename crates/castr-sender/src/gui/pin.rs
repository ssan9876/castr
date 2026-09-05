//! PIN entry, which is not the same length for both protocols.
//!
//! Pure. castr's pairing shows six digits; a Miracast display shows eight —
//! the Pi's own log says `PIN 82128616`. The entry field was hardcoded to six,
//! so a Miracast PIN could never have been submitted.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKind {
    Castr,
    Miracast,
}

impl PinKind {
    pub fn digits(self) -> usize {
        match self {
            PinKind::Castr => 6,
            PinKind::Miracast => 8,
        }
    }
}

/// What the user is allowed to have typed so far.
///
/// Anything that is not a digit is dropped rather than rejected: people paste
/// PINs with spaces in them, and a field that silently refuses a keystroke is
/// worse than one that tidies it.
pub fn sanitise(input: &str, kind: PinKind) -> String {
    input
        .chars()
        .filter(char::is_ascii_digit)
        .take(kind.digits())
        .collect()
}

/// Whether Submit should be enabled.
pub fn is_complete(input: &str, kind: PinKind) -> bool {
    let digits = input.chars().filter(char::is_ascii_digit).count();
    digits == kind.digits() && input.chars().all(|c| c.is_ascii_digit())
}

pub fn prompt(kind: PinKind, target: &str) -> String {
    format!(
        "Enter the {}-digit PIN shown on '{target}'",
        kind.digits()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn castr_wants_six_and_miracast_eight() {
        assert_eq!(PinKind::Castr.digits(), 6);
        assert_eq!(PinKind::Miracast.digits(), 8);
    }

    #[test]
    fn a_six_digit_pin_completes_a_castr_pairing() {
        assert!(is_complete("123456", PinKind::Castr));
        assert!(!is_complete("12345", PinKind::Castr));
        assert!(!is_complete("1234567", PinKind::Castr));
    }

    #[test]
    fn an_eight_digit_pin_completes_a_miracast_pairing() {
        // The exact PIN the Pi minted during the radio verification.
        assert!(is_complete("82128616", PinKind::Miracast));
        assert!(!is_complete("821286", PinKind::Miracast));
    }

    #[test]
    fn a_miracast_pin_is_not_accepted_as_a_castr_one() {
        // The bug this prevents: the field was hardcoded to six digits, so an
        // eight-digit Miracast PIN could never be submitted.
        assert!(!is_complete("82128616", PinKind::Castr));
    }

    #[test]
    fn letters_never_complete_a_pin() {
        assert!(!is_complete("12345a", PinKind::Castr));
        assert!(!is_complete("abcdef", PinKind::Castr));
    }

    #[test]
    fn pasting_a_spaced_pin_keeps_the_digits() {
        assert_eq!(sanitise("8212 8616", PinKind::Miracast), "82128616");
        assert_eq!(sanitise("123 456", PinKind::Castr), "123456");
    }

    #[test]
    fn typing_past_the_length_is_capped_rather_than_rejected() {
        assert_eq!(sanitise("1234567890", PinKind::Castr), "123456");
        assert_eq!(sanitise("1234567890", PinKind::Miracast), "12345678");
    }

    #[test]
    fn an_empty_field_is_left_empty() {
        assert_eq!(sanitise("", PinKind::Castr), "");
        assert!(!is_complete("", PinKind::Castr));
    }

    #[test]
    fn the_prompt_says_how_many_digits_to_expect() {
        assert_eq!(
            prompt(PinKind::Miracast, "DietPi"),
            "Enter the 8-digit PIN shown on 'DietPi'"
        );
        assert!(prompt(PinKind::Castr, "living room").contains("6-digit"));
    }
}
