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
        findings
            .iter()
            .map(|f| f.severity)
            .max()
            .unwrap_or(Severity::Ok)
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

/// Adapters known to put Wi-Fi and Bluetooth on one antenna: the Realtek
/// 8723/8821/8822 combo families and the Intel 3165/3168/9461/9462 combo
/// parts. Not exhaustive: an adapter that is not listed produces no finding
/// rather than a wrong one.
const SINGLE_ANTENNA_COMBOS: &[&str] = &[
    "8821ce", "8821ae", "8723be", "8723de", "8723ae", "8822be", "3165", "3168", "9461", "9462",
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
        found: note_for(facts, check).unwrap_or_else(|| "could not be read on this machine".into()),
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
        Some(d) => match d.year {
            Some(y) => {
                let age_ok = y + 3 >= facts.this_year;
                out.push(Finding {
                    check: "Driver age",
                    severity: if age_ok { Severity::Ok } else { Severity::Warn },
                    found: format!("{}, version {}, dated {}", d.name, d.version, y),
                    why: "Wi-Fi Direct fixes land in vendor drivers; a driver several years old is a common cause of drops.",
                    fix: None,
                });
            }
            None => out.push(Finding {
                check: "Driver age",
                severity: Severity::Unknown,
                found: format!(
                    "{}, version {}, but the driver date could not be read",
                    d.name, d.version
                ),
                why: "Wi-Fi Direct fixes land in vendor drivers; without a date we cannot tell whether this driver is current.",
                fix: None,
            }),
        },
        None => out.push(unknown(
            "Driver age",
            facts,
            "The adapter's driver version tells us whether known Wi-Fi Direct fixes are present.",
        )),
    }

    // Shared antenna.
    out.push(match facts.driver.as_ref() {
        None => unknown(
            "Shared Wi-Fi and Bluetooth antenna",
            facts,
            "Adapters that share one antenna with Bluetooth drop Miracast sessions more often.",
        ),
        Some(d) => match (is_single_antenna_combo(&d.name), facts.bluetooth_active) {
            (true, Some(true)) => Finding {
                check: "Shared Wi-Fi and Bluetooth antenna",
                severity: Severity::Warn,
                found: "this adapter shares one antenna between Wi-Fi and Bluetooth, and Bluetooth is active".into(),
                why: "A Miracast session and Bluetooth traffic then take turns on one antenna, which is a leading cause of mid-cast drops. Turning Bluetooth off during a cast is a reliable test.",
                fix: None,
            },
            (true, Some(false)) => Finding {
                check: "Shared Wi-Fi and Bluetooth antenna",
                severity: Severity::Info,
                found: "this adapter shares one antenna with Bluetooth, but no Bluetooth device is active".into(),
                why: "Nothing is competing for the antenna right now.",
                fix: None,
            },
            (true, None) => unknown(
                "Shared Wi-Fi and Bluetooth antenna",
                facts,
                "Adapters that share one antenna with Bluetooth drop Miracast sessions more often.",
            ),
            (false, _) => Finding {
                check: "Shared Wi-Fi and Bluetooth antenna",
                severity: Severity::Ok,
                found: "not a known single-antenna combination adapter".into(),
                why: "Adapters that share one antenna with Bluetooth drop Miracast sessions more often.",
                fix: None,
            },
        },
    });

    // Band and signal, only meaningful while connected. An absent interface
    // means the probe itself failed, not that the radio is idle, so it reads
    // Unknown rather than the confident "not connected" Info.
    out.push(match facts.interface.as_ref() {
        None => unknown(
            "Station band vs sink band",
            facts,
            "A station link on a different band from the sink makes the radio alternate.",
        ),
        Some(i) if !i.connected => Finding {
            check: "Station band vs sink band",
            severity: Severity::Info,
            found: "the Wi-Fi radio is not connected to a network".into(),
            why: "With no station link there is no band to alternate with, which is the best case for casting.",
            fix: None,
        },
        Some(i) => match i.band.as_deref() {
            Some(b) if b.starts_with('5') && facts.sink_band_ghz < 5.0 => Finding {
                check: "Station band vs sink band",
                severity: Severity::Warn,
                found: format!("connected on {b}, while the sink is on 2.4 GHz"),
                why: "One radio then has to alternate between two bands, and the cast is what starves. Moving this machine to the 2.4 GHz network for the duration removes the split.",
                fix: None,
            },
            Some(b) => Finding {
                check: "Station band vs sink band",
                severity: Severity::Ok,
                found: format!("connected on {b}, the same band as the sink"),
                why: "One radio serving two bands has to alternate, which starves the cast.",
                fix: None,
            },
            None => unknown(
                "Station band vs sink band",
                facts,
                "A station link on a different band from the sink makes the radio alternate.",
            ),
        },
    });

    out.push(match facts.interface.as_ref() {
        None => unknown(
            "Signal strength",
            facts,
            "A weak link means a noisy radio environment.",
        ),
        Some(i) if !i.connected => Finding {
            check: "Signal strength",
            severity: Severity::Info,
            found: "the Wi-Fi radio is not connected to a network".into(),
            why: "Signal strength only means something while connected.",
            fix: None,
        },
        Some(i) => match i.signal_pct {
            Some(s) if s < 60 => Finding {
                check: "Signal strength",
                severity: Severity::Warn,
                found: format!("{s}% on the current network"),
                why: "A weak station link means a noisy radio environment, and the cast shares it.",
                fix: None,
            },
            Some(s) => Finding {
                check: "Signal strength",
                severity: Severity::Ok,
                found: format!("{s}% on the current network"),
                why: "A weak link means a noisy environment that the cast also has to live in.",
                fix: None,
            },
            None => unknown(
                "Signal strength",
                facts,
                "A weak link means a noisy radio environment.",
            ),
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
        (None, _) => unknown(
            "USB selective suspend",
            facts,
            "USB selective suspend only affects adapters on the USB bus, and this machine's probe for that could not tell us which bus the adapter is on.",
        ),
        (Some(false), _) => Finding {
            check: "USB selective suspend",
            severity: Severity::Info,
            found: "not a USB adapter".into(),
            why: "USB selective suspend only affects adapters on the USB bus.",
            fix: None,
        },
        (Some(true), Some(p)) if p.ac == 0 && p.dc == 0 => Finding {
            check: "USB selective suspend",
            severity: Severity::Ok,
            found: "disabled on mains and on battery".into(),
            why: "Suspending a USB Wi-Fi adapter mid-cast drops the session.",
            fix: None,
        },
        (Some(true), Some(_)) => Finding {
            check: "USB selective suspend",
            severity: Severity::Warn,
            found: "enabled for USB devices".into(),
            why: "Suspending a USB Wi-Fi adapter mid-cast drops the session.",
            fix: Some(FixId::UsbSelectiveSuspend),
        },
        (Some(true), None) => unknown(
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
                wireless_display: Some(WirelessDisplay {
                    graphics: true,
                    wifi: true,
                }),
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
            bluetooth_active: Some(false),
            adapter_is_usb: Some(false),
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
        f.driver.as_mut().unwrap().wireless_display = Some(WirelessDisplay {
            graphics: true,
            wifi: false,
        });
        let fs = analyse(&f);
        assert_eq!(
            find(&fs, "Wireless display support").severity,
            Severity::Fail
        );
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
        assert_eq!(
            find(&fs, "Wireless display support").severity,
            Severity::Unknown
        );
        let p = find(&fs, "Adapter power-off permission");
        assert_eq!(p.severity, Severity::Unknown);
        assert!(
            p.found.contains("not functioning"),
            "the reason is shown: {}",
            p.found
        );
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
        assert_eq!(
            find(&analyse(&f), "Shared Wi-Fi and Bluetooth antenna").severity,
            Severity::Info
        );
        f.bluetooth_active = Some(true);
        let fs = analyse(&f);
        assert_eq!(
            find(&fs, "Shared Wi-Fi and Bluetooth antenna").severity,
            Severity::Warn
        );
    }

    #[test]
    fn a_combo_chip_with_an_unreadable_bluetooth_probe_is_unknown() {
        let mut f = base();
        f.driver.as_mut().unwrap().name = "Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC".into();
        f.bluetooth_active = None;
        let fs = analyse(&f);
        assert_eq!(
            find(&fs, "Shared Wi-Fi and Bluetooth antenna").severity,
            Severity::Unknown
        );
    }

    #[test]
    fn known_combo_chips_are_recognised_and_others_are_not() {
        assert!(is_single_antenna_combo(
            "Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC"
        ));
        assert!(is_single_antenna_combo(
            "Realtek RTL8723BE Wireless LAN 802.11n PCIe NIC"
        ));
        assert!(!is_single_antenna_combo("Intel(R) Wi-Fi 6E AX211 160MHz"));
        assert!(!is_single_antenna_combo(
            "Realtek RTL8188EU Wireless LAN 802.11n USB NIC"
        ));
    }

    #[test]
    fn a_missing_driver_leaves_the_shared_antenna_check_unknown() {
        let mut f = base();
        f.driver = None;
        let fs = analyse(&f);
        assert_eq!(
            find(&fs, "Shared Wi-Fi and Bluetooth antenna").severity,
            Severity::Unknown
        );
    }

    #[test]
    fn a_missing_interface_leaves_band_and_signal_unknown() {
        let mut f = base();
        f.interface = None;
        f.notes.push((
            "Station band vs sink band".into(),
            "The wireless interface query returned nothing.".into(),
        ));
        let fs = analyse(&f);
        let band = find(&fs, "Station band vs sink band");
        assert_eq!(band.severity, Severity::Unknown);
        assert!(
            band.found.contains("returned nothing"),
            "the reason is shown: {}",
            band.found
        );
        assert_eq!(find(&fs, "Signal strength").severity, Severity::Unknown);
    }

    #[test]
    fn a_driver_with_no_readable_year_is_unknown() {
        let mut f = base();
        f.driver.as_mut().unwrap().year = None;
        let fs = analyse(&f);
        let age = find(&fs, "Driver age");
        assert_eq!(age.severity, Severity::Unknown);
        assert!(
            age.found.contains(&f.driver.as_ref().unwrap().name),
            "names the adapter: {}",
            age.found
        );
    }

    #[test]
    fn a_five_gigahertz_station_link_warns_against_a_two_point_four_sink() {
        let mut f = base();
        f.interface.as_mut().unwrap().band = Some("5 GHz".into());
        assert_eq!(
            find(&analyse(&f), "Station band vs sink band").severity,
            Severity::Warn
        );
    }

    #[test]
    fn band_is_not_judged_while_the_radio_is_idle() {
        let mut f = base();
        let i = f.interface.as_mut().unwrap();
        i.connected = false;
        i.band = None;
        i.signal_pct = None;
        let fs = analyse(&f);
        assert_eq!(
            find(&fs, "Station band vs sink band").severity,
            Severity::Info
        );
        assert_eq!(find(&fs, "Signal strength").severity, Severity::Info);
    }

    #[test]
    fn power_saving_on_either_supply_warns_and_offers_the_fix() {
        let mut f = base();
        f.wifi_power = Some(PowerSetting { ac: 0, dc: 2 });
        let fs = analyse(&f);
        let w = find(&fs, "Wireless adapter power saving");
        assert_eq!(w.severity, Severity::Warn);
        assert_eq!(w.fix, Some(FixId::WifiPowerSaving));
        assert!(w.found.contains("battery"), "names the supply: {}", w.found);
    }

    #[test]
    fn the_power_off_permission_warns_and_offers_the_fix() {
        let mut f = base();
        f.allow_power_off = Some(true);
        let fs = analyse(&f);
        let w = find(&fs, "Adapter power-off permission");
        assert_eq!(w.severity, Severity::Warn);
        assert_eq!(w.fix, Some(FixId::AdapterPowerOff));
    }

    #[test]
    fn usb_selective_suspend_is_only_judged_for_usb_adapters() {
        let mut f = base();
        assert_eq!(
            find(&analyse(&f), "USB selective suspend").severity,
            Severity::Info
        );
        f.adapter_is_usb = Some(true);
        let fs = analyse(&f);
        let w = find(&fs, "USB selective suspend");
        assert_eq!(w.severity, Severity::Warn);
        assert_eq!(w.fix, Some(FixId::UsbSelectiveSuspend));
    }

    #[test]
    fn an_unreadable_usb_bus_probe_is_unknown_not_a_confident_no() {
        let mut f = base();
        f.adapter_is_usb = None;
        let fs = analyse(&f);
        assert_eq!(
            find(&fs, "USB selective suspend").severity,
            Severity::Unknown
        );
    }

    #[test]
    fn every_finding_carries_a_reason() {
        for f in analyse(&base()) {
            assert!(!f.why.is_empty(), "{} has no reason", f.check);
            assert!(!f.found.is_empty(), "{} has no observation", f.check);
        }
    }
}
