//! The three changes this tool is willing to make. Each is expressed as the
//! literal commands to apply and to undo it, so the report can print the undo
//! before anything is changed and a reader can run either by hand.

use crate::diagnose::facts::{
    ps_quote, Facts, PowerSetting, SETTING_USB_SUSPEND, SETTING_WIFI_POWER, SUB_USB, SUB_WIFI,
};
#[cfg(windows)]
use crate::diagnose::rules::Finding;
use crate::diagnose::rules::FixId;

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
                    format!(
                        "powercfg /setacvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} {}",
                        cur.ac
                    ),
                    format!(
                        "powercfg /setdcvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} {}",
                        cur.dc
                    ),
                    "powercfg /setactive SCHEME_CURRENT".to_string(),
                ],
                needs_admin: true,
            }
        }
        FixId::AdapterPowerOff => {
            let raw_name = adapter_name(facts);
            if raw_name.contains('"') {
                // A double quote in the name would break out of the
                // PowerShell -Command string; refuse rather than print a
                // command that cannot be copy-pasted safely.
                FixPlan {
                    label: format!(
                        "Cannot safely quote the adapter name \"{raw_name}\" for a PowerShell command; skipping the power-off fix"
                    ),
                    apply: vec![],
                    undo: vec![],
                    needs_admin: true,
                }
            } else {
                let n = ps_quote(&raw_name);
                FixPlan {
                    label: "Stop Windows powering the adapter down to save energy".into(),
                    apply: vec![format!(
                        "powershell -NoProfile -NonInteractive -Command \"Set-NetAdapterPowerManagement -Name {n} -AllowComputerToTurnOffDevice Disabled\""
                    )],
                    undo: vec![format!(
                        "powershell -NoProfile -NonInteractive -Command \"Set-NetAdapterPowerManagement -Name {n} -AllowComputerToTurnOffDevice Enabled\""
                    )],
                    needs_admin: true,
                }
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
                    format!(
                        "powercfg /setacvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} {}",
                        cur.ac
                    ),
                    format!(
                        "powercfg /setdcvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} {}",
                        cur.dc
                    ),
                    "powercfg /setactive SCHEME_CURRENT".to_string(),
                ],
                needs_admin: true,
            }
        }
    }
}

/// The result of running a plan's `apply` commands in order, stopping at the
/// first failure so a broken machine is never left running commands that
/// assume an earlier one succeeded.
#[derive(Debug, PartialEq, Eq)]
struct ApplyOutcome {
    /// How many commands ran successfully before either finishing or hitting
    /// a failure.
    ran: usize,
    total: usize,
    /// `command: error`, present only when a command failed.
    failed: Option<String>,
}

