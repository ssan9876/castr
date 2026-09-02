use crate::audio_out::AudioOut;
use crate::render::Renderer;
use anyhow::anyhow;
use castr_media::clock::AvClock;
use castr_media::jitter::JitterBuffer;
use castr_media::{sw::SwDecoder, RawFrame, VideoDecoder};
use castr_net::*;
use castr_proto::*;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DecoderChoice {
    Auto,
    Mf,
    Sw,
}

pub struct ReceiverConfig {
    pub name: String,
    pub fullscreen: bool,
    pub max_bitrate: u32,
    pub decoder: DecoderChoice,
    pub bind: SocketAddr,
    pub config_dir: PathBuf,
}

/// Messages from the network side to the SDL main thread.
pub enum UiEvent {
    Overlay(Option<String>),
    Frame(RawFrame),
    AudioPacket { ts_us: u64, data: Vec<u8> },
    Mode(Mode),
    Quit,
}

/// How far an audio packet's timestamp may fall behind the newest video
/// timestamp before it stops driving the audio-master clock.
const AUDIO_LAG_LIMIT_US: u64 = 1_000_000;

fn now_us(start: Instant) -> u64 {
    start.elapsed().as_micros() as u64
}

fn open_decoder(choice: DecoderChoice) -> anyhow::Result<Box<dyn VideoDecoder>> {
    #[cfg(windows)]
    {
        if matches!(choice, DecoderChoice::Auto | DecoderChoice::Mf) {
            match castr_codec_win::MfDecoder::new() {
                Ok(d) => return Ok(Box::new(d)),
                Err(e) if choice == DecoderChoice::Mf => return Err(e),
                Err(e) => tracing::warn!("MF decoder unavailable, falling back to openh264: {e:#}"),
            }
        }
    }
    #[cfg(not(windows))]
    {
        if choice == DecoderChoice::Mf {
            anyhow::bail!("Media Foundation is Windows only");
        }
    }
    Ok(Box::new(SwDecoder::new()?))
}

