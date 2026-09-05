//! Which display, and how long to keep looking for it.
//!
//! Pure: the radio hands in what it saw, and this decides what to do about it.
//! Keeping the decisions here is what makes them testable at all, since nothing
//! that touches WinRT can be.

use castr_miracast::wfd::{DeviceCaps, DeviceKind};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    /// What its Wi-Fi Display element said, if it published one at all. A
    /// device with none is not offering to be a display right now.
    pub caps: Option<DeviceCaps>,
    /// How it is willing to be paired with, from its WPS element. `None` when
    /// it publishes none, which is treated as "the PIN ceremony, as before".
    pub pairing: Option<castr_miracast::wfd::ConfigMethods>,
}

impl Candidate {
    /// Something we could actually cast to: it published an element, and that
    /// element says it can show a picture.
    pub fn is_display(&self) -> bool {
        matches!(
            self.caps.map(|c| c.kind),
            Some(DeviceKind::PrimarySink)
                | Some(DeviceKind::SecondarySink)
                | Some(DeviceKind::DualRole)
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum NoMatch {
    NotFound,
    /// It is there; it is just not something that can show a picture.
    NotADisplay(String),
    Ambiguous(Vec<String>),
}

impl std::fmt::Display for NoMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoMatch::NotFound => write!(
                f,
                "discovery: no display of that name is advertising. Open Screen \
                 Mirroring on it - most displays advertise only while that screen is up"
            ),
            NoMatch::NotADisplay(n) => write!(
                f,
                "discovery: {n:?} is a Wi-Fi Direct device but not a display"
            ),
            NoMatch::Ambiguous(names) => write!(
                f,
                "discovery: that name could mean any of: {}",
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for NoMatch {}

/// Finds the one display a name refers to: exactly, then ignoring case, then by
/// unique prefix. Two matches is an error rather than a guess - picking the
/// wrong television is worse than asking which.
pub fn match_by_name(candidates: &[Candidate], name: &str) -> Result<Candidate, NoMatch> {
    let exact: Vec<&Candidate> = candidates.iter().filter(|c| c.name == name).collect();
    let insensitive: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.name.eq_ignore_ascii_case(name))
        .collect();
    let wanted = name.to_ascii_lowercase();
    let prefix: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.name.to_ascii_lowercase().starts_with(&wanted))
        .collect();

    let hits = if !exact.is_empty() {
        exact
    } else if !insensitive.is_empty() {
        insensitive
    } else {
        prefix
    };

    match hits.len() {
        0 => Err(NoMatch::NotFound),
        1 => {
            let c = hits[0];
            if c.is_display() {
                Ok(c.clone())
            } else {
                Err(NoMatch::NotADisplay(c.name.clone()))
            }
        }
        _ => Err(NoMatch::Ambiguous(
            hits.iter().map(|c| c.name.clone()).collect(),
        )),
    }
}

/// How long to keep looking for a display that is not advertising yet.
///
/// Displays usually are not there when the command is run: they advertise only
/// while their mirroring screen is open, so the normal sequence is to start the
/// cast and then walk over and put the display into that mode.
#[derive(Debug, Clone, Copy)]
pub struct WaitPolicy {
    pub timeout: Duration,
}

impl WaitPolicy {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn keep_waiting(&self, since: Instant) -> bool {
        since.elapsed() < self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(name: &str) -> Candidate {
        Candidate {
            id: format!("WiFiDirect#{name}"),
            name: name.to_string(),
            caps: Some(DeviceCaps {
                kind: DeviceKind::PrimarySink,
                session_available: true,
                content_protection: false,
                rtsp_port: 7236,
                max_throughput_mbps: 54,
            }),
            pairing: None,
        }
    }

    fn printer(name: &str) -> Candidate {
        Candidate {
            id: format!("WiFiDirect#{name}"),
            name: name.to_string(),
            caps: None,
            pairing: None,
        }
    }

    #[test]
    fn an_exact_name_wins() {
        let all = [sink("DietPi"), sink("Living Room TV")];
        assert_eq!(match_by_name(&all, "DietPi").unwrap().name, "DietPi");
    }

    #[test]
    fn case_does_not_matter() {
        let all = [sink("Living Room TV")];
        assert_eq!(
            match_by_name(&all, "living room tv").unwrap().name,
            "Living Room TV"
        );
    }

    #[test]
    fn a_unique_prefix_is_enough() {
        let all = [sink("Living Room TV"), sink("DietPi")];
        assert_eq!(
            match_by_name(&all, "Living").unwrap().name,
            "Living Room TV"
        );
    }

    #[test]
    fn an_exact_match_beats_a_longer_prefix_match() {
        // "TV" naming both a display and the prefix of another must not be
        // ambiguous: the exact name is what the user typed.
        let all = [sink("TV"), sink("TV Bedroom")];
        assert_eq!(match_by_name(&all, "TV").unwrap().name, "TV");
    }

    #[test]
    fn an_ambiguous_prefix_lists_the_candidates() {
        // Guessing between two displays is worse than asking which.
        let all = [sink("Living Room TV"), sink("Living Room Soundbar")];
        match match_by_name(&all, "Living") {
            Err(NoMatch::Ambiguous(names)) => assert_eq!(names.len(), 2),
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn a_printer_is_named_rather_than_silently_skipped() {
        // "Not found" would be a lie: it is right there, it is just not a
        // display, and the user needs to know which of the two is true.
        let all = [printer("DIRECT-D0-EPSON-WF-2960 Series")];
        match match_by_name(&all, "DIRECT-D0-EPSON-WF-2960 Series") {
            Err(NoMatch::NotADisplay(n)) => assert!(n.contains("EPSON")),
            other => panic!("expected NotADisplay, got {other:?}"),
        }
    }

    #[test]
    fn a_source_is_not_a_display_to_cast_to() {
        let mut c = sink("Someones Laptop");
        c.caps = Some(DeviceCaps {
            kind: DeviceKind::Source,
            ..c.caps.unwrap()
        });
        assert!(matches!(
            match_by_name(&[c], "Someones Laptop"),
            Err(NoMatch::NotADisplay(_))
        ));
    }

    #[test]
    fn nothing_matching_is_not_found() {
        assert!(matches!(
            match_by_name(&[sink("DietPi")], "Living Room TV"),
            Err(NoMatch::NotFound)
        ));
    }

    #[test]
    fn not_found_says_how_to_fix_it() {
        // The commonest cause by far is a display whose mirroring screen is
        // shut, and the message has to say so or the user retries blindly.
        let text = NoMatch::NotFound.to_string();
        assert!(text.starts_with("discovery:"), "every failure names its stage");
        assert!(text.contains("Screen Mirroring"));
    }

    #[test]
    fn waiting_gives_up_at_the_timeout() {
        let p = WaitPolicy::new(Duration::from_secs(60));
        assert!(p.keep_waiting(Instant::now()));
        assert!(!p.keep_waiting(Instant::now() - Duration::from_secs(61)));
    }
}

/// Which pairing ceremony to use with a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceremony {
    /// Confirm without a PIN. Nobody has to read anything off a screen, which
    /// is what makes an unattended cast to an adapter possible.
    PushButton,
    /// Type in the PIN the display shows.
    Pin,
}

/// What the caller asked for, when they have an opinion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preference {
    /// Take whichever the display offers, favouring the one needing no human.
    #[default]
    Auto,
    ForcePushButton,
    ForcePin,
}

/// Chooses how to pair, from what the display says it accepts.
///
/// Push-button is preferred when offered because it needs nobody: a wireless
/// display adapter shows a "ready to connect" screen and generates its PIN per
/// attempt, so there is often no PIN for anyone to read even though it
/// advertises that it can display one.
///
/// A display that offers no push-button gets the PIN ceremony, and so does one
/// that advertises nothing at all - which is exactly what this did before it
/// could choose, so no display that already worked changes behaviour.
pub fn choose_ceremony(
    methods: Option<castr_miracast::wfd::ConfigMethods>,
    preference: Preference,
) -> Ceremony {
    match preference {
        Preference::ForcePin => Ceremony::Pin,
        Preference::ForcePushButton => Ceremony::PushButton,
        Preference::Auto => match methods {
            Some(m) if m.push_button() => Ceremony::PushButton,
            _ => Ceremony::Pin,
        },
    }
}

#[cfg(test)]
mod ceremony_tests {
    use super::*;
    use castr_miracast::wfd::ConfigMethods;

    // The real bitmaps, read from the devices in range on 2026-09-05.
    const ADAPTER: ConfigMethods = ConfigMethods(0x2288);
    const SAMSUNG: ConfigMethods = ConfigMethods(0x4388);
    const FIRE_TV: ConfigMethods = ConfigMethods(0x4108);
    const PRINTER: ConfigMethods = ConfigMethods(0x0000);

    #[test]
    fn a_display_offering_a_button_gets_the_button() {
        // No human has to read anything, so this is always the kinder path.
        assert_eq!(
            choose_ceremony(Some(ADAPTER), Preference::Auto),
            Ceremony::PushButton
        );
        assert_eq!(
            choose_ceremony(Some(SAMSUNG), Preference::Auto),
            Ceremony::PushButton
        );
    }

    #[test]
    fn a_display_with_no_button_gets_the_pin() {
        assert_eq!(
            choose_ceremony(Some(FIRE_TV), Preference::Auto),
            Ceremony::Pin
        );
    }

    #[test]
    fn a_display_that_advertises_nothing_gets_the_pin() {
        // Which is what happened before there was a choice, so nothing that
        // already worked starts behaving differently.
        assert_eq!(choose_ceremony(None, Preference::Auto), Ceremony::Pin);
        assert_eq!(
            choose_ceremony(Some(PRINTER), Preference::Auto),
            Ceremony::Pin
        );
    }

    #[test]
    fn the_caller_can_insist() {
        assert_eq!(
            choose_ceremony(Some(ADAPTER), Preference::ForcePin),
            Ceremony::Pin
        );
        assert_eq!(
            choose_ceremony(Some(FIRE_TV), Preference::ForcePushButton),
            Ceremony::PushButton
        );
        // Insisting works even when nothing is known about the display.
        assert_eq!(
            choose_ceremony(None, Preference::ForcePushButton),
            Ceremony::PushButton
        );
    }
}
