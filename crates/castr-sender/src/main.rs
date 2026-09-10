mod cast;
mod control;
mod diagnose;
mod firewall;
mod gui;
mod miracast_cast;

use cast::*;
use castr_proto::Mode;
use clap::{Parser, Subcommand};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "castr-sender", about = "castr screen sender")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List receivers on the network
    List,
    /// Pair with a receiver (shows a PIN on the receiver)
    Pair { target: String },
    /// Check this machine's Wi-Fi for the known causes of Miracast drops
    Diagnose {
        /// Offer to apply the safe fixes, prompting for each
        #[arg(long)]
        fix: bool,
    },
    /// Show where this run's log went, and the runs before it
    Logs {
        /// Open the folder rather than printing paths
        #[arg(long)]
        open: bool,
    },
    /// Show, add or remove the firewall rule that lets a Miracast display
    /// connect back to this machine
    Firewall {
        /// Add the rule for this exe. Needs an administrator terminal; without
        /// one the command to run is printed instead.
        #[arg(long, conflicts_with = "remove")]
        allow: bool,
        /// Remove the rule for this exe again
        #[arg(long)]
        remove: bool,
    },
    /// List the Wi-Fi Direct devices in range, and which of them are displays
    MiracastList,
    /// Cast the screen to an ordinary Miracast display, by name or address
    MiracastCast {
        /// The display's name, or its RTSP address as host:port
        target: String,
        /// Stop automatically after this many seconds (mainly for testing)
        #[arg(long)]
        duration: Option<u64>,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        /// Which way to lean when the display offers several picture modes:
        /// quality takes the bigger picture, game the faster one
        #[arg(long, value_enum, default_value_t = ModeArg::Quality)]
        mode: ModeArg,
        /// How to pair the first time: auto takes whichever the display
        /// offers, preferring the button because it needs nobody
        #[arg(long, value_enum, default_value_t = PairArg::Auto)]
        pair: PairArg,
    },
    /// Report what the running Miracast cast is sending
    MiracastStatus,
    /// Stop the running Miracast cast
    MiracastStop,
    /// Cast the screen to a receiver
    Cast {
        target: String,
        #[arg(long, value_enum, default_value_t = ModeArg::Game)]
        mode: ModeArg,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long)]
        max_bitrate: Option<u32>,
        /// Stop automatically after this many seconds (mainly for testing)
        #[arg(long)]
        duration: Option<u64>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Game,
    Quality,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum PairArg {
    Auto,
    Push,
    Pin,
}

#[cfg(windows)]
impl From<PairArg> for castr_wifidirect_win::select::Preference {
    fn from(p: PairArg) -> Self {
        use castr_wifidirect_win::select::Preference;
        match p {
            PairArg::Auto => Preference::Auto,
            PairArg::Push => Preference::ForcePushButton,
            PairArg::Pin => Preference::ForcePin,
        }
    }
}
impl From<ModeArg> for Mode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::Game => Mode::Game,
            ModeArg::Quality => Mode::Quality,
        }
    }
}

/// What to say when there was nothing to talk to. Shared by `miracast-status`
/// and `miracast-stop`, which have the same three ways of finding nothing.
fn print_absent(report: control::client::Report) {
    use control::client::Report;
    match report {
        Report::NoCast => println!("no Miracast cast is running"),
        Report::Stale { started: Some(t) } => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            println!(
                "no Miracast cast is running; cleaned up a stale record from a cast \
                 started {}",
                control::record::describe_age(t, now)
            )
        }
        Report::Stale { started: None } => {
            println!("no Miracast cast is running; cleaned up an unreadable record")
        }
        Report::Answered(control::wire::Response::Err(why)) => {
            println!("the running cast refused: {why}")
        }
        Report::Answered(control::wire::Response::Ok(_)) => unreachable!("handled by the caller"),
    }
}

fn sender_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "castr sender".into())
}

/// The log's filename prefix, and what `logs` looks for.
const APP: &str = "castr-sender";

