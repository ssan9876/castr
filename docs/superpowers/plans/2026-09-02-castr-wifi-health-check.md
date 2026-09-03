# Windows Wi-Fi health check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `castr-sender diagnose` tells a Windows user why their Miracast keeps dropping and offers to fix the causes that are safe and reversible.

**Architecture:** Three pure layers plus one impure one. `facts` holds plain data types and the parsers that build them from the text `netsh`, `powercfg` and PowerShell print. `rules` turns facts into findings with a severity and a plain-language reason. `render` prints them. Only `collect` and `fix` touch the system, so everything that carries judgment is unit-tested on any platform with fixtures captured from a real machine.

**Tech Stack:** Rust, `clap` for the subcommand, `eframe`/`egui` for the button, and the operating system's own `netsh`, `powercfg` and PowerShell for data. No new crates.

**Spec:** `docs/superpowers/specs/2026-09-02-castr-miracast-sink-design.md` (part one, section 4)

## Global Constraints

- This is part one of sub-project 3 and ships before the Miracast sink. It touches only `crates/castr-sender`; the receiver, protocol and Pi are untouched.
- No new Rust dependencies. Data comes from `netsh`, `powercfg` and PowerShell, which ship with Windows.
- `facts` and `rules` must compile and their tests must run on every platform, so they contain no Windows-only types and no process execution. `collect` and `fix` are `#[cfg(windows)]`; on other platforms `diagnose` exits with "diagnose is Windows only".
- The tool changes exactly three settings, each only after an explicit prompt, each printing its undo command: wireless adapter power saving, the adapter's power-off permission, and USB selective suspend. It never touches driver settings, never reinstalls anything, never disables Bluetooth.
- Any collection command may fail on a healthy machine. A failed probe produces an `Unknown` finding with the reason, never a crash and never a false `Fail`. The adapter power-management query is known to fail with "A device attached to the system is not functioning" while the radio is idle.
- The report states plainly that this cannot improve Windows' own Miracast implementation.
- Severity vocabulary is fixed: `Ok`, `Info`, `Warn`, `Fail`, `Unknown`.
- Every commit: `cargo fmt -p castr-sender`, `cargo clippy --workspace --tests` clean of new warnings (four pre-existing ones in `clock.rs`, `reassemble.rs`, `session.rs`, `packetize.rs` are known), `cargo test -q --workspace` green.
- Windows dev shell: `export PATH="$PATH:$HOME/.cargo/bin:/c/Program Files/CMake/bin"` before cargo.

---

## File structure

```
crates/castr-sender/src/diagnose/
  mod.rs        public entry points: run(), Report, Severity; platform gate
  facts.rs      Facts and the parsers (pure, tested everywhere)
  rules.rs      Facts -> Vec<Finding> (pure, tested everywhere)
  render.rs     Findings -> text (pure, tested everywhere)
  collect.rs    #[cfg(windows)] runs netsh/powercfg/PowerShell, builds Facts
  fix.rs        #[cfg(windows)] applies and undoes the three safe settings
crates/castr-sender/src/main.rs    + `diagnose` subcommand
crates/castr-sender/src/gui.rs     + "Check my Wi-Fi" button and results panel
README.md                          + a section on the health check
```

---

### Task 1: Facts and parsers

**Files:**
- Create: `crates/castr-sender/src/diagnose/mod.rs`
- Create: `crates/castr-sender/src/diagnose/facts.rs`
- Modify: `crates/castr-sender/src/main.rs` (add `mod diagnose;`)

**Interfaces:**
- Produces: `Facts`, `WlanDriver`, `WirelessDisplay`, `WlanInterface`, `PowerSetting`, and the parsers `parse_wlan_driver(&str) -> Option<WlanDriver>`, `parse_wlan_interface(&str) -> Option<WlanInterface>`, `parse_powercfg_indices(&str) -> Option<PowerSetting>`, `parse_bool_yes_no(&str) -> Option<bool>`.

The fixtures below are verbatim output from the developer's machine on 2026-09-02, except `CONNECTED_IFACE`, which is constructed in the documented format because that machine's radio was idle.

- [ ] **Step 1: Write the failing tests**

Create `facts.rs` with this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const DRIVER: &str = "\
Interface name: Wi-Fi
    Driver                    : Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC
    Vendor                    : Realtek Semiconductor Corp.
    Date                      : 1/15/2024
    Version                   : 2024.10.139.3
    Type                      : Native Wi-Fi Driver
    Radio types supported     : 802.11n 802.11g 802.11b 802.11ac 802.11n 802.11a
    Hosted network supported  : No
    Wireless Display Supported: Yes (Graphics Driver: Yes, Wi-Fi Driver: Yes)
";

    const IDLE_IFACE: &str = "\

There is 1 interface on the system:

    Name                   : Wi-Fi
    Description            : Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC
    GUID                   : 6a2b3c4d-0000-0000-0000-000000000000
    Physical address       : 00:11:22:33:44:55
    State                  : disconnected
";

    const CONNECTED_IFACE: &str = "\

There is 1 interface on the system:

    Name                   : Wi-Fi
    Description            : Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC
    State                  : connected
    SSID                   : home-5g
    BSSID                  : aa:bb:cc:dd:ee:ff
    Network type           : Infrastructure
    Radio type             : 802.11ac
    Authentication         : WPA2-Personal
    Cipher                 : CCMP
    Connection mode        : Profile
    Band                   : 5 GHz
    Channel                : 44
    Receive rate (Mbps)    : 433.3
    Transmit rate (Mbps)   : 433.3
    Signal                 : 74%
";

    const POWERCFG_WIFI: &str = "\
Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)
  GUID Alias: SCHEME_BALANCED
  Subgroup GUID: 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1  (Wireless Adapter Settings)
    Power Setting GUID: 12bbebe6-58d6-4636-95bb-3217ef867c1a  (Power Saving Mode)
      Possible Setting Index: 000
      Possible Setting Friendly Name: Maximum Performance
      Possible Setting Index: 003
      Possible Setting Friendly Name: Maximum Power Saving
    Current AC Power Setting Index: 0x00000000
    Current DC Power Setting Index: 0x00000002
