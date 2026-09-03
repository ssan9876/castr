//! The only part of the health check that touches Windows. Each probe is
//! independent and failure-tolerant: a command that errors or prints
//! something unexpected records a note and leaves its fact `None`. Every
//! command is given a 10-second deadline: `run` polls the child rather than
//! blocking on `wait()`, and a command that has not exited by the deadline is
//! killed and reported as a timeout, so a hung `netsh` or `powershell` (a
//! stuck WMI service is the realistic trigger) cannot hang collection.

use crate::diagnose::facts::*;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long a single external command is given to finish before it is killed.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the child is polled for exit while waiting for the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Polls `try_exited` (an analogue of `Child::try_wait().is_ok_and(|s| s.is_some())`)
/// until it reports the child has exited or `deadline` (measured against
/// `now`, both injected so this is testable without spawning a real process)
/// passes. Sleeps `poll_interval` between polls. Returns `true` if the child
/// exited in time, `false` if the deadline was reached first.
fn wait_with_deadline(
    deadline: Duration,
    poll_interval: Duration,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
    mut try_exited: impl FnMut() -> bool,
) -> bool {
    let start = now();
    loop {
        if try_exited() {
            return true;
        }
        if now().duration_since(start) >= deadline {
            return false;
        }
        sleep(poll_interval);
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let mut child: Child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;

    let exited = wait_with_deadline(
        COMMAND_TIMEOUT,
        POLL_INTERVAL,
        Instant::now,
        std::thread::sleep,
        || matches!(child.try_wait(), Ok(Some(_))),
    );

    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{program}: timed out after 10 s"));
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not read output of {program}: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A fake clock: each call to `now()` advances by one `tick` over the
    /// previous value, so the deadline logic can be exercised without a real
    /// process or a real sleep.
    struct FakeClock {
        current: Cell<Instant>,
        tick: Duration,
    }

    impl FakeClock {
        fn new(tick: Duration) -> Self {
            FakeClock {
                current: Cell::new(Instant::now()),
                tick,
            }
        }
        fn now(&self) -> Instant {
            let t = self.current.get();
            self.current.set(t + self.tick);
            t
        }
    }

    #[test]
    fn returns_true_as_soon_as_the_child_reports_exited() {
        let clock = FakeClock::new(Duration::from_millis(1));
        let mut calls = 0;
        let exited = wait_with_deadline(
            Duration::from_secs(10),
            Duration::from_millis(50),
            || clock.now(),
            |_| {},
            || {
                calls += 1;
                calls >= 3
            },
        );
        assert!(exited);
        assert_eq!(calls, 3);
    }

    #[test]
    fn returns_false_once_the_deadline_passes_without_exit() {
        let clock = FakeClock::new(Duration::from_millis(3));
        let exited = wait_with_deadline(
            Duration::from_millis(10),
            Duration::from_millis(1),
            || clock.now(),
            |_| {},
            || false,
        );
        assert!(!exited);
    }

    #[test]
    fn never_sleeps_once_the_child_has_already_exited() {
        let mut slept = false;
        let exited = wait_with_deadline(
            Duration::from_secs(10),
            Duration::from_millis(50),
            Instant::now,
            |_| slept = true,
            || true,
        );
        assert!(exited);
        assert!(!slept);
    }
}
