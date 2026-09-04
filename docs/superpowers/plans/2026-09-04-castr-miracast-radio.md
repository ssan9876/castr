# Wi-Fi Direct Radio Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `castr-sender miracast-cast "Living Room TV"` find the display, pair with it, form the Wi-Fi Direct group, cast, and tear down.

**Architecture:** A new Windows-only crate `castr-wifidirect-win` holding the WinRT shell, with every decision pushed out into pure modules beside it. The Wi-Fi Display information-element parser goes into `castr-miracast/src/wfd.rs`, next to the builder that already emits the same structure for the sink.

**Tech Stack:** Rust, the `windows` crate 0.58 (`Devices_WiFiDirect`, `Devices_Enumeration`, `Foundation`, `Foundation_Collections`, `Networking`, `Storage_Streams`), `wevtutil` for the WLAN log.

**Spec:** `docs/superpowers/specs/2026-09-04-castr-miracast-radio-design.md`

## Global Constraints

- **Nothing in `radio.rs` is unit-testable**; if a behaviour can be tested, it does not belong there. Parsing, matching, choosing and mapping live in pure modules.
- **`Connection` owns the group.** Dropping it is the teardown; there is no separate disconnect call to forget.
- **Never unpair on teardown.** The stored pairing is what makes the second cast silent.
- **Every failure names its stage**: discovery, pairing, association, address.
- Windows keeps a group alive for roughly 60 s after the owning process exits. Not a bug; tolerate it.
- Suites: `cargo test -q --workspace` on Windows, `bash scripts/pi/test-linux.sh` (needs Docker Desktop running — check the script's own exit status, not a piped `grep`'s).
- Commits: lowercase `type(scope): summary`, then why it matters, trailer `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/castr-miracast/src/wfd.rs` | modified: parse a device-information subelement |
| `crates/castr-wifidirect-win/Cargo.toml` | the new crate |
| `crates/castr-wifidirect-win/src/lib.rs` | public surface: `Display`, `Connection`, `discover`, `connect` |
| `crates/castr-wifidirect-win/src/select.rs` | pure: name matching, ceremony choice, waiting policy |
| `crates/castr-wifidirect-win/src/failure.rs` | pure: status and log-text to a named stage failure |
| `crates/castr-wifidirect-win/src/radio.rs` | the WinRT shell |
| `crates/castr-sender/src/miracast_cast.rs` | modified: accept a name, hold the connection |
| `crates/castr-sender/src/main.rs` | modified: prompt for the PIN |

---

### Task 1: Parse the Wi-Fi Display information element

**Files:**
- Modify: `crates/castr-miracast/src/wfd.rs`

**Interfaces:**
- Produces: `wfd::DeviceKind { Source, PrimarySink, SecondarySink, DualRole }`, `wfd::DeviceCaps { kind: DeviceKind, session_available: bool, content_protection: bool, rtsp_port: u16, max_throughput_mbps: u16 }`, `wfd::parse_device_info(body: &[u8]) -> Option<DeviceCaps>`, `wfd::WFD_OUI: [u8; 3]`, `wfd::WFD_OUI_TYPE: u8`

- [ ] **Step 1: Write the failing test**

Append inside the existing `mod tests` in `wfd.rs`:

