//! The sink's lifecycle: start the supplicant, own a Wi-Fi Direct group,
//! address the peer, take the RTSP connection, and hand decoded units to the
//! caller. Everything above this file is pure and tested without a radio; this
//! is the layer that touches the hardware, so it logs every command it sends
//! and every reply it gets.
//!
//! One thread runs the whole lifecycle. It blocks in `poll` on the supplicant
//! control socket, the DHCP socket, the RTSP listener, the RTSP connection and
//! the RTP socket at once, so an idle sink costs nothing and a busy one wakes
//! only when there is work.

use crate::dhcp;
use crate::p2p::{Command, Control, Event};
use crate::session::{Session, SinkEvent};
use crate::wfd::{AudioCodecs, Capabilities, ClientPorts, DeviceInfo, VideoFormats};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Where the supplicant we start puts its control socket. Its own instance,
/// not the system one, so a station connection on `wlan0` is untouched.
pub const CTRL_DIR: &str = "/run/wpa_supplicant_castr";
/// The configuration `setup.sh` installs.
pub const CONF_PATH: &str = "/etc/castr/wpa_supplicant-p2p.conf";
/// The radio interface. The group owner appears as a second, virtual one.
pub const WLAN: &str = "wlan0";
/// The 2.4 GHz channels that do not overlap. The Pi's radio has no 5 GHz.
const CHANNELS: [u32; 3] = [1, 6, 11];
/// Used when a scan tells us nothing, which is the common case indoors.
const DEFAULT_CHANNEL: u32 = 6;
/// How long we wait for the group to come up before starting over.
const GROUP_TIMEOUT: Duration = Duration::from_secs(20);
/// The pause between a session ending and the next advertisement.
const RESTART_DELAY: Duration = Duration::from_secs(2);
/// The longest a poll sleeps, which is also how often the session ticks.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The receiver's display arbiter, seen from here. A trait so this crate does
/// not depend on the receiver binary.
pub trait DisplayArbiterHandle: Send + Sync {
    fn try_acquire(&self) -> bool;
    fn release(&self);
}

pub struct SinkConfig {
    /// Shown in the Windows cast list.
    pub name: String,
    /// `None` picks the least busy of channels 1, 6 and 11.
    pub channel: Option<u32>,
    pub rtsp_port: u16,
    pub rtp_port: u16,
    /// Where peers that have completed WPS are remembered.
    pub paired_path: PathBuf,
}

impl Default for SinkConfig {
    fn default() -> Self {
        Self {
            name: "castr".into(),
            channel: None,
            rtsp_port: 7236,
            rtp_port: 5000,
            paired_path: PathBuf::from("/var/lib/castr/miracast-peers.toml"),
        }
    }
}

/// What the sink hands back to the receiver.
#[derive(Debug)]
pub enum SinkOut {
    /// Show this eight-digit PIN on the television.
    Pin(String),
    Video {
        data: Vec<u8>,
        pts_us: Option<u64>,
    },
    Audio {
        data: Vec<u8>,
        pts_us: Option<u64>,
    },
    Started,
    Ended(String),
}

enum Cmd {
    DecodeError,
}

