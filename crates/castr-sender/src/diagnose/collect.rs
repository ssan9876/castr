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
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
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
        Err(e) => {
            f.notes.push(("Wireless display support".into(), e.clone()));
            f.notes
                .push(("Shared Wi-Fi and Bluetooth antenna".into(), e));
        }
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
