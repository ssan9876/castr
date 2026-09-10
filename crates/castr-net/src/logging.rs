//! One log file per run, so a test on real hardware can be handed over whole.
//!
//! Both binaries already logged through `tracing`, but only to the console -
//! and the sender's GUI path calls `FreeConsole()`, so the ordinary way of
//! using castr (double-click the exe, cast, watch it fail) produced no
//! diagnostic record at all. Whatever went wrong was gone the moment the
//! window closed.
//!
//! So: the console keeps exactly the output it had, and a file gets the same
//! events plus everything castr's own crates log at debug. One file per run
//! rather than one rolling file, because the question is always "what happened
//! that time", and a run is the unit a person can point at.
//!
//! What a log contains, so that handing one over is an informed act: the
//! command line, this machine's OS and architecture, the exe's path, and then
//! the run itself - display names, Wi-Fi Direct device names, and the IP
//! addresses of the peer-to-peer link. It does not contain pairing PINs, the
//! identity key, or anything from `paired.toml`; none of those are logged
//! anywhere, at any level.
//!
//! Everything that decides *what* to write is pure and tested on every
//! platform. Only `init`, `newest` and `prune` touch the disk.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many runs to keep. Enough to cover a testing session and still find the
/// one that mattered; the alternative is a directory that grows for ever on a
/// Pi's SD card. Note that short runs count too - twenty `--help`s will age out
/// a cast worth reading, so a log worth keeping is worth copying out.
pub const KEEP: usize = 30;

/// Days since the Unix epoch as a civil date, by Howard Hinnant's
/// `civil_from_days`. The sender's `diagnose::facts` carries the year-only
/// cousin of this for reading driver dates.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

