//! The one list: castr receivers and Miracast displays together.
//!
//! Pure, and deliberately free of any Windows type — a display arrives here as
//! a [`DisplayInfo`] rather than as the radio crate's `Candidate`, so this
//! module and its tests build wherever `castr-sender` does.
//!
//! A person picks where their screen should go, not which protocol carries it.
//! What differs between the two kinds is which buttons apply, and that is
//! [`actions_for`].

use castr_net::ReceiverInfo;

/// A Miracast display, as much of one as the list needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// The radio's own identifier, stable across scans.
    pub id: String,
    pub name: String,
    /// What its Wi-Fi Display element advertised it can carry.
    pub max_mbps: u16,
    pub hdcp: bool,
}

#[derive(Debug, Clone)]
pub enum Target {
    Receiver(ReceiverInfo),
    Display(DisplayInfo),
}

/// Identity that survives a rescan.
///
/// The selection is held by this rather than by a row number: a rescan can
/// reorder the list, and an index would then quietly point at a different
/// machine than the one the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetId {
    Receiver([u8; 32]),
    Display(String),
}

impl Target {
    pub fn id(&self) -> TargetId {
        match self {
            Target::Receiver(r) => TargetId::Receiver(r.fingerprint),
            Target::Display(d) => TargetId::Display(d.id.clone()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Target::Receiver(r) => &r.name,
            Target::Display(d) => &d.name,
        }
    }

    /// The row's text: what it is, then the one fact worth knowing about it.
    pub fn label(&self) -> String {
        match self {
            Target::Receiver(r) => format!("{}   castr · {}", r.name, r.addr.ip()),
            Target::Display(d) => {
                let mut s = format!("{}   Miracast · up to {} Mbps", d.name, d.max_mbps);
                if d.hdcp {
                    s.push_str(" · HDCP");
                }
                s
            }
        }
    }
}

/// Which buttons apply to the selected row.
///
/// A castr receiver pairs as a separate step and then casts. A Miracast
/// display pairs inside the connect, prompting for its PIN only if Windows has
/// not paired with it before, so there is nothing for a Pair button to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Actions {
    pub pair: bool,
    pub cast: bool,
}

pub fn actions_for(selected: Option<&Target>) -> Actions {
    match selected {
        None => Actions::default(),
        Some(Target::Receiver(_)) => Actions {
            pair: true,
            cast: true,
        },
        Some(Target::Display(_)) => Actions {
            pair: false,
            cast: true,
        },
    }
}

/// One list, receivers first, each kind by name.
///
/// castr receivers lead because they are the better path when one is
/// available — no radio, no PIN ceremony after the first time.
pub fn merge(receivers: &[ReceiverInfo], displays: &[DisplayInfo]) -> Vec<Target> {
    let mut rs: Vec<_> = receivers.to_vec();
    rs.sort_by_key(|r| r.name.to_lowercase());
    let mut ds: Vec<_> = displays.to_vec();
    ds.sort_by_key(|d| d.name.to_lowercase());
    rs.into_iter()
        .map(Target::Receiver)
        .chain(ds.into_iter().map(Target::Display))
        .collect()
}

/// Where the selected target sits now, if it is still there.
pub fn position(list: &[Target], id: &TargetId) -> Option<usize> {
    list.iter().position(|t| &t.id() == id)
}

