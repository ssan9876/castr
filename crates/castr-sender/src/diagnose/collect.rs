//! The only part of the health check that touches Windows. Each probe is
//! independent and failure-tolerant: a command that errors or prints
//! something unexpected records a note and leaves its fact `None`. Commands
//! are run to completion; nothing here imposes a timeout of its own.

use crate::diagnose::facts::*;
use std::process::Command;

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn powershell(script: &str) -> Result<String, String> {
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
    .map(|s| s.trim().to_string())
    .map_err(|e| clean_powershell_error(&e))
}

/// PowerShell errors print the message, then `At line:1 char:N`, the offending
/// line, a caret block and a `CategoryInfo`/`FullyQualifiedErrorId` footer.
/// Only the first line is fit to show inline in a report; the rest is noise
/// once we already know which command produced it.
fn clean_powershell_error(e: &str) -> String {
    e.lines().next().unwrap_or(e).trim().to_string()
}

pub fn facts() -> Facts {
    let mut f = Facts {
        this_year: this_year(),
        sink_band_ghz: 2.4,
        ..Default::default()
    };

    match run("netsh", &["wlan", "show", "driver"]) {
        Ok(text) => {
            f.driver = parse_wlan_driver(&text);
            if f.driver.is_none() {
                f.notes.push((
                    "Driver age".into(),
                    "no wireless adapter was reported by netsh".into(),
                ));
            }
        }
        Err(e) => {
            f.notes.push(("Wireless display support".into(), e.clone()));
            f.notes.push(("Driver age".into(), e.clone()));
            f.notes
                .push(("Shared Wi-Fi and Bluetooth antenna".into(), e));
        }
    }

    match run("netsh", &["wlan", "show", "interfaces"]) {
        Ok(text) => {
            f.interface = parse_wlan_interface(&text);
            if f.interface.is_none() {
                f.notes.push((
                    "Station band vs sink band".into(),
                    "no wireless interface was reported by netsh".into(),
                ));
                f.notes.push((
                    "Signal strength".into(),
                    "no wireless interface was reported by netsh".into(),
                ));
            }
        }
        Err(e) => {
            f.notes
                .push(("Station band vs sink band".into(), e.clone()));
            f.notes.push(("Signal strength".into(), e));
        }
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
    let quoted_name = ps_quote(&name);

    match powershell(&format!(
        "(Get-NetAdapterPowerManagement -Name {quoted_name} -ErrorAction Stop).AllowComputerToTurnOffDevice"
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

    match powershell(&format!(
        "(Get-NetAdapter -Name {quoted_name} -ErrorAction Stop).PnPDeviceID"
    )) {
        Ok(id) => f.adapter_is_usb = Some(id.to_ascii_uppercase().starts_with("USB")),
        Err(e) => f.notes.push(("USB selective suspend".into(), e)),
    }
    if f.adapter_is_usb == Some(true) {
        f.usb_suspend = powercfg_query(SUB_USB);
    }

    match powershell("@(Get-PnpDevice -Class Bluetooth -Status OK -ErrorAction Stop).Count") {
        Ok(s) => match s.trim().parse::<u32>() {
            Ok(n) => f.bluetooth_active = Some(n > 0),
            Err(_) => f.notes.push((
                "Shared Wi-Fi and Bluetooth antenna".into(),
                format!("unexpected value from the Bluetooth device count: {s:?}"),
            )),
        },
        Err(e) => f
            .notes
            .push(("Shared Wi-Fi and Bluetooth antenna".into(), e)),
    }

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

/// Current year, computed from the system clock without launching a process
/// or a date crate: days since the Unix epoch, converted to a civil date.
fn this_year() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86_400) as i64;
    year_from_epoch_days(days).max(0) as u32
}