/// Runs `commands` in order through `run`, stopping at the first error. Pure
/// aside from calling `run`, so the stopping behaviour and the reported
/// counts are testable with a closure instead of a real process.
fn apply_plan(
    commands: &[String],
    run: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> ApplyOutcome {
    let total = commands.len();
    for (ran, c) in commands.iter().enumerate() {
        if let Err(e) = run(c) {
            return ApplyOutcome {
                ran,
                total,
                failed: Some(format!("{c}: {e}")),
            };
        }
    }
    ApplyOutcome {
        ran: total,
        total,
        failed: None,
    }
}

/// Offers each available fix in turn. Nothing is changed without a typed `y`,
/// and the undo commands are printed before the change is made, not after.
/// A plan that needs administrator rights is never attempted from an
/// unelevated shell; its commands are printed instead, exactly like a plan
/// this tool refused to build (an unquotable adapter name) is skipped with
/// its reason instead of being run.
#[cfg(windows)]
pub fn prompt_and_apply(findings: &[Finding], facts: &Facts) -> anyhow::Result<()> {
    use std::io::Write;
    let plans: Vec<FixPlan> = findings
        .iter()
        .filter_map(|f| f.fix)
        .map(|id| plan(id, facts))
        .collect();
    if plans.is_empty() {
        return Ok(());
    }
    if !facts.elevated && plans.iter().any(|p| p.needs_admin) {
        println!("\nThese changes need an administrator prompt, and this window is not elevated.");
        println!("Re-run `castr-sender diagnose --fix` from an administrator terminal, or run:");
    }
    for p in &plans {
        if p.apply.is_empty() {
            println!("\n{}", p.label);
            println!("  skipped: no commands to run");
            continue;
        }
        if p.needs_admin && !facts.elevated {
            println!("\n  {}", p.label);
            for c in &p.apply {
                println!("    {c}");
            }
            println!("  to undo:");
            for c in &p.undo {
                println!("    {c}");
            }
            continue;
        }
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
        let outcome = apply_plan(&p.apply, &mut |c| run_command_line(c));
        match &outcome.failed {
            Some(reason) => {
                println!("  failed: {reason}");
                println!(
                    "  partially applied ({} of {} commands ran); run the undo commands above to restore the previous settings",
                    outcome.ran, outcome.total
                );
            }
            None => println!("  applied; undo with the commands above"),
        }
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
        let script = extract_powershell_script(line).unwrap_or(rest);
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

/// The part of `run_command_line`'s parsing that is pure: pulling the quoted
/// PowerShell script back out of one of our own generated command lines.
/// Exercised directly so the round trip is proven on every platform, not just
/// Windows where `run_command_line` itself is compiled; also used there, so
/// it carries the same cfg plus `test` rather than being dead code on a
/// non-Windows build.
#[cfg(any(windows, test))]
fn extract_powershell_script(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("powershell ")?;
    rest.split_once("-Command ")
        .map(|(_, s)| s.trim().trim_matches('"'))
}

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
        assert_eq!(
            p.apply,
            vec![
                format!(
                    "powercfg /setacvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} 0"
                ),
                format!(
                    "powercfg /setdcvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} 0"
                ),
                "powercfg /setactive SCHEME_CURRENT".to_string(),
            ]
        );
        assert!(p.needs_admin);
    }

    #[test]
    fn the_power_saving_undo_restores_the_values_that_were_found() {
        let p = plan(FixId::WifiPowerSaving, &facts_with_power(1, 2));
        assert_eq!(
            p.undo,
            vec![
                format!(
                    "powercfg /setacvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} 1"
                ),
                format!(
                    "powercfg /setdcvalueindex SCHEME_CURRENT {SUB_WIFI} {SETTING_WIFI_POWER} 2"
                ),
                "powercfg /setactive SCHEME_CURRENT".to_string(),
            ]
        );
    }

    #[test]
    fn the_power_off_fix_names_the_real_adapter() {
        let p = plan(FixId::AdapterPowerOff, &facts_with_power(0, 0));
        assert!(p.apply[0].contains("-Name 'Wi-Fi'"));
        assert!(p.apply[0].contains("Disabled"));
        assert!(p.undo[0].contains("Enabled"));
    }

    #[test]
    fn the_power_off_fix_doubles_an_apostrophe_in_the_adapter_name() {
        let mut f = facts_with_power(0, 0);
        f.interface.as_mut().unwrap().name = "Bob's Wi-Fi".into();
        let p = plan(FixId::AdapterPowerOff, &f);
        assert!(p.apply[0].contains("-Name 'Bob''s Wi-Fi'"));
    }

    #[test]
    fn the_power_off_fix_refuses_a_name_that_cannot_be_quoted_safely() {
        let mut f = facts_with_power(0, 0);
        f.interface.as_mut().unwrap().name = "Wi\"Fi".into();
        let p = plan(FixId::AdapterPowerOff, &f);
        assert!(p.apply.is_empty(), "no command is offered: {:?}", p.apply);
        assert!(p.undo.is_empty());
        assert!(
            p.label.to_ascii_lowercase().contains("cannot"),
            "the label explains why: {}",
            p.label
        );
    }

    #[test]
    fn the_usb_fix_targets_the_usb_subgroup() {
        let p = plan(FixId::UsbSelectiveSuspend, &Facts::default());
        assert_eq!(
            p.apply,
            vec![
                format!(
                    "powercfg /setacvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} 0"
                ),
                format!(
                    "powercfg /setdcvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} 0"
                ),
                "powercfg /setactive SCHEME_CURRENT".to_string(),
            ]
        );
        assert_eq!(
            p.undo,
            vec![
                format!(
                    "powercfg /setacvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} 1"
                ),
                format!(
                    "powercfg /setdcvalueindex SCHEME_CURRENT {SUB_USB} {SETTING_USB_SUSPEND} 1"
                ),
                "powercfg /setactive SCHEME_CURRENT".to_string(),
            ]
        );
    }

    #[test]
    fn the_adapter_power_off_command_lines_round_trip_through_the_split() {
        let p = plan(FixId::AdapterPowerOff, &facts_with_power(0, 0));
        let apply_script = extract_powershell_script(&p.apply[0]).expect("apply parses");
        assert!(apply_script.contains("-Name 'Wi-Fi'"));
        assert!(apply_script.contains("Disabled"));
        assert!(
            !apply_script.contains('"'),
            "the quotes were stripped: {apply_script}"
        );
        let undo_script = extract_powershell_script(&p.undo[0]).expect("undo parses");
        assert!(undo_script.contains("Enabled"));

        // A name with an apostrophe still survives the split intact.
        let mut f = facts_with_power(0, 0);
        f.interface.as_mut().unwrap().name = "Bob's Wi-Fi".into();
        let p2 = plan(FixId::AdapterPowerOff, &f);
        let script2 = extract_powershell_script(&p2.apply[0]).expect("apply parses");
        assert!(script2.contains("-Name 'Bob''s Wi-Fi'"));
    }

    #[test]
    fn apply_plan_stops_after_the_first_failure_and_reports_progress() {
        let commands = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut calls: Vec<String> = Vec::new();
        let outcome = apply_plan(&commands, &mut |c| {
            calls.push(c.to_string());
            if c == "b" {
                anyhow::bail!("boom");
            }
            Ok(())
        });
        assert_eq!(outcome.ran, 1);
        assert_eq!(outcome.total, 3);
        let failed = outcome.failed.expect("a failure is reported");
        assert!(failed.contains('b'), "names the failing command: {failed}");
        assert!(failed.contains("boom"), "carries the error: {failed}");
        assert_eq!(
            calls,
            vec!["a".to_string(), "b".to_string()],
            "the third command is never attempted"
        );
    }

    #[test]
    fn apply_plan_reports_no_failure_when_every_command_succeeds() {
        let commands = vec!["a".to_string(), "b".to_string()];
        let outcome = apply_plan(&commands, &mut |_| Ok(()));
        assert_eq!(outcome.ran, 2);
        assert_eq!(outcome.total, 2);
        assert!(outcome.failed.is_none());
    }
}
