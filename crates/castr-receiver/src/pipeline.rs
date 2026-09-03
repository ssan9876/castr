use crate::audio_out::AudioOut;
use crate::display;
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DecoderChoice {
    Auto,
    Mf,
    V4l2,
    Sw,
}

/// Whether to accept Miracast sources alongside castr's own protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum MiracastChoice {
    /// Always run the sink; report loudly if the radio is unavailable.
    On,
    /// Never run it.
    Off,
    /// Run it when a wireless interface exists, logging the reason if not.
    Auto,
}

/// Counts decoder errors and trips when `limit` occur within `within`.
pub struct ErrorWindow {
    limit: usize,
    within: Duration,
    times: std::collections::VecDeque<Instant>,
}

impl ErrorWindow {
    pub fn new(limit: usize, within: Duration) -> Self {
        Self {
            limit,
            within,
            times: std::collections::VecDeque::new(),
        }
    }
    /// Records an error at `now`; true when the window has tripped (and resets).
    pub fn record(&mut self, now: Instant) -> bool {
        while self
            .times
            .front()
            .is_some_and(|&t| now.duration_since(t) > self.within)
        {
            self.times.pop_front();
        }
        self.times.push_back(now);
        if self.times.len() >= self.limit {
            self.times.clear();
            true
        } else {
            false
        }
    }
}

/// Decode/present timing shared between the decode thread and the SDL loop,
/// reported every few seconds (spec section 5). `calls` counts every
/// `decode` invocation (the denominator for decode avg/max); `pictures`
/// counts every decoded frame actually produced, whether returned directly
/// by `decode` or fetched afterwards via `poll_frame` ("drained"). On the
/// Pi most 1080p pictures arrive through `poll_frame`, and each drain can
/// cost real time (a buffer copy), so it is timed separately from `decode`.
#[derive(Default)]
pub struct PerfStats {
    calls: u32,
    decode_total: Duration,
    decode_max: Duration,
    pictures: u32,
    drains: u32,
    drain_total: Duration,
    drain_max: Duration,
    presented: u32,
    present_total: Duration,
    present_max: Duration,
}

impl PerfStats {
    /// Records one `decode` call taking `d`. `picture` is true when it
    /// returned a decoded frame directly (as opposed to it showing up later
    /// via `poll_frame`).
    pub fn decode(&mut self, d: Duration, picture: bool) {
        self.calls += 1;
        self.decode_total += d;
        self.decode_max = self.decode_max.max(d);
        if picture {
            self.pictures += 1;
        }
    }
    /// Records one picture fetched via `poll_frame`, which took `d`.
    pub fn drained(&mut self, d: Duration) {
        self.pictures += 1;
        self.drains += 1;
        self.drain_total += d;
        self.drain_max = self.drain_max.max(d);
    }
    pub fn present(&mut self, d: Duration) {
        self.presented += 1;
        self.present_total += d;
        self.present_max = self.present_max.max(d);
    }
    fn avg(total: Duration, n: u32) -> f64 {
        if n == 0 {
            0.0
        } else {
            total.as_secs_f64() * 1000.0 / n as f64
        }
    }
    /// One log line, then reset.
    pub fn take_report(&mut self, queue_depth: u32, dropped: u32) -> String {
        let s = format!(
            "perf: pictures {} (decode calls {} avg {:.1} ms max {:.1} ms, drain avg {:.1} ms max {:.1} ms), presented {} present avg {:.1} ms max {:.1} ms, queue {}, dropped {}",
            self.pictures,
            self.calls,
            Self::avg(self.decode_total, self.calls),
            self.decode_max.as_secs_f64() * 1000.0,
            Self::avg(self.drain_total, self.drains),
            self.drain_max.as_secs_f64() * 1000.0,
            self.presented,
            Self::avg(self.present_total, self.presented),
            self.present_max.as_secs_f64() * 1000.0,
            queue_depth,
            dropped
        );
        *self = Self::default();
        s
    }
}