/// What to say when the list is thin, or nothing when it is not.
///
/// The Miracast half exists because a display that is not in mirroring mode
/// publishes nothing at all, which looks exactly like a display that is not
/// there — the single most common way this feature appears broken.
pub fn advice(list: &[Target], scanned: bool) -> Option<&'static str> {
    if !scanned {
        return None;
    }
    let receivers = list
        .iter()
        .filter(|t| matches!(t, Target::Receiver(_)))
        .count();
    let displays = list.len() - receivers;
    match (receivers, displays) {
        (0, 0) => Some(
            "Nothing found. Is a castr receiver running on this network? \
             Most Miracast displays appear only while their Screen Mirroring \
             page is open.",
        ),
        (_, 0) => Some(
            "No Miracast displays. Most appear only while their Screen \
             Mirroring page is open.",
        ),
        (0, _) => Some("No castr receivers on this network."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn receiver(name: &str, last: u8) -> ReceiverInfo {
        let mut fingerprint = [0u8; 32];
        fingerprint[31] = last;
        ReceiverInfo {
            name: name.into(),
            fingerprint,
            addr: format!("192.168.1.{}:7332", 10 + last).parse::<SocketAddr>().unwrap(),
            version: 1,
        }
    }

    fn display(name: &str, mbps: u16, hdcp: bool) -> DisplayInfo {
        DisplayInfo {
            id: format!("id-{name}"),
            name: name.into(),
            max_mbps: mbps,
            hdcp,
        }
    }

    #[test]
    fn receivers_come_before_displays() {
        let list = merge(&[receiver("zeta", 1)], &[display("alpha", 54, false)]);
        assert_eq!(list[0].name(), "zeta");
        assert_eq!(list[1].name(), "alpha");
    }

    #[test]
    fn each_kind_is_ordered_by_name_regardless_of_discovery_order() {
        let list = merge(
            &[receiver("living room", 1), receiver("Bedroom", 2)],
            &[display("Zebra TV", 54, false), display("attic", 10, false)],
        );
        let names: Vec<_> = list.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["Bedroom", "living room", "attic", "Zebra TV"]);
    }

    #[test]
    fn a_row_says_what_kind_it_is() {
        assert_eq!(
            Target::Receiver(receiver("living room", 1)).label(),
            "living room   castr · 192.168.1.11"
        );
        assert_eq!(
            Target::Display(display("DietPi", 10, false)).label(),
            "DietPi   Miracast · up to 10 Mbps"
        );
    }

    #[test]
    fn a_display_wanting_content_protection_says_so() {
        // We cannot satisfy HDCP at all, so it is worth seeing before casting.
        let label = Target::Display(display("75\" Crystal UHD", 54, true)).label();
        assert!(label.ends_with("up to 54 Mbps · HDCP"), "got {label}");
    }

    #[test]
    fn a_receiver_can_be_paired_and_cast_to() {
        let t = Target::Receiver(receiver("living room", 1));
        assert_eq!(
            actions_for(Some(&t)),
            Actions {
                pair: true,
                cast: true
            }
        );
    }

    #[test]
    fn a_display_can_only_be_cast_to() {
        // Pairing happens inside the connect, so a Pair button would do nothing.
        let t = Target::Display(display("DietPi", 10, false));
        assert_eq!(
            actions_for(Some(&t)),
            Actions {
                pair: false,
                cast: true
            }
        );
    }

    #[test]
    fn nothing_selected_enables_nothing() {
        assert_eq!(actions_for(None), Actions::default());
    }

    #[test]
    fn a_selection_survives_the_list_being_reordered() {
        // The bug this prevents: a rescan reorders the list and an index-based
        // selection silently points at a different machine.
        let before = merge(&[receiver("bedroom", 2)], &[]);
        let chosen = before[0].id();
        let after = merge(&[receiver("attic", 9), receiver("bedroom", 2)], &[]);
        assert_eq!(position(&after, &chosen), Some(1));
        assert_eq!(after[1].name(), "bedroom");
    }

    #[test]
    fn a_selection_that_went_away_is_reported_as_gone() {
        let before = merge(&[receiver("bedroom", 2)], &[]);
        let chosen = before[0].id();
        assert_eq!(position(&merge(&[], &[]), &chosen), None);
    }

    #[test]
    fn two_receivers_with_the_same_name_are_still_distinguishable() {
        // Two Pis both named after their hostname is not far-fetched.
        let list = merge(&[receiver("DietPi", 1), receiver("DietPi", 2)], &[]);
        assert_ne!(list[0].id(), list[1].id());
    }

    #[test]
    fn no_advice_before_the_first_scan_finishes() {
        assert_eq!(advice(&[], false), None);
    }

    #[test]
    fn an_empty_list_explains_both_halves() {
        let text = advice(&[], true).unwrap();
        assert!(text.contains("receiver"));
        assert!(text.contains("Screen Mirroring"));
    }

    #[test]
    fn receivers_but_no_displays_gives_the_mirroring_advice() {
        let list = merge(&[receiver("living room", 1)], &[]);
        assert!(advice(&list, true).unwrap().contains("Screen Mirroring"));
    }

    #[test]
    fn displays_but_no_receivers_says_so() {
        let list = merge(&[], &[display("DietPi", 10, false)]);
        assert_eq!(advice(&list, true), Some("No castr receivers on this network."));
    }

    #[test]
    fn a_full_list_needs_no_advice() {
        let list = merge(&[receiver("living room", 1)], &[display("DietPi", 10, false)]);
        assert_eq!(advice(&list, true), None);
    }
}
