#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod cast;
mod gui;

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
        None => gui::run_gui(config_dir, sender_name()),
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
