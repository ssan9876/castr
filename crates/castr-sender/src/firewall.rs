//! `castr-sender firewall`: the one inbound rule the Miracast source role
//! needs, added on demand rather than at install time.
//!
//! Why this exists at all. `miracast_cast::establish` listens on 7236 because
//! **real Miracast sinks are the TCP initiator** - measured against a wireless
//! display adapter, which listens on nothing and dials us. That listener is
//! unsolicited inbound traffic, so Windows Firewall blocks it unless something
//! has said otherwise. Nothing else the sender does needs a rule: RTP is
//! send-only, the QUIC path dials out, and the control socket is loopback.
//!
//! Why it is not only the installer's job. A firewall rule lives in the
//! machine-wide policy store - there is no per-user firewall - so no installer
//! can add one without elevation. The per-user MSI therefore installs without
//! a single prompt and leaves this to be run once, deliberately, from an
//! administrator terminal. It also fixes a gap the per-machine MSI has: its
//! rule names the installed path, so the portable exe run from anywhere else
//! was never covered by it. This one names whichever exe is actually running.
//!
//! The layering is `diagnose`'s: everything that decides what to run is pure
//! and tested on every platform; only `run` touches Windows.

// On a non-Windows build the only thing `run` does is bail, so the pure
// helpers around it look unused there. They are not: the tests below exercise
// every one of them on every platform, which is the point of keeping them pure.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

/// The rule's name. Deliberately the same string the MSI's `fw:FirewallException`
/// uses, so a machine that has both does not end up with two rules that look
/// unrelated in the firewall UI. The delete below is scoped by program path,
/// so removing one never silently removes the other.
pub const RULE_NAME: &str = "castr sender";

/// The port a Wi-Fi Display source listens on. Duplicated from
/// `miracast_cast` rather than shared, because the rule is program-scoped and
/// does not name a port; this is here for the explanation printed to a person.
pub const WFD_RTSP_PORT: u16 = 7236;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Say whether the rule is there, and print the command that would add it.
    Status,
    Allow,
    Remove,
}

/// The arguments for the `netsh` that adds the rule.
///
/// Scope, direction and program deliberately match the MSI's
/// `fw:FirewallException` exactly: someone who installed per-machine and
/// someone who ran this should end up with the same rule, not two rules that
/// differ in ways nobody documented. Program-scoped rather than port-scoped
/// for the same reason.
///
/// `Err` when the path cannot be rendered as a command line a person could
/// copy and paste - the same refusal `diagnose`'s adapter-name fix makes,
/// rather than printing something that would not survive a paste.
pub fn add_args(exe: &Path) -> Result<Vec<String>, String> {
    let program = checked_path(exe)?;
    Ok(vec![
        "advfirewall".into(),
        "firewall".into(),
        "add".into(),
        "rule".into(),
        format!("name={RULE_NAME}"),
        "dir=in".into(),
        "action=allow".into(),
        format!("program={program}"),
        "enable=yes".into(),
        "profile=any".into(),
        "remoteip=localsubnet".into(),
    ])
}

/// The arguments for the `netsh` that removes it again.
///
/// Scoped by `program=`, not by name alone. `netsh ... delete rule name="castr
/// sender"` would delete *every* rule with that name, including the one a
/// per-machine MSI installed for a different exe - so uninstalling the
/// portable copy would quietly disarm the installed one.
pub fn delete_args(exe: &Path) -> Result<Vec<String>, String> {
    let program = checked_path(exe)?;
    Ok(vec![
        "advfirewall".into(),
        "firewall".into(),
        "delete".into(),
        "rule".into(),
        format!("name={RULE_NAME}"),
        "dir=in".into(),
        format!("program={program}"),
    ])
}

/// The arguments that ask what rules of this name exist.
pub fn show_args() -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "show".into(),
        "rule".into(),
        format!("name={RULE_NAME}"),
        "verbose".into(),
    ]
}

fn checked_path(exe: &Path) -> Result<String, String> {
    let s = exe.to_string_lossy().to_string();
    // Windows paths cannot contain a double quote, so this is a guard against
    // the impossible rather than the likely - but the alternative is printing
    // a command line that breaks when pasted, which is what the same guard in
    // `diagnose::fix` exists to avoid.
    if s.contains('"') {
        return Err(format!("cannot quote the path {s:?} for a command line"));
    }
    Ok(s)
}

