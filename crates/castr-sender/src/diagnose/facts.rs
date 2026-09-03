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
    /// `None` when the probe could not tell us; the reason is in `notes`.
    pub bluetooth_active: Option<bool>,
    /// `None` when the probe could not tell us which bus the adapter is on;
    /// the reason is in `notes`.
    pub adapter_is_usb: Option<bool>,
    pub elevated: bool,
    /// Current year, injected so the driver-age rule is testable.
    pub this_year: u32,
    /// The band our own sink uses, so the mismatch rule has something to
    /// compare against. The Pi's radio is 2.4 GHz only.
    pub sink_band_ghz: f32,
    /// Probe failures, keyed by check name, shown verbatim in the report.
    pub notes: Vec<(String, String)>,
}

/// Quotes `s` for interpolation into a PowerShell single-quoted string:
/// doubles every embedded single quote and wraps the result in single quotes.
/// Windows allows apostrophes (and worse) in adapter names, so every name
/// that reaches a PowerShell command line must go through this first.
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Current year from the system clock, computed without a date crate or a
/// process launch: days since the Unix epoch, converted to a civil date by
/// Howard Hinnant's `civil_from_days` algorithm.
pub fn year_from_epoch_days(days: i64) -> i32 {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }) as i32
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
        .rfind(|p| p.len() == 4)
        .and_then(|p| p.parse().ok())
}

pub fn parse_wlan_driver(text: &str) -> Option<WlanDriver> {
    let name = field(text, "Driver")?.to_string();
    let interface = field(text, "Interface name").unwrap_or("Wi-Fi").to_string();
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
        signal_pct: field(text, "Signal").and_then(|s| s.trim_end_matches('%').parse().ok()),
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

    const IDLE_IFACE: &str = "\n\
There is 1 interface on the system:

    Name                   : Wi-Fi
    Description            : Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC
    GUID                   : 6a2b3c4d-0000-0000-0000-000000000000
    Physical address       : 00:11:22:33:44:55
    State                  : disconnected
";

    const CONNECTED_IFACE: &str = "\n\
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
            Some(WirelessDisplay {
                graphics: true,
                wifi: true
            })
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
            Some(WirelessDisplay {
                graphics: true,
                wifi: false
            })
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
    fn ps_quote_escapes_embedded_single_quotes() {
        assert_eq!(ps_quote("Bob's Wi-Fi"), "'Bob''s Wi-Fi'");
        assert_eq!(ps_quote("Wi-Fi"), "'Wi-Fi'");
    }

    #[test]
    fn epoch_days_for_2026_01_01_is_2026() {
        assert_eq!(year_from_epoch_days(20454), 2026);
        // A day earlier is still the previous year.
        assert_eq!(year_from_epoch_days(20453), 2025);
    }

    #[test]
    fn yes_no_parsing_is_case_insensitive_and_tolerant() {
        assert_eq!(parse_bool_yes_no("Yes"), Some(true));
        assert_eq!(parse_bool_yes_no("  no "), Some(false));
        assert_eq!(parse_bool_yes_no("True"), Some(true));
        assert_eq!(parse_bool_yes_no("maybe"), None);
    }
}