";

    #[test]
    fn reads_the_driver_block() {
        let d = parse_wlan_driver(DRIVER).expect("driver");
        assert_eq!(d.interface, "Wi-Fi");
        assert_eq!(d.name, "Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC");
        assert_eq!(d.version, "2024.10.139.3");
        assert_eq!(d.year, Some(2024));
        assert_eq!(
            d.wireless_display,
            Some(WirelessDisplay { graphics: true, wifi: true })
        );
    }

    #[test]
    fn reads_wireless_display_when_unsupported() {
        let text = DRIVER.replace(
            "Wireless Display Supported: Yes (Graphics Driver: Yes, Wi-Fi Driver: Yes)",
            "Wireless Display Supported: No (Graphics Driver: Yes, Wi-Fi Driver: No)",
        );
        let d = parse_wlan_driver(&text).expect("driver");
        assert_eq!(
            d.wireless_display,
            Some(WirelessDisplay { graphics: true, wifi: false })
        );
    }

    #[test]
    fn a_driver_block_without_the_display_line_is_unknown_not_false() {
        let text: String = DRIVER
            .lines()
            .filter(|l| !l.contains("Wireless Display"))
            .collect::<Vec<_>>()
            .join("\n");
        let d = parse_wlan_driver(&text).expect("driver");
        assert_eq!(d.wireless_display, None);
    }

    #[test]
    fn reads_an_idle_interface() {
        let i = parse_wlan_interface(IDLE_IFACE).expect("interface");
        assert_eq!(i.name, "Wi-Fi");
        assert!(!i.connected);
        assert_eq!(i.band, None);
        assert_eq!(i.signal_pct, None);
    }

    #[test]
    fn reads_a_connected_interface() {
        let i = parse_wlan_interface(CONNECTED_IFACE).expect("interface");
        assert!(i.connected);
        assert_eq!(i.ssid.as_deref(), Some("home-5g"));
        assert_eq!(i.band.as_deref(), Some("5 GHz"));
        assert_eq!(i.channel, Some(44));
        assert_eq!(i.signal_pct, Some(74));
        assert_eq!(i.radio_type.as_deref(), Some("802.11ac"));
    }

    #[test]
    fn no_wireless_interface_at_all_is_none() {
        assert!(parse_wlan_interface("There is 0 interfaces on the system:\n").is_none());
    }

    #[test]
    fn reads_both_powercfg_indices() {
        let p = parse_powercfg_indices(POWERCFG_WIFI).expect("indices");
        assert_eq!(p, PowerSetting { ac: 0, dc: 2 });
    }

    #[test]
    fn powercfg_without_indices_is_none() {
        assert!(parse_powercfg_indices("Invalid Parameters").is_none());
    }

    #[test]
    fn yes_no_parsing_is_case_insensitive_and_tolerant() {
        assert_eq!(parse_bool_yes_no("Yes"), Some(true));
        assert_eq!(parse_bool_yes_no("  no "), Some(false));
        assert_eq!(parse_bool_yes_no("True"), Some(true));
        assert_eq!(parse_bool_yes_no("maybe"), None);
    }
}
```

- [ ] **Step 2: Run the tests to watch them fail**

Run: `cargo test -q -p castr-sender diagnose::facts`
Expected: compile errors, the types and parsers do not exist.

- [ ] **Step 3: Write `mod.rs`**

```rust
//! `castr-sender diagnose`: find and, with consent, remove the local causes of
//! Miracast disconnects on this machine.
//!
//! Layering matters here. `facts` and `rules` are pure: they hold data and
//! judgment and are tested on every platform against output captured from a
//! real machine. `collect` and `fix` are the only parts that touch Windows.

pub mod facts;
pub mod render;
pub mod rules;

#[cfg(windows)]
pub mod collect;
#[cfg(windows)]
pub mod fix;

pub use facts::Facts;
pub use rules::{Finding, Severity};
```

- [ ] **Step 4: Write `facts.rs`**

```rust
//! Plain data describing this machine's Wi-Fi, and the parsers that read it
//! out of the text Windows tools print. Every parser is total: unexpected or
//! missing input yields `None` for that field rather than an error, because a
//! probe that cannot answer must not become a false verdict.

/// Which halves of the wireless-display stack Windows reports as present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WirelessDisplay {
    pub graphics: bool,
    pub wifi: bool,
}

impl WirelessDisplay {
    pub fn both(&self) -> bool {
        self.graphics && self.wifi
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WlanDriver {
    pub interface: String,
    pub name: String,
    pub version: String,
    /// Year only. `netsh` prints the date in the machine's locale, so the day
    /// and month order is ambiguous; the year is all the age check needs.
    pub year: Option<u32>,
    pub wireless_display: Option<WirelessDisplay>,
    pub radio_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WlanInterface {
    pub name: String,
    pub description: String,
    pub connected: bool,
    pub ssid: Option<String>,
    pub band: Option<String>,
    pub channel: Option<u32>,
    pub radio_type: Option<String>,
    pub signal_pct: Option<u32>,
}

/// A power setting's current value on mains and on battery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerSetting {
    pub ac: u32,
    pub dc: u32,
}

// Power-scheme identifiers, here rather than in `collect` so the fix plans can
// name them without depending on a Windows-only module.
/// Wireless Adapter Settings subgroup.
pub const SUB_WIFI: &str = "19cbb8fa-5279-450e-9fac-8a3d5fedd0c1";
/// Power Saving Mode setting inside `SUB_WIFI`.
pub const SETTING_WIFI_POWER: &str = "12bbebe6-58d6-4636-95bb-3217ef867c1a";
/// USB settings subgroup.
pub const SUB_USB: &str = "2a737441-1930-4402-8d77-b2bebba308a3";
/// USB selective suspend setting inside `SUB_USB`.
pub const SETTING_USB_SUSPEND: &str = "48e6b7a6-50f5-4782-a5d4-53bb8f07e226";

/// Everything the checks reason about. Any field may be `None` when its probe
/// failed; `rules` reports that as `Unknown` with the recorded reason.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub driver: Option<WlanDriver>,
    pub interface: Option<WlanInterface>,
    pub wifi_power: Option<PowerSetting>,
    pub usb_suspend: Option<PowerSetting>,
    /// `None` when the query failed; the reason is in `notes`.
    pub allow_power_off: Option<bool>,
    pub bluetooth_active: bool,
    pub adapter_is_usb: bool,
    pub elevated: bool,
    /// Current year, injected so the driver-age rule is testable.
    pub this_year: u32,
    /// The band our own sink uses, so the mismatch rule has something to
    /// compare against. The Pi's radio is 2.4 GHz only.
    pub sink_band_ghz: f32,
    /// Probe failures, keyed by check name, shown verbatim in the report.
    pub notes: Vec<(String, String)>,
}

/// Value after the first colon on a line whose text before the colon contains
/// `key`. `netsh` pads keys with spaces and uses a colon separator.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (lhs, rhs) = line.split_once(':')?;
        if lhs.trim().eq_ignore_ascii_case(key) {
            Some(rhs.trim())
        } else {
            None
        }
    })
}