```rust
    #[test]
    fn a_real_televisions_element_parses() {
        // Captured from a Samsung 75" Crystal UHD, 2026-09-04. Bytes from a
        // vendor we did not write are the only interoperability evidence
        // available before a television can be cast to.
        let body = [0x01, 0x11, 0x1c, 0x44, 0x00, 0x36];
        let c = parse_device_info(&body).expect("a real element must parse");
        assert_eq!(c.kind, DeviceKind::PrimarySink);
        assert!(c.session_available, "it was advertising a free session");
        assert!(c.content_protection, "this television supports HDCP");
        assert_eq!(c.rtsp_port, 7236);
        assert_eq!(c.max_throughput_mbps, 54);
    }

    #[test]
    fn our_own_sinks_element_parses_as_what_we_built() {
        // The builder and the parser must agree, or the source and the sink
        // disagree about the sink's own advertisement.
        let hex = device_info_subelement(&DeviceInfo {
            session_available: true,
            rtsp_port: 7236,
            max_throughput_mbps: 10,
        });
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex"))
            .collect();
        // The first two bytes are the subelement's length, not its body.
        let c = parse_device_info(&bytes[2..]).expect("our own element must parse");
        assert_eq!(c.kind, DeviceKind::PrimarySink);
        assert!(c.session_available);
        assert!(!c.content_protection);
        assert_eq!(c.rtsp_port, 7236);
        assert_eq!(c.max_throughput_mbps, 10);
    }

    #[test]
    fn a_source_is_not_mistaken_for_a_sink() {
        // Device type 00 is a source. Casting to one would never work.
        let body = [0x00, 0x10, 0x1c, 0x44, 0x00, 0x0a];
        assert_eq!(
            parse_device_info(&body).expect("parse").kind,
            DeviceKind::Source
        );
    }

    #[test]
    fn a_truncated_element_is_rejected_rather_than_guessed() {
        assert!(parse_device_info(&[0x01, 0x11, 0x1c]).is_none());
        assert!(parse_device_info(&[]).is_none());
    }

    #[test]
    fn an_unavailable_sink_says_so() {
        // Bits 4-5 clear: a sink that is already busy with someone else.
        let body = [0x01, 0x01, 0x1c, 0x44, 0x00, 0x0a];
        let c = parse_device_info(&body).expect("parse");
        assert_eq!(c.kind, DeviceKind::PrimarySink);
        assert!(!c.session_available);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p castr-miracast wfd`
Expected: FAIL, `cannot find function parse_device_info`.

- [ ] **Step 3: Implement**

Add to `wfd.rs`, beside `device_info_subelement`:

```rust
/// The Wi-Fi Alliance display OUI, and the type that marks its information
/// element among the vendor elements a device advertises.
pub const WFD_OUI: [u8; 3] = [0x50, 0x6f, 0x9a];
pub const WFD_OUI_TYPE: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Source,
    PrimarySink,
    SecondarySink,
    DualRole,
}

/// What a device says about itself before anything connects to it: enough to
/// tell a television from a printer, and to know the port, the ceiling and
/// whether it wants content protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCaps {
    pub kind: DeviceKind,
    pub session_available: bool,
    pub content_protection: bool,
    pub rtsp_port: u16,
    pub max_throughput_mbps: u16,
}

/// Reads the 6-byte device-information subelement body: a flags field, the
/// session-management port, and a throughput ceiling in Mbit/s.
///
/// The mirror of `device_info_subelement`, which builds the same thing.
pub fn parse_device_info(body: &[u8]) -> Option<DeviceCaps> {
    if body.len() < 6 {
        return None;
    }
    let info = u16::from_be_bytes([body[0], body[1]]);
    Some(DeviceCaps {
        kind: match info & 0x0003 {
            0 => DeviceKind::Source,
            1 => DeviceKind::PrimarySink,
            2 => DeviceKind::SecondarySink,
            _ => DeviceKind::DualRole,
        },
        // Bits 4-5: 01 means a session is free to be started.
        session_available: (info >> 4) & 0x0003 == 1,
        content_protection: info & 0x0100 != 0,
        rtsp_port: u16::from_be_bytes([body[2], body[3]]),
        max_throughput_mbps: u16::from_be_bytes([body[4], body[5]]),
    })
}
```

- [ ] **Step 4: Run both suites, then commit**

Run: `cargo test -q --workspace` and `bash scripts/pi/test-linux.sh`

```bash
git add crates/castr-miracast/src/wfd.rs
git commit -m "feat(miracast): read a display's Wi-Fi Display information element

Tested against bytes captured from a Samsung television, our own sink and
the builder that produces it - a display can now be told from a printer, and
its port, ceiling and content-protection support known, before anything
connects to it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: The crate, and the decisions it makes

**Files:**
- Create: `crates/castr-wifidirect-win/Cargo.toml`
- Create: `crates/castr-wifidirect-win/src/lib.rs`
- Create: `crates/castr-wifidirect-win/src/select.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Produces: `select::Candidate { id: String, name: String, caps: Option<DeviceCaps> }`, `select::match_by_name(candidates: &[Candidate], name: &str) -> Result<Candidate, NoMatch>`, `select::NoMatch { NotFound, NotADisplay(String), Ambiguous(Vec<String>) }`, `select::WaitPolicy`

