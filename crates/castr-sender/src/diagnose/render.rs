//! Text rendering of a report. Kept separate from judgment so the wording can
//! change without touching the rules, and so the GUI and the CLI show exactly
//! the same words.

use crate::diagnose::facts::Facts;
use crate::diagnose::rules::{Finding, Severity};

/// Renders `text` as one or more seven-space-indented lines, so a value that
/// happens to contain a newline (a probe's error text, for instance) never
/// spills unindented text into the report.
fn indented(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        out.push_str("       ");
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("       \n");
    }
    out
}

pub fn report(findings: &[Finding], facts: &Facts) -> String {
    let mut s = String::new();
    s.push_str("castr Wi-Fi health check\n\n");
    if let Some(d) = &facts.driver {
        s.push_str(&format!("Adapter: {}\n\n", d.name));
    }
    for f in findings {
        s.push_str(&format!("[{}] {}\n", f.severity.marker(), f.check));
        s.push_str(&indented(&f.found));
        s.push_str(&indented(f.why));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::facts::Facts;
    use crate::diagnose::rules::{Finding, FixId, Severity};

    fn f(check: &'static str, severity: Severity, fix: Option<FixId>) -> Finding {
        Finding {
            check,
            severity,
            found: "observed".into(),
            why: "because",
            fix,
        }
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
        assert!(
            out.contains("--fix"),
            "tells the user how to apply fixes: {out}"
        );
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