pub fn parse_bool_yes_no(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// `Wireless Display Supported: Yes (Graphics Driver: Yes, Wi-Fi Driver: Yes)`
fn parse_wireless_display(line: &str) -> Option<WirelessDisplay> {
    let inner = line.split_once('(')?.1;
    let inner = inner.split_once(')')?.0;
    let mut graphics = None;
    let mut wifi = None;
    for part in inner.split(',') {
        let (k, v) = part.split_once(':')?;
        let v = parse_bool_yes_no(v);
        if k.to_ascii_lowercase().contains("graphics") {
            graphics = v;
        } else if k.to_ascii_lowercase().contains("wi-fi") {
            wifi = v;
        }
    }
    Some(WirelessDisplay {
        graphics: graphics?,
        wifi: wifi?,
    })
}

/// Last four-digit run in the string, which is the year in every locale order.
fn year_of(date: &str) -> Option<u32> {
    date.split(|c: char| !c.is_ascii_digit())
        .filter(|p| p.len() == 4)
        .last()
        .and_then(|p| p.parse().ok())
}

pub fn parse_wlan_driver(text: &str) -> Option<WlanDriver> {
    let name = field(text, "Driver")?.to_string();
    let interface = field(text, "Interface name")
        .unwrap_or("Wi-Fi")
        .to_string();
    let wireless_display = text
        .lines()
        .find(|l| l.to_ascii_lowercase().contains("wireless display"))
        .and_then(parse_wireless_display);
    Some(WlanDriver {
        interface,
        name,
        version: field(text, "Version").unwrap_or_default().to_string(),
        year: field(text, "Date").and_then(year_of),
        wireless_display,
        radio_types: field(text, "Radio types supported")
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

pub fn parse_wlan_interface(text: &str) -> Option<WlanInterface> {
    let name = field(text, "Name")?.to_string();
    let state = field(text, "State").unwrap_or("").to_ascii_lowercase();
    Some(WlanInterface {
        name,
        description: field(text, "Description").unwrap_or_default().to_string(),
        connected: state == "connected",
        ssid: field(text, "SSID").map(str::to_string),
        band: field(text, "Band").map(str::to_string),
        channel: field(text, "Channel").and_then(|s| s.parse().ok()),
        radio_type: field(text, "Radio type").map(str::to_string),
        signal_pct: field(text, "Signal")
            .and_then(|s| s.trim_end_matches('%').parse().ok()),
    })
}

/// Reads `Current AC/DC Power Setting Index: 0x00000000` out of a
/// `powercfg /q` block.
pub fn parse_powercfg_indices(text: &str) -> Option<PowerSetting> {
    fn index(text: &str, which: &str) -> Option<u32> {
        let line = text
            .lines()
            .find(|l| l.contains(which) && l.contains("Power Setting Index"))?;
        let raw = line.split(':').nth(1)?.trim();
        let hex = raw.strip_prefix("0x").unwrap_or(raw);
        u32::from_str_radix(hex, 16).ok()
    }
    Some(PowerSetting {
        ac: index(text, "Current AC")?,
        dc: index(text, "Current DC")?,
    })
}
```

Add `mod diagnose;` to `crates/castr-sender/src/main.rs` beside `mod cast;`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -q -p castr-sender diagnose::facts`
Expected: 9 passed.

- [ ] **Step 6: Commit**

```bash
cargo fmt -p castr-sender
git add crates/castr-sender/src/diagnose crates/castr-sender/src/main.rs
git commit -m "feat(diagnose): Wi-Fi facts and parsers with fixtures from a real machine"
```

---

### Task 2: Rules

**Files:**
- Create: `crates/castr-sender/src/diagnose/rules.rs`

**Interfaces:**
- Consumes: everything in `facts`.
- Produces: `Severity` (`Ok`, `Info`, `Warn`, `Fail`, `Unknown`), `FixId` (`WifiPowerSaving`, `AdapterPowerOff`, `UsbSelectiveSuspend`), `Finding { check: &'static str, severity: Severity, found: String, why: &'static str, fix: Option<FixId> }`, `analyse(&Facts) -> Vec<Finding>`, `Severity::worst_of(&[Finding]) -> Severity`, `is_single_antenna_combo(&str) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::facts::*;

    fn base() -> Facts {
        Facts {
            driver: Some(WlanDriver {
                interface: "Wi-Fi".into(),
                name: "Intel(R) Wi-Fi 6E AX211 160MHz".into(),
                version: "23.60.1.1".into(),
                year: Some(2025),
                wireless_display: Some(WirelessDisplay { graphics: true, wifi: true }),
                radio_types: vec!["802.11ac".into()],
            }),
            interface: Some(WlanInterface {
                name: "Wi-Fi".into(),
                description: "Intel(R) Wi-Fi 6E AX211 160MHz".into(),
                connected: true,
                ssid: Some("home".into()),
                band: Some("2.4 GHz".into()),
                channel: Some(6),
                radio_type: Some("802.11n".into()),
                signal_pct: Some(90),
            }),
            wifi_power: Some(PowerSetting { ac: 0, dc: 0 }),
            usb_suspend: Some(PowerSetting { ac: 1, dc: 1 }),
            allow_power_off: Some(false),
            bluetooth_active: false,
            adapter_is_usb: false,
            elevated: false,
            this_year: 2026,
            sink_band_ghz: 2.4,
            notes: vec![],
        }
    }

    fn find<'a>(fs: &'a [Finding], check: &str) -> &'a Finding {
        fs.iter().find(|f| f.check == check).expect(check)
    }

    #[test]
    fn a_healthy_machine_reports_no_warnings() {
        let fs = analyse(&base());
        // `Info` is the resting state for a check whose precondition does not
        // hold (this machine has no USB adapter), so the bar is "nothing worse
        // than Info", which is also what the exit code treats as success.
        assert!(Severity::worst_of(&fs) <= Severity::Info, "{fs:#?}");
    }

    #[test]
    fn missing_wireless_display_support_fails() {
        let mut f = base();
        f.driver.as_mut().unwrap().wireless_display =
            Some(WirelessDisplay { graphics: true, wifi: false });
        let fs = analyse(&f);
        assert_eq!(find(&fs, "Wireless display support").severity, Severity::Fail);
    }

    #[test]
    fn an_unreadable_probe_is_unknown_never_a_failure() {
        let mut f = base();
        f.driver.as_mut().unwrap().wireless_display = None;
        f.allow_power_off = None;
        f.notes.push((
            "Adapter power-off permission".into(),
            "A device attached to the system is not functioning.".into(),
        ));
        let fs = analyse(&f);
        assert_eq!(find(&fs, "Wireless display support").severity, Severity::Unknown);
        let p = find(&fs, "Adapter power-off permission");
        assert_eq!(p.severity, Severity::Unknown);
        assert!(p.found.contains("not functioning"), "the reason is shown: {}", p.found);
        assert_eq!(Severity::worst_of(&fs), Severity::Unknown);
    }

    #[test]
    fn a_driver_older_than_three_years_warns() {
        let mut f = base();
        f.driver.as_mut().unwrap().year = Some(2022);
        assert_eq!(find(&analyse(&f), "Driver age").severity, Severity::Warn);
        f.driver.as_mut().unwrap().year = Some(2023);
        assert_eq!(find(&analyse(&f), "Driver age").severity, Severity::Ok);
    }

    #[test]
    fn a_combo_chip_only_warns_while_bluetooth_is_active() {
        let mut f = base();
        f.driver.as_mut().unwrap().name = "Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC".into();
        assert_eq!(find(&analyse(&f), "Shared Wi-Fi and Bluetooth antenna").severity, Severity::Info);
        f.bluetooth_active = true;
        let fs = analyse(&f);
        assert_eq!(find(&fs, "Shared Wi-Fi and Bluetooth antenna").severity, Severity::Warn);
    }

    #[test]
    fn known_combo_chips_are_recognised_and_others_are_not() {
        assert!(is_single_antenna_combo("Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC"));
        assert!(is_single_antenna_combo("Realtek RTL8723BE Wireless LAN 802.11n PCIe NIC"));
        assert!(!is_single_antenna_combo("Intel(R) Wi-Fi 6E AX211 160MHz"));
    }

    #[test]
    fn a_five_gigahertz_station_link_warns_against_a_two_point_four_sink() {
        let mut f = base();
        f.interface.as_mut().unwrap().band = Some("5 GHz".into());
        assert_eq!(find(&analyse(&f), "Station band vs sink band").severity, Severity::Warn);
    }

    #[test]
    fn band_is_not_judged_while_the_radio_is_idle() {
        let mut f = base();
        let i = f.interface.as_mut().unwrap();
        i.connected = false;
        i.band = None;
        i.signal_pct = None;
        let fs = analyse(&f);
        assert_eq!(find(&fs, "Station band vs sink band").severity, Severity::Info);
        assert_eq!(find(&fs, "Signal strength").severity, Severity::Info);
    }

    #[test]
    fn power_saving_on_either_supply_warns_and_offers_the_fix() {
        let mut f = base();
        f.wifi_power = Some(PowerSetting { ac: 0, dc: 2 });
        let w = find(&analyse(&f), "Wireless adapter power saving");
        assert_eq!(w.severity, Severity::Warn);
        assert_eq!(w.fix, Some(FixId::WifiPowerSaving));
        assert!(w.found.contains("battery"), "names the supply: {}", w.found);
    }

    #[test]
    fn the_power_off_permission_warns_and_offers_the_fix() {
        let mut f = base();
        f.allow_power_off = Some(true);
        let w = find(&analyse(&f), "Adapter power-off permission");
        assert_eq!(w.severity, Severity::Warn);
        assert_eq!(w.fix, Some(FixId::AdapterPowerOff));
    }

    #[test]
    fn usb_selective_suspend_is_only_judged_for_usb_adapters() {
        let mut f = base();
        assert_eq!(find(&analyse(&f), "USB selective suspend").severity, Severity::Info);
        f.adapter_is_usb = true;
        let w = find(&analyse(&f), "USB selective suspend");
        assert_eq!(w.severity, Severity::Warn);
        assert_eq!(w.fix, Some(FixId::UsbSelectiveSuspend));
    }

    #[test]
    fn every_finding_carries_a_reason() {
        for f in analyse(&base()) {
            assert!(!f.why.is_empty(), "{} has no reason", f.check);
            assert!(!f.found.is_empty(), "{} has no observation", f.check);
        }
    }
}
```

- [ ] **Step 2: Run the tests to watch them fail**

Run: `cargo test -q -p castr-sender diagnose::rules`
Expected: compile errors, `analyse` does not exist.

- [ ] **Step 3: Write `rules.rs`**

```rust
//! Judgment. Pure functions from `Facts` to findings, so every rule is
//! testable without a Windows machine.
//!
//! Two principles run through this file. A probe that could not answer yields
//! `Unknown` with the reason, never a `Fail`, because telling somebody their
//! machine is broken on the strength of a failed query is worse than saying
//! nothing. And a rule only fires when its precondition holds: band and signal
//! say nothing while the radio is idle, USB suspend says nothing about a PCIe
//! adapter.

use crate::diagnose::facts::Facts;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Info,
    Unknown,
    Warn,
    Fail,
}

impl Severity {
    pub fn worst_of(findings: &[Finding]) -> Severity {
        findings.iter().map(|f| f.severity).max().unwrap_or(Severity::Ok)
    }
    pub fn marker(&self) -> &'static str {
        match self {
            Severity::Ok => "ok  ",
            Severity::Info => "--  ",
            Severity::Unknown => "?   ",
            Severity::Warn => "warn",
            Severity::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixId {
    WifiPowerSaving,
    AdapterPowerOff,
    UsbSelectiveSuspend,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub check: &'static str,
    pub severity: Severity,
    /// What was observed, in the user's terms.
    pub found: String,
    /// Why it matters for Miracast.
    pub why: &'static str,
    pub fix: Option<FixId>,
}

/// Adapters known to put Wi-Fi and Bluetooth on one antenna. Not exhaustive:
/// an adapter that is not listed produces no finding rather than a wrong one.
const SINGLE_ANTENNA_COMBOS: &[&str] = &[
    "8821ce", "8821ae", "8723be", "8723de", "8723ae", "8822be", "8188", "8192",
    "3165", "3168", "9461", "9462",
];

pub fn is_single_antenna_combo(adapter: &str) -> bool {
    let a = adapter.to_ascii_lowercase();
    SINGLE_ANTENNA_COMBOS.iter().any(|m| a.contains(m))
}

fn note_for(facts: &Facts, check: &str) -> Option<String> {
    facts
        .notes
        .iter()
        .find(|(k, _)| k == check)
        .map(|(_, v)| v.clone())
}

fn unknown(check: &'static str, facts: &Facts, why: &'static str) -> Finding {
    Finding {
        check,
        severity: Severity::Unknown,
        found: note_for(facts, check)
            .unwrap_or_else(|| "could not be read on this machine".into()),
        why,
        fix: None,
    }
}

pub fn analyse(facts: &Facts) -> Vec<Finding> {
    let mut out = Vec::new();

    // Wireless display support.
    match facts.driver.as_ref().and_then(|d| d.wireless_display) {
        Some(wd) if wd.both() => out.push(Finding {
            check: "Wireless display support",
            severity: Severity::Ok,
            found: "the graphics and Wi-Fi drivers both support wireless display".into(),
            why: "Miracast needs both halves; without them Windows will not offer to cast at all.",
            fix: None,
        }),
        Some(wd) => out.push(Finding {
            check: "Wireless display support",
            severity: Severity::Fail,
            found: format!(
                "graphics driver: {}, Wi-Fi driver: {}",
                if wd.graphics { "yes" } else { "no" },
                if wd.wifi { "yes" } else { "no" }
            ),
            why: "Miracast needs both halves; a driver update from the machine's manufacturer is the only fix.",
            fix: None,
        }),
        None => out.push(unknown(
            "Wireless display support",
            facts,
            "Miracast needs both the graphics and Wi-Fi drivers to support it.",
        )),
    }

    // Adapter identity and driver age.
    match facts.driver.as_ref() {
        Some(d) => {
            let age_ok = d.year.is_none_or(|y| y + 3 > facts.this_year);
            out.push(Finding {
                check: "Driver age",
                severity: if age_ok { Severity::Ok } else { Severity::Warn },
                found: match d.year {
                    Some(y) => format!("{}, version {}, dated {}", d.name, d.version, y),
                    None => format!("{}, version {}", d.name, d.version),
                },
                why: "Wi-Fi Direct fixes land in vendor drivers; a driver several years old is a common cause of drops.",
                fix: None,
            });
        }
        None => out.push(unknown(
            "Driver age",
            facts,
            "The adapter's driver version tells us whether known Wi-Fi Direct fixes are present.",
        )),
    }

    // Shared antenna.
    let combo = facts
        .driver
        .as_ref()
        .map(|d| is_single_antenna_combo(&d.name))
        .unwrap_or(false);
    out.push(match (combo, facts.bluetooth_active) {
        (true, true) => Finding {
            check: "Shared Wi-Fi and Bluetooth antenna",
            severity: Severity::Warn,
            found: "this adapter shares one antenna between Wi-Fi and Bluetooth, and Bluetooth is active".into(),
            why: "A Miracast session and Bluetooth traffic then take turns on one antenna, which is a leading cause of mid-cast drops. Turning Bluetooth off during a cast is a reliable test.",
            fix: None,
        },
        (true, false) => Finding {
            check: "Shared Wi-Fi and Bluetooth antenna",
            severity: Severity::Info,
            found: "this adapter shares one antenna with Bluetooth, but no Bluetooth device is active".into(),
            why: "Nothing is competing for the antenna right now.",
            fix: None,
        },
        (false, _) => Finding {
            check: "Shared Wi-Fi and Bluetooth antenna",
            severity: Severity::Ok,
            found: "not a known single-antenna combination adapter".into(),
            why: "Adapters that share one antenna with Bluetooth drop Miracast sessions more often.",
            fix: None,
        },
    });

    // Band and signal, only meaningful while connected.
    let connected = facts.interface.as_ref().map(|i| i.connected).unwrap_or(false);
    let band = facts.interface.as_ref().and_then(|i| i.band.clone());
    out.push(match (connected, band.as_deref()) {
        (true, Some(b)) if b.starts_with('5') && facts.sink_band_ghz < 5.0 => Finding {
            check: "Station band vs sink band",
            severity: Severity::Warn,
            found: format!("connected on {b}, while the sink is on 2.4 GHz"),
            why: "One radio then has to alternate between two bands, and the cast is what starves. Moving this machine to the 2.4 GHz network for the duration removes the split.",
            fix: None,
        },
        (true, Some(b)) => Finding {
            check: "Station band vs sink band",
            severity: Severity::Ok,
            found: format!("connected on {b}, the same band as the sink"),
            why: "One radio serving two bands has to alternate, which starves the cast.",
            fix: None,
        },
        (true, None) => unknown(
            "Station band vs sink band",
            facts,
            "A station link on a different band from the sink makes the radio alternate.",
        ),
        (false, _) => Finding {
            check: "Station band vs sink band",
            severity: Severity::Info,
            found: "the Wi-Fi radio is not connected to a network".into(),
            why: "With no station link there is no band to alternate with, which is the best case for casting.",
            fix: None,
        },
    });

    out.push(match (connected, facts.interface.as_ref().and_then(|i| i.signal_pct)) {
        (true, Some(s)) if s < 60 => Finding {
            check: "Signal strength",
            severity: Severity::Warn,
            found: format!("{s}% on the current network"),
            why: "A weak station link means a noisy radio environment, and the cast shares it.",
            fix: None,
        },
        (true, Some(s)) => Finding {
            check: "Signal strength",
            severity: Severity::Ok,
            found: format!("{s}% on the current network"),
            why: "A weak link means a noisy environment that the cast also has to live in.",
            fix: None,
        },
        (true, None) => unknown("Signal strength", facts, "A weak link means a noisy radio environment."),
        (false, _) => Finding {
            check: "Signal strength",
            severity: Severity::Info,
            found: "the Wi-Fi radio is not connected to a network".into(),
            why: "Signal strength only means something while connected.",
            fix: None,
        },
    });

    // Power saving.
    out.push(match facts.wifi_power {
        Some(p) if p.ac == 0 && p.dc == 0 => Finding {
            check: "Wireless adapter power saving",
            severity: Severity::Ok,
            found: "maximum performance on mains and on battery".into(),
            why: "Power saving parks the radio between packets, which a Wi-Fi Direct link reads as a dropped peer.",
            fix: None,
        },
        Some(p) => Finding {
            check: "Wireless adapter power saving",
            severity: Severity::Warn,
            found: format!(
                "power saving is on ({} on mains, {} on battery)",
                power_level(p.ac),
                power_level(p.dc)
            ),
            why: "Power saving parks the radio between packets, which a Wi-Fi Direct link reads as a dropped peer.",
            fix: Some(FixId::WifiPowerSaving),
        },
        None => unknown(
            "Wireless adapter power saving",
            facts,
            "Power saving parks the radio between packets and drops Wi-Fi Direct links.",
        ),
    });

    out.push(match facts.allow_power_off {
        Some(false) => Finding {
            check: "Adapter power-off permission",
            severity: Severity::Ok,
            found: "Windows is not allowed to power the adapter down".into(),
            why: "When Windows powers the adapter down mid-session the cast ends without explanation.",
            fix: None,
        },
        Some(true) => Finding {
            check: "Adapter power-off permission",
            severity: Severity::Warn,
            found: "Windows is allowed to power this adapter down to save energy".into(),
            why: "When Windows powers the adapter down mid-session the cast ends without explanation.",
            fix: Some(FixId::AdapterPowerOff),
        },
        None => unknown(
            "Adapter power-off permission",
            facts,
            "When Windows powers the adapter down mid-session the cast ends without explanation.",
        ),
    });

    out.push(match (facts.adapter_is_usb, facts.usb_suspend) {
        (false, _) => Finding {
            check: "USB selective suspend",
            severity: Severity::Info,
            found: "not a USB adapter".into(),
            why: "USB selective suspend only affects adapters on the USB bus.",
            fix: None,
        },
        (true, Some(p)) if p.ac == 0 && p.dc == 0 => Finding {
            check: "USB selective suspend",
            severity: Severity::Ok,
            found: "disabled on mains and on battery".into(),
            why: "Suspending a USB Wi-Fi adapter mid-cast drops the session.",
            fix: None,
        },
        (true, Some(_)) => Finding {
            check: "USB selective suspend",
            severity: Severity::Warn,
            found: "enabled for USB devices".into(),
            why: "Suspending a USB Wi-Fi adapter mid-cast drops the session.",
            fix: Some(FixId::UsbSelectiveSuspend),
        },
        (true, None) => unknown(
            "USB selective suspend",
            facts,
            "Suspending a USB Wi-Fi adapter mid-cast drops the session.",
        ),
    });

    out
}

fn power_level(i: u32) -> &'static str {
    match i {
        0 => "maximum performance",
        1 => "low power saving",
        2 => "medium power saving",
        _ => "maximum power saving",
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-sender diagnose::rules`
Expected: 12 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p castr-sender
git add crates/castr-sender/src/diagnose/rules.rs
git commit -m "feat(diagnose): rules turning Wi-Fi facts into findings"
```

---

### Task 3: Report rendering, collection and the `diagnose` subcommand

**Files:**
- Create: `crates/castr-sender/src/diagnose/render.rs`
- Create: `crates/castr-sender/src/diagnose/collect.rs`
- Modify: `crates/castr-sender/src/diagnose/mod.rs` (add `run`)
- Modify: `crates/castr-sender/src/main.rs` (add the subcommand)

**Interfaces:**
- Consumes: `analyse`, `Finding`, `Severity`, `Facts`.
- Produces: `render::report(&[Finding], &Facts) -> String`; `collect::facts() -> Facts` (Windows only); `mod::run(fix: bool) -> anyhow::Result<i32>` returning the process exit code.

- [ ] **Step 1: Write the failing render tests**

At the bottom of `render.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::facts::Facts;
    use crate::diagnose::rules::{FixId, Finding, Severity};

    fn f(check: &'static str, severity: Severity, fix: Option<FixId>) -> Finding {
        Finding { check, severity, found: "observed".into(), why: "because", fix }
    }

    #[test]
    fn every_finding_appears_with_its_marker_and_reason() {
        let fs = vec![
            f("Alpha", Severity::Ok, None),
            f("Beta", Severity::Warn, Some(FixId::WifiPowerSaving)),
        ];
        let out = report(&fs, &Facts::default());
        assert!(out.contains("Alpha"));
        assert!(out.contains("Beta"));
        assert!(out.contains("because"));
        assert!(out.contains("warn"));
    }

    #[test]
    fn the_summary_names_what_can_be_fixed() {
        let fs = vec![f("Beta", Severity::Warn, Some(FixId::WifiPowerSaving))];
        let out = report(&fs, &Facts::default());
        assert!(out.contains("--fix"), "tells the user how to apply fixes: {out}");
    }

    #[test]
    fn a_clean_machine_says_so_and_offers_nothing() {
        let out = report(&[f("Alpha", Severity::Ok, None)], &Facts::default());
        assert!(!out.contains("--fix"));
        assert!(out.to_lowercase().contains("nothing"), "{out}");
    }

    #[test]
    fn the_report_states_its_own_limits() {
        let out = report(&[f("Alpha", Severity::Ok, None)], &Facts::default());
        assert!(
            out.contains("cannot change how Windows itself"),
            "the honesty paragraph is always present: {out}"
        );
    }
}
```

- [ ] **Step 2: Run to watch it fail**

Run: `cargo test -q -p castr-sender diagnose::render`
Expected: compile error, `report` does not exist.

- [ ] **Step 3: Write `render.rs`**

```rust
//! Text rendering of a report. Kept separate from judgment so the wording can
//! change without touching the rules, and so the GUI and the CLI show exactly
//! the same words.

use crate::diagnose::facts::Facts;
use crate::diagnose::rules::{Finding, Severity};

pub fn report(findings: &[Finding], facts: &Facts) -> String {
    let mut s = String::new();
    s.push_str("castr Wi-Fi health check\n\n");
    if let Some(d) = &facts.driver {
        s.push_str(&format!("Adapter: {}\n\n", d.name));
    }
    for f in findings {
        s.push_str(&format!("[{}] {}\n", f.severity.marker(), f.check));
        s.push_str(&format!("       {}\n", f.found));
        s.push_str(&format!("       {}\n", f.why));
    }
    let fixable = findings.iter().filter(|f| f.fix.is_some()).count();
    s.push('\n');
    match Severity::worst_of(findings) {
        Severity::Ok | Severity::Info => {
            s.push_str("Nothing here would explain a Miracast disconnect.\n")
        }
        _ if fixable > 0 => s.push_str(&format!(
            "{fixable} of these can be fixed safely. Run `castr-sender diagnose --fix` to be\nprompted for each one; every change prints the command that undoes it.\n"
        )),
        _ => s.push_str("Nothing here can be fixed automatically.\n"),
    }
    s.push_str(
        "\nThis check cannot change how Windows itself implements Miracast, which lives in\nthe operating system. It finds the local causes of drops and removes the ones\nthat are safe to touch. For your own machines, castr's own protocol over the\nwire avoids the radio entirely.\n",
    );
    s
}
```

- [ ] **Step 4: Run the render tests**

Run: `cargo test -q -p castr-sender diagnose::render`
Expected: 4 passed.

- [ ] **Step 5: Write `collect.rs`**

Every probe is wrapped so a failure becomes a note rather than an error.

```rust
//! The only part of the health check that touches Windows. Each probe is
//! independent and failure-tolerant: a command that errors, times out or
//! prints something unexpected records a note and leaves its fact `None`.

use crate::diagnose::facts::*;
use std::process::Command;

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn powershell(script: &str) -> Result<String, String> {
    run("powershell", &["-NoProfile", "-NonInteractive", "-Command", script])
        .map(|s| s.trim().to_string())
}

pub fn facts() -> Facts {
    let mut f = Facts {
        this_year: this_year(),
        sink_band_ghz: 2.4,
        ..Default::default()
    };

    match run("netsh", &["wlan", "show", "driver"]) {
        Ok(text) => f.driver = parse_wlan_driver(&text),
        Err(e) => f.notes.push(("Wireless display support".into(), e)),
    }
    if f.driver.is_none() {
        f.notes.push((
            "Driver age".into(),
            "no wireless adapter was reported by netsh".into(),
        ));
    }

    match run("netsh", &["wlan", "show", "interfaces"]) {
        Ok(text) => f.interface = parse_wlan_interface(&text),
        Err(e) => f.notes.push(("Station band vs sink band".into(), e)),
    }

    f.wifi_power = powercfg_query(SUB_WIFI);
    if f.wifi_power.is_none() {
        f.notes.push((
            "Wireless adapter power saving".into(),
            "powercfg did not report a wireless power setting".into(),
        ));
    }

    let name = f
        .interface
        .as_ref()
        .map(|i| i.name.clone())
        .or_else(|| f.driver.as_ref().map(|d| d.interface.clone()))
        .unwrap_or_else(|| "Wi-Fi".into());

    match powershell(&format!(
        "(Get-NetAdapterPowerManagement -Name '{name}' -ErrorAction Stop).AllowComputerToTurnOffDevice"
    )) {
        Ok(s) => match s.to_ascii_lowercase().as_str() {
            "enabled" => f.allow_power_off = Some(true),
            "disabled" => f.allow_power_off = Some(false),
            other => f.notes.push((
                "Adapter power-off permission".into(),
                format!("unexpected value {other:?}"),
            )),
        },
        Err(e) => f.notes.push(("Adapter power-off permission".into(), e)),
    }

    f.adapter_is_usb = powershell(&format!(
        "(Get-NetAdapter -Name '{name}' -ErrorAction SilentlyContinue).PnPDeviceID"
    ))
    .map(|id| id.to_ascii_uppercase().starts_with("USB"))
    .unwrap_or(false);
    if f.adapter_is_usb {
        f.usb_suspend = powercfg_query(SUB_USB);
    }

    f.bluetooth_active = powershell(
        "@(Get-PnpDevice -Class Bluetooth -Status OK -ErrorAction SilentlyContinue).Count",
    )
    .ok()
    .and_then(|s| s.trim().parse::<u32>().ok())
    .map(|n| n > 0)
    .unwrap_or(false);

    f.elevated = powershell(
        "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
    )
    .map(|s| s.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(false);

    f
}

fn powercfg_query(subgroup: &str) -> Option<PowerSetting> {
    run("powercfg", &["/q", "SCHEME_CURRENT", subgroup])
        .ok()
        .as_deref()
        .and_then(parse_powercfg_indices)
}

/// Year from the system clock, without pulling in a date crate.
fn this_year() -> u32 {
    powershell("(Get-Date).Year")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(2026)
}
```

- [ ] **Step 6: Wire `run` into `mod.rs` and the CLI**

Append to `mod.rs`. Two whole functions rather than one with `cfg` blocks
inside it, so neither body has to be written around the other's absence:

```rust
/// Runs the health check. Returns the process exit code: 0 when nothing is
/// wrong, 1 when anything warned, failed or could not be read.
#[cfg(not(windows))]
pub fn run(_apply_fixes: bool) -> anyhow::Result<i32> {
    anyhow::bail!("diagnose is Windows only")
}

#[cfg(windows)]
pub fn run(apply_fixes: bool) -> anyhow::Result<i32> {
    let facts = collect::facts();
    let findings = rules::analyse(&facts);
    print!("{}", render::report(&findings, &facts));
    if apply_fixes {
        fix::prompt_and_apply(&findings, &facts)?;
    }
    Ok(match rules::Severity::worst_of(&findings) {
        rules::Severity::Ok | rules::Severity::Info => 0,
        _ => 1,
    })
}
```

In `main.rs`, add the variant to `Cmd`:

```rust
    /// Check this machine's Wi-Fi for the known causes of Miracast drops
    Diagnose {
        /// Offer to apply the safe fixes, prompting for each
        #[arg(long)]
        fix: bool,
    },
```

and the arm in `match cli.cmd`, before the `Cast` arm:

```rust
        Some(Cmd::Diagnose { fix }) => {
            let code = diagnose::run(fix)?;
            std::process::exit(code);
        }
```

`fix::prompt_and_apply` arrives in Task 4. So that this task compiles and is testable on its own, add this interim module to `mod.rs`; Task 4 deletes it and replaces it with `pub mod fix;` plus the real file:

```rust
/// Interim: replaced wholesale by `fix.rs` in Task 4.
#[cfg(windows)]
mod fix {
    use super::{facts::Facts, rules::Finding};
    pub fn prompt_and_apply(_f: &[Finding], _facts: &Facts) -> anyhow::Result<()> {
        println!("(fixes are not implemented yet)");
        Ok(())
    }
}
```

- [ ] **Step 7: Run it against this machine**

Run: `cargo run -q -p castr-sender -- diagnose`
Expected: a report naming the real adapter. On the development machine that is the Realtek 8821CE, with Bluetooth active, the radio disconnected, mains power saving at maximum performance and battery at medium, and the power-off permission unreadable. Compare each line against the values in the spec's section 2 and paste the output into the task report.

Run: `cargo test -q --workspace` and `cargo clippy --workspace --tests`
Expected: green, no new warnings.

- [ ] **Step 8: Commit**

```bash
cargo fmt -p castr-sender
git add crates/castr-sender/src/diagnose crates/castr-sender/src/main.rs
git commit -m "feat(diagnose): collection, rendering and the diagnose subcommand"
```

---

### Task 4: The three fixes

**Files:**
- Create: `crates/castr-sender/src/diagnose/fix.rs`
- Modify: `crates/castr-sender/src/diagnose/mod.rs` (drop the stub, declare the real module)

**Interfaces:**
- Consumes: `FixId`, `Finding`, `Facts`.
- Produces: `plan(FixId, &Facts) -> FixPlan` where `FixPlan { pub label: String, pub apply: Vec<String>, pub undo: Vec<String>, pub needs_admin: bool }`, and `prompt_and_apply(&[Finding], &Facts) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing tests**

`fix.rs` is declared ungated in `mod.rs` (`pub mod fix;`), because `FixPlan` and
`plan` are pure string building and belong with the other tested judgment. Only
`prompt_and_apply` and `run_command_line`, which execute commands and read
stdin, carry `#[cfg(windows)]`. The tests below therefore run on any platform.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::facts::{Facts, PowerSetting, WlanInterface};

    fn facts_with_power(ac: u32, dc: u32) -> Facts {
        Facts {
            interface: Some(WlanInterface {
                name: "Wi-Fi".into(),
                description: String::new(),
                connected: false,
                ssid: None,
                band: None,
                channel: None,
                radio_type: None,
                signal_pct: None,
            }),
            wifi_power: Some(PowerSetting { ac, dc }),
            ..Default::default()
        }
    }

    #[test]
    fn the_power_saving_fix_sets_both_supplies_and_activates_the_scheme() {
        let p = plan(FixId::WifiPowerSaving, &facts_with_power(1, 2));
        assert!(p.apply.iter().any(|c| c.contains("setacvalueindex") && c.ends_with(" 0")));
        assert!(p.apply.iter().any(|c| c.contains("setdcvalueindex") && c.ends_with(" 0")));
        assert!(p.apply.last().unwrap().contains("setactive"));
        assert!(p.needs_admin);
    }

    #[test]
    fn the_power_saving_undo_restores_the_values_that_were_found() {
        let p = plan(FixId::WifiPowerSaving, &facts_with_power(1, 2));
        assert!(p.undo.iter().any(|c| c.contains("setacvalueindex") && c.ends_with(" 1")));
        assert!(p.undo.iter().any(|c| c.contains("setdcvalueindex") && c.ends_with(" 2")));
    }

    #[test]
    fn the_power_off_fix_names_the_real_adapter() {
        let p = plan(FixId::AdapterPowerOff, &facts_with_power(0, 0));
        assert!(p.apply[0].contains("-Name 'Wi-Fi'"));
        assert!(p.apply[0].contains("Disabled"));
        assert!(p.undo[0].contains("Enabled"));
    }

    #[test]
    fn the_usb_fix_targets_the_usb_subgroup() {
        let p = plan(FixId::UsbSelectiveSuspend, &Facts::default());
        assert!(p.apply.iter().any(|c| c.contains(crate::diagnose::facts::SUB_USB)));
    }
}
```

- [ ] **Step 2: Run to watch it fail**

Run: `cargo test -q -p castr-sender diagnose::fix`
Expected: compile error, `plan` does not exist.

- [ ] **Step 3: Write `fix.rs`**

```rust
//! The three changes this tool is willing to make. Each is expressed as the
//! literal commands to apply and to undo it, so the report can print the undo
//! before anything is changed and a reader can run either by hand.

use crate::diagnose::facts::{
    Facts, PowerSetting, SETTING_USB_SUSPEND, SETTING_WIFI_POWER, SUB_USB, SUB_WIFI,
};
use crate::diagnose::rules::{FixId, Finding};

pub struct FixPlan {
    pub label: String,
    pub apply: Vec<String>,
    pub undo: Vec<String>,
    pub needs_admin: bool,
}

fn adapter_name(facts: &Facts) -> String {
    facts
        .interface
        .as_ref()
        .map(|i| i.name.clone())
        .or_else(|| facts.driver.as_ref().map(|d| d.interface.clone()))
        .unwrap_or_else(|| "Wi-Fi".into())
}

pub fn plan(id: FixId, facts: &Facts) -> FixPlan {
    match id {
        FixId::WifiPowerSaving => {
            let cur = facts.wifi_power.unwrap_or(PowerSetting { ac: 0, dc: 0 });
            FixPlan {
                label: "Set wireless adapter power saving to maximum performance".into(),
                apply: vec![
                    format!("powercfg /setacvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} 0"),
                    format!("powercfg /setdcvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} 0"),
                    "powercfg /setactive SCHEME_CURRENT".to_string(),
                ],
                undo: vec![
                    format!("powercfg /setacvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} {}", cur.ac),
                    format!("powercfg /setdcvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} {}", cur.dc),
                    "powercfg /setactive SCHEME_CURRENT".to_string(),
                ],
                needs_admin: true,
            }
        }
        FixId::AdapterPowerOff => {
            let n = adapter_name(facts);
            FixPlan {
                label: "Stop Windows powering the adapter down to save energy".into(),
                apply: vec![format!(
                    "powershell -NoProfile -Command \"Set-NetAdapterPowerManagement -Name '{n}' -AllowComputerToTurnOffDevice Disabled\""
                )],
                undo: vec![format!(
                    "powershell -NoProfile -Command \"Set-NetAdapterPowerManagement -Name '{n}' -AllowComputerToTurnOffDevice Enabled\""
                )],
                needs_admin: true,
            }
        }
        FixId::UsbSelectiveSuspend => {
            let cur = facts.usb_suspend.unwrap_or(PowerSetting { ac: 1, dc: 1 });
            FixPlan {
                label: "Disable USB selective suspend".into(),
                apply: vec![
                    format!("powercfg /setacvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} 0"),
                    format!("powercfg /setdcvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} 0"),
                    "powercfg /setactive SCHEME_CURRENT".to_string(),
                ],
                undo: vec![
                    format!("powercfg /setacvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} {}", cur.ac),
                    format!("powercfg /setdcvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} {}", cur.dc),
                    "powercfg /setactive SCHEME_CURRENT".to_string(),
                ],
                needs_admin: true,
            }
        }
    }
}

/// Offers each available fix in turn. Nothing is changed without a typed `y`,
/// and the undo commands are printed before the change is made, not after.
#[cfg(windows)]
pub fn prompt_and_apply(findings: &[Finding], facts: &Facts) -> anyhow::Result<()> {
    use std::io::Write;
    let plans: Vec<FixPlan> = findings.iter().filter_map(|f| f.fix).map(|id| plan(id, facts)).collect();
    if plans.is_empty() {
        return Ok(());
    }
    if !facts.elevated {
        println!("\nThese changes need an administrator prompt, and this window is not elevated.");
        println!("Re-run `castr-sender diagnose --fix` from an administrator terminal, or run:");
        for p in &plans {
            println!("\n  {}", p.label);
            for c in &p.apply {
                println!("    {c}");
            }
            println!("  to undo:");
            for c in &p.undo {
                println!("    {c}");
            }
        }
        return Ok(());
    }
    for p in &plans {
        println!("\n{}", p.label);
        println!("  will run:");
        for c in &p.apply {
            println!("    {c}");
        }
        println!("  undo with:");
        for c in &p.undo {
            println!("    {c}");
        }
        print!("  apply? [y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("  skipped");
            continue;
        }
        for c in &p.apply {
            match run_command_line(c) {
                Ok(()) => {}
                Err(e) => {
                    println!("  failed: {e}");
                    break;
                }
            }
        }
        println!("  applied; undo with the commands above");
    }
    Ok(())
}

/// Runs one of the literal command lines above. Split is deliberate and
/// simple: these are our own strings, not user input.
#[cfg(windows)]
fn run_command_line(line: &str) -> anyhow::Result<()> {
    let mut parts = line.splitn(2, ' ');
    let program = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or_default();
    let status = if program == "powershell" {
        // The PowerShell forms carry one quoted -Command argument.
        let script = rest
            .split_once("-Command ")
            .map(|(_, s)| s.trim().trim_matches('"'))
            .unwrap_or(rest);
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .status()?
    } else {
        std::process::Command::new(program)
            .args(rest.split_whitespace())
            .status()?
    };
    anyhow::ensure!(status.success(), "{line} exited with {status}");
    Ok(())
}
```

Delete the interim `mod fix` block from `mod.rs` and declare the real module
beside the other pure ones: `pub mod fix;` with no `cfg`, since only two of its
functions are Windows-gated.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-sender diagnose`
Expected: all `facts`, `rules`, `render` and `fix` tests pass.

- [ ] **Step 5: Try it for real, without changing anything**

Run: `cargo run -q -p castr-sender -- diagnose --fix`
Expected: because the shell is not elevated, it prints the commands and the undo lines and changes nothing. Confirm no setting changed by re-running `powercfg /q SCHEME_CURRENT 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1` and comparing with the values in the spec (AC index 0, DC index 2). Paste both into the task report.

- [ ] **Step 6: Commit**

```bash
cargo fmt -p castr-sender
git add crates/castr-sender/src/diagnose
git commit -m "feat(diagnose): the three safe fixes, with undo printed before apply"
```

---

### Task 5: GUI button, README, and verification on the real machine

**Files:**
- Modify: `crates/castr-sender/src/gui.rs`
- Modify: `README.md`
- Create: `docs/superpowers/verification/2026-09-02-castr-wifi-health-check.md`

**Interfaces:**
- Consumes: `diagnose::collect::facts`, `diagnose::rules::analyse`, `diagnose::render::report`.

- [ ] **Step 1: Add the button and results panel**

In `gui.rs`, add to `App`:

```rust
    /// `None` until the check has been run; `Some(text)` afterwards.
    wifi_report: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    wifi_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
```

initialise both where `App` is built inside `run_gui`, then add this to the top
row of `update`, beside the existing controls:

```rust
                if ui
                    .add_enabled(
                        !self.wifi_running.load(std::sync::atomic::Ordering::Relaxed),
                        egui::Button::new("Check my Wi-Fi"),
                    )
                    .on_hover_text("Looks for the local causes of Miracast disconnects")
                    .clicked()
                {
                    let out = self.wifi_report.clone();
                    let running = self.wifi_running.clone();
                    running.store(true, std::sync::atomic::Ordering::Relaxed);
                    std::thread::spawn(move || {
                        #[cfg(windows)]
                        let text = {
                            let facts = crate::diagnose::collect::facts();
                            let findings = crate::diagnose::rules::analyse(&facts);
                            crate::diagnose::render::report(&findings, &facts)
                        };
                        #[cfg(not(windows))]
                        let text = "The Wi-Fi health check is Windows only.".to_string();
                        *out.lock().unwrap() = Some(text);
                        running.store(false, std::sync::atomic::Ordering::Relaxed);
                    });
                }
```

and, below the receiver list, the panel:

```rust
            let report = self.wifi_report.lock().unwrap().clone();
            if let Some(text) = report {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.heading("Wi-Fi health");
                    if ui.button("Copy").clicked() {
                        ui.output_mut(|o| o.copied_text = text.clone());
                    }
                    if ui.button("Close").clicked() {
                        *self.wifi_report.lock().unwrap() = None;
                    }
                });
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.monospace(text);
                    });
            }
```

The button runs the check on a worker thread because the probes shell out and take a second or two; the GUI thread must not block.

- [ ] **Step 2: Check the GUI compiles and the button works**

Run: `cargo build -q -p castr-sender && cargo run -q -p castr-sender`
Expected: the window opens, "Check my Wi-Fi" is clickable, and after a moment the report appears in a scrollable panel naming the real adapter. Close the window.

- [ ] **Step 3: README**

Add after the "Pairing and casting from the CLI" section (the outer fence here
is four backticks because the snippet contains a fenced block of its own):

````markdown
### Wi-Fi health check

Miracast drops are usually caused by the sending machine, not the display.

```
castr-sender diagnose         # report only
castr-sender diagnose --fix   # offers each safe fix, prompting for every one
```

It checks whether the graphics and Wi-Fi drivers support wireless display, how
old the driver is, whether the adapter shares one antenna with Bluetooth,
whether the station link is on a different band from the sink, the signal
strength, and the three power settings that park a radio mid-session. The three
power settings are the only things it will change, always after a prompt, and
it prints the command to undo each one before applying it. It never touches
driver settings and never disables Bluetooth.

It cannot change how Windows implements Miracast, which lives in the operating
system. For your own machines, castr's own protocol avoids the radio entirely.
````

- [ ] **Step 4: Verify against the values measured by hand**

Run `cargo run -q -p castr-sender -- diagnose` and compare every line with the spec's section 2, which recorded this machine on 2026-09-02: Realtek 8821CE, driver 2024.10.139.3 dated 2024, wireless display supported by both halves, Wi-Fi disconnected, Bluetooth active, mains power saving at maximum performance and battery at medium power saving, and the adapter power-management query failing with a device error.

Write `docs/superpowers/verification/2026-09-02-castr-wifi-health-check.md` with: the commands run, the full report output, a table comparing each check against the hand-measured value with a PASS or FAIL, the `--fix` run showing that an unelevated shell changes nothing, and a `powercfg` readback before and after proving nothing moved.

- [ ] **Step 5: Full suite and commit**

Run: `cargo test -q --workspace`, `cargo clippy --workspace --tests`
Expected: green, no new warnings beyond the four known ones.

```bash
cargo fmt -p castr-sender
git add crates/castr-sender/src/gui.rs README.md docs/superpowers/verification/2026-09-02-castr-wifi-health-check.md
git commit -m "feat(diagnose): GUI button, README, and verification on a real machine"
```