- [ ] **Step 1: Create the crate**

`crates/castr-wifidirect-win/Cargo.toml`:

```toml
[package]
name = "castr-wifidirect-win"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
castr-miracast = { path = "../castr-miracast" }

[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [
    "Foundation",
    "Foundation_Collections",
    "Devices_Enumeration",
    "Devices_WiFiDirect",
    "Networking",
    "Storage_Streams",
] }
```

Add `"crates/castr-wifidirect-win"` to the workspace `members` in the root
`Cargo.toml`.

`crates/castr-wifidirect-win/src/lib.rs`:

```rust
//! Forming a Wi-Fi Direct group with a Miracast display, on Windows.
//!
//! The radio half of casting to an ordinary display: find it, pair with it,
//! bring the group up, and hand back an address the media path can use.
//!
//! WinRT needs real hardware, so nothing in `radio` can be tested. Every
//! decision therefore lives in `select` and `failure`, which are pure; `radio`
//! is only the calls.

pub mod failure;
pub mod select;
#[cfg(windows)]
pub mod radio;
```

- [ ] **Step 2: Write the failing tests for `select`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use castr_miracast::wfd::{DeviceCaps, DeviceKind};

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
        }
    }

    fn printer(name: &str) -> Candidate {
        Candidate {
            id: format!("WiFiDirect#{name}"),
            name: name.to_string(),
            caps: None,
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
        assert_eq!(match_by_name(&all, "Living").unwrap().name, "Living Room TV");
    }

    #[test]
    fn an_ambiguous_prefix_lists_the_candidates() {
        // Guessing between two televisions is worse than asking.
        let all = [sink("Living Room TV"), sink("Living Room Soundbar")];
        match match_by_name(&all, "Living") {
            Err(NoMatch::Ambiguous(names)) => assert_eq!(names.len(), 2),
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn a_printer_is_named_rather_than_silently_skipped() {
        // "Not found" would be a lie: it is right there, it is just not a
        // display, and the user needs to know which.
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
    fn waiting_gives_up_at_the_timeout() {
        let p = WaitPolicy::new(std::time::Duration::from_secs(60));
        let start = std::time::Instant::now();
        assert!(p.keep_waiting(start));
        assert!(!p.keep_waiting(start - std::time::Duration::from_secs(61)));
    }
}
```

- [ ] **Step 3: Run and watch it fail**

Run: `cargo test -q -p castr-wifidirect-win`
Expected: FAIL, `cannot find type Candidate`.

- [ ] **Step 4: Implement `select.rs`**

```rust
//! Which display, by what ceremony, and how long to keep looking.
//!
//! Pure: the radio hands in what it saw, this decides what to do about it.

use castr_miracast::wfd::{DeviceCaps, DeviceKind};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    /// What its Wi-Fi Display element said, if it published one at all.
    pub caps: Option<DeviceCaps>,
}

impl Candidate {
    /// A display we could cast to: it published an element and it is a sink.
    pub fn is_display(&self) -> bool {
        matches!(
            self.caps.map(|c| c.kind),
            Some(DeviceKind::PrimarySink) | Some(DeviceKind::SecondarySink) | Some(DeviceKind::DualRole)
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum NoMatch {
    NotFound,
    /// It is there, it is just not something that can show a picture.
    NotADisplay(String),
    Ambiguous(Vec<String>),
}

impl std::fmt::Display for NoMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoMatch::NotFound => write!(
                f,
                "discovery: no display of that name is advertising. \
                 Open Screen Mirroring on it - most displays advertise only while that screen is up"
            ),
            NoMatch::NotADisplay(n) => write!(
                f,
                "discovery: {n:?} is a Wi-Fi Direct device but not a display"
            ),
            NoMatch::Ambiguous(names) => {
                write!(f, "discovery: {:?} could mean any of: {}", "that name", names.join(", "))
            }
        }
    }
}

impl std::error::Error for NoMatch {}

/// Exact name, then case-insensitively, then a unique prefix.
pub fn match_by_name(candidates: &[Candidate], name: &str) -> Result<Candidate, NoMatch> {
    let exact: Vec<&Candidate> = candidates.iter().filter(|c| c.name == name).collect();
    let insensitive: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.name.eq_ignore_ascii_case(name))
        .collect();
    let prefix: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            c.name
                .to_ascii_lowercase()
                .starts_with(&name.to_ascii_lowercase())
        })
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
```

- [ ] **Step 5: Run until green, then commit**

```bash
git add crates/castr-wifidirect-win Cargo.toml
git commit -m "feat(wifidirect): choose a display, a ceremony, and when to give up

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Naming the failures

