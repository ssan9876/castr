use anyhow::{anyhow, bail, Context};
use castr_media::audio::{AudioEncoder, FrameChunker};
use castr_media::bitrate::{BitrateController, Decision, Resolution};
use castr_media::*;
use castr_net::*;
use castr_proto::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

pub struct CastOptions {
    pub target: String,
    pub mode: Mode,
    pub fps: u32,
    pub max_bitrate: Option<u32>,
    pub sender_name: String,
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum CastCommand {
    // Constructed by the GUI (Task 23); the CLI only ever sends `Stop`.
    #[allow(dead_code)]
    SetMode(Mode),
    Stop,
}

#[derive(Debug, Clone, Default)]
pub struct CastStatus {
    pub state: String,
    pub width: u32,
    pub height: u32,
    pub bitrate_bps: u32,
    pub rtt_ms: u32,
    pub loss_pct: f32,
    pub fps: f32,
}

pub fn resize_bgra_nearest(src: &[u8], w: u32, h: u32, stride: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dw * dh * 4) as usize);
    for y in 0..dh {
        let sy = (y as u64 * h as u64 / dh as u64) as usize;
        for x in 0..dw {
            let sx = (x as u64 * w as u64 / dw as u64) as usize;
            let p = sy * stride as usize + sx * 4;
            out.extend_from_slice(&src[p..p + 4]);
        }
    }
    out
}

pub fn choose_params(
    native: (u32, u32),
    fps: u32,
    max_bitrate: Option<u32>,
    mode: Mode,
    caps: &Capabilities,
) -> StreamParams {
    let scale = (caps.max_width as f64 / native.0 as f64)
        .min(caps.max_height as f64 / native.1 as f64)
        .min(1.0);
    let width = ((native.0 as f64 * scale) as u32) & !1;
    let height = ((native.1 as f64 * scale) as u32) & !1;
    let ceiling = max_bitrate.unwrap_or(u32::MAX).min(caps.max_bitrate_bps);
    StreamParams {
        codec: Codec::H264,
        width,
        height,
        fps: fps.min(caps.max_fps).max(1),
        mode,
        bitrate_bps: ceiling / 2,
    }
}

pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<ReceiverInfo>> {
    browse(timeout, PROBE_PORT).await
}

pub async fn resolve_target(target: &str, timeout: Duration) -> anyhow::Result<ReceiverInfo> {
    let found = discover(timeout).await?;
    let t = target.to_lowercase();
    found
        .into_iter()
        .find(|r| {
            r.name.to_lowercase() == t
                || (t.len() >= 6 && hex::encode(r.fingerprint).starts_with(&t))
        })
        .ok_or_else(|| anyhow!("receiver '{target}' not found; run `castr-sender list`"))
}

fn client_for(target: &ReceiverInfo, config_dir: &Path) -> anyhow::Result<(Identity, Endpoint)> {
    let id = Identity::load_or_create(config_dir)?;
    let trust = Arc::new(RwLock::new(HashSet::from([target.fingerprint])));
    let ep = Endpoint::client("0.0.0.0:0".parse()?, &id, trust_fingerprints(trust))?;
    Ok((id, ep))
}

/// Non-interactive pairing with an already-known PIN; kept for the GUI
/// (Task 23), which can obtain the PIN from a dialog after its own probe
/// connection. The CLI uses `pair_interactive` below instead.
#[allow(dead_code)]
pub async fn pair(target: &ReceiverInfo, pin: &str, config_dir: &Path) -> anyhow::Result<()> {
    let pin = pin.to_string();
    pair_interactive(target, config_dir, move || Ok(pin)).await
}