/// The same arguments rendered as one copy-pasteable line.
///
/// Kept separate from `add_args` on purpose: what is *run* goes to
/// `Command::args`, which needs no quoting and no shell, and what is *printed*
/// is quoted for a human's terminal. Rendering one and executing the other is
/// how a command that works when run turns into one that fails when pasted.
pub fn command_line(args: &[String]) -> String {
    let mut out = String::from("netsh");
    for a in args {
        out.push(' ');
        // Only the value after `=` can contain a space (a path, or the rule
        // name); quoting the whole `name=castr sender` token is what netsh
        // itself expects.
        if a.contains(' ') {
            match a.split_once('=') {
                Some((k, v)) => out.push_str(&format!("{k}=\"{v}\"")),
                None => out.push_str(&format!("\"{a}\"")),
            }
        } else {
            out.push_str(a);
        }
    }
    out
}

/// Whether `show` output describes a rule for this exe.
///
/// Matches on the program path rather than on any label. `netsh` prints its
/// headings in the machine's locale - the same trap `diagnose::facts` records
/// for the date it prints - so anything that parsed "Enabled:" or "Rule Name:"
/// would work here and fail on a German machine. A path is a path everywhere.
pub fn rule_covers(show_output: &str, exe: &Path) -> bool {
    let want = exe.to_string_lossy().to_lowercase();
    if want.is_empty() {
        return false;
    }
    show_output.to_lowercase().contains(&want)
}

/// What to tell someone about the rule they do not have. Pure so the wording
/// is fixed by a test rather than by whoever last edited the print statements.
pub fn why() -> String {
    format!(
        "A Miracast display connects back to this machine on port {WFD_RTSP_PORT}; without \
         the rule\nWindows drops that connection and `miracast-cast` to a real display \
         adapter times\nout. Casting to a castr receiver, and to castr's own sink, is \
         unaffected - both are\ndialled out from here."
    )
}

/// Runs one of the three actions. Returns the process exit code, on
/// `diagnose`'s convention: 0 when there is nothing wrong or nothing to do, 1
/// when the rule is missing or the change was not made. `firewall` on its own
/// is therefore usable as a check, not only as a readout.
#[cfg(windows)]
pub fn run(action: Action) -> anyhow::Result<i32> {
    use std::process::Command;

    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("cannot find this exe: {e}"))?;
    let show = Command::new("netsh").args(show_args()).output();
    // A missing rule makes netsh exit non-zero with "No rules match the
    // specified criteria" - not an error, an answer. Only a netsh that could
    // not be run at all is worth reporting as one.
    let present = match &show {
        Ok(out) => rule_covers(&String::from_utf8_lossy(&out.stdout), &exe),
        Err(e) => {
            anyhow::bail!("could not run netsh: {e}");
        }
    };

    println!("rule    : {RULE_NAME}");
    println!("program : {}", exe.display());
    println!("present : {}", if present { "yes" } else { "no" });

    let (args, verb) = match action {
        Action::Status => {
            if present {
                return Ok(0);
            }
            println!("\n{}\n", why());
            match add_args(&exe) {
                Ok(a) => {
                    println!(
                        "Add it with `castr-sender firewall --allow` from an administrator \
                         terminal, or run:"
                    );
                    println!("  {}", command_line(&a));
                }
                Err(e) => println!("{e}"),
            }
            return Ok(1);
        }
        Action::Allow if present => {
            println!("\nnothing to do");
            return Ok(0);
        }
        Action::Remove if !present => {
            println!("\nnothing to do");
            return Ok(0);
        }
        Action::Allow => (add_args(&exe), "add"),
        Action::Remove => (delete_args(&exe), "remove"),
    };
    let args = match args {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return Ok(1);
        }
    };

    // Never attempted from an unelevated shell; printed instead. The same
    // choice `diagnose --fix` makes, and for the same reason: a command that
    // fails halfway through a privileged change is worse than one that was
    // never started.
    if !crate::diagnose::collect::elevated() {
        println!("\nChanging the firewall needs an administrator terminal, and this one is not.");
        println!("Run this from one, or run:");
        println!("  {}", command_line(&args));
        return Ok(1);
    }

    let status = Command::new("netsh").args(&args).status()?;
    if !status.success() {
        anyhow::bail!("{} exited with {status}", command_line(&args));
    }
    println!("\n{verb}d");
    Ok(0)
}

