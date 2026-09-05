//! Casting to an ordinary Miracast display over IP.
//!
//! The impure half of the source role: this owns the RTSP connection, the RTP
//! socket, the encoder, the capture and the clock. Everything it decides is
//! decided in `castr_miracast::source`, which is pure and tested; what happens
//! here is sockets and threads.
//!
//! No radio. The address is given, which over Ethernet is Miracast over
//! Infrastructure and over a Wi-Fi Direct group is ordinary Miracast - the
//! media path does not care which.

use crate::control::server::{Command, ControlServer, Published};
use crate::control::stats::{Context as StatsContext, Stats};
use anyhow::Context;
use castr_media::codec::{EncoderConfig, Mode, PixelFormat, RawFrame, VideoEncoder};
use castr_miracast::rtsp::{self, Action};
use castr_miracast::source::{rtp_pack::Packetizer, session::*, ts_mux::Muxer};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to wait for the display to answer at all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the session's timers are serviced when nothing is arriving.
const TICK: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct MiracastOptions {
    pub duration: Option<Duration>,
    /// Which monitor to cast; the same meaning as `CASTR_OUTPUT` elsewhere.
    pub output: u32,
    pub fps: u32,
    /// Bigger picture or faster one, when the display offers both.
    pub mode: Mode,
    /// What the display's advertisement said it can carry, when the radio read
    /// one. Nothing here invents a ceiling of its own.
    pub ceiling_mbps: Option<u16>,
    /// What to call this display in the status readout: its name when we found
    /// it by one, otherwise its address.
    pub display: String,
    /// Where the control record goes, so another process can find this cast.
    pub config_dir: PathBuf,
}

impl Default for MiracastOptions {
    fn default() -> Self {
        Self {
            duration: None,
            output: 0,
            fps: 30,
            mode: Mode::Quality,
            ceiling_mbps: None,
            display: String::new(),
            config_dir: PathBuf::from("."),
        }
    }
}

enum Media {
    Video {
        data: Vec<u8>,
        pts_us: u64,
        /// The desktop did not change, so the last frame was sent again. A
        /// still screen is the normal case and looks identical to a stalled
        /// capture unless this is counted.
        repeated: bool,
    },
    Audio {
        samples: Vec<i16>,
        pts_us: u64,
    },
}