/// Same handshake as `pair`, but the PIN is not required up front: the
/// receiver only generates and displays its PIN after it sees our `Hello`
/// and answers `Error { code: 4 }` (pairing required), so `read_pin` is
/// invoked only once that reply has arrived and the pairing link is open,
/// giving a human time to read the PIN off the receiver before typing it.
pub async fn pair_interactive(
    target: &ReceiverInfo,
    config_dir: &Path,
    read_pin: impl FnOnce() -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    let (id, ep) = client_for(target, config_dir)?;
    let link = ep.connect(target.addr).await?;
    link.send_control(&ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        name: "pairing".into(),
        resume_token: None,
    })
    .await?;
    match link.recv_control().await? {
        ControlMessage::Error { code: 4, .. } => {}
        ControlMessage::HelloAck { .. } => {
            link.send_control(&ControlMessage::Goodbye {
                reason: "already paired".into(),
            })
            .await?;
            println!("already paired with {}", target.name);
            return Ok(());
        }
        other => bail!("unexpected reply {other:?}"),
    }
    let pin = read_pin()?;
    pair_as_sender(&link, id.fingerprint, pin.trim()).await?;
    let mut store = PairedStore::load(config_dir.join("paired.toml"))?;
    store.add(target.fingerprint, target.name.clone());
    store.save()?;
    link.send_control(&ControlMessage::Goodbye {
        reason: "paired".into(),
    })
    .await?;
    Ok(())
}

/// Commands from the network task to the capture/encode thread.
enum EncCmd {
    Bitrate(u32),
    Keyframe,
    Mode(Mode),
    Resolution(u32, u32),
    Stop,
}

/// One select! branch handles both "waiting for the receiver to open the NACK stream" and "reading NACKs from it".
enum NackEv {
    Nack(anyhow::Result<Nack>),
    Stream(anyhow::Result<NackReceiver>),
}

struct VideoOut {
    frame: EncodedFrame,
}

#[cfg(windows)]
fn spawn_capture(
    params_tx: std::sync::mpsc::Sender<(u32, u32)>,
    cmd_rx: std::sync::mpsc::Receiver<EncCmd>,
    out: mpsc::Sender<VideoOut>,
    fps: u32,
    mode: Mode,
    bitrate: u32,
    start: Instant,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    use castr_capture_win::DesktopCapture;
    let handle = std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            let mut cap = match DesktopCapture::new(0) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("capture init: {e:#}");
                    return;
                }
            };
            let native = cap.size();
            let _ = params_tx.send(native);
            let (mut w, mut h) = native;
            let mut cfg = EncoderConfig {
                width: w,
                height: h,
                fps,
                bitrate_bps: bitrate,
                mode,
            };
            let mut enc: Box<dyn VideoEncoder> = match castr_codec_win::MfEncoder::new(cfg) {
                Ok(e) => Box::new(e),
                Err(e) => {
                    tracing::warn!("MF encoder unavailable ({e:#}), using openh264");
                    match sw::SwEncoder::new(cfg) {
                        Ok(e) => Box::new(e),
                        Err(e) => {
                            tracing::error!("no encoder: {e:#}");
                            return;
                        }
                    }
                }
            };
            tracing::info!("encoder: {}", enc.name());
            let interval = Duration::from_micros(1_000_000 / fps as u64);
            let mut last: Option<RawFrame> = None;
            let mut last_sent = Instant::now();
            loop {
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        EncCmd::Bitrate(b) => {
                            cfg.bitrate_bps = b;
                            let _ = enc.set_bitrate(b);
                        }
                        EncCmd::Keyframe => enc.request_keyframe(),
                        EncCmd::Mode(m) => {
                            cfg.mode = m;
                            let _ = enc.set_mode(m);
                        }
                        EncCmd::Resolution(nw, nh) => {
                            w = nw;
                            h = nh;
                            cfg.width = w;
                            cfg.height = h;
                            enc = match castr_codec_win::MfEncoder::new(cfg) {
                                Ok(e) => Box::new(e),
                                Err(_) => match sw::SwEncoder::new(cfg) {
                                    Ok(e) => Box::new(e),
                                    Err(e) => {
                                        tracing::error!("encoder: {e:#}");
                                        return;
                                    }
                                },
                            };
                        }
                        EncCmd::Stop => return,
                    }
                }
                let ts = start.elapsed().as_micros() as u64;
                let frame = match cap.next_frame(interval.as_millis() as u32, ts) {
                    Ok(Some(f)) => Some(f),
                    Ok(None) => {
                        if last_sent.elapsed() >= Duration::from_millis(500) {
                            last.clone().map(|mut f| {
                                f.timestamp_us = ts;
                                f
                            })
                        } else {
                            None
                        }
                    }
                    Err(e) => {
                        tracing::warn!("capture: {e:#}; reopening");
                        std::thread::sleep(Duration::from_millis(250));
                        match DesktopCapture::new(0) {
                            Ok(c) => cap = c,
                            Err(e) => tracing::warn!("reopen failed: {e:#}"),
                        }
                        enc.request_keyframe();
                        None
                    }
                };
                let Some(f) = frame else { continue };
                last = Some(f.clone());
                let scaled = if (f.width, f.height) != (w, h) {
                    RawFrame {
                        format: PixelFormat::Bgra,
                        width: w,
                        height: h,
                        stride: w * 4,
                        data: resize_bgra_nearest(&f.data, f.width, f.height, f.stride, w, h),
                        timestamp_us: f.timestamp_us,
                    }
                } else {
                    f
                };
                let input = convert::convert(&scaled, enc.input_format());
                match enc.encode(&input) {
                    Ok(Some(e)) => {
                        last_sent = Instant::now();
                        if out.blocking_send(VideoOut { frame: e }).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("encode: {e:#}"),
                }
            }
        })?;
    Ok(handle)
}