/// The SDL loop's one-frame lookahead slot. In game mode the decoder can
/// produce pictures much faster than the audio clock allows them to be
/// shown (measured on the Pi: 151 decoded per 5 s, 9 presented), and the
/// naive "overwrite `pending` with whatever just arrived" policy discards
/// every frame that arrives before its predecessor became due - which is
/// almost all of them, since a newer frame's mere existence says nothing
/// about whether the older one was ever shown. A frame that already has a
/// successor waiting is by definition the best frame available right now,
/// so displacing it should present it immediately rather than drop it; the
/// audio clock then only has to gate the newest frame. Quality mode, whose
/// pictures arrive close to playout rate, is unaffected: displacement there
/// is rare, so this changes essentially nothing for it.
#[derive(Default)]
struct PendingFrame(Option<RawFrame>);

impl PendingFrame {
    /// Puts `f` in the slot, returning whatever was already there so the
    /// caller can present it right away instead of losing it.
    fn offer(&mut self, f: RawFrame) -> Option<RawFrame> {
        self.0.replace(f)
    }

    /// Takes the held frame if `clock` says it is due, leaving it in place
    /// otherwise.
    fn take_if_due(&mut self, clock: &mut AvClock, now_us: u64) -> Option<RawFrame> {
        if self
            .0
            .as_ref()
            .is_some_and(|f| clock.video_due(f.timestamp_us, now_us))
        {
            self.0.take()
        } else {
            None
        }
    }
}

/// Presents `f` and does the bookkeeping the SDL loop needs whichever path
/// produced it (a frame due per the audio clock, or one displaced from
/// `pending` by a newer arrival): times the call into `perf`, and updates
/// `last_video`/`last_present`, restarting the perf-report window on the
/// idle -> streaming transition exactly as the other call site does. Kept as
/// one function so the two present sites cannot drift apart.
fn present_and_record(
    renderer: &mut Renderer,
    perf: &Mutex<PerfStats>,
    f: &RawFrame,
    last_video: &mut Instant,
    last_present: &mut Instant,
    streaming_seen: &mut bool,
    last_perf: &mut Instant,
) -> anyhow::Result<()> {
    let t = Instant::now();
    renderer.present(f)?;
    perf.lock().unwrap().present(t.elapsed());
    *last_video = Instant::now();
    *last_present = Instant::now();
    if !*streaming_seen {
        // Resuming after idle: start the report window fresh so the first
        // report doesn't fire immediately over a mostly-idle partial window.
        *streaming_seen = true;
        *last_perf = Instant::now();
    }
    Ok(())
}

pub struct ReceiverConfig {
    pub name: String,
    pub fullscreen: bool,
    pub max_bitrate: u32,
    pub decoder: DecoderChoice,
    pub bind: SocketAddr,
    pub config_dir: PathBuf,
    // Read by the sink lifecycle, which lands in the next task.
    #[allow(dead_code)]
    pub miracast: MiracastChoice,
    /// Name shown in the Windows cast list; defaults to the hostname.
    #[allow(dead_code)]
    pub miracast_name: Option<String>,
    /// 2.4 GHz channel for the Wi-Fi Direct group; `None` picks one.
    #[allow(dead_code)]
    pub miracast_channel: Option<u32>,
}