/// Casts this desktop to the Miracast display at `addr` until it ends.
///
/// `cmds` carries `Stop` from `miracast-stop` and from Ctrl-C; `cmd_tx` is the
/// same channel's sending half, handed to the control listener. Both are made
/// by the caller so the Ctrl-C handler can exist before the cast does.
pub fn cast_to(
    addr: SocketAddr,
    opts: MiracastOptions,
    cmd_tx: mpsc::Sender<Command>,
    cmds: mpsc::Receiver<Command>,
) -> anyhow::Result<()> {
    let mut sock = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .with_context(|| format!("connect: {addr} did not answer within {CONNECT_TIMEOUT:?}"))?;
    sock.set_read_timeout(Some(TICK))?;
    sock.set_nodelay(true).ok();
    tracing::info!("miracast: connected to {addr}");

    // Bind before negotiating: the port we will send from is part of what M4
    // tells the display.
    let rtp_sock = UdpSocket::bind("0.0.0.0:0").context("binding an RTP socket")?;
    let mut session = SourceSession::new(SourceConfig {
        rtp_port: rtp_sock.local_addr()?.port(),
        mode: opts.mode,
        ceiling_mbps: opts.ceiling_mbps,
        ..SourceConfig::default()
    });

    let stop = Arc::new(AtomicBool::new(false));
    let (media_tx, media_rx) = mpsc::channel::<Media>();
    let mut media_started = false;
    let mut muxer = Muxer::new();
    let mut packetizer = Packetizer::new(rand_ssrc());
    let mut rtp_target: Option<SocketAddr> = None;
    let started = Instant::now();

    // The control channel is an accessory: if it cannot bind, the cast runs
    // anyway, `miracast-stop` stops working, Ctrl-C still does, and the log
    // says why.
    let published: Published = Arc::new(Mutex::new(None));
    let display = if opts.display.is_empty() {
        addr.to_string()
    } else {
        opts.display.clone()
    };
    let control = match ControlServer::start(
        &opts.config_dir,
        &display,
        &addr.to_string(),
        cmd_tx,
        published.clone(),
    ) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("miracast: no control channel, so miracast-stop will not work: {e:#}");
            None
        }
    };
    let mut stats = Stats::new();
    let mut stats_ctx = StatsContext {
        display,
        address: addr.to_string(),
        ceiling_mbps: opts.ceiling_mbps,
        ..StatsContext::default()
    };

    write_actions(&mut sock, session.start())?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let reason = loop {
        if let Some(limit) = opts.duration {
            if started.elapsed() >= limit {
                break "the requested duration elapsed";
            }
        }

        // `miracast-stop`, or Ctrl-C. Both leave by the same door `--duration`
        // does, so teardown is the code that already works.
        let mut stopped = false;
        while let Ok(Command::Stop) = cmds.try_recv() {
            stopped = true;
        }
        if stopped {
            break "stopped by request";
        }

        // Anything the display has to say.
        match sock.read(&mut chunk) {
            Ok(0) => break "session: the display closed the connection",
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e).context("session: reading the control connection"),
        }

        let mut ended = None;
        while let Some((msg, used)) = rtsp::parse(&buf).context("session: unreadable RTSP")? {
            buf.drain(..used);
            for action in session.on_message(&msg) {
                match action {
                    Action::Send(m) => sock.write_all(m.format().as_bytes())?,
                    Action::Play => {
                        let port = session.sink_rtp_port().unwrap_or(5000);
                        rtp_target = Some(SocketAddr::new(addr.ip(), port));
                        if !media_started {
                            media_started = true;
                            start_media(&session, &opts, media_tx.clone(), stop.clone());
                            tracing::info!(
                                "miracast: playing {:?} to {}",
                                session.chosen(),
                                rtp_target.expect("just set")
                            );
                        }
                    }
                    Action::Teardown(why) => ended = Some(why),
                }
            }
            if ended.is_some() {
                break;
            }
        }
        if let Some(why) = ended {
            break why;
        }

        for action in session.tick(Instant::now()) {
            match action {
                Action::Send(m) => sock.write_all(m.format().as_bytes())?,
                Action::Play => {}
                Action::Teardown(why) => ended = Some(why),
            }
        }
        if let Some(why) = ended {
            break why;
        }

        // Whatever the encoder and the audio capture have produced.
        if let Some(target) = rtp_target {
            while let Ok(unit) = media_rx.try_recv() {
                let (packets, pts_us) = match unit {
                    Media::Video {
                        data,
                        pts_us,
                        repeated,
                    } => {
                        stats.video(repeated);
                        (muxer.push_video(&data, pts_us), pts_us)
                    }
                    Media::Audio { samples, pts_us } => {
                        stats.audio();
                        (muxer.push_audio(&samples, pts_us), pts_us)
                    }
                };
                let stamp = (pts_us * 9 / 100) as u32;
                let (mut sent, mut bytes) = (0u64, 0u64);
                for datagram in packetizer.push(&packets, stamp) {
                    match rtp_sock.send_to(&datagram, target) {
                        Ok(n) => {
                            sent += 1;
                            bytes += n as u64;
                        }
                        Err(e) => tracing::warn!("miracast: sending media: {e:#}"),
                    }
                }
                if sent > 0 {
                    stats.sent(sent, bytes, Instant::now());
                }
            }
        }

        // What `miracast-status` will report if it asks in the next moment.
        let now = Instant::now();
        stats_ctx.mode = session
            .chosen()
            .map(|m| format!("{}x{}@{}", m.width, m.height, m.fps));
        stats_ctx.last_heard = session.last_heard();
        if let Ok(mut slot) = published.lock() {
            *slot = Some(stats.snapshot(now, started, &stats_ctx));
        }
    };

    // Always, on every path out: a display left believing a session is live can
    // refuse the next one.
    stop.store(true, Ordering::SeqCst);
    let bye = session.teardown();
    let _ = sock.write_all(bye.format().as_bytes());
    tracing::info!("miracast: teardown: {reason}");
    // Takes the record with it, so the next command sees no cast rather than a
    // stale one.
    drop(control);
    Ok(())
}

fn write_actions(sock: &mut TcpStream, actions: Vec<Action>) -> anyhow::Result<()> {
    for action in actions {
        if let Action::Send(m) = action {
            sock.write_all(m.format().as_bytes())?;
        }
    }
    Ok(())
}

/// A source identifier that will not collide with another cast on the same
/// display. Nothing here needs cryptographic quality.
fn rand_ssrc() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish() as u32
}