/// Opens a folder in the system file manager, for the person who has castr's
/// window in front of them and no terminal at all. Best effort: a machine with
/// no file manager is not a reason for anything here to fail.
pub fn open_dir(dir: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

/// `castr-sender logs`: where the logs are, and which is which.
///
/// Newest first, because the run being asked about is nearly always the last
/// one. Sizes are shown because the useful signal that a run did nothing at
/// all is a log that is only its own header.
fn show_logs(root: &std::path::Path, open: bool) -> anyhow::Result<()> {
    let dir = castr_net::logging::dir(root);
    println!("folder  {}", dir.display());
    if open {
        open_dir(&dir);
        return Ok(());
    }

    let prefix = format!("{APP}-");
    let mut logs: Vec<(String, u64)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().map(|m| m.len()).unwrap_or(0),
            )
        })
        .filter(|(n, _)| n.starts_with(&prefix) && n.ends_with(".log"))
        .collect();
    // The timestamp in the name is what orders these, not the filesystem's
    // idea of modification time, which a copy or a sync can rewrite.
    logs.sort_by(|a, b| b.0.cmp(&a.0));

    if logs.is_empty() {
        println!("\nno logs yet; they appear here the next time castr runs");
        return Ok(());
    }
    println!();
    for (name, size) in logs.iter().take(10) {
        println!("  {name}  {:.0} KB", *size as f64 / 1024.0);
    }
    if logs.len() > 10 {
        println!("  ... and {} older", logs.len() - 10);
    }
    println!(
        "\nThe newest is the run before this command. To report a problem, send\n{}",
        dir.join(&logs[0].0).display()
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    // Before anything else, so a failure while starting up is in the log
    // rather than only on a console the GUI path is about to detach from.
    let root = castr_net::config_dir();
    // `logs` is the exception: its whole job is to point at the other runs'
    // files, and a log of its own would become the newest one - so the file
    // someone was asked to hand over would be the one that just listed the
    // directory. Read from the arguments rather than the parsed command
    // because the subscriber has to be up before anything can fail.
    let log = if std::env::args().nth(1).as_deref() == Some("logs") {
        castr_net::logging::console_only();
        None
    } else {
        castr_net::logging::init(&root, APP, env!("CARGO_PKG_VERSION"), &[])
    };
    castr_net::logging::log_panics();

    let cli = Cli::parse();
    let config_dir = root.join("sender");
    let rt = tokio::runtime::Runtime::new()?;
    let result = match cli.cmd {
        None => {
            // The GUI path is what a double-clicked exe hits. The binary is a
            // console subsystem exe (so `list`/`pair`/`cast` keep a working
            // stdin/stdout when run from a shell); detach the console that
            // Explorer allocated for us so it does not sit behind the window.
            #[cfg(windows)]
            unsafe {
                let _ = windows::Win32::System::Console::FreeConsole();
            }
            gui::run_gui(config_dir, sender_name(), log.clone())
        }
        Some(Cmd::List) => rt.block_on(async {
            for r in discover(Duration::from_secs(2)).await? {
                println!("{:<24} {}  {}", r.name, r.addr, hex::encode(r.fingerprint));
            }
            Ok(())
        }),
        Some(Cmd::Pair { target }) => rt.block_on(async {
            let info = resolve_target(&target, Duration::from_secs(2)).await?;
            let name = info.name.clone();
            pair_interactive(&info, &config_dir, move || {
                println!("Enter the PIN shown on '{name}':");
                let mut pin = String::new();
                std::io::stdin().read_line(&mut pin)?;
                Ok(pin)
            })
            .await?;
            println!("paired with {}", info.name);
            Ok(())
        }),
        Some(Cmd::Diagnose { fix }) => {
            let code = diagnose::run(fix)?;
            std::process::exit(code);
        }
        Some(Cmd::Logs { open }) => show_logs(&root, open),
        Some(Cmd::Firewall { allow, remove }) => {
            let action = match (allow, remove) {
                (true, _) => firewall::Action::Allow,
                (_, true) => firewall::Action::Remove,
                _ => firewall::Action::Status,
            };
            let code = firewall::run(action)?;
            std::process::exit(code);
        }
        Some(Cmd::MiracastList) => {
            for c in castr_wifidirect_win::radio::discover()? {
                match c.caps {
                    Some(caps) if c.is_display() => println!(
                        "{:<32} display, RTSP {}, up to {} Mbps{}, pairs by {}",
                        c.name,
                        caps.rtsp_port,
                        caps.max_throughput_mbps,
                        if caps.content_protection { ", HDCP" } else { "" },
                        c.pairing
                            .map(|m| m.describe())
                            .unwrap_or_else(|| "an unstated method".into())
                    ),
                    _ => println!("{:<32} not a display", c.name),
                }
            }
            Ok(())
        }
        Some(Cmd::MiracastCast {
            target,
            duration,
            fps,
            mode,
            pair,
        }) => {
            // A name is the ordinary case; an address skips the radio entirely,
            // which is how this was tested before the radio existed and how a
            // display on the ordinary network is reached.
            let addr = target
                .parse::<std::net::SocketAddr>()
                .or_else(|_| format!("{target}:7236").parse::<std::net::SocketAddr>())
                .ok();
            // Which monitor to cast, the same control the other cast path uses.
            let output = std::env::var("CASTR_OUTPUT")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            // One cast at a time: there is one radio, one group and one
            // encoder on the monitor. A stale record is cleaned up here rather
            // than blocking the cast someone actually asked for.
            if let Some(running) = control::client::running(&config_dir) {
                anyhow::bail!(
                    "already casting to {:?} (since {}); stop it with `castr-sender miracast-stop`",
                    running.display,
                    running.address
                );
            }

            let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<control::server::Command>();
            {
                // The Miracast path had no Ctrl-C handler at all, so Ctrl-C
                // killed the process before TEARDOWN was written and before
                // the Wi-Fi Direct group was released - leaving the display
                // believing a session was live.
                let tx = cmd_tx.clone();
                rt.spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        eprintln!("stopping the cast; press Ctrl-C again to abort");
                        let _ = tx.send(control::server::Command::Stop);
                    }
                    if tokio::signal::ctrl_c().await.is_ok() {
                        std::process::exit(130);
                    }
                });
            }

            let mut opts = miracast_cast::MiracastOptions {
                duration: duration.map(Duration::from_secs),
                output,
                fps,
                mode: mode.into(),
                // Filled in below when the radio has read the display's own
                // advertisement; an address alone tells us nothing about it.
                ceiling_mbps: None,
                display: target.clone(),
                config_dir: config_dir.clone(),
            };
            match addr {
                Some(addr) => miracast_cast::cast_to(addr, opts, cmd_tx, cmd_rx),
                None => {
                    let wait = castr_wifidirect_win::select::WaitPolicy::new(
                        Duration::from_secs(60),
                    );
                    let name = target.clone();
                    // Called during the pairing, once the display has actually
                    // been told to show a PIN - not before it has one.
                    let ask: castr_wifidirect_win::radio::PinSource =
                        std::sync::Arc::new(move || {
                            println!("Enter the PIN shown on {name:?}:");
                            let mut pin = String::new();
                            let read = std::io::stdin().read_line(&mut pin)?;
                            if read == 0 {
                                anyhow::bail!(
                                    "no PIN was given (nothing on standard input); run this \
                                     from a terminal, or pipe the PIN in"
                                );
                            }
                            Ok(pin.trim().to_string())
                        });
                    let connection =
                        castr_wifidirect_win::radio::connect(&target, wait, &ask, pair.into())?;
                    let addr = std::net::SocketAddr::new(
                        connection.remote_ip(),
                        connection.rtsp_port(),
                    );
                    opts.ceiling_mbps = connection.max_throughput_mbps();
                    let result = miracast_cast::cast_to(addr, opts, cmd_tx, cmd_rx);
                    // The group goes when this does, which is the teardown.
                    drop(connection);
                    result
                }
            }
        }
        Some(Cmd::MiracastStatus) => {
            match control::client::talk(&config_dir, control::wire::Request::Status)? {
                control::client::Report::Answered(control::wire::Response::Ok(body)) => {
                    for (k, v) in control::stats::fields(&body) {
                        println!("{k:<16} {v}");
                    }
                }
                other => print_absent(other),
            }
            Ok(())
        }
        Some(Cmd::MiracastStop) => {
            match control::client::talk(&config_dir, control::wire::Request::Stop)? {
                control::client::Report::Answered(control::wire::Response::Ok(_)) => {
                    println!("stopping the cast");
                }
                other => print_absent(other),
            }
            Ok(())
        }
        Some(Cmd::Cast {
            target,
            mode,
            fps,
            max_bitrate,
            duration,
        }) => rt.block_on(async {
            let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(4);
            let (status_tx, mut status_rx) = tokio::sync::watch::channel(CastStatus::default());
            {
                let cmd_tx = cmd_tx.clone();
                tokio::spawn(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = cmd_tx.send(CastCommand::Stop).await;
                });
            }
            if let Some(secs) = duration {
                let cmd_tx = cmd_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                    let _ = cmd_tx.send(CastCommand::Stop).await;
                });
            }
            tokio::spawn(async move {
                while status_rx.changed().await.is_ok() {
                    let s = status_rx.borrow().clone();
                    tracing::info!(
                        "{} {}x{} {:.1} Mbps rtt {} ms loss {:.1}% {:.0} fps",
                        s.state,
                        s.width,
                        s.height,
                        s.bitrate_bps as f64 / 1e6,
                        s.rtt_ms,
                        s.loss_pct,
                        s.fps
                    );
                }
            });
            cast(
                CastOptions {
                    target,
                    mode: mode.into(),
                    fps,
                    max_bitrate,
                    sender_name: sender_name(),
                    config_dir,
                },
                cmd_rx,
                status_tx,
            )
            .await
        }),
    };
    // The last line of the log says how the run ended. Without this, a command
    // that failed left a log that simply stopped, which reads the same as one
    // that was killed - and the error text went only to a console that may not
    // exist. `{e:#}` for the whole anyhow chain: the context is usually the
    // half that says what was being attempted.
    match &result {
        Ok(()) => tracing::debug!("finished"),
        Err(e) => tracing::error!("failed: {e:#}"),
    }
    result
}
