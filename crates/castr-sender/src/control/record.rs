//! The record a running Miracast cast leaves so another process can find it.
//!
//! Pure: this decides the file's shape and what a client should conclude from
//! one. Reading and writing it is `server.rs` and `client.rs`.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// TOML, matching `paired.toml` beside it in the same directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Recorded because it is the first thing anyone wants when a cast will
    /// not die. It is deliberately *not* what decides staleness; see
    /// [`interpret`].
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub display: String,
    pub address: String,
    /// Unix seconds.
    pub started: u64,
}

impl Record {
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("a record has no unserialisable field")
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        toml::from_str(text).context("the Miracast cast record is not readable")
    }

}

/// How long ago a cast started, in words.
///
/// A stale record's `started` is a unix timestamp, and printing one at a
/// person is no help at all when what they want to know is whether the cast it
/// describes was a minute ago or last week.
pub fn describe_age(started: u64, now_unix: u64) -> String {
    let secs = now_unix.saturating_sub(started);
    match secs {
        0..=59 => "less than a minute ago".into(),
        60..=5399 => {
            let m = secs / 60;
            format!("{m} minute{} ago", if m == 1 { "" } else { "s" })
        }
        // Rounded to the nearest hour up to a day; past that, days read better
        // than a large hour count.
        5400..=86_399 => {
            let h = (secs + 1800) / 3600;
            format!("{h} hour{} ago", if h == 1 { "" } else { "s" })
        }
        _ => {
            let d = secs / 86_400;
            format!("{d} day{} ago", if d == 1 { "" } else { "s" })
        }
    }
}

/// Where the record lives, given the sender's configuration directory.
pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("miracast-cast.toml")
}

/// What a client should conclude from a record and whether the port in it
/// answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    NoCast,
    /// A record left behind by a cast that is gone. The caller removes it.
    Stale { started: u64 },
    Live,
}

/// Whether a record describes a cast that is actually running.
///
/// `reachable` is the outcome of connecting to the recorded port, and it is
/// the discriminator on purpose. The pid is not: pids are reused, so a live
/// pid is not evidence of a live cast — it is evidence of *a* process, which
/// may be anything the machine started since.
pub fn interpret(record: Option<&Record>, reachable: bool) -> Outcome {
    match record {
        None => Outcome::NoCast,
        Some(r) if !reachable => Outcome::Stale { started: r.started },
        Some(_) => Outcome::Live,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record() -> Record {
        Record {
            pid: 12345,
            port: 54321,
            token: "8f3a1c".into(),
            display: "DietPi".into(),
            address: "192.168.173.1:7236".into(),
            started: 1_757_001_234,
        }
    }

    #[test]
    fn a_record_survives_a_round_trip() {
        let r = a_record();
        assert_eq!(Record::parse(&r.to_toml()).unwrap(), r);
    }

    #[test]
    fn a_display_name_with_spaces_and_quotes_survives() {
        // The Samsung in range is literally named `75" Crystal UHD`.
        let r = Record {
            display: "75\" Crystal UHD".into(),
            ..a_record()
        };
        assert_eq!(Record::parse(&r.to_toml()).unwrap().display, r.display);
    }

    #[test]
    fn a_truncated_record_is_an_error_rather_than_a_default() {
        // A cast killed mid-write leaves a partial file. Reading it as a
        // record full of zeroes would have us connect to port 0.
        let text = "pid = 12345\nport = 5432";
        assert!(Record::parse(text).is_err());
    }

    #[test]
    fn a_record_missing_a_field_is_an_error() {
        let mut text = a_record().to_toml();
        text = text
            .lines()
            .filter(|l| !l.starts_with("token"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(Record::parse(&text).is_err());
    }

    #[test]
    fn nonsense_is_an_error() {
        assert!(Record::parse("this is not toml at all {{{").is_err());
    }

    #[test]
    fn no_record_means_no_cast() {
        assert_eq!(interpret(None, false), Outcome::NoCast);
        // Even if something happens to be listening, no record means we have
        // no idea what it is and must not talk to it.
        assert_eq!(interpret(None, true), Outcome::NoCast);
    }

    #[test]
    fn a_record_nothing_answers_is_stale() {
        let r = a_record();
        assert_eq!(
            interpret(Some(&r), false),
            Outcome::Stale {
                started: 1_757_001_234
            }
        );
    }

    #[test]
    fn a_record_whose_port_answers_is_live() {
        assert_eq!(interpret(Some(&a_record()), true), Outcome::Live);
    }

    #[test]
    fn an_age_is_described_in_words_not_a_timestamp() {
        let t = 1_757_001_234;
        assert_eq!(describe_age(t, t + 5), "less than a minute ago");
        assert_eq!(describe_age(t, t + 60), "1 minute ago");
        assert_eq!(describe_age(t, t + 1500), "25 minutes ago");
        assert_eq!(describe_age(t, t + 7200), "2 hours ago");
        assert_eq!(describe_age(t, t + 90_000), "1 day ago");
        assert_eq!(describe_age(t, t + 300_000), "3 days ago");
    }

    #[test]
    fn an_age_does_not_underflow_when_the_clock_moved_backwards() {
        // A record written before a clock correction would otherwise wrap to
        // an age of several hundred billion years.
        assert_eq!(describe_age(1_757_001_234, 0), "less than a minute ago");
    }
}