#[cfg(windows)]
fn start_media(
    session: &SourceSession,
    opts: &MiracastOptions,
    tx: mpsc::Sender<Media>,
    stop: Arc<AtomicBool>,
) {
    let mode = session.chosen();
    let (width, height) = mode.map(|m| (m.width, m.height)).unwrap_or((1280, 720));
    let fps = mode.map(|m| m.fps).unwrap_or(opts.fps);
    // The display told us what it can take; exceeding it is how a stream gets
    // refused for reasons that never reach us.
    let bitrate_bps = session
        .max_bitrate_kbps()
        .map(|k| k.saturating_mul(1000))
        .unwrap_or(10_000_000);
    let output = opts.output;
    let enc_mode = opts.mode;
    let start = Instant::now();

    let vtx = tx.clone();
    let vstop = stop.clone();
    let _ = std::thread::Builder::new()
        .name("miracast-video".into())
        .spawn(move || {
            let mut cap = match castr_capture_win::DesktopCapture::new(output) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("miracast: capture init: {e:#}");
                    return;
                }
            };
            let cfg = EncoderConfig {
                width,
                height,
                fps,
                bitrate_bps,
                mode: enc_mode,
            };
            let mut enc = match castr_codec_win::MfEncoder::new(cfg) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("miracast: no encoder: {e:#}");
                    return;
                }
            };
            tracing::info!(
                "miracast: encoding {width}x{height}p{fps} at {} kbps with {}",
                bitrate_bps / 1000,
                enc.name()
            );
            let want = enc.input_format();
            let interval = Duration::from_micros(1_000_000 / fps.max(1) as u64);
            let mut last: Option<RawFrame> = None;
            let mut next_due = Instant::now();
            // Whether anything new has arrived since the last frame we encoded.
            let mut fresh = false;
            while !vstop.load(Ordering::Relaxed) {
                let now = start.elapsed().as_micros() as u64;
                // Duplication hands over a frame only when the desktop changes,
                // so a still screen yields nothing at all. A display reads that
                // as a dead source and drops the session within seconds, which
                // is why the last frame is repeated rather than nothing sent.
                let wait = next_due.saturating_duration_since(Instant::now());
                match cap.next_frame(wait.as_millis().max(1) as u32, now) {
                    Ok(Some(f)) => {
                        last = Some(f);
                        fresh = true;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("miracast: capture: {e:#}");
                        break;
                    }
                }
                if Instant::now() < next_due {
                    continue;
                }
                next_due += interval;
                let Some(mut frame) = last.clone() else { continue };
                frame.timestamp_us = start.elapsed().as_micros() as u64;
                // The display negotiated a size; the desktop is whatever it is.
                // Scale first, then convert, because the encoder takes only its
                // own input format and will refuse anything else outright.
                let scaled = if frame.width != width || frame.height != height {
                    RawFrame {
                        format: PixelFormat::Bgra,
                        width,
                        height,
                        stride: width * 4,
                        data: crate::cast::resize_bgra_nearest(
                            &frame.data,
                            frame.width,
                            frame.height,
                            frame.stride,
                            width,
                            height,
                        ),
                        timestamp_us: frame.timestamp_us,
                    }
                } else {
                    frame
                };
                let input = castr_media::convert::convert(&scaled, want);
                let repeated = !std::mem::take(&mut fresh);
                match enc.encode(&input) {
                    Ok(Some(out)) => {
                        if vtx
                            .send(Media::Video {
                                data: out.data,
                                pts_us: out.timestamp_us,
                                repeated,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("miracast: encode: {e:#}");
                        break;
                    }
                }
            }
        });

    let _ = std::thread::Builder::new()
        .name("miracast-audio".into())
        .spawn(move || {
            let mut cap = match castr_capture_win::LoopbackCapture::new() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("miracast: audio capture unavailable: {e:#}");
                    return;
                }
            };
            let mut buf = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                buf.clear();
                if let Err(e) = cap.drain(&mut buf) {
                    tracing::warn!("miracast: audio drain: {e:#}");
                    break;
                }
                if buf.is_empty() {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let pts_us = start.elapsed().as_micros() as u64;
                if tx
                    .send(Media::Audio {
                        samples: buf.clone(),
                        pts_us,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
}

#[cfg(not(windows))]
fn start_media(
    _session: &SourceSession,
    _opts: &MiracastOptions,
    _tx: mpsc::Sender<Media>,
    _stop: Arc<AtomicBool>,
) {
    tracing::error!("miracast: casting is Windows-only for now");
}