#[cfg(not(windows))]
fn spawn_capture(
    _: std::sync::mpsc::Sender<(u32, u32)>,
    _: std::sync::mpsc::Receiver<EncCmd>,
    _: mpsc::Sender<VideoOut>,
    _: u32,
    _: Mode,
    _: u32,
    _: Instant,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    bail!("screen capture is only implemented on Windows in this version")
}

struct AudioOut {
    ts_us: u64,
    packet: Vec<u8>,
}

/// Timestamps for the 10 ms Opus frames produced by one WASAPI drain.
///
/// WASAPI loopback delivers nothing while the desktop is silent, so a
/// free-running counter (`origin + n * 10 ms`) drifts behind the video clock
/// by the total silence time and, since the receiver uses audio as its master
/// clock, eventually wedges video. Instead every drain re-anchors to the wall
/// clock: `now_us` is the moment the drain returned, `buffered_samples_per_channel`
/// is everything still queued in the chunker (including what was just drained),
/// so the oldest queued sample was captured `buffered / 48 kHz` ago. Frames are
/// then stamped 10 ms apart from that anchor, clamped to stay monotonic.
fn audio_frame_timestamps(
    now_us: u64,
    buffered_samples_per_channel: usize,
    frames_out: usize,
    last_ts: Option<u64>,
) -> Vec<u64> {
    let backlog_us =
        buffered_samples_per_channel as u64 * 1_000_000 / castr_media::audio::SAMPLE_RATE as u64;
    let base = now_us.saturating_sub(backlog_us);
    let mut out = Vec::with_capacity(frames_out);
    let mut last = last_ts;
    for i in 0..frames_out {
        let mut ts = base + i as u64 * 10_000;
        if let Some(l) = last {
            ts = ts.max(l + 10_000);
        }
        out.push(ts);
        last = Some(ts);
    }
    out
}