pub struct Sink {
    cmd: mpsc::Sender<Cmd>,
    /// Handed out once. The plan's `events(&self)` returns the receiver
    /// itself, which can only be given to one consumer, so this says so in
    /// the type rather than panicking on a second call.
    events: Mutex<Option<mpsc::Receiver<SinkOut>>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Sink {
    /// Starts the lifecycle thread. Returns as soon as the thread is running;
    /// the radio work happens there, and its progress arrives on `events`.
    pub fn start(cfg: SinkConfig, arbiter: Arc<dyn DisplayArbiterHandle>) -> anyhow::Result<Self> {
        let (out_tx, out_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let join = std::thread::Builder::new()
            .name("miracast-sink".into())
            .spawn(move || run(cfg, arbiter, out_tx, cmd_rx, stop_thread))?;
        Ok(Self {
            cmd: cmd_tx,
            events: Mutex::new(Some(out_rx)),
            stop,
            join: Some(join),
        })
    }

    /// The event stream. `None` after the first call.
    pub fn events(&self) -> Option<mpsc::Receiver<SinkOut>> {
        self.events.lock().unwrap().take()
    }

    /// The decoder lost its reference frame; ask the source for a keyframe.
    pub fn note_decode_error(&self) {
        let _ = self.cmd.send(Cmd::DecodeError);
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The outer loop: one pass is one group, from advertisement to teardown.
fn run(
    cfg: SinkConfig,
    arbiter: Arc<dyn DisplayArbiterHandle>,
    out: mpsc::Sender<SinkOut>,
    cmds: mpsc::Receiver<Cmd>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        match one_group(&cfg, &arbiter, &out, &cmds, &stop) {
            Ok(()) => tracing::info!("miracast: group ended, re-advertising"),
            Err(e) => {
                tracing::warn!("miracast: {e:#}");
                // A failure here is usually the radio being busy or the
                // supplicant still starting. Wait, then try the whole
                // sequence again rather than leaving the sink dead.
                sleep_unless_stopped(Duration::from_secs(5), &stop);
            }
        }
        sleep_unless_stopped(RESTART_DELAY, &stop);
    }
    tracing::info!("miracast: sink stopped");
}

fn one_group(
    cfg: &SinkConfig,
    arbiter: &Arc<dyn DisplayArbiterHandle>,
    out: &mpsc::Sender<SinkOut>,
    cmds: &mpsc::Receiver<Cmd>,
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    ensure_supplicant()?;
    let mut ctrl = Control::open(Path::new(CTRL_DIR), WLAN)?;
    ctrl.attach()?;
    say(&mut ctrl, &Command::device_name(&cfg.name))?;
    say(&mut ctrl, &Command::wifi_display_enable())?;
    let info = DeviceInfo {
        session_available: true,
        rtsp_port: cfg.rtsp_port,
        // 720p30 at our ceiling bitrate, rounded up: what we tell the source
        // we can take, not a promise about the air.
        max_throughput_mbps: 10,
    };
    say(&mut ctrl, &Command::subelement(0, &crate::wfd::device_info_subelement(&info)))?;
    let channel = cfg.channel.unwrap_or_else(|| pick_channel(&mut ctrl));
    let freq = channel_to_freq(channel);
    tracing::info!("miracast: advertising as {:?} on channel {channel}", cfg.name);
    say(&mut ctrl, &Command::group_add_persistent(freq))?;

    let iface = wait_for_group(&mut ctrl, stop)?;
    let _group = GroupGuard {
        iface: iface.clone(),
    };
    configure_interface(&iface)?;

    let server = dhcp::DEFAULT_LEASE.server;
    let dhcp_sock = open_dhcp_socket(&iface)?;
    let rtp = UdpSocket::bind(SocketAddrV4::new(server, cfg.rtp_port))?;
    rtp.set_nonblocking(true)?;
    let listener = TcpListener::bind(SocketAddrV4::new(server, cfg.rtsp_port))?;
    listener.set_nonblocking(true)?;
    tracing::info!(
        "miracast: group {iface} up, RTSP on {server}:{}, RTP on {server}:{}",
        cfg.rtsp_port,
        cfg.rtp_port
    );

    serve(cfg, arbiter, out, cmds, stop, &mut ctrl, &dhcp_sock, &rtp, &listener)
}

/// Removes the group when the pass ends, however it ends. Without this a
/// failure part-way through leaves a stale group interface that blocks the
/// next attempt.
struct GroupGuard {
    iface: String,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Ok(mut c) = Control::open(Path::new(CTRL_DIR), WLAN) {
            let _ = say(&mut c, &Command::group_remove(&self.iface));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn serve(
    cfg: &SinkConfig,
    arbiter: &Arc<dyn DisplayArbiterHandle>,
    out: &mpsc::Sender<SinkOut>,
    cmds: &mpsc::Receiver<Cmd>,
    stop: &Arc<AtomicBool>,
    ctrl: &mut Control,
    dhcp_sock: &UdpSocket,
    rtp: &UdpSocket,
    listener: &TcpListener,
) -> anyhow::Result<()> {
    let mut peers = PeerStore::load(&cfg.paired_path);
    let mut conn: Option<TcpStream> = None;
    let mut session: Option<Session> = None;
    let mut held = false;
    let mut buf = vec![0u8; 65536];

    loop {
        if stop.load(Ordering::SeqCst) {
            if held {
                arbiter.release();
            }
            return Ok(());
        }
        let fds = [
            ctrl.as_raw_fd(),
            dhcp_sock.as_raw_fd(),
            rtp.as_raw_fd(),
            listener.as_raw_fd(),
            conn.as_ref().map(|c| c.as_raw_fd()).unwrap_or(-1),
        ];
        poll_readable(&fds, POLL_INTERVAL);

        // Supplicant events: pairing, and the peer going away.
        while let Ok(Some(ev)) = ctrl.poll_event(Duration::from_millis(0)) {
            tracing::info!("miracast: event {ev:?}");
            match ev {
                Event::ProvisionRequest { peer } => {
                    // Provisioning only happens when the peer has no stored
                    // credentials, so a returning PC never reaches here: the
                    // persistent group lets it back in silently.
                    let pin = generate_pin();
                    tracing::info!(
                        "miracast: {peer} is enrolling{}; PIN {pin}",
                        if peers.contains(&peer) {
                            " again"
                        } else {
                            ""
                        }
                    );
                    let _ = out.send(SinkOut::Pin(pin.clone()));
                    say(ctrl, &Command::wps_pin(&pin))?;
                }
                Event::WpsSuccess => {}
                Event::WpsFail => {
                    let _ = out.send(SinkOut::Ended("pairing failed".into()));
                }
                Event::ClientConnected { mac } => {
                    peers.remember(&mac);
                }
                Event::ClientDisconnected { .. } | Event::GroupRemoved { .. } => {
                    if held {
                        arbiter.release();
                    }
                    let _ = out.send(SinkOut::Ended("peer disconnected".into()));
                    return Ok(());
                }
                Event::GroupStarted { .. } => {}
            }
        }

        // Address the peer. One lease, one client, no state to keep.
        while let Ok((n, from)) = dhcp_sock.recv_from(&mut buf) {
            let Some(req) = dhcp::parse(&buf[..n]) else {
                continue;
            };
            let Some(reply) = dhcp::reply(&req, &dhcp::DEFAULT_LEASE) else {
                continue;
            };
            tracing::info!("miracast: DHCP {:?} from {from}, offering the lease", req.kind);
            // The client has no address yet, so the reply goes to the
            // broadcast address on the group interface.
            let dest = SocketAddrV4::new(Ipv4Addr::BROADCAST, 68);
            if let Err(e) = dhcp_sock.send_to(&reply, dest) {
                tracing::warn!("miracast: DHCP reply: {e}");
            }
        }

        // One RTSP connection at a time; the display arbiter decides whether
        // we may take it at all.
        if conn.is_none() {
            match listener.accept() {
                Ok((s, from)) => {
                    if !arbiter.try_acquire() {
                        tracing::info!("miracast: refusing {from}: the display is busy");
                        drop(s);
                    } else {
                        tracing::info!("miracast: RTSP connection from {from}");
                        held = true;
                        s.set_nonblocking(true)?;
                        s.set_nodelay(true)?;
                        conn = Some(s);
                        session = Some(Session::new(capabilities(cfg), session_id()));
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => tracing::warn!("miracast: accept: {e}"),
            }
        }

        // RTSP bytes in, RTSP bytes out, media events out.
        let mut ended = None;
        if let (Some(s), Some(sess)) = (conn.as_mut(), session.as_mut()) {
            loop {
                match s.read(&mut buf) {
                    Ok(0) => {
                        ended = Some("source closed the connection".to_string());
                        break;
                    }
                    Ok(n) => {
                        for e in sess.on_rtsp_bytes(&buf[..n]) {
                            dispatch(e, s, out, &mut ended);
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => {
                        ended = Some(format!("RTSP read: {e}"));
                        break;
                    }
                }
            }
        }

        // Media. Drained fully each pass so a burst does not queue up.
        if let (Some(s), Some(sess)) = (conn.as_mut(), session.as_mut()) {
            while let Ok(n) = rtp.recv(&mut buf) {
                for e in sess.on_rtp_datagram(&buf[..n]) {
                    dispatch(e, s, out, &mut ended);
                }
            }
            while let Ok(Cmd::DecodeError) = cmds.try_recv() {
                for e in sess.note_decode_error(Instant::now()) {
                    dispatch(e, s, out, &mut ended);
                }
            }
            for e in sess.tick(Instant::now()) {
                dispatch(e, s, out, &mut ended);
            }
        } else {
            // Nobody is connected; drop any stale decode-error notices.
            while cmds.try_recv().is_ok() {}
        }

        if let Some(reason) = ended {
            tracing::info!("miracast: session ended: {reason}");
            let _ = out.send(SinkOut::Ended(reason));
            if held {
                arbiter.release();
            }
            // A new group for the next connection: the source expects a fresh
            // advertisement, and this clears any half-open radio state.
            return Ok(());
        }
    }
}

/// Turns one session event into bytes on the wire or an event for the caller.
fn dispatch(
    e: SinkEvent,
    s: &mut TcpStream,
    out: &mpsc::Sender<SinkOut>,
    ended: &mut Option<String>,
) {
    match e {
        SinkEvent::SendRtsp(text) => {
            tracing::debug!("miracast: > {}", text.lines().next().unwrap_or(""));
            if let Err(e) = s.write_all(text.as_bytes()) {
                *ended = Some(format!("RTSP write: {e}"));
            }
        }
        SinkEvent::Video { data, pts_us } => {
            let _ = out.send(SinkOut::Video { data, pts_us });
        }
        SinkEvent::Audio { data, pts_us } => {
            let _ = out.send(SinkOut::Audio { data, pts_us });
        }
        SinkEvent::Started(mode) => {
            tracing::info!("miracast: playing {}x{}@{}", mode.width, mode.height, mode.fps);
            let _ = out.send(SinkOut::Started);
        }
        SinkEvent::Ended(reason) => *ended = Some(reason.to_string()),
    }
}

fn capabilities(cfg: &SinkConfig) -> Capabilities {
    Capabilities {
        video: VideoFormats::only_720p30(),
        audio: AudioCodecs::lpcm_48k_stereo(),
        ports: ClientPorts {
            rtp_port: cfg.rtp_port,
        },
        // The 2.4 GHz radio cannot hold more than this over the air, and
        // asking for more only buys retransmissions.
        max_bitrate_kbps: 8000,
        latency_management: true,
        format_change: true,
    }
}

/// Starts our own supplicant instance if its control socket is not there.
fn ensure_supplicant() -> anyhow::Result<()> {
    let sock = Path::new(CTRL_DIR).join(WLAN);
    if sock.exists() {
        return Ok(());
    }
    tracing::info!("miracast: starting wpa_supplicant for {WLAN}");
    let _ = std::process::Command::new(tool("rfkill"))
        .args(["unblock", "wifi"])
        .status();
    let _ = std::process::Command::new(tool("ip"))
        .args(["link", "set", WLAN, "up"])
        .status();
    let status = std::process::Command::new(tool("wpa_supplicant"))
        .args(["-i", WLAN, "-c", CONF_PATH, "-B"])
        .status()?;
    if !status.success() {
        anyhow::bail!("wpa_supplicant exited with {status}");
    }
    // It daemonises before the socket appears.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if sock.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("wpa_supplicant started but {} never appeared", sock.display())
}

/// The service runs as an unprivileged user whose PATH may not carry the
/// system directories, so every external tool is resolved by absolute path.
fn tool(name: &str) -> String {
    for dir in ["/usr/sbin", "/sbin", "/usr/bin", "/bin"] {
        let p = format!("{dir}/{name}");
        if Path::new(&p).exists() {
            return p;
        }
    }
    name.to_string()
}

/// Sends a command, logs both halves, and fails on a reply that is not OK.
fn say(ctrl: &mut Control, cmd: &str) -> anyhow::Result<()> {
    let reply = ctrl.request(cmd)?;
    let reply = reply.trim();
    tracing::info!("miracast: {cmd} -> {reply}");
    if reply == "FAIL" || reply.starts_with("FAIL ") {
        anyhow::bail!("{cmd}: {reply}");
    }
    Ok(())
}

/// The least busy of the non-overlapping channels, from one scan. A scan that
/// tells us nothing is normal, and channel 6 is the safe default.
fn pick_channel(ctrl: &mut Control) -> u32 {
    if ctrl.request("SCAN").is_err() {
        return DEFAULT_CHANNEL;
    }
    std::thread::sleep(Duration::from_secs(4));
    let Ok(results) = ctrl.request("SCAN_RESULTS") else {
        return DEFAULT_CHANNEL;
    };
    let mut counts = [0u32; CHANNELS.len()];
    for line in results.lines().skip(1) {
        let Some(freq) = line.split('\t').nth(1).and_then(|f| f.trim().parse::<u32>().ok()) else {
            continue;
        };
        for (i, ch) in CHANNELS.iter().enumerate() {
            // A network within two channels of ours lands in our band.
            let centre = channel_to_freq(*ch);
            if freq.abs_diff(centre) <= 10 {
                counts[i] += 1;
            }
        }
    }
    if counts.iter().all(|c| *c == 0) {
        return DEFAULT_CHANNEL;
    }
    let best = counts
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| **c)
        .map(|(i, _)| CHANNELS[i])
        .unwrap_or(DEFAULT_CHANNEL);
    tracing::info!("miracast: scan counts {counts:?} for channels {CHANNELS:?}, choosing {best}");
    best
}

fn channel_to_freq(channel: u32) -> u32 {
    2407 + 5 * channel
}

fn wait_for_group(ctrl: &mut Control, stop: &Arc<AtomicBool>) -> anyhow::Result<String> {
    let deadline = Instant::now() + GROUP_TIMEOUT;
    while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        match ctrl.poll_event(Duration::from_millis(500))? {
            Some(Event::GroupStarted { interface, go, .. }) => {
                if !go {
                    anyhow::bail!("the group started with us as client, not owner");
                }
                return Ok(interface);
            }
            Some(other) => tracing::debug!("miracast: event while waiting: {other:?}"),
            None => {}
        }
    }
    anyhow::bail!("no P2P-GROUP-STARTED within {GROUP_TIMEOUT:?}")
}

/// Gives the group interface the sink's address. `ip` rather than netlink:
/// one command, and its failure text is what a person would see by hand.
fn configure_interface(iface: &str) -> anyhow::Result<()> {
    let addr = format!("{}/29", dhcp::DEFAULT_LEASE.server);
    for args in [
        vec!["addr", "flush", "dev", iface],
        vec!["addr", "add", &addr, "dev", iface],
        vec!["link", "set", iface, "up"],
    ] {
        let out = std::process::Command::new(tool("ip")).args(&args).output()?;
        tracing::info!(
            "miracast: ip {} -> {}",
            args.join(" "),
            if out.status.success() {
                "ok".to_string()
            } else {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            }
        );
        if !out.status.success() {
            anyhow::bail!("ip {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
    }
    Ok(())
}

/// A broadcast-capable socket on port 67, tied to the group interface so it
/// never answers DHCP on the home network.
fn open_dhcp_socket(iface: &str) -> anyhow::Result<UdpSocket> {
    let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 67))?;
    sock.set_broadcast(true)?;
    sock.set_nonblocking(true)?;
    let name = iface.as_bytes();
    // SAFETY: `name` outlives the call and its length is passed with it.
    let r = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            name.as_ptr() as *const libc::c_void,
            name.len() as libc::socklen_t,
        )
    };
    if r < 0 {
        return Err(std::io::Error::last_os_error())
            .map_err(|e| anyhow::anyhow!("binding the DHCP socket to {iface}: {e}"));
    }
    Ok(sock)
}

/// Blocks until one of the descriptors is readable or the timeout passes.
/// Negative entries are ignored, which is how an absent connection is handled.
fn poll_readable(fds: &[RawFd], timeout: Duration) {
    let mut pollfds: Vec<libc::pollfd> = fds
        .iter()
        .filter(|f| **f >= 0)
        .map(|f| libc::pollfd {
            fd: *f,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    if pollfds.is_empty() {
        std::thread::sleep(timeout);
        return;
    }
    // SAFETY: the slice is valid for the call and its length is passed.
    unsafe {
        libc::poll(
            pollfds.as_mut_ptr(),
            pollfds.len() as libc::nfds_t,
            timeout.as_millis() as libc::c_int,
        );
    }
}

fn sleep_unless_stopped(d: Duration, stop: &Arc<AtomicBool>) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Eight random hex digits, which is all the RTSP session identifier has to be.
fn session_id() -> String {
    let mut b = [0u8; 4];
    fill_random(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A WPS PIN: seven random digits and the checksum digit the standard defines.
/// Windows rejects a PIN whose checksum does not match, so this is not
/// decoration.
fn generate_pin() -> String {
    let mut b = [0u8; 4];
    fill_random(&mut b);
    let value = u32::from_be_bytes(b) % 10_000_000;
    format!("{:07}{}", value, pin_checksum(value))
}

/// The WPS checksum digit, as defined by the specification and implemented by
/// `wpa_supplicant`: the argument is the seven-digit value, and the digit
/// this returns is appended to it.
fn pin_checksum(mut pin: u32) -> u32 {
    let mut accum = 0;
    while pin > 0 {
        accum += 3 * (pin % 10);
        pin /= 10;
        accum += pin % 10;
        pin /= 10;
    }
    (10 - accum % 10) % 10
}

fn fill_random(buf: &mut [u8]) {
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    // Never expected on Linux, but a predictable PIN beats a panic: the PIN
    // is shown on a screen in the room, not used as a secret at a distance.
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (n >> (8 * (i % 4))) as u8;
    }
}

/// The peers that have completed WPS, one MAC per line. Its own file rather
/// than a section of `paired.toml`: two independent writers of that file would
/// silently drop each other's tables, since each parses it into its own shape.
struct PeerStore {
    path: PathBuf,
    macs: Vec<String>,
}

impl PeerStore {
    fn load(path: &Path) -> Self {
        let macs = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                (!l.is_empty() && !l.starts_with('#')).then(|| l.to_ascii_lowercase())
            })
            .collect();
        Self {
            path: path.to_path_buf(),
            macs,
        }
    }

    fn contains(&self, mac: &str) -> bool {
        self.macs.contains(&mac.to_ascii_lowercase())
    }

    fn remember(&mut self, mac: &str) {
        let mac = mac.to_ascii_lowercase();
        if self.macs.contains(&mac) {
            return;
        }
        self.macs.push(mac);
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = format!(
            "# Miracast peers that have completed pairing.\n{}\n",
            self.macs.join("\n")
        );
        if let Err(e) = std::fs::write(&self.path, body) {
            tracing::warn!("miracast: writing {}: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_eight_digits_and_carries_a_valid_checksum() {
        for _ in 0..50 {
            let pin = generate_pin();
            assert_eq!(pin.len(), 8, "{pin}");
            assert!(pin.chars().all(|c| c.is_ascii_digit()), "{pin}");
            let value: u32 = pin[..7].parse().unwrap();
            let check: u32 = pin[7..].parse().unwrap();
            assert_eq!(check, pin_checksum(value), "{pin}");
        }
    }

    #[test]
    fn the_checksum_matches_the_values_the_specification_publishes() {
        // 12345670 and 87654325 are the two PINs the specification and
        // wpa_supplicant use as examples; both must validate.
        assert_eq!(pin_checksum(1234567), 0);
        assert_eq!(pin_checksum(8765432), 5);
    }

    #[test]
    fn channels_map_to_the_frequencies_the_supplicant_expects() {
        assert_eq!(channel_to_freq(1), 2412);
        assert_eq!(channel_to_freq(6), 2437);
        assert_eq!(channel_to_freq(11), 2462);
    }

    #[test]
    fn peers_survive_a_reload_and_are_matched_case_insensitively() {
        let path = std::env::temp_dir().join(format!("castr-peers-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut s = PeerStore::load(&path);
        assert!(!s.contains("AA:BB:CC:DD:EE:FF"));
        s.remember("AA:BB:CC:DD:EE:FF");
        let s2 = PeerStore::load(&path);
        assert!(s2.contains("aa:bb:cc:dd:ee:ff"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_session_identifier_is_eight_hex_digits() {
        let id = session_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