/// Records in the log whether the inbound rule covers the running exe.
///
/// Called once at the start of a Miracast cast, because "was the firewall rule
/// there?" is the first question any failure to connect raises, and a log that
/// cannot answer it sends everyone looking in the wrong place. A netsh that
/// could not be run at all is itself worth writing down.
#[cfg(windows)]
pub fn note_state() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    match std::process::Command::new("netsh").args(show_args()).output() {
        Ok(out) => {
            if rule_covers(&String::from_utf8_lossy(&out.stdout), &exe) {
                tracing::info!("firewall: the inbound rule covers this exe");
            } else {
                tracing::warn!(
                    "firewall: NO inbound rule for this exe. A display that dials us on \
                     {WFD_RTSP_PORT} will be blocked; `castr-sender firewall --allow` adds it"
                );
            }
        }
        Err(e) => tracing::warn!("firewall: could not ask netsh whether the rule exists ({e})"),
    }
}

#[cfg(not(windows))]
pub fn note_state() {}

#[cfg(not(windows))]
pub fn run(_action: Action) -> anyhow::Result<i32> {
    anyhow::bail!("firewall is Windows only")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\castr\castr-sender.exe")
    }

    #[test]
    fn the_added_rule_matches_what_the_msi_installs() {
        let a = add_args(&exe()).unwrap();
        // Direction, action, scope and program - the four things that decide
        // whether the rule does anything. Held to the MSI's values so the two
        // ways of getting the rule cannot drift apart unnoticed.
        assert!(a.contains(&"dir=in".to_string()));
        assert!(a.contains(&"action=allow".to_string()));
        assert!(a.contains(&"remoteip=localsubnet".to_string()));
        assert!(a.contains(&"profile=any".to_string()));
        assert!(a.contains(&format!("program={}", exe().display())));
        assert!(a.contains(&format!("name={RULE_NAME}")));
    }

    #[test]
    fn the_delete_names_the_program_so_it_cannot_disarm_another_copy() {
        // Deleting by name alone would take the per-machine MSI's rule with
        // it. This is the whole reason the delete carries a program filter.
        let d = delete_args(&exe()).unwrap();
        assert!(d.contains(&format!("program={}", exe().display())));
        assert!(d.contains(&"dir=in".to_string()));
    }

    #[test]
    fn a_path_that_cannot_be_quoted_is_refused_rather_than_mangled() {
        let bad = PathBuf::from(r#"C:\od"d\castr-sender.exe"#);
        assert!(add_args(&bad).is_err());
        assert!(delete_args(&bad).is_err());
    }

    #[test]
    fn the_printed_command_quotes_the_values_that_contain_spaces() {
        let line = command_line(&add_args(&exe()).unwrap());
        assert!(line.starts_with("netsh advfirewall firewall add rule "));
        assert!(line.contains(r#"name="castr sender""#));
        assert!(line.contains(r#"program="C:\Program Files\castr\castr-sender.exe""#));
        // A value without a space is left alone rather than quoted for show.
        assert!(line.contains(" dir=in "));
    }

    #[test]
    fn a_rule_is_recognised_whatever_case_netsh_prints_the_path_in() {
        let out = "Rule Name: castr sender\n\
                   Program:   c:\\program files\\castr\\CASTR-SENDER.EXE\n";
        assert!(rule_covers(out, &exe()));
    }

    #[test]
    fn a_rule_for_a_different_copy_of_the_exe_does_not_count() {
        // The portable exe and the installed one are different programs to
        // the firewall, and reporting the rule as present for the wrong one
        // would send someone looking for a bug in the Miracast code instead.
        let out = "Rule Name: castr sender\n\
                   Program:   C:\\Users\\me\\Downloads\\castr-sender.exe\n";
        assert!(!rule_covers(out, &exe()));
    }

    #[test]
    fn no_rules_at_all_is_an_answer_not_a_match() {
        assert!(!rule_covers("No rules match the specified criteria.", &exe()));
    }

    #[test]
    fn the_explanation_names_the_port_and_what_still_works_without_it() {
        let w = why();
        assert!(w.contains("7236"));
        assert!(w.contains("castr receiver"));
    }
}