**Files:**
- Create: `crates/castr-wifidirect-win/src/failure.rs`

**Interfaces:**
- Produces: `failure::Stage { Discovery, Pairing, Association, Address }`, `failure::pairing_status(code: i32) -> String`, `failure::parse_wlan_failure(text: &str) -> Option<String>`, `failure::looks_like_stale_credentials(reason: &str) -> bool`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly what `wevtutil` printed on 2026-09-04, when an hour went into
    /// finding this sentence by hand.
    const REAL_8002: &str = "\
Event[0]:
  Log Name: Microsoft-Windows-WLAN-AutoConfig/Operational
  Source: Microsoft-Windows-WLAN-AutoConfig
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
        assert!(!looks_like_stale_credentials("The network password is not correct."));
    }

    #[test]
    fn a_pairing_status_reads_as_words_not_a_number() {
        // DevicePairingResultStatus(19) cost an afternoon as a bare integer.
        assert!(pairing_status(19).contains("failed"));
        assert!(pairing_status(0).contains("paired"));
        assert!(pairing_status(3).contains("already"));
        assert!(pairing_status(14).contains("cancel"));
        assert!(pairing_status(4242).contains("4242"), "unknown codes keep their number");
    }

    #[test]
    fn every_stage_names_itself() {
        assert_eq!(Stage::Discovery.to_string(), "discovery");
        assert_eq!(Stage::Association.to_string(), "association");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -q -p castr-wifidirect-win failure`
Expected: FAIL, `cannot find function parse_wlan_failure`.

- [ ] **Step 3: Implement**

```rust
//! Turning what Windows says into something a person can act on.
//!
//! A bare `DevicePairingResultStatus(19)` cost an afternoon; the sentence that
//! actually solved it was in the WLAN AutoConfig log, where nobody would think
//! to look. So the radio reads that log itself and quotes it.
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
        5 => "the display has too many connections".into(),
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
/// that is not there, because from the radio's point of view that is what it
/// is.
pub fn looks_like_stale_credentials(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    r.contains("not available") || r.contains("cannot be found") || r.contains("not found")
}
```

- [ ] **Step 4: Run and commit**

```bash
git add crates/castr-wifidirect-win/src/failure.rs
git commit -m "feat(wifidirect): say what failed, in words, at the stage it failed

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: The WinRT shell

**Files:**
- Create: `crates/castr-wifidirect-win/src/radio.rs`

**Interfaces:**
- Consumes: `select`, `failure`, `castr_miracast::wfd`
- Produces: `radio::discover(timeout: Duration) -> anyhow::Result<Vec<Candidate>>`, `radio::Connection`, `Connection::remote_ip(&self) -> IpAddr`, `Connection::rtsp_port(&self) -> u16`, `radio::connect(name: &str, wait: WaitPolicy, pin: &mut dyn FnMut() -> anyhow::Result<String>) -> anyhow::Result<Connection>`

Every call below was run in the spike on 2026-09-04 and worked; use them as
written rather than searching for alternatives.

- [ ] **Step 1: Write `radio.rs`**

The shape, with the proven calls:

```rust
use windows::core::HSTRING;
use windows::Devices::Enumeration::{
    DeviceInformation, DeviceInformationCustomPairing, DevicePairingKinds,
    DevicePairingProtectionLevel, DevicePairingRequestedEventArgs,
};
use windows::Devices::WiFiDirect::{
    WiFiDirectConfigurationMethod, WiFiDirectConnectionParameters, WiFiDirectDevice,
    WiFiDirectDeviceSelectorType, WiFiDirectInformationElement,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::DataReader;
```

1. **`selector()`** —
   `WiFiDirectDevice::GetDeviceSelector2(WiFiDirectDeviceSelectorType::AssociationEndpoint)`.
2. **`enumerate()`** — `DeviceInformation::FindAllAsyncAqsFilterAndAdditionalProperties`
   with `["System.Devices.WiFiDirect.InformationElements"]`, then for each
   device `WiFiDirectInformationElement::CreateFromDeviceInformation(&d)`.
   Read each element's `Oui()` and `Value()` through
   `DataReader::FromBuffer(&buf)` then `ReadBytes(&mut v)`. Keep the element
   whose OUI is `wfd::WFD_OUI` and whose `OuiType()` is `wfd::WFD_OUI_TYPE`;
   its value is the subelement list, so skip the 1-byte id and 2-byte length
   and hand the rest to `wfd::parse_device_info`. Build a `Candidate` per
   device.
3. **`discover(timeout)`** — enumerate once; used for listing.
4. **`connect(name, wait, pin)`** —
   - Poll `enumerate()` while `wait.keep_waiting(start)`, applying
     `select::match_by_name` each time; return its `NoMatch` if the wait
     expires. Log once when starting to wait, so a hanging command explains
     itself.
   - If not `Pairing()?.IsPaired()?`, run the ceremony: build
     `WiFiDirectConnectionParameters::new()?`, `SetGroupOwnerIntent(0)?` (the
     display should own the group), then
     `PreferenceOrderedConfigurationMethods()?` — `Clear()?` and `Append()?`
     `WiFiDirectConfigurationMethod::ProvidePin`. Register
     `Custom()?.PairingRequested(&TypedEventHandler::new(...))` whose body calls
     `args.AcceptWithPin(&HSTRING::from(pin()?))`, then
     `PairWithProtectionLevelAndSettingsAsync(DevicePairingKinds::ProvidePin,
     DevicePairingProtectionLevel::Default, &params)?.get()?`. Anything but
     `Paired` or `AlreadyPaired` is a `Stage::Pairing` failure carrying
     `failure::pairing_status(status.0)`.
   - `WiFiDirectDevice::FromIdAsync(&id)?.get()?`, keeping the device.
   - `GetConnectionEndpointPairs()?`, taking `RemoteHostName()?.ToString()?`;
     retry for a few seconds, because DHCP has to finish first. Empty after
     that is a `Stage::Address` failure.
   - On an association or address failure while the display was already paired:
     read the WLAN log (step 2), and if
     `failure::looks_like_stale_credentials` says so, `UnpairAsync` and retry
     the whole connect exactly once, logging why.
5. **`Connection`** holds the `WiFiDirectDevice`, the remote `IpAddr` and the
   RTSP port from the element. It implements `Drop` with a log line; dropping
   the device is the teardown, so `Drop` needs no explicit call.
6. **Watch the link.** Register `ConnectionStatusChanged` on the device and set
   an `Arc<AtomicBool>` when it reports `Disconnected`. `Connection::is_up()`
   exposes it, so a display switched off mid-cast is noticed by the radio
   rather than only by a keep-alive expiring thirty seconds later.

- [ ] **Step 2: Read the WLAN log**

```rust
/// The most recent WLAN AutoConfig connection failure, as Windows recorded it.
///
/// Shelling out to `wevtutil` rather than binding the event-log API: it is
/// present on every Windows, this runs once on a failure path, and the parsing
/// it feeds is tested.
fn last_wlan_failure() -> Option<String> {
    let out = std::process::Command::new("wevtutil")
        .args([
            "qe",
            "Microsoft-Windows-WLAN-AutoConfig/Operational",
            "/q:*[System[(EventID=8002)]]",
            "/c:1",
            "/rd:true",
            "/f:text",
        ])
        .output()
        .ok()?;
    crate::failure::parse_wlan_failure(&String::from_utf8_lossy(&out.stdout))
}
```

- [ ] **Step 3: Verify it builds, then commit**

Run: `cargo build -q -p castr-wifidirect-win` and `cargo test -q --workspace`

```bash
git add crates/castr-wifidirect-win/src/radio.rs
git commit -m "feat(wifidirect): form a group with a display and hand back its address

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Cast by name

**Files:**
- Modify: `crates/castr-sender/src/miracast_cast.rs`
- Modify: `crates/castr-sender/src/main.rs`
- Modify: `crates/castr-sender/Cargo.toml`

- [ ] **Step 1: Accept a name as well as an address**

Add `castr-wifidirect-win = { path = "../castr-wifidirect-win" }` to the
sender's `cfg(windows)` dependencies.

In `main.rs`, the `MiracastCast` arm: if the target parses as a socket address,
behave exactly as today. Otherwise treat it as a display name, and:

```rust
let mut connection = castr_wifidirect_win::radio::connect(
    &target,
    castr_wifidirect_win::select::WaitPolicy::new(Duration::from_secs(60)),
    &mut || {
        println!("Enter the PIN shown on {target:?}:");
        let mut pin = String::new();
        std::io::stdin().read_line(&mut pin)?;
        Ok(pin.trim().to_string())
    },
)?;
let addr = SocketAddr::new(connection.remote_ip(), connection.rtsp_port());
let result = miracast_cast::cast_to(addr, opts);
drop(connection); // the group goes with it
result
```

The connection must outlive the cast and be dropped after it, which is what
tears the group down.

- [ ] **Step 2: Add a listing command**

`castr-sender miracast-list` prints what is advertising, marking which are
displays and which are not, with port and throughput. It is the first thing
anyone will reach for when a cast says "not advertising".

- [ ] **Step 3: Build, run both suites, commit**

```bash
git add crates/castr-sender
git commit -m "feat(sender): cast to a Miracast display by name

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Verify against the Pi, and write it up

**Files:**
- Create: `docs/superpowers/verification/2026-09-04-castr-miracast-radio-e2e.md`
- Modify: `README.md`

- [ ] **Step 1: List**

Run: `castr-sender miracast-list`
Expected: the Pi shown as a display with port 7236; the Epson printer shown as
not a display.

- [ ] **Step 2: Cast by name, twice**

```
castr-sender miracast-cast DietPi --duration 60
castr-sender miracast-cast DietPi --duration 60
```

Expected: the first may prompt for the PIN the Pi is showing; the second should
not. Both cast. Between them the Pi returns to advertising with a fresh PIN.

Watch: `ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver -f'`

- [ ] **Step 3: Provoke the four failures that can be provoked**

- an unknown name → "not advertising", with the advice
- a printer's name → "not a display"
- an ambiguous prefix → the candidates listed
- a wrong PIN → "the PIN was not accepted"

- [ ] **Step 4: Write the verification document and update the README**

Follow `docs/superpowers/verification/2026-09-04-castr-miracast-source-e2e.md`:
one row per claim, PASS / INCONCLUSIVE / NOT RUN, evidence quoted. Association
and address failures are unit-tested only and must be marked as such.

Update the README: `miracast-cast` now takes a name, `miracast-list` exists, and
the "Known gaps" entry about needing an address is no longer true.

```bash
git add docs README.md
git commit -m "docs: Wi-Fi Direct radio layer verification

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Notes for the executor

- **The spike is the reference, not a starting point to copy.** It lives in the
  scratchpad, is throwaway, and has no error handling worth keeping. What it
  provides is certainty about which API calls work.
- **`Connection` owning the group is the design.** If you find yourself adding a
  `disconnect()`, stop: something else is wrong.
- **Do not unpair on teardown.** The stored pairing is what makes the second
  cast silent.
- **Only the PIN ceremony is implemented.** Push-button pairing is deliberately
  left out: the spike proved PIN works against a real sink, and push-button
  cannot be exercised without a display that demands it. Adding an untestable
  branch now would be speculation. It belongs in part 4, with hardware.
