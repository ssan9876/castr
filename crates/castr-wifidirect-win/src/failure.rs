//! Turning what Windows says into something a person can act on.
//!
//! A bare `DevicePairingResultStatus(19)` cost an afternoon. The sentence that
//! actually solved it was in the WLAN AutoConfig event log, where nobody would
//! think to look, so the radio reads that log itself and quotes it rather than
//! sending anyone to Event Viewer.
//!
//! Pure: text in, text out. Fetching the log is the caller's job.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Discovery,
    Pairing,
    Association,
    Address,
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Stage::Discovery => "discovery",
            Stage::Pairing => "pairing",
            Stage::Association => "association",
            Stage::Address => "address",
        })
    }
}

/// `DevicePairingResultStatus`, in words.
pub fn pairing_status(code: i32) -> String {
    match code {
        0 => "paired".into(),
        1 => "the display was not ready to pair".into(),
        2 => "not paired".into(),
        3 => "already paired".into(),
        4 => "the display rejected the connection".into(),
        5 => "the display has too many connections already".into(),
        6 => "the radio reported a hardware failure".into(),
        7 => "the display did not answer in time".into(),
        8 => "that pairing method is not allowed".into(),
        9 => "the PIN was not accepted".into(),
        14 => "pairing was cancelled".into(),
        18 => "the display is already associated with something else".into(),
        19 => "pairing failed, with no reason given".into(),
        other => format!("pairing failed with status {other}"),
    }
}

/// The `Failure Reason:` line of a WLAN AutoConfig 8002 event.
pub fn parse_wlan_failure(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Failure Reason:"))
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// Whether a failure reason is the one a stale pairing produces.
///
/// Credentials for a group that no longer exists look exactly like a network
/// that is not there, because from the radio's point of view that is what they
/// are. Recognising it is what lets a single unpair-and-retry rescue a source
/// that would otherwise sit on "connecting" for ever.
pub fn looks_like_stale_credentials(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    r.contains("not available") || r.contains("cannot be found") || r.contains("not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly what the log held on 2026-09-04, when an hour went into finding
    /// this sentence by hand.
    const REAL_8002: &str = "\
Event[0]:
  Log Name: Microsoft-Windows-WLAN-AutoConfig/Operational
  Event ID: 8002
  Description:
WLAN AutoConfig service failed to connect to a wireless network.

Network Adapter: Microsoft Wi-Fi Direct Virtual Adapter #2
Connection Mode: Connection with a temporary profile
Profile Name: WCN Temporary Profile
SSID: DIRECT-yN
BSS Type: Infrastructure
Failure Reason:The specific network is not available.
RSSI: 255
";

    #[test]
    fn the_reason_is_lifted_out_of_the_log() {
        assert_eq!(
            parse_wlan_failure(REAL_8002).as_deref(),
            Some("The specific network is not available.")
        );
    }

    #[test]
    fn a_log_with_no_reason_yields_nothing_rather_than_noise() {
        assert_eq!(parse_wlan_failure("Event[0]:\n  nothing useful\n"), None);
        assert_eq!(parse_wlan_failure(""), None);
    }

    #[test]
    fn that_reason_is_recognised_as_stale_credentials() {
        // A paired display whose group no longer exists looks exactly like a
        // network that is not there. One unpair-and-retry is worth it.
        assert!(looks_like_stale_credentials(
            "The specific network is not available."
        ));
    }

    #[test]
    fn a_wrong_password_is_not_treated_as_stale_credentials() {
        // Unpairing would not help here, and would cost a PIN prompt.
        assert!(!looks_like_stale_credentials(
            "The network password is not correct."
        ));
        assert!(!looks_like_stale_credentials("The network is busy."));
    }

    #[test]
    fn a_pairing_status_reads_as_words_not_a_number() {
        // DevicePairingResultStatus(19) as a bare integer cost an afternoon.
        assert!(pairing_status(19).contains("failed"));
        assert!(pairing_status(0).contains("paired"));
        assert!(pairing_status(3).contains("already"));
        assert!(pairing_status(9).contains("PIN"));
        assert!(pairing_status(14).contains("cancel"));
    }

    #[test]
    fn an_unknown_status_keeps_its_number() {
        // Better an unfamiliar code than a confident wrong description.
        assert!(pairing_status(4242).contains("4242"));
    }

    #[test]
    fn every_stage_names_itself() {
        assert_eq!(Stage::Discovery.to_string(), "discovery");
        assert_eq!(Stage::Pairing.to_string(), "pairing");
        assert_eq!(Stage::Association.to_string(), "association");
        assert_eq!(Stage::Address.to_string(), "address");
    }
}
