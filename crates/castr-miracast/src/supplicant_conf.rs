//! The working `wpa_supplicant` configuration, which has to be writable.
//!
//! `P2P_GROUP_ADD persistent` builds a persistent group, but the supplicant can
//! only keep it across restarts if it may write its configuration file back:
//! with `update_config=0` the group lives in memory and dies with the process,
//! so every start invents a fresh SSID and passphrase. A source that paired
//! once then holds credentials for a group that no longer exists, and sits on
//! "connecting" forever with nothing arriving at the sink to explain why.
//!
//! The deployed file is root-owned and replaced on every deploy, so it stays
//! the template. This module derives a working copy the sink's user can write,
//! and re-derives it when the template changes without losing the stored
//! groups - which are the whole point of the exercise.
//!
//! Pure: string in, string out. The file handling is the caller's.

/// Turns the deployed template into the working configuration: the same
/// settings, but writable, so the supplicant can save a persistent group.
pub fn from_template(template: &str) -> String {
    let mut out = String::with_capacity(template.len() + 64);
    let mut seen = false;
    for line in template.lines() {
        if line.trim_start().starts_with("update_config=") {
            seen = true;
            out.push_str("update_config=1\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !seen {
        out.push_str("update_config=1\n");
    }
    out
}

/// The `network={...}` blocks of a configuration, verbatim, in order.
///
/// These hold the persistent group's SSID and passphrase. The supplicant
/// writes them itself, so they must survive a template change untouched.
pub fn network_blocks(conf: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in conf.lines() {
        match &mut current {
            None => {
                if line.trim_start().starts_with("network=") {
                    current = Some(format!("{line}\n"));
                }
            }
            Some(buf) => {
                buf.push_str(line);
                buf.push('\n');
                if line.trim() == "}" {
                    blocks.push(current.take().expect("in a block"));
                }
            }
        }
    }
    blocks
}

/// The configuration to write when the template has changed: the new settings,
/// carrying over the groups the old working copy had learned.
pub fn reseed(template: &str, existing: &str) -> String {
    let mut out = from_template(template);
    for block in network_blocks(existing) {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = "\
ctrl_interface=DIR=/run/wpa_supplicant_castr GROUP=castr
update_config=0
device_name=castr
p2p_go_intent=15
";

    const WITH_GROUP: &str = "\
ctrl_interface=DIR=/run/wpa_supplicant_castr GROUP=castr
update_config=1
device_name=castr
p2p_go_intent=15
network={
\tssid=\"DIRECT-xyDietPi\"
\tpsk=\"secretpassphrase\"
\tmode=3
\tdisabled=2
}
";

    #[test]
    fn the_working_copy_lets_the_supplicant_save() {
        // Without this the persistent group is not persistent at all, and a
        // source that paired once can never get back in.
        let out = from_template(TEMPLATE);
        assert!(out.contains("update_config=1"));
        assert!(!out.contains("update_config=0"));
    }

    #[test]
    fn every_other_setting_survives_unchanged() {
        let out = from_template(TEMPLATE);
        for line in [
            "ctrl_interface=DIR=/run/wpa_supplicant_castr GROUP=castr",
            "device_name=castr",
            "p2p_go_intent=15",
        ] {
            assert!(out.contains(line), "lost {line:?}");
        }
    }

    #[test]
    fn a_template_with_no_update_config_still_gets_one() {
        let out = from_template("device_name=castr\n");
        assert!(out.contains("update_config=1"));
        assert!(out.contains("device_name=castr"));
    }

    #[test]
    fn the_stored_group_is_read_back_verbatim() {
        let blocks = network_blocks(WITH_GROUP);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("DIRECT-xyDietPi"));
        assert!(blocks[0].contains("secretpassphrase"));
        assert!(blocks[0].trim_end().ends_with('}'));
    }

    #[test]
    fn a_configuration_with_no_group_yields_none() {
        assert!(network_blocks(TEMPLATE).is_empty());
    }

    #[test]
    fn reseeding_keeps_the_groups_and_takes_the_new_settings() {
        // A deploy that changes a radio setting must not cost every paired PC
        // its credentials.
        let newer = TEMPLATE.replace("p2p_go_intent=15", "p2p_go_intent=15\np2p_go_ht40=1");
        let out = reseed(&newer, WITH_GROUP);
        assert!(out.contains("p2p_go_ht40=1"), "new setting missing");
        assert!(out.contains("update_config=1"));
        assert!(out.contains("DIRECT-xyDietPi"), "stored group lost");
        assert!(out.contains("secretpassphrase"));
        assert_eq!(network_blocks(&out).len(), 1);
    }

    #[test]
    fn reseeding_carries_over_several_groups() {
        let two = format!("{WITH_GROUP}network={{\n\tssid=\"DIRECT-ab\"\n}}\n");
        let out = reseed(TEMPLATE, &two);
        assert_eq!(network_blocks(&out).len(), 2);
    }

    #[test]
    fn the_working_copy_is_stable_when_derived_twice() {
        // Re-deriving from the same template must not drift, or the sink would
        // rewrite the file - and restart pairing - on every start.
        assert_eq!(from_template(TEMPLATE), from_template(&from_template(TEMPLATE)));
    }
}
