mod audio_out;
mod display;
mod pipeline;
mod render;

use clap::Parser;
use pipeline::{DecoderChoice, MiracastChoice, ReceiverConfig};

#[derive(Parser)]
#[command(name = "castr-receiver", about = "castr screen receiver")]
struct Cli {
    /// Display name advertised on the network
    #[arg(long, default_value_t = default_name())]
    name: String,
    #[arg(long)]
    fullscreen: bool,
    /// Max video bitrate in bits per second
    #[arg(long, default_value_t = default_bitrate())]
    max_bitrate: u32,
    #[arg(long, value_enum, default_value_t = DecoderChoice::Auto)]
    decoder: DecoderChoice,
    /// UDP address to bind for QUIC
    #[arg(long, default_value = "0.0.0.0:7332")]
    bind: std::net::SocketAddr,
    /// Accept Miracast sources as well (Linux only)
    #[arg(long, value_enum, default_value_t = MiracastChoice::Auto)]
    miracast: MiracastChoice,
    /// Name shown in the Windows cast list (defaults to the hostname)
    #[arg(long)]
    miracast_name: Option<String>,
    /// 2.4 GHz channel for the Wi-Fi Direct group
    #[arg(long, value_parser = ["1", "6", "11", "auto"], default_value = "auto")]
    miracast_channel: String,
}

fn default_name() -> String {
    if let Ok(n) = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")) {
        if !n.trim().is_empty() {
            return n;
        }
    }
    // systemd services have no HOSTNAME in their environment.
    if let Ok(n) = std::fs::read_to_string("/etc/hostname") {
        let n = n.trim();
        if !n.is_empty() {
            return n.to_string();
        }
    }
    "castr receiver".into()
}

fn default_bitrate() -> u32 {
    if cfg!(all(
        target_os = "linux",
        any(target_arch = "arm", target_arch = "aarch64")
    )) {
        10_000_000
    } else {
        40_000_000
    }
}

fn main() -> anyhow::Result<()> {
    // A file as well as the console. journald already keeps the Pi's output
    // when it runs as a service, but a receiver started by hand over SSH left
    // nothing behind once the terminal closed - and that is exactly how it is
    // run while something is being debugged.
    let root = castr_net::config_dir();
    castr_net::logging::init(&root, "castr-receiver", env!("CARGO_PKG_VERSION"), &[]);
    castr_net::logging::log_panics();
    let cli = Cli::parse();
    pipeline::run(ReceiverConfig {
        name: cli.name,
        fullscreen: cli.fullscreen,
        max_bitrate: cli.max_bitrate,
        decoder: cli.decoder,
        bind: cli.bind,
        miracast: cli.miracast,
        miracast_name: cli.miracast_name,
        // "auto" means we pick the least busy channel at group creation.
        miracast_channel: cli.miracast_channel.parse().ok(),
        config_dir: castr_net::config_dir().join("receiver"),
    })
}