/// Year, month, day, hour, minute, second in UTC.
pub fn parts(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// The header's timestamp: readable, unambiguous, and explicitly UTC so a log
/// from another timezone cannot be misread as local.
pub fn timestamp(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = parts(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
}

/// The filename's timestamp. Sorts chronologically as text, which is what
/// `prunable` and `newest` rely on instead of asking the filesystem for times.
pub fn file_stamp(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = parts(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// `castr-sender-20260910-140311-04216.log`.
///
/// The pid is in the name because two runs can start inside the same second -
/// a script casting in a loop does exactly that - and a log that overwrote the
/// previous run's would lose the very thing being looked for.
pub fn log_name(app: &str, secs: u64, pid: u32) -> String {
    format!("{app}-{}-{pid:05}.log", file_stamp(secs))
}

/// Which of `names` to delete, keeping the newest `keep`.
///
/// Sorted as text, which the timestamp in the name makes chronological. Names
/// that are not ours are never returned: a directory someone put something
/// else in is not this function's to tidy.
pub fn prunable(app: &str, names: &[String], keep: usize) -> Vec<String> {
    let prefix = format!("{app}-");
    let mut ours: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with(&prefix) && n.ends_with(".log"))
        .collect();
    ours.sort();
    let excess = ours.len().saturating_sub(keep);
    ours.into_iter().take(excess).cloned().collect()
}

/// The header written above the events, as lines of `key  value`.
///
/// Pure, and returned rather than printed, so a test can hold the shape of it
/// without a file. `facts` is whatever only the caller knows - the sender's
/// Windows facts have no business being gathered in here.
pub fn header(
    app: &str,
    version: &str,
    secs: u64,
    args: &[String],
    facts: &[(String, String)],
) -> String {
    let mut out = String::new();
    let mut line = |k: &str, v: &str| {
        out.push_str(&format!("{k:<10} {v}\n"));
    };
    line("app", &format!("{app} {version}"));
    line("started", &timestamp(secs));
    // The command line as given. A display name with spaces in it stays one
    // argument here rather than being re-split into something that was never
    // typed, so what is read back is what was run.
    line(
        "command",
        &args
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    );
    line(
        "os",
        &format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    );
    for (k, v) in facts {
        line(k, v);
    }
    out.push_str(&"-".repeat(72));
    out.push('\n');
    out
}

/// Where logs live: one directory for both binaries, since a machine only
/// ever runs one of them and a single folder is easier to describe to someone
/// who has to find it.
pub fn dir(config_dir: &Path) -> PathBuf {
    config_dir.join("logs")
}

/// The most recent log in `dir`, by the timestamp in its name.
pub fn newest(dir: &Path, app: &str) -> Option<PathBuf> {
    let prefix = format!("{app}-");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix) && n.ends_with(".log"))
        .collect();
    names.sort();
    names.pop().map(|n| dir.join(n))
}

/// The file every layer shares. Cloned per writer rather than reopened, so
/// interleaved lines from several threads land in one file in one order.
#[derive(Clone)]
struct FileWriter(Arc<Mutex<File>>);

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.lock() {
            Ok(mut f) => f.write(buf),
            // A poisoned lock means another thread panicked mid-write. Drop
            // the line: panicking inside the logger would turn a diagnostic
            // into a second, more confusing crash.
            Err(_) => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self.0.lock() {
            Ok(mut f) => f.flush(),
            Err(_) => Ok(()),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FileWriter {
    type Writer = FileWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// castr's own crates, which the file records at debug while everything else
/// stays at info. Named rather than globbed because the noisy debug output is
/// all in the dependencies - quinn's congestion control, winit's event loop -
/// and a log nobody can read is not a log.
const OURS: [&str; 10] = [
    "castr_sender",
    "castr_receiver",
    "castr_net",
    "castr_proto",
    "castr_media",
    "castr_miracast",
    "castr_capture_win",
    "castr_codec_win",
    "castr_codec_v4l2",
    "castr_wifidirect_win",
];

/// The console's filter: exactly what both binaries had before this module -
/// info, with `RUST_LOG` still steering it.
fn console_filter() -> tracing_subscriber::EnvFilter {
    let mut f = tracing_subscriber::EnvFilter::from_default_env();
    if let Ok(d) = "info".parse() {
        f = f.add_directive(d);
    }
    f
}

/// The file's filter: the console's, plus castr's own crates at debug.
///
/// `RUST_LOG` is read first and these are added after it, so setting it turns
/// the file up rather than replacing what it would have held.
fn file_filter() -> tracing_subscriber::EnvFilter {
    let mut f = console_filter();
    for name in OURS {
        if let Ok(d) = format!("{name}=debug").parse() {
            f = f.add_directive(d);
        }
    }
    f
}

/// Installs the console and file layers and writes the header.
///
/// Returns the log's path, or `None` when there is nowhere to write one. A
/// machine that cannot open a log file still has an application: logging is
/// never the reason castr fails to start, so every error here degrades to the
/// console-only behaviour this replaced, with one line saying so.
pub fn init(
    config_dir: &Path,
    app: &'static str,
    version: &str,
    facts: &[(String, String)],
) -> Option<PathBuf> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let args: Vec<String> = std::env::args().collect();

    let dir = dir(config_dir);
    let opened = std::fs::create_dir_all(&dir).and_then(|_| {
        let path = dir.join(log_name(app, secs, std::process::id()));
        let file = File::create(&path)?;
        Ok((path, file))
    });

    let (path, mut file) = match opened {
        Ok(v) => v,
        Err(e) => {
            console_only();
            tracing::warn!("no log file: {} could not be written ({e})", dir.display());
            return None;
        }
    };

    let mut facts = facts.to_vec();
    facts.push((
        "exe".into(),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "unknown".into()),
    ));
    let _ = file.write_all(header(app, version, secs, &args, &facts).as_bytes());

    let writer = FileWriter(Arc::new(Mutex::new(file)));
    // No ANSI: the file is read in a text editor or pasted into a message, and
    // escape codes there are noise that hides the words they wrap.
    let to_file = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(writer)
        .with_filter(file_filter());

    let console = tracing_subscriber::fmt::layer().with_filter(console_filter());
    tracing_subscriber::registry()
        .with(console)
        .with(to_file)
        .init();
    tracing::debug!("logging to {}", path.display());
    prune(&dir, app);
    Some(path)
}

/// Installs the console layer alone: exactly the logging castr had before
/// there were files.
///
/// For the commands whose job is to talk *about* the logs. `logs` writing a
/// log would make the newest file its own - so the run someone was asked to
/// hand over would be the one that just listed the directory, and empty.
pub fn console_only() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;
    let console = tracing_subscriber::fmt::layer().with_filter(console_filter());
    let _ = tracing_subscriber::registry().with(console).try_init();
}

/// Deletes all but the newest `KEEP` logs. Failures are ignored: a log that
/// could not be removed is clutter, not a reason to stop.
fn prune(dir: &Path, app: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for name in prunable(app, &names, KEEP) {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Records panics in the log before they reach the console.
///
/// The GUI path detaches its console, so a panic there was previously silent:
/// the window vanished and nothing was written down. The panic's location
/// survives `strip = true` in the release profile because it is a static
/// string the compiler puts there, which is why this is worth having even
/// though a stripped backtrace names no functions - and why the backtrace
/// itself is only captured when `RUST_BACKTRACE` asks for one.
pub fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let trace = std::backtrace::Backtrace::capture();
        if matches!(trace.status(), std::backtrace::BacktraceStatus::Captured) {
            tracing::error!("panic: {info}\n{trace}");
        } else {
            tracing::error!("panic: {info}");
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_and_the_awkward_dates_round_trip() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00Z");
        // A leap day, and the day after a leap day in a century year that is
        // a leap year - the two dates a hand-rolled calendar gets wrong.
        assert_eq!(timestamp(951_782_400), "2000-02-29 00:00:00Z");
        assert_eq!(timestamp(1_583_020_800), "2020-03-01 00:00:00Z");
        assert_eq!(timestamp(1_757_512_991), "2025-09-10 14:03:11Z");
        assert_eq!(timestamp(4_102_444_799), "2099-12-31 23:59:59Z");
    }

    #[test]
    fn a_log_name_sorts_chronologically_as_text() {
        // What `prunable` and `newest` both depend on, so it is held here
        // rather than assumed at each use.
        let early = log_name("castr-sender", 1_757_512_991, 4);
        let late = log_name("castr-sender", 1_757_512_992, 4);
        assert!(early < late, "{early} should sort before {late}");
    }

    #[test]
    fn two_runs_in_the_same_second_do_not_share_a_name() {
        assert_ne!(
            log_name("castr-sender", 1_757_512_991, 4216),
            log_name("castr-sender", 1_757_512_991, 4217)
        );
    }

    #[test]
    fn pruning_keeps_the_newest_and_names_the_rest() {
        let names: Vec<String> = (0..5)
            .map(|i| log_name("castr-sender", 1_757_512_991 + i, 1))
            .collect();
        let gone = prunable("castr-sender", &names, 2);
        assert_eq!(gone, names[..3].to_vec());
    }

    #[test]
    fn nothing_is_pruned_while_there_is_room() {
        let names: Vec<String> = (0..3)
            .map(|i| log_name("castr-sender", 1_757_512_991 + i, 1))
            .collect();
        assert!(prunable("castr-sender", &names, KEEP).is_empty());
    }

    #[test]
    fn only_our_own_logs_are_ever_pruned() {
        // A directory someone else put a file in is not ours to tidy, and the
        // other binary's logs are not ours either.
        let names = vec![
            "notes.txt".to_string(),
            "castr-receiver-20260910-140311-00001.log".to_string(),
            log_name("castr-sender", 1_757_512_991, 1),
            log_name("castr-sender", 1_757_512_992, 1),
        ];
        assert_eq!(
            prunable("castr-sender", &names, 1),
            vec![log_name("castr-sender", 1_757_512_991, 1)]
        );
    }

    #[test]
    fn the_header_says_what_was_run_and_when() {
        let args = vec![
            "castr-sender".to_string(),
            "miracast-cast".to_string(),
            "Living Room TV".to_string(),
        ];
        let h = header("castr-sender", "0.1.0", 1_757_512_991, &args, &[]);
        assert!(h.contains("castr-sender 0.1.0"));
        assert!(h.contains("2025-09-10 14:03:11Z"));
        // The display name stays one argument, quoted, rather than reading as
        // three separate ones.
        assert!(h.contains("miracast-cast \"Living Room TV\""), "{h}");
    }

    #[test]
    fn the_header_carries_what_the_caller_knows() {
        let h = header(
            "castr-sender",
            "0.1.0",
            0,
            &[],
            &[("firewall".into(), "rule present".into())],
        );
        assert!(h.contains("firewall   rule present"), "{h}");
    }
}