#[cfg(windows)]
fn spawn_audio(
    out: mpsc::Sender<AudioOut>,
    start: Instant,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let mut cap = match castr_capture_win::LoopbackCapture::new() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("audio capture unavailable: {e:#}");
                    return;
                }
            };
            let mut enc = match AudioEncoder::new() {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("opus: {e:#}");
                    return;
                }
            };
            let mut chunker = FrameChunker::new();
            let mut buf = Vec::new();
            // The chunker has no length accessor, so mirror its fill level here.
            let mut queued_interleaved: usize = 0;
            let mut last_ts: Option<u64> = None;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                buf.clear();
                if let Err(e) = cap.drain(&mut buf) {
                    tracing::warn!("audio drain: {e:#}");
                    break;
                }
                let now = start.elapsed().as_micros() as u64;
                chunker.push(&buf);
                queued_interleaved += buf.len();
                let frames_out = queued_interleaved / castr_media::audio::FRAME_INTERLEAVED;
                let stamps = audio_frame_timestamps(
                    now,
                    queued_interleaved / castr_media::audio::CHANNELS,
                    frames_out,
                    last_ts,
                );
                let mut stamps = stamps.into_iter();
                while let Some(frame) = chunker.next_frame() {
                    queued_interleaved -= frame.len();
                    let ts = match stamps.next() {
                        Some(t) => t,
                        None => last_ts.map(|l| l + 10_000).unwrap_or(now),
                    };
                    last_ts = Some(ts);
                    match enc.encode(&frame) {
                        Ok(p) => {
                            if out
                                .blocking_send(AudioOut {
                                    ts_us: ts,
                                    packet: p,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => tracing::warn!("opus encode: {e:#}"),
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .ok();
}

#[cfg(not(windows))]
fn spawn_audio(_: mpsc::Sender<AudioOut>, _: Instant, _: Arc<std::sync::atomic::AtomicBool>) {}

pub async fn cast(
    opts: CastOptions,
    mut cmds: mpsc::Receiver<CastCommand>,
    status: watch::Sender<CastStatus>,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let set_state = |s: &str| {
        status.send_modify(|st| st.state = s.to_string());
    };
    // Owned out here so teardown below runs on every exit path, including the
    // early `?`/`bail!` returns inside the session: the capture and audio
    // threads must always be told to stop, and the GUI must always see a
    // terminal state ("stopped" or "failed") so it clears the active cast.
    let (video_tx, mut video_rx) = mpsc::channel::<VideoOut>(4);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioOut>(32);
    let (enc_tx, enc_rx) = std::sync::mpsc::channel::<EncCmd>();
    let (native_tx, native_rx) = std::sync::mpsc::channel::<(u32, u32)>();
    let stop_audio = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut open_link: Option<Link> = None;

    let result: anyhow::Result<()> = async {
        set_state("discovering");
        let target = resolve_target(&opts.target, Duration::from_secs(2)).await?;
        let (_id, ep) = client_for(&target, &opts.config_dir)?;

        // Connect first so we know caps before opening the encoder.
        set_state("connecting");
        let mut token: Option<[u8; 16]> = None;
        let mut link = connect_with_retry(
            &ep,
            target.addr,
            &opts.sender_name,
            &mut token,
            Duration::from_secs(5),
        )
        .await?;
        open_link = Some(link.clone());
        let caps = match link.recv_control().await? {
            ControlMessage::HelloAck { caps, .. } => caps,
            ControlMessage::Error { code: 4, .. } => bail!(
                "not paired with '{}'; run: castr-sender pair \"{}\"",
                target.name,
                target.name
            ),
            other => bail!("unexpected {other:?}"),
        };

        let mode = opts.mode;
        let ceiling = opts
            .max_bitrate
            .unwrap_or(u32::MAX)
            .min(caps.max_bitrate_bps);
        // `choose_params` clamps fps the same way; compute it once up front so the
        // capture/encode thread is started at the fps that will actually be used,
        // rather than the requested (possibly higher) fps.
        let fps = opts.fps.min(caps.max_fps).max(1);
        let _cap_thread = spawn_capture(native_tx, enc_rx, video_tx, fps, mode, ceiling / 2, start)?;
        let native =
            tokio::task::spawn_blocking(move || native_rx.recv_timeout(Duration::from_secs(5)))
                .await?
                .context("capture did not start")?;
        let mut params = choose_params(native, opts.fps, opts.max_bitrate, mode, &caps);
        debug_assert_eq!(params.fps, fps);
        if (params.width, params.height) != native {
            enc_tx
                .send(EncCmd::Resolution(params.width, params.height))
                .ok();
        }
        enc_tx.send(EncCmd::Bitrate(params.bitrate_bps)).ok();
        link.send_control(&ControlMessage::StartStream(params.clone()))
            .await?;
        spawn_audio(audio_tx, start, stop_audio.clone());

        let mut ctl = BitrateController::new(
            ceiling,
            params.bitrate_bps,
            Resolution {
                width: params.width,
                height: params.height,
            },
            mode,
        );
        let mut packetizer = Packetizer::new();
        let mut rtx = RetransmitBuffer::new(500_000);
        let frame_interval_us = 1_000_000 / params.fps as u64;
        let mut sent_frames = 0u32;
        let mut fps_window = Instant::now();
        let mut nack_rx: Option<NackReceiver> = None;
        let mut control_errors: u32 = 0;
        let mut audio_alive = true;
        set_state("casting");
        status.send_modify(|st| {
            st.width = params.width;
            st.height = params.height;
            st.bitrate_bps = params.bitrate_bps;
        });

        loop {
            let now = start.elapsed().as_micros() as u64;
            tokio::select! {
                v = video_rx.recv() => {
                    let Some(v) = v else { bail!("capture thread exited") };
                    let frags = packetizer.packetize(STREAM_VIDEO, v.frame.keyframe, v.frame.timestamp_us, &v.frame.data, link.max_datagram_size());
                    rtx.record(packetizer.last_frame_number(), v.frame.keyframe, frags.clone(), now);
                    for f in frags { if let Err(e) = link.send_datagram(f) { tracing::debug!("send: {e:#}"); } }
                    sent_frames += 1;
                    if fps_window.elapsed() >= Duration::from_secs(1) {
                        let fps = sent_frames as f32 / fps_window.elapsed().as_secs_f32();
                        status.send_modify(|st| { st.fps = fps; st.rtt_ms = link.rtt().as_millis() as u32; });
                        sent_frames = 0; fps_window = Instant::now();
                    }
                }
                a = audio_rx.recv(), if audio_alive => {
                    match a {
                        Some(a) => {
                            for f in packetizer.packetize(STREAM_AUDIO, false, a.ts_us, &a.packet, link.max_datagram_size()) {
                                let _ = link.send_datagram(f);
                            }
                        }
                        None => {
                            tracing::warn!("audio thread exited; continuing without audio");
                            audio_alive = false;
                        }
                    }
                }
                m = link.recv_control() => {
                    match m {
                        Ok(ControlMessage::SessionToken(t)) => { token = Some(t); control_errors = 0; }
                        Ok(ControlMessage::RequestKeyframe) => { enc_tx.send(EncCmd::Keyframe).ok(); control_errors = 0; }
                        Ok(ControlMessage::Stats(s)) => {
                            control_errors = 0;
                            let total = s.fragments_lost + s.fragments_received;
                            let loss = if total == 0 { 0.0 } else { s.fragments_lost as f32 * 100.0 / total as f32 };
                            status.send_modify(|st| st.loss_pct = loss);
                            if let Some(Decision { bitrate_bps, resolution }) = ctl.on_stats(&s, now) {
                                if bitrate_bps != params.bitrate_bps {
                                    params.bitrate_bps = bitrate_bps;
                                    enc_tx.send(EncCmd::Bitrate(bitrate_bps)).ok();
                                }
                                if (resolution.width, resolution.height) != (params.width, params.height) {
                                    params.width = resolution.width; params.height = resolution.height;
                                    enc_tx.send(EncCmd::Resolution(params.width, params.height)).ok();
                                    link.send_control(&ControlMessage::StartStream(params.clone())).await?;
                                }
                                status.send_modify(|st| { st.bitrate_bps = params.bitrate_bps; st.width = params.width; st.height = params.height; });
                            }
                        }
                        Ok(ControlMessage::Error { code, message }) => { control_errors = 0; tracing::warn!("receiver error {code}: {message}"); }
                        Ok(ControlMessage::Goodbye { reason }) => { tracing::info!("receiver said goodbye: {reason}"); break; }
                        Ok(_) => { control_errors = 0; }
                        Err(e) => {
                            control_errors += 1;
                            tracing::debug!("control: {e:#} ({control_errors}/20)");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            if control_errors >= 20 {
                                tracing::warn!("control stream unresponsive, treating link as lost");
                                link.close("control stream failed");
                                control_errors = 0;
                            }
                        }
                    }
                }
                ev = async {
                    match nack_rx.as_mut() {
                        Some(r) => NackEv::Nack(r.recv().await),
                        None => NackEv::Stream(link.accept_nack_stream().await),
                    }
                } => {
                    match ev {
                        NackEv::Nack(Ok(nack)) => for f in rtx.lookup(&nack, now, frame_interval_us) { let _ = link.send_datagram(f); },
                        NackEv::Nack(Err(_)) => nack_rx = None,
                        NackEv::Stream(Ok(rx)) => nack_rx = Some(rx),
                        NackEv::Stream(Err(e)) => { tracing::debug!("nack stream: {e:#}"); tokio::time::sleep(Duration::from_millis(100)).await; }
                    }
                }
                Some(cmd) = cmds.recv() => {
                    match cmd {
                        CastCommand::SetMode(m) => {
                            params.mode = m;
                            ctl.set_mode(m);
                            enc_tx.send(EncCmd::Mode(m)).ok();
                            link.send_control(&ControlMessage::SetMode(m)).await?;
                        }
                        CastCommand::Stop => break,
                    }
                }
                _ = link.closed() => {
                    set_state("reconnecting");
                    tracing::warn!("connection lost, reconnecting");
                    let mut video_alive = true;
                    let channels = ReconnectChannels {
                        video_rx: &mut video_rx,
                        audio_rx: &mut audio_rx,
                        video_alive: &mut video_alive,
                        audio_alive: &mut audio_alive,
                    };
                    match reconnect_draining(&ep, target.addr, &opts.sender_name, &mut token, channels).await {
                        Ok(l) => {
                            link = l;
                            open_link = Some(link.clone());
                            if !video_alive {
                                bail!("capture thread exited");
                            }
                            match link.recv_control().await? {
                                ControlMessage::HelloAck { .. } => {}
                                other => bail!("resume failed: {other:?}"),
                            }
                            nack_rx = None;
                            control_errors = 0;
                            link.send_control(&ControlMessage::StartStream(params.clone())).await?;
                            enc_tx.send(EncCmd::Keyframe).ok();
                            set_state("casting");
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    enc_tx.send(EncCmd::Stop).ok();
    stop_audio.store(true, std::sync::atomic::Ordering::Relaxed);
    let reason = if result.is_ok() { "stopped" } else { "failed" };
    if let Some(link) = open_link {
        let _ = link
            .send_control(&ControlMessage::Goodbye {
                reason: reason.into(),
            })
            .await;
        // Closing the connection right after queuing the Goodbye can race the
        // QUIC stream flush and cut it off before the receiver reads it (seen as
        // "connection lost" on the receiver instead of "goodbye: stopped"). Give
        // the receiver a moment to read it and close its end first; only force
        // the close if it doesn't.
        tokio::select! {
            _ = link.closed() => {}
            _ = tokio::time::sleep(Duration::from_millis(500)) => { link.close(reason); }
        }
    }
    set_state(reason);
    result
}

/// Bundles the capture/audio drain state `reconnect_draining` needs, so the
/// function stays under clippy's argument-count lint instead of taking each
/// receiver/flag as its own parameter.
struct ReconnectChannels<'a> {
    video_rx: &'a mut mpsc::Receiver<VideoOut>,
    audio_rx: &'a mut mpsc::Receiver<AudioOut>,
    video_alive: &'a mut bool,
    audio_alive: &'a mut bool,
}

/// Reconnects like `connect_with_retry`, but keeps draining `video_rx` and
/// `audio_rx` while doing so. Without this, the capture/audio threads block
/// on `blocking_send` into full channels once reconnection takes longer than
/// a few frames, stalling capture for the whole outage.
///
/// `video_alive`/`audio_alive` reflect the caller's current belief about
/// whether the capture/audio threads are still running; once a channel
/// reports closed, its drain arm is disabled (via the `if` guard) instead of
/// busy-polling a `recv()` that now resolves to `None` immediately on every
/// poll. The caller's flags are updated in place so its own state (e.g. the
/// main loop's `audio_alive`) stays consistent with what happened during the
/// outage; video is not optional, so the caller must check `*video_alive`
/// after this returns `Ok` and bail out if it went false.
async fn reconnect_draining(
    ep: &Endpoint,
    addr: std::net::SocketAddr,
    name: &str,
    token: &mut Option<[u8; 16]>,
    channels: ReconnectChannels<'_>,
) -> anyhow::Result<Link> {
    let ReconnectChannels {
        video_rx,
        audio_rx,
        video_alive,
        audio_alive,
    } = channels;
    let mut dropped = 0u64;
    let mut connect_fut = std::pin::pin!(connect_with_retry(
        ep,
        addr,
        name,
        token,
        Duration::from_secs(30)
    ));
    loop {
        tokio::select! {
            biased;
            res = &mut connect_fut => {
                if dropped > 0 {
                    tracing::info!("discarded {dropped} capture/audio frames while reconnecting");
                }
                return res;
            }
            v = video_rx.recv(), if *video_alive => {
                match v {
                    Some(_) => dropped += 1,
                    None => {
                        tracing::warn!("capture thread exited while reconnecting");
                        *video_alive = false;
                    }
                }
            }
            a = audio_rx.recv(), if *audio_alive => {
                match a {
                    Some(_) => dropped += 1,
                    None => *audio_alive = false,
                }
            }
        }
    }
}

async fn connect_with_retry(
    ep: &Endpoint,
    addr: std::net::SocketAddr,
    name: &str,
    token: &mut Option<[u8; 16]>,
    total: Duration,
) -> anyhow::Result<Link> {
    let deadline = Instant::now() + total;
    let mut backoff = Duration::from_millis(200);
    loop {
        match ep.connect(addr).await {
            Ok(link) => {
                let hello = ControlMessage::Hello {
                    version: PROTOCOL_VERSION,
                    name: name.to_string(),
                    resume_token: *token,
                };
                match link.send_control(&hello).await {
                    Ok(()) => return Ok(link),
                    Err(e) if Instant::now() + backoff < deadline => {
                        tracing::debug!("hello send failed ({e:#}), retry in {backoff:?}");
                        link.close("hello failed");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                    }
                    Err(e) => {
                        link.close("hello failed");
                        return Err(e).context("could not reach receiver");
                    }
                }
            }
            Err(e) if Instant::now() + backoff < deadline => {
                tracing::debug!("connect failed ({e:#}), retry in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
            Err(e) => return Err(e).context("could not reach receiver"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_halves_a_checkerboard() {
        let (w, h) = (4u32, 4u32);
        let mut src = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 2 + y / 2) % 2 == 0 { 255 } else { 0 };
                src.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let out = resize_bgra_nearest(&src, w, h, w * 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
        assert_eq!(&out[0..4], &[255, 255, 255, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 255]);
        assert_eq!(&out[8..12], &[0, 0, 0, 255]);
        assert_eq!(&out[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn resize_respects_source_stride() {
        let src = vec![9u8; 2 * 16];
        let out = resize_bgra_nearest(&src, 2, 2, 16, 2, 2);
        assert_eq!(out, vec![9u8; 16]);
    }

    #[test]
    fn choose_params_clamps_to_caps_and_evens() {
        let caps = Capabilities {
            max_width: 1280,
            max_height: 720,
            max_fps: 30,
            max_bitrate_bps: 10_000_000,
            codecs: vec![Codec::H264],
            audio: true,
        };
        let p = choose_params((2560, 1440), 60, Some(40_000_000), Mode::Game, &caps);
        assert_eq!((p.width, p.height, p.fps), (1280, 720, 30));
        assert_eq!(p.bitrate_bps, 5_000_000);
        let p2 = choose_params((1000, 601), 60, None, Mode::Quality, &caps);
        assert_eq!((p2.width, p2.height), (1000, 600));
    }

    #[test]
    fn audio_timestamps_without_backlog_use_now() {
        assert_eq!(
            audio_frame_timestamps(1_000_000, 0, 1, None),
            vec![1_000_000]
        );
    }

    #[test]
    fn audio_timestamps_subtract_the_backlog() {
        // 480 samples per channel = exactly one 10 ms frame still queued.
        assert_eq!(
            audio_frame_timestamps(1_000_000, 480, 1, None),
            vec![990_000]
        );
        // Two frames queued: the oldest is 20 ms old, the next 10 ms later.
        assert_eq!(
            audio_frame_timestamps(1_000_000, 960, 2, None),
            vec![980_000, 990_000]
        );
    }

    #[test]
    fn audio_timestamps_are_clamped_monotonic() {
        // A drain whose anchor would go backwards past the last emitted frame
        // is pushed forward to last + 10 ms instead.
        assert_eq!(
            audio_frame_timestamps(1_000_000, 480, 2, Some(995_000)),
            vec![1_005_000, 1_015_000]
        );
        // A forward-moving anchor is left alone.
        assert_eq!(
            audio_frame_timestamps(1_000_000, 480, 1, Some(900_000)),
            vec![990_000]
        );
    }
}
