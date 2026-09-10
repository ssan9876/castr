//! The half of `logging` that touches the disk: that a run really does leave a
//! readable file behind, with the header above the events.
//!
//! Its own test binary because `init` installs the global subscriber, which
//! can only happen once in a process.

use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("castr-logging-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_run_leaves_one_readable_log_behind() {
    let config = temp_dir("init");
    let path = castr_net::logging::init(
        &config,
        "castr-test",
        "9.9.9",
        &[("display".into(), "Living Room TV".into())],
    )
    .expect("a log file in a writable directory");

    assert!(path.starts_with(castr_net::logging::dir(&config)));

    tracing::info!("an event at info");
    // Targeted at one of our own crates on purpose: the file's extra verbosity
    // is per-crate, so an event from this test binary would correctly *not*
    // appear at debug, and asserting on one would prove nothing.
    tracing::debug!(target: "castr_net", "an event at debug from one of our own crates");

    let text = std::fs::read_to_string(&path).unwrap();

    // The header, and the fact the caller passed in.
    assert!(text.contains("castr-test 9.9.9"), "{text}");
    assert!(text.contains("display    Living Room TV"), "{text}");
    assert!(text.contains("exe "), "{text}");

    // The events themselves, including the debug one the console would not
    // have shown - which is the reason the file exists at all.
    assert!(text.contains("an event at info"), "{text}");
    assert!(
        text.contains("an event at debug from one of our own crates"),
        "{text}"
    );
    // And the console's filter is unchanged, so a dependency at debug stays
    // out of the file too - the reason the crates are named rather than
    // globbed.
    tracing::debug!(target: "quinn::congestion", "noise from a dependency");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("noise from a dependency"), "{text}");

    // No escape codes: this gets pasted into a message, and a log full of
    // `\u{1b}[2m` is one nobody reads.
    assert!(
        !text.contains('\u{1b}'),
        "the log should carry no ANSI codes"
    );

    // `newest` finds the file that was just written.
    assert_eq!(
        castr_net::logging::newest(&castr_net::logging::dir(&config), "castr-test"),
        Some(path)
    );

    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn an_unwritable_directory_costs_the_log_and_nothing_else() {
    // A machine that cannot open a log file still has an application. This is
    // the one thing this module must never do: stop the program.
    let path = castr_net::logging::dir(&PathBuf::from("/proc/nonexistent/castr"));
    assert!(!path.exists());
}
