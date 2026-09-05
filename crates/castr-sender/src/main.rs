mod cast;
mod control;
mod diagnose;
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
        Report::Stale { started: Some(t) } => println!(
            "no Miracast cast is running; cleaned up a stale record from a cast \
             started {t} (unix seconds)"
        ),
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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let cli = Cli::parse();
    let config_dir = castr_net::config_dir().join("sender");
    let rt = tokio::runtime::Runtime::new()?;
    match cli.cmd {
        None => {
            // The GUI path is what a double-clicked exe hits. The binary is a
            // console subsystem exe (so `list`/`pair`/`cast` keep a working
            // stdin/stdout when run from a shell); detach the console that
            // Explorer allocated for us so it does not sit behind the window.
            #[cfg(windows)]
            unsafe {
                let _ = windows::Win32::System::Console::FreeConsole();
            }
            gui::run_gui(config_dir, sender_name())
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
        Some(Cmd::MiracastList) => {
            for c in castr_wifidirect_win::radio::discover()? {
                match c.caps {
                    Some(caps) if c.is_display() => println!(
                        "{:<32} display, RTSP {}, up to {} Mbps{}",
                        c.name,
                        caps.rtsp_port,
                        caps.max_throughput_mbps,
                        if caps.content_protection { ", HDCP" } else { "" }
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
                    let connection = castr_wifidirect_win::radio::connect(&target, wait, &mut || {
                        println!("Enter the PIN shown on {name:?}:");
                        let mut pin = String::new();
                        std::io::stdin().read_line(&mut pin)?;
                        Ok(pin.trim().to_string())
                    })?;
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
    }
}