/// Messages from the network side to the SDL main thread.
pub enum UiEvent {
    Overlay(Option<String>),
    Frame(RawFrame),
    AudioPacket { ts_us: u64, data: Vec<u8> },
    /// Ready-to-play samples. Miracast carries uncompressed audio, so it
    /// bypasses the Opus decoder the castr path uses.
    AudioPcm(Vec<i16>),
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
        if choice == DecoderChoice::V4l2 {
            anyhow::bail!("V4L2 decode is Linux only");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if matches!(choice, DecoderChoice::Auto | DecoderChoice::V4l2) {
            match castr_codec_v4l2::V4l2Decoder::open() {
                Ok(d) => return Ok(Box::new(d)),
                Err(e) if choice == DecoderChoice::V4l2 => return Err(e),
                Err(e) => {
                    tracing::warn!("V4L2 decoder unavailable, falling back to openh264: {e:#}")
                }
            }
        }
        if choice == DecoderChoice::Mf {
            anyhow::bail!("Media Foundation is Windows only");
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        if matches!(choice, DecoderChoice::Mf | DecoderChoice::V4l2) {
            anyhow::bail!("{choice:?} is not available on this platform");
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
    let perf = Arc::new(Mutex::new(PerfStats::default()));
    let dropped_since_report = Arc::new(std::sync::atomic::AtomicU32::new(0));
    // One screen, two protocols: the arbiter decides who has it. Shared with
    // the Miracast sink when that is running.
    let display = Arc::new(display::DisplayArbiter::new());
    // Microseconds-since-`start` of the last `decode` call, so the SDL loop's
    // idle detection (below) can key off decoding as well as presenting: a
    // receiver whose decoder is running but whose presenter is stuck (e.g.
    // starved by a broken audio clock) should not go quiet in the log right
    // when the numbers would matter most. A plain atomic, not a `Mutex`,
    // because the decode thread's hot path already pays for one `decode`
    // call per frame and this must not add a lock to it.
    let last_decode_us = Arc::new(AtomicU64::new(0));

    // The Miracast sink, when this build and this machine can run one. It
    // feeds the same jitter buffer and the same decoder as the castr path;
    // the arbiter guarantees only one of them is feeding at a time.
    #[cfg(target_os = "linux")]
    let miracast = start_miracast(&cfg, display.clone(), jitter.clone(), ui_tx.clone(), start);

    // Decode thread: jitter buffer -> decoder -> UI.
    {
        let jitter = jitter.clone();
        let ui = ui_tx.clone();
        let stats = stats.clone();
        let perf = perf.clone();
        let last_decode_us = last_decode_us.clone();
        let choice = cfg.decoder;
        #[cfg(target_os = "linux")]
        let miracast = miracast.clone();
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
                let mut last_decoded: Option<u32> = None;
                let mut errors = ErrorWindow::new(3, Duration::from_secs(10));
                let mut choice = choice;
                loop {
                    let frame = jitter.lock().unwrap().pop(now_us(start));
                    let Some(f) = frame else {
                        let t = Instant::now();
                        match decoder.poll_frame() {
                            Ok(Some(raw)) => {
                                perf.lock().unwrap().drained(t.elapsed());
                                if ui.blocking_send(UiEvent::Frame(raw)).is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {}
                            Err(e) => tracing::warn!("poll_frame error: {e:#}"),
                        }
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stats.lock().unwrap().decode_queue_depth =
                        jitter.lock().unwrap().depth() as u32;
                    tracing::debug!("decode frame {} key={}", f.frame_number, f.keyframe);
                    let t = Instant::now();
                    let result = decoder.decode(&f.data, f.timestamp_us);
                    last_decode_us.store(now_us(start), Ordering::Relaxed);
                    perf.lock()
                        .unwrap()
                        .decode(t.elapsed(), matches!(result, Ok(Some(_))));
                    match result {
                        Ok(Some(raw)) => {
                            last_decoded = Some(f.frame_number);
                            if ui.blocking_send(UiEvent::Frame(raw)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(
                                "decode error on frame {} (keyframe={}, last decoded {:?}): {e:#}",
                                f.frame_number,
                                f.keyframe,
                                last_decoded
                            );
                            // Deltas are useless without a reference: skip
                            // them until a fresh IDR arrives. The castr path
                            // asks for one over its control channel; the
                            // Miracast path asks over RTSP, and the sink
                            // rate-limits the request.
                            #[cfg(target_os = "linux")]
                            if let Some(s) = &miracast {
                                s.note_decode_error();
                            }
                            jitter.lock().unwrap().require_keyframe();
                            if errors.record(Instant::now()) {
                                // Three failures in ten seconds: rebuild, then
                                // fall back to software for the session (spec 3.1).
                                tracing::warn!("rebuilding decoder after repeated errors");
                                // Drop the failing decoder (e.g. its /dev/video10
                                // fd and MMAP buffers on the Pi) before opening a
                                // replacement, or the driver may refuse a second
                                // concurrent open and the rebuild fails spuriously.
                                drop(decoder);
                                decoder = match open_decoder(choice) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        tracing::error!("decoder rebuild failed ({e:#}); using openh264 for the rest of the session");
                                        choice = DecoderChoice::Sw;
                                        match open_decoder(choice) {
                                            Ok(d) => d,
                                            Err(e) => {
                                                tracing::error!("software decoder failed too: {e:#}");
                                                let _ = ui.blocking_send(UiEvent::Quit);
                                                return;
                                            }
                                        }
                                    }
                                };
                                tracing::info!("decoder: {}", decoder.name());
                            }
                            continue;
                        }
                    }
                    loop {
                        let t = Instant::now();
                        match decoder.poll_frame() {
                            Ok(Some(raw)) => {
                                perf.lock().unwrap().drained(t.elapsed());
                                if ui.blocking_send(UiEvent::Frame(raw)).is_err() {
                                    return;
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                tracing::warn!("poll_frame error: {e:#}");
                                break;
                            }
                        }
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
        display: display.clone(),
        dropped: dropped_since_report.clone(),
    };
    rt.spawn(async move {
        if let Err(e) = network_main(net_cfg).await {
            tracing::error!("network: {e:#}");
        }
    });

    // SDL main loop.
    let mut pending = PendingFrame::default();
    let mut last_video = Instant::now();
    // Newest video timestamp seen (presented or pending). Audio that lags far
    // behind it must not drive the master clock, or a sender with a broken
    // audio clock holds every video frame back forever.
    let mut latest_video_ts: Option<u64> = None;
    let mut warned_audio_lag = false;
    let mut last_perf = Instant::now();
    // Time of the last actual `present`, separate from `last_video` (which the
    // redraw branch below also touches every ~50 ms even while idle, so it
    // can never be used to detect idleness).
    let mut last_present = Instant::now();
    let mut streaming_seen = false;
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
                    // A frame that displaces a not-yet-due `pending` already
                    // has a successor, so it is the best frame to show now -
                    // present it immediately rather than let it be silently
                    // dropped (spec fix: game mode was presenting 9 of 151
                    // pictures per 5 s window this way).
                    if let Some(displaced) = pending.offer(f) {
                        present_and_record(
                            &mut renderer,
                            &perf,
                            &displaced,
                            &mut last_video,
                            &mut last_present,
                            &mut streaming_seen,
                            &mut last_perf,
                        )?;
                    }
                }
                UiEvent::AudioPcm(samples) => {
                    let ratio = clock.drift_ratio(audio.buffered_us(), audio.target_us);
                    if let Err(e) = audio.push_pcm(&samples, ratio) {
                        tracing::warn!("miracast audio: {e:#}");
                    }
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
        if let Some(f) = pending.take_if_due(&mut clock, now_us(start)) {
            present_and_record(
                &mut renderer,
                &perf,
                &f,
                &mut last_video,
                &mut last_present,
                &mut streaming_seen,
                &mut last_perf,
            )?;
        }
        if streaming_seen && last_perf.elapsed() >= Duration::from_secs(5) {
            last_perf = Instant::now();
            let depth = jitter.lock().unwrap().depth() as u32;
            let dropped = dropped_since_report.swap(0, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("{}", perf.lock().unwrap().take_report(depth, dropped));
            // Idle means neither presenting nor decoding recently: a
            // receiver that decodes but cannot present (e.g. starved by a
            // stuck audio clock) must not go quiet in the log exactly when
            // that gap matters most.
            let decode_idle = now_us(start).saturating_sub(last_decode_us.load(Ordering::Relaxed))
                > Duration::from_secs(5).as_micros() as u64;
            if last_present.elapsed() > Duration::from_secs(5) && decode_idle {
                streaming_seen = false; // idle: stop reporting until video flows again
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
    /// Which protocol owns the screen; shared with the Miracast sink.
    display: Arc<display::DisplayArbiter>,
    dropped: Arc<std::sync::atomic::AtomicU32>,
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
        // Every exit from the handler frees the screen, including the error
        // paths; a release from a protocol that does not hold it is a no-op,
        // so a refused connection cannot free the owner's display.
        cfg.display.release(display::Owner::Castr);
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
                    // One display, two protocols: whoever is already casting
                    // keeps it. Refuse rather than steal the screen from a
                    // guest mid-presentation.
                    if !cfg.display.try_acquire(display::Owner::Castr) {
                        tracing::info!(
                            "refusing castr sender: display owned by {:?}",
                            cfg.display.owner()
                        );
                        link.send_control(&ControlMessage::Error {
                            code: 5,
                            message: "display busy".into(),
                        })
                        .await?;
                        link.close("display busy");
                        return Ok(());
                    }
                    if was_disconnected {
                        tracing::info!("resuming stream");
                    }
                    break;
                }
            }
            other => tracing::debug!("ignoring {other:?} before streaming"),
        }
    }
    // The overlay is cleared once real video parameters are known (a
    // resumed session's cached params, or a fresh `StartStream`, both
    // handled inside `stream` below) - not merely because the control
    // handshake reached `Streaming`, which a bare capability probe (Hello,
    // then an immediate Goodbye with no video ever sent, as the GUI's
    // "already paired?" check does) also does. Clearing on Hello alone let a
    // probe's own teardown re-arm the "Waiting for sender" overlay after
    // this connection's clear had already been sent, racing the real cast on
    // the very same channel.
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
    // Last time a NACK went out for each frame number, for rate limiting.
    let mut last_nack: std::collections::HashMap<u32, Instant> = std::collections::HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut stats_tick = tokio::time::interval(Duration::from_millis(100));
    let mut stall_check = tokio::time::interval(Duration::from_millis(250));
    let mut last_video = Instant::now();
    let mut last_key_req = Instant::now() - Duration::from_secs(1);
    let mut any_video = false;
    let mut frames_received = 0u32;
    let mut fragments_received = 0u32;
    let mut last_audio_ts: Option<u64> = None;
    if let Some(p) = session.params() {
        // A resumed session already knows its stream parameters (a fresh
        // `StartStream` normally follows anyway, but there is no need to
        // wait for it to clear the overlay: the resume itself is already
        // proof real video was flowing on this session).
        cfg.jitter.lock().unwrap().set_mode(p.mode);
        cfg.ui.send(UiEvent::Mode(p.mode)).await.ok();
        cfg.ui.send(UiEvent::Overlay(None)).await.ok();
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
                            tracing::debug!("video frame {} key={} {} B", f.frame_number, f.keyframe, f.data.len());
                            frames_received += 1;
                            any_video = true;
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
                        // `StartStream` is the strongest evidence real video
                        // is about to flow (a bare Hello also reaches
                        // `Streaming` for a capability probe that never
                        // sends this), so clearing the overlay here, rather
                        // than on Hello, cannot race a probe's own teardown.
                        cfg.ui.send(UiEvent::Overlay(None)).await.ok();
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
                let nacks = video_reasm.tick(now_us(cfg.start));
                // Re-NACKing the same frame every 20 ms floods the sender with
                // requests for retransmits still in flight. Wait at least one
                // RTT (floor 20 ms) before asking for the same frame again.
                let now = Instant::now();
                let min_gap = link.rtt().max(Duration::from_millis(20));
                let mut still_pending = std::collections::HashSet::new();
                for n in &nacks {
                    still_pending.insert(n.frame_number);
                    let due = last_nack.get(&n.frame_number).is_none_or(|t| now.duration_since(*t) >= min_gap);
                    if due {
                        last_nack.insert(n.frame_number, now);
                        nack_tx.send(n).await?;
                    }
                }
                // Frames that no longer appear have completed or expired.
                last_nack.retain(|f, t| {
                    still_pending.contains(f) && now.duration_since(*t) < Duration::from_secs(1)
                });
            }
            _ = stats_tick.tick() => {
                let dropped = cfg.jitter.lock().unwrap().dropped();
                cfg.dropped.fetch_add(dropped, Ordering::Relaxed);
                let depth = cfg.stats.lock().unwrap().decode_queue_depth;
                let s = Stats { frames_received, frames_dropped: dropped, fragments_lost: (video_reasm.fragments_lost() + audio_reasm.fragments_lost()) as u32, fragments_received, decode_queue_depth: depth, interval_ms: 100 };
                frames_received = 0; fragments_received = 0;
                if s.fragments_lost > 0 || s.frames_dropped > 0 {
                    tracing::debug!("stats: {} fragments lost, {} frames dropped, queue {}", s.fragments_lost, s.frames_dropped, s.decode_queue_depth);
                }
                link.send_control(&ControlMessage::Stats(s)).await?;
            }
            _ = stall_check.tick() => {
                // One RequestKeyframe per 500 ms at most, for a stalled stream or a
                // jitter buffer waiting on a keyframe (decode error, or too far
                // behind). The buffer clears its flag when the keyframe arrives, so
                // the request repeats until it does.
                if last_key_req.elapsed() >= Duration::from_millis(500) {
                    let stalled = last_video.elapsed() > Duration::from_secs(1);
                    // Before the first frame the buffer wants a keyframe too, but the
                    // sender opens with one; only ask once video has actually flowed.
                    let lost_ref = any_video && cfg.jitter.lock().unwrap().keyframe_needed();
                    if stalled || lost_ref {
                        tracing::info!("requesting keyframe ({})", if stalled { "stall" } else { "reference lost" });
                        link.send_control(&ControlMessage::RequestKeyframe).await?;
                        last_key_req = Instant::now();
                        if stalled {
                            last_video = Instant::now();
                        }
                    }
                }
            }
            _ = link.closed() => return Err(anyhow!("connection lost")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ts: u64) -> RawFrame {
        RawFrame {
            format: castr_media::PixelFormat::Nv12,
            width: 2,
            height: 2,
            stride: 2,
            data: vec![0u8; 6],
            timestamp_us: ts,
        }
    }

    #[test]
    fn offering_into_an_empty_slot_returns_none() {
        let mut p = PendingFrame::default();
        assert!(p.offer(frame(1)).is_none());
    }

    #[test]
    fn offering_into_an_occupied_slot_returns_the_older_frame_and_keeps_the_newer() {
        let mut p = PendingFrame::default();
        assert!(p.offer(frame(1)).is_none());
        let displaced = p
            .offer(frame(2))
            .expect("the first frame is displaced, not dropped");
        assert_eq!(displaced.timestamp_us, 1);
        // The newer frame is retained in the slot.
        let mut clock = AvClock::new();
        let kept = p
            .take_if_due(&mut clock, 0)
            .expect("the newer frame is still pending");
        assert_eq!(kept.timestamp_us, 2);
    }

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

    #[test]
    fn error_window_trips_on_the_third_error_within_ten_seconds() {
        let t0 = Instant::now();
        let mut w = ErrorWindow::new(3, Duration::from_secs(10));
        assert!(!w.record(t0));
        assert!(!w.record(t0 + Duration::from_secs(4)));
        assert!(w.record(t0 + Duration::from_secs(9)));
        // Tripping clears the window.
        assert!(!w.record(t0 + Duration::from_secs(9)));
    }

    #[test]
    fn error_window_forgets_old_errors() {
        let t0 = Instant::now();
        let mut w = ErrorWindow::new(3, Duration::from_secs(10));
        w.record(t0);
        w.record(t0 + Duration::from_secs(1));
        assert!(!w.record(t0 + Duration::from_secs(12)));
    }

    #[test]
    fn perf_stats_report_averages_and_maxima() {
        let mut p = PerfStats::default();
        p.decode(Duration::from_millis(10), false);
        p.decode(Duration::from_millis(30), true);
        p.drained(Duration::from_millis(20));
        p.present(Duration::from_millis(5));
        let s = p.take_report(2, 1);
        assert!(s.contains("pictures 2"), "{s}");
        assert!(s.contains("decode calls 2 avg 20.0 ms max 30.0 ms"), "{s}");
        assert!(s.contains("drain avg 20.0 ms max 20.0 ms"), "{s}");
        assert!(
            s.contains("presented 1 present avg 5.0 ms max 5.0 ms"),
            "{s}"
        );
        assert!(s.contains("queue 2"), "{s}");
        assert!(s.contains("dropped 1"), "{s}");
        // Taking resets the counters.
        assert!(p.take_report(0, 0).contains("pictures 0"));
    }
}

/// Starts the Miracast sink and the thread that drains its events into the
/// pipeline. `None` when this machine has no wireless interface and the choice
/// was `Auto`, or when the sink cannot start at all.
#[cfg(target_os = "linux")]
fn start_miracast(
    cfg: &ReceiverConfig,
    display: Arc<display::DisplayArbiter>,
    jitter: Arc<Mutex<JitterBuffer>>,
    ui: mpsc::Sender<UiEvent>,
    start: Instant,
) -> Option<Arc<castr_miracast::sink::Sink>> {
    use castr_miracast::sink::{Sink, SinkConfig, SinkOut};

    match cfg.miracast {
        MiracastChoice::Off => return None,
        MiracastChoice::Auto => {
            let iface = std::path::Path::new("/sys/class/net").join(castr_miracast::sink::WLAN);
            if !iface.exists() {
                tracing::info!(
                    "miracast: not starting, {} does not exist (pass --miracast on to force it)",
                    iface.display()
                );
                return None;
            }
        }
        MiracastChoice::On => {}
    }

    struct Arbiter(Arc<display::DisplayArbiter>);
    impl castr_miracast::sink::DisplayArbiterHandle for Arbiter {
        fn try_acquire(&self) -> bool {
            self.0.try_acquire(display::Owner::Miracast)
        }
        fn release(&self) {
            self.0.release(display::Owner::Miracast);
        }
    }

    let sink_cfg = SinkConfig {
        name: cfg.miracast_name.clone().unwrap_or_else(|| cfg.name.clone()),
        channel: cfg.miracast_channel,
        paired_path: cfg.config_dir.join("miracast-peers.txt"),
        ..SinkConfig::default()
    };
    let sink = match Sink::start(sink_cfg, Arc::new(Arbiter(display))) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!("miracast: sink did not start: {e:#}");
            return None;
        }
    };
    let Some(events) = sink.events() else {
        return Some(sink);
    };

    std::thread::Builder::new()
        .name("miracast-events".into())
        .spawn(move || {
            // The jitter buffer orders by frame number, which Miracast does not
            // carry, so we number the access units as they arrive. RTP
            // reordering has already happened inside the sink.
            let mut frame_number: u32 = 0;
            while let Ok(ev) = events.recv() {
                match ev {
                    SinkOut::Pin(pin) => {
                        let _ = ui.blocking_send(UiEvent::Overlay(Some(format!(
                            "Miracast PIN: {pin}"
                        ))));
                    }
                    SinkOut::Started => {
                        let _ = ui.blocking_send(UiEvent::Overlay(None));
                    }
                    SinkOut::Video { data, pts_us } => {
                        let keyframe = has_keyframe(&data);
                        let frame = CompleteFrame {
                            stream: 0,
                            frame_number,
                            timestamp_us: pts_us.unwrap_or_else(|| now_us(start)),
                            keyframe,
                            data,
                        };
                        frame_number = frame_number.wrapping_add(1);
                        jitter.lock().unwrap().push(frame, now_us(start));
                    }
                    SinkOut::Audio { data, .. } => {
                        // LPCM, 16-bit big-endian stereo at 48 kHz, which is
                        // the only audio format we offer the source.
                        let samples: Vec<i16> = data
                            .chunks_exact(2)
                            .map(|b| i16::from_be_bytes([b[0], b[1]]))
                            .collect();
                        let _ = ui.blocking_send(UiEvent::AudioPcm(samples));
                    }
                    SinkOut::Ended(reason) => {
                        tracing::info!("miracast: {reason}");
                        let _ = ui.blocking_send(UiEvent::Overlay(None));
                        jitter.lock().unwrap().flush();
                    }
                }
            }
        })
        .ok()?;
    Some(sink)
}

/// True when an Annex B access unit contains an IDR slice or a parameter set,
/// which is what the decoder needs to start or to recover.
#[cfg(target_os = "linux")]
fn has_keyframe(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let nal = data[i + 3] & 0x1f;
            if nal == 5 || nal == 7 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}