pub fn run(cfg: ReceiverConfig) -> anyhow::Result<()> {
    let start = Instant::now();
    let id = Identity::load_or_create(&cfg.config_dir)?;
    tracing::info!(
        "receiver '{}' fingerprint {}",
        cfg.name,
        id.fingerprint_hex()
    );
    let paired = Arc::new(Mutex::new(PairedStore::load(
        cfg.config_dir.join("paired.toml"),
    )?));
    let caps = Capabilities {
        max_width: 1920,
        max_height: 1080,
        max_fps: 60,
        max_bitrate_bps: cfg.max_bitrate,
        codecs: vec![Codec::H264],
        audio: true,
    };

    let mut renderer = Renderer::new(&format!("castr - {}", cfg.name), cfg.fullscreen)?;
    let audio_sys = renderer.sdl.audio().map_err(|e| anyhow!(e))?;
    let mut audio = AudioOut::new(&audio_sys, 40_000)?;
    let mut clock = AvClock::new();

    let (ui_tx, mut ui_rx) = mpsc::channel::<UiEvent>(64);
    let jitter = Arc::new(Mutex::new(JitterBuffer::new(Mode::Game, 33_333)));
    let stats = Arc::new(Mutex::new(Stats::default()));

    // Decode thread: jitter buffer -> decoder -> UI.
    {
        let jitter = jitter.clone();
        let ui = ui_tx.clone();
        let stats = stats.clone();
        let choice = cfg.decoder;
        std::thread::Builder::new()
            .name("decode".into())
            .spawn(move || {
                let mut decoder = match open_decoder(choice) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("decoder init failed: {e:#}");
                        let _ = ui.blocking_send(UiEvent::Quit);
                        return;
                    }
                };
                tracing::info!("decoder: {}", decoder.name());
                loop {
                    let frame = jitter.lock().unwrap().pop(now_us(start));
                    let Some(f) = frame else {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stats.lock().unwrap().decode_queue_depth =
                        jitter.lock().unwrap().depth() as u32;
                    match decoder.decode(&f.data, f.timestamp_us) {
                        Ok(Some(raw)) => {
                            if ui.blocking_send(UiEvent::Frame(raw)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("decode error: {e:#}"),
                    }
                }
            })?;
    }

    // Network runtime.
    let rt = tokio::runtime::Runtime::new()?;
    let net_cfg = NetConfig {
        name: cfg.name.clone(),
        bind: cfg.bind,
        caps,
        id,
        paired,
        jitter: jitter.clone(),
        stats: stats.clone(),
        ui: ui_tx,
        start,
        pairing_guard: Mutex::new(PairingGuard::new()),
    };
    rt.spawn(async move {
        if let Err(e) = network_main(net_cfg).await {
            tracing::error!("network: {e:#}");
        }
    });

    // SDL main loop.
    let mut pending: Option<RawFrame> = None;
    let mut last_video = Instant::now();
    // Newest video timestamp seen (presented or pending). Audio that lags far
    // behind it must not drive the master clock, or a sender with a broken
    // audio clock holds every video frame back forever.
    let mut latest_video_ts: Option<u64> = None;
    let mut warned_audio_lag = false;
    loop {
        if renderer.poll_quit() {
            break;
        }
        while let Ok(ev) = ui_rx.try_recv() {
            match ev {
                UiEvent::Overlay(t) => renderer.set_overlay(t.as_deref()),
                UiEvent::Frame(f) => {
                    latest_video_ts = Some(match latest_video_ts {
                        Some(v) => v.max(f.timestamp_us),
                        None => f.timestamp_us,
                    });
                    pending = Some(f);
                }
                UiEvent::AudioPacket { ts_us, data } if data.is_empty() => {
                    // Lost packet: let Opus conceal it so the clock keeps advancing smoothly.
                    let _ = audio.conceal_one();
                    let _ = ts_us;
                }
                UiEvent::AudioPacket { ts_us, data } => {
                    let lagging = latest_video_ts
                        .is_some_and(|v| v.saturating_sub(ts_us) > AUDIO_LAG_LIMIT_US);
                    let ratio = clock.drift_ratio(audio.buffered_us(), audio.target_us);
                    let queued = audio.push_packet(&data, ratio).unwrap_or(false);
                    if lagging {
                        if !warned_audio_lag {
                            warned_audio_lag = true;
                            tracing::warn!(
                                "audio timestamps lag video by more than {} ms; \
                                 ignoring audio for the master clock",
                                AUDIO_LAG_LIMIT_US / 1000
                            );
                        }
                    } else if queued {
                        let played_ts = ts_us.saturating_sub(audio.buffered_us());
                        clock.audio_played(played_ts, now_us(start));
                    }
                }
                UiEvent::Mode(m) => {
                    audio.target_us = match m {
                        Mode::Game => 40_000,
                        Mode::Quality => 100_000,
                    };
                    audio.clear();
                }
                UiEvent::Quit => return Err(anyhow!("fatal error, see log")),
            }
        }
        if let Some(f) = pending.take() {
            if clock.video_due(f.timestamp_us, now_us(start)) {
                renderer.present(&f)?;
                last_video = Instant::now();
            } else {
                pending = Some(f);
            }
        }
        if last_video.elapsed() > Duration::from_millis(50) {
            renderer.redraw()?;
            last_video = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    rt.shutdown_background();
    Ok(())
}

/// Attempt limiter for PIN pairing (spec 5.2: three failed attempts close the
/// connection). Three failures inside a minute lock pairing for a further
/// minute, during which an unpaired `Hello` is refused without generating or
/// displaying a PIN, so an attacker cannot brute-force the 6-digit PIN by
/// reconnecting in a loop.
struct PairingGuard {
    failures: VecDeque<Instant>,
    locked_until: Option<Instant>,
}

const PAIRING_WINDOW: Duration = Duration::from_secs(60);
const PAIRING_LOCKOUT: Duration = Duration::from_secs(60);
const PAIRING_MAX_FAILURES: usize = 3;

impl PairingGuard {
    fn new() -> Self {
        Self {
            failures: VecDeque::new(),
            locked_until: None,
        }
    }

    fn is_locked(&self, now: Instant) -> bool {
        self.locked_until.is_some_and(|until| now < until)
    }

    fn record_failure(&mut self, now: Instant) {
        while let Some(&oldest) = self.failures.front() {
            if now.duration_since(oldest) > PAIRING_WINDOW {
                self.failures.pop_front();
            } else {
                break;
            }
        }
        self.failures.push_back(now);
        if self.failures.len() >= PAIRING_MAX_FAILURES {
            self.failures.clear();
            self.locked_until = Some(now + PAIRING_LOCKOUT);
        }
    }
}

struct NetConfig {
    name: String,
    bind: SocketAddr,
    caps: Capabilities,
    id: Identity,
    paired: Arc<Mutex<PairedStore>>,
    jitter: Arc<Mutex<JitterBuffer>>,
    stats: Arc<Mutex<Stats>>,
    ui: mpsc::Sender<UiEvent>,
    start: Instant,
    pairing_guard: Mutex<PairingGuard>,
}

async fn network_main(cfg: NetConfig) -> anyhow::Result<()> {
    let endpoint = Endpoint::server(cfg.bind, &cfg.id, accept_any())?;
    let port = endpoint.local_addr()?.port();
    let _adv = Advertiser::start(&cfg.name, cfg.id.fingerprint, port, PROBE_PORT).await?;
    tracing::info!(
        "listening on {} (QUIC), probe port {}",
        endpoint.local_addr()?,
        PROBE_PORT
    );
    let mut session = ReceiverSession::new(cfg.name.clone(), cfg.caps.clone(), rand::random());
    cfg.ui
        .send(UiEvent::Overlay(Some("Waiting for sender".into())))
        .await
        .ok();
    loop {
        let link = endpoint.accept().await?;
        tracing::info!(
            "connection from {} fp {}",
            link.remote_addr(),
            hex_short(&link.peer_fingerprint())
        );
        if matches!(session.state(), ReceiverState::Closed) {
            session = ReceiverSession::new(cfg.name.clone(), cfg.caps.clone(), rand::random());
        }
        match handle_connection(&cfg, &link, &mut session).await {
            Ok(()) => tracing::info!("session ended"),
            Err(e) => tracing::warn!("connection error: {e:#}"),
        }
        if should_mark_disconnected(session.state()) {
            session.on_disconnect(now_us(cfg.start));
        }
        cfg.jitter.lock().unwrap().flush();
        let overlay = if matches!(session.state(), ReceiverState::Disconnected { .. }) {
            "Reconnecting"
        } else {
            "Waiting for sender"
        };
        cfg.ui
            .send(UiEvent::Overlay(Some(overlay.into())))
            .await
            .ok();
    }
}

fn hex_short(fp: &[u8; 32]) -> String {
    hex::encode(&fp[..6])
}

/// Only a session that had actually reached `Streaming` should be marked
/// disconnected when its connection drops. A connection that ended before
/// streaming began (failed pairing, or a control read error before `Hello`)
/// must leave the session untouched, otherwise a fresh `Hello` on the very
/// next connection lands on the `Disconnected` arm of `on_message` and is
/// rejected.
fn should_mark_disconnected(state: &ReceiverState) -> bool {
    matches!(state, ReceiverState::Streaming { .. })
}

async fn handle_connection(
    cfg: &NetConfig,
    link: &Link,
    session: &mut ReceiverSession,
) -> anyhow::Result<()> {
    // Phase 1: Hello / pairing until the session says Streaming.
    loop {
        let msg = link.recv_control().await?;
        let fp = link.peer_fingerprint();
        let is_paired = cfg.paired.lock().unwrap().is_paired(&fp);
        match msg {
            ControlMessage::Hello { .. } if !is_paired => {
                if cfg.pairing_guard.lock().unwrap().is_locked(Instant::now()) {
                    tracing::warn!("pairing locked out; refusing {}", hex_short(&fp));
                    link.send_control(&ControlMessage::Error {
                        code: 4,
                        message: "pairing locked, try again later".into(),
                    })
                    .await?;
                    link.close("pairing locked");
                    return Ok(());
                }
                link.send_control(&ControlMessage::Error {
                    code: 4,
                    message: "pairing required".into(),
                })
                .await?;
                let pin = generate_pin();
                println!("\n=== PAIRING PIN: {pin} ===\n");
                cfg.ui
                    .send(UiEvent::Overlay(Some(format!("PIN {pin}"))))
                    .await
                    .ok();
                match pair_as_receiver(link, cfg.id.fingerprint, &pin).await {
                    Ok(()) => {
                        let mut store = cfg.paired.lock().unwrap();
                        store.add(fp, format!("sender-{}", hex_short(&fp)));
                        store.save()?;
                        tracing::info!("paired with {}", hex_short(&fp));
                    }
                    Err(e) => {
                        tracing::warn!("pairing failed: {e:#}");
                        let mut guard = cfg.pairing_guard.lock().unwrap();
                        guard.record_failure(Instant::now());
                        if guard.is_locked(Instant::now()) {
                            tracing::warn!(
                                "{PAIRING_MAX_FAILURES} failed PIN attempts; pairing locked for {}s",
                                PAIRING_LOCKOUT.as_secs()
                            );
                        }
                        return Ok(());
                    }
                }
            }
            hello @ ControlMessage::Hello { .. } => {
                let was_disconnected =
                    matches!(session.state(), ReceiverState::Disconnected { .. });
                for a in session.on_message(hello, now_us(cfg.start)) {
                    match a {
                        Action::Send(m) => link.send_control(&m).await?,
                        Action::Resumed => tracing::info!("session resumed"),
                        Action::Fail(why) => return Err(anyhow!(why)),
                    }
                }
                if matches!(session.state(), ReceiverState::Streaming { .. }) {
                    if was_disconnected {
                        tracing::info!("resuming stream");
                    }
                    break;
                }
            }
            other => tracing::debug!("ignoring {other:?} before streaming"),
        }
    }
    cfg.ui.send(UiEvent::Overlay(None)).await.ok();
    stream(cfg, link, session).await
}

async fn stream(cfg: &NetConfig, link: &Link, session: &mut ReceiverSession) -> anyhow::Result<()> {
    // One reassembler per stream. A shared one lets an audio packet advance the
    // `newest_completed` watermark past a video frame that is still missing
    // fragments, after which the video frame's remaining fragments are dropped
    // as "old" and the frame never completes.
    let mut video_reasm = Reassembler::new(500_000);
    let mut audio_reasm = Reassembler::new(500_000);
    let mut nack_tx = link.open_nack_stream().await?;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut stats_tick = tokio::time::interval(Duration::from_millis(100));
    let mut stall_check = tokio::time::interval(Duration::from_millis(250));
    let mut last_video = Instant::now();
    let mut frames_received = 0u32;
    let mut fragments_received = 0u32;
    let mut last_audio_ts: Option<u64> = None;
    if let Some(p) = session.params() {
        cfg.jitter.lock().unwrap().set_mode(p.mode);
        cfg.ui.send(UiEvent::Mode(p.mode)).await.ok();
    }
    loop {
        tokio::select! {
            d = link.recv_datagram() => {
                let d = d?;
                fragments_received += 1;
                // Stream id is byte 0 of the datagram header (spec 6.2).
                let is_video = d.first() == Some(&STREAM_VIDEO);
                let reasm = if is_video { &mut video_reasm } else { &mut audio_reasm };
                if let Some(f) = reasm.push(&d, now_us(cfg.start))? {
                    match f.stream {
                        STREAM_VIDEO => {
                            frames_received += 1;
                            last_video = Instant::now();
                            cfg.jitter.lock().unwrap().push(f, now_us(cfg.start));
                        }
                        _ => {
                            if let Some(prev) = last_audio_ts {
                                let gap = f.timestamp_us.saturating_sub(prev);
                                if (15_000..200_000).contains(&gap) {
                                    // Lost packets: one PLC frame per missing 10 ms, capped.
                                    let missing = ((gap - 10_000) / 10_000).min(5);
                                    for _ in 0..missing { cfg.ui.send(UiEvent::AudioPacket { ts_us: prev, data: Vec::new() }).await.ok(); }
                                }
                            }
                            last_audio_ts = Some(f.timestamp_us);
                            cfg.ui.send(UiEvent::AudioPacket { ts_us: f.timestamp_us, data: f.data }).await.ok();
                        }
                    }
                }
            }
            m = link.recv_control() => {
                let m = m?;
                match &m {
                    ControlMessage::StartStream(p) => {
                        tracing::info!("stream {}x{}@{} {:?} {} bps", p.width, p.height, p.fps, p.mode, p.bitrate_bps);
                        {
                            let mut j = cfg.jitter.lock().unwrap();
                            *j = JitterBuffer::new(p.mode, 1_000_000 / p.fps.max(1) as u64);
                        }
                        cfg.ui.send(UiEvent::Mode(p.mode)).await.ok();
                    }
                    ControlMessage::SetMode(mode) => {
                        cfg.jitter.lock().unwrap().set_mode(*mode);
                        cfg.ui.send(UiEvent::Mode(*mode)).await.ok();
                    }
                    ControlMessage::Goodbye { reason } => tracing::info!("goodbye: {reason}"),
                    ControlMessage::Error { code, message } => tracing::warn!("peer error {code}: {message}"),
                    _ => {}
                }
                let goodbye = matches!(&m, ControlMessage::Goodbye { .. });
                for a in session.on_message(m, now_us(cfg.start)) {
                    if let Action::Send(r) = a { link.send_control(&r).await?; }
                }
                if goodbye { return Ok(()); }
            }
            _ = tick.tick() => {
                // Audio frames are never fragmented, so its reassembler is only
                // ticked to expire partials; its NACKs are meaningless.
                let _ = audio_reasm.tick(now_us(cfg.start));
                for n in video_reasm.tick(now_us(cfg.start)) { nack_tx.send(&n).await?; }
            }
            _ = stats_tick.tick() => {
                let dropped = cfg.jitter.lock().unwrap().dropped();
                let depth = cfg.stats.lock().unwrap().decode_queue_depth;
                let s = Stats { frames_received, frames_dropped: dropped, fragments_lost: (video_reasm.fragments_lost() + audio_reasm.fragments_lost()) as u32, fragments_received, decode_queue_depth: depth, interval_ms: 100 };
                frames_received = 0; fragments_received = 0;
                link.send_control(&ControlMessage::Stats(s)).await?;
            }
            _ = stall_check.tick() => {
                if last_video.elapsed() > Duration::from_secs(1) {
                    link.send_control(&ControlMessage::RequestKeyframe).await?;
                    last_video = Instant::now();
                }
            }
            _ = link.closed() => return Err(anyhow!("connection lost")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_streaming_marks_disconnected() {
        assert!(should_mark_disconnected(&ReceiverState::Streaming {
            params: None
        }));
        assert!(!should_mark_disconnected(&ReceiverState::AwaitingHello));
        assert!(!should_mark_disconnected(&ReceiverState::Disconnected {
            since_us: 0
        }));
        assert!(!should_mark_disconnected(&ReceiverState::Closed));
    }

    #[test]
    fn three_failures_lock_pairing_and_the_lock_expires() {
        let t0 = Instant::now();
        let mut g = PairingGuard::new();
        assert!(!g.is_locked(t0));
        g.record_failure(t0);
        g.record_failure(t0 + Duration::from_secs(1));
        assert!(!g.is_locked(t0 + Duration::from_secs(1)));
        g.record_failure(t0 + Duration::from_secs(2));
        assert!(g.is_locked(t0 + Duration::from_secs(2)));
        assert!(g.is_locked(t0 + Duration::from_secs(61)));
        assert!(!g.is_locked(t0 + Duration::from_secs(63)));
    }

    #[test]
    fn failures_spread_beyond_the_window_do_not_lock() {
        let t0 = Instant::now();
        let mut g = PairingGuard::new();
        g.record_failure(t0);
        g.record_failure(t0 + Duration::from_secs(70));
        g.record_failure(t0 + Duration::from_secs(140));
        assert!(!g.is_locked(t0 + Duration::from_secs(140)));
    }
}
