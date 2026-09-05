//! RTSP/1.0 messages, enough for the Wi-Fi Display exchange. Pure parsing and
//! formatting over byte slices: the caller owns the socket and the buffer, so
//! this module is testable without one.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartLine {
    Request { method: String, uri: String },
    Response { status: u16, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub start: StartLine,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MalformedStartLine(String),
    MalformedHeader(String),
    BadContentLength(String),
    NotUtf8,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MalformedStartLine(s) => write!(f, "malformed start line: {s:?}"),
            ParseError::MalformedHeader(s) => write!(f, "malformed header: {s:?}"),
            ParseError::BadContentLength(s) => write!(f, "bad Content-Length: {s:?}"),
            ParseError::NotUtf8 => write!(f, "message is not UTF-8"),
        }
    }
}

impl std::error::Error for ParseError {}

impl Message {
    /// Case-insensitive header lookup, as RTSP requires.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn cseq(&self) -> Option<u32> {
        self.header("cseq").and_then(|v| v.trim().parse().ok())
    }

    pub fn format(&self) -> String {
        let mut s = String::new();
        match &self.start {
            StartLine::Request { method, uri } => {
                let _ = write!(s, "{method} {uri} RTSP/1.0\r\n");
            }
            StartLine::Response { status, reason } => {
                let _ = write!(s, "RTSP/1.0 {status} {reason}\r\n");
            }
        }
        for (k, v) in &self.headers {
            let _ = write!(s, "{k}: {v}\r\n");
        }
        if !self.body.is_empty() {
            let _ = write!(s, "Content-Type: text/parameters\r\n");
            let _ = write!(s, "Content-Length: {}\r\n", self.body.len());
        }
        s.push_str("\r\n");
        s.push_str(&self.body);
        s
    }
}

/// Parses one message from the front of `buf`.
///
/// `Ok(None)` means the buffer does not yet hold a complete message and the
/// caller should read more. `Ok(Some((msg, used)))` returns the message and
/// how many bytes it consumed, so the caller can drain exactly that much.
pub fn parse(buf: &[u8]) -> Result<Option<(Message, usize)>, ParseError> {
    let Some(head_end) = find_double_crlf(buf) else {
        return Ok(None);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| ParseError::NotUtf8)?;
    let mut lines = head.split("\r\n");
    let start_line = lines.next().unwrap_or_default();
    let start = parse_start_line(start_line)?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once(':')
            .ok_or_else(|| ParseError::MalformedHeader(line.to_string()))?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let body_len = match headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        Some((_, v)) => v
            .trim()
            .parse::<usize>()
            .map_err(|_| ParseError::BadContentLength(v.clone()))?,
        None => 0,
    };
    let body_start = head_end + 4;
    if buf.len() < body_start + body_len {
        return Ok(None);
    }
    let body = std::str::from_utf8(&buf[body_start..body_start + body_len])
        .map_err(|_| ParseError::NotUtf8)?
        .to_string();
    Ok(Some((
        Message {
            start,
            headers,
            body,
        },
        body_start + body_len,
    )))
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_start_line(line: &str) -> Result<StartLine, ParseError> {
    if let Some(rest) = line.strip_prefix("RTSP/1.0 ") {
        let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
        let status = code
            .parse()
            .map_err(|_| ParseError::MalformedStartLine(line.to_string()))?;
        return Ok(StartLine::Response {
            status,
            reason: reason.to_string(),
        });
    }
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or_default();
    let uri = parts.next();
    let version = parts.next();
    match (method.is_empty(), uri, version) {
        (false, Some(uri), Some(v)) if v.starts_with("RTSP/") => Ok(StartLine::Request {
            method: method.to_string(),
            uri: uri.to_string(),
        }),
        _ => Err(ParseError::MalformedStartLine(line.to_string())),
    }
}

fn reason_for(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        454 => "Session Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

pub fn response(status: u16, cseq: u32, body: &str) -> Message {
    Message {
        start: StartLine::Response {
            status,
            reason: reason_for(status).to_string(),
        },
        headers: vec![("CSeq".into(), cseq.to_string())],
        body: body.to_string(),
    }
}

pub fn request(method: &str, uri: &str, cseq: u32, body: &str) -> Message {
    Message {
        start: StartLine::Request {
            method: method.into(),
            uri: uri.into(),
        },
        headers: vec![("CSeq".into(), cseq.to_string())],
        body: body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M1: &[u8] = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.wfa.wfd1.0\r\n\r\n";

    const M3: &[u8] = b"GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\n\
CSeq: 2\r\n\
Content-Type: text/parameters\r\n\
Content-Length: 59\r\n\
\r\n\
wfd_video_formats\r\nwfd_audio_codecs\r\nwfd_client_rtp_ports\r\n";

    #[test]
    fn a_request_without_a_body_parses_whole() {
        let (m, used) = parse(M1).unwrap().unwrap();
        assert_eq!(used, M1.len());
        match &m.start {
            StartLine::Request { method, uri } => {
                assert_eq!(method, "OPTIONS");
                assert_eq!(uri, "*");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m.cseq(), Some(1));
        assert_eq!(m.header("require"), Some("org.wfa.wfd1.0"));
        assert!(m.body.is_empty());
    }

    #[test]
    fn header_lookup_ignores_case() {
        let (m, _) = parse(M1).unwrap().unwrap();
        assert_eq!(m.header("CSeq"), m.header("cseq"));
    }

    #[test]
    fn a_body_is_read_by_content_length() {
        let (m, used) = parse(M3).unwrap().unwrap();
        assert_eq!(used, M3.len());
        assert_eq!(m.body.len(), 59);
        assert!(m.body.starts_with("wfd_video_formats"));
    }

    #[test]
    fn an_incomplete_message_asks_for_more() {
        assert!(parse(&M1[..10]).unwrap().is_none());
        // Headers complete, body short.
        let cut = M3.len() - 5;
        assert!(parse(&M3[..cut]).unwrap().is_none());
    }

    #[test]
    fn two_messages_in_one_buffer_are_returned_one_at_a_time() {
        let mut buf = M1.to_vec();
        buf.extend_from_slice(M1);
        let (_, used) = parse(&buf).unwrap().unwrap();
        assert_eq!(used, M1.len());
        let (m2, used2) = parse(&buf[used..]).unwrap().unwrap();
        assert_eq!(used2, M1.len());
        assert_eq!(m2.cseq(), Some(1));
    }

    #[test]
    fn a_response_start_line_parses() {
        let raw = b"RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n";
        let (m, _) = parse(raw).unwrap().unwrap();
        match &m.start {
            StartLine::Response { status, reason } => {
                assert_eq!(*status, 200);
                assert_eq!(reason, "OK");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_malformed_start_line_is_an_error_not_a_hang() {
        let raw = b"GARBAGE\r\n\r\n";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn a_response_formats_with_content_length_and_crlf() {
        let m = response(200, 7, "wfd_video_formats: 40\r\n");
        let s = m.format();
        assert!(s.starts_with("RTSP/1.0 200 OK\r\nCSeq: 7\r\n"));
        assert!(s.contains("Content-Type: text/parameters\r\n"));
        assert!(s.contains("Content-Length: 23\r\n"));
        assert!(s.ends_with("\r\n\r\nwfd_video_formats: 40\r\n"));
    }

    #[test]
    fn an_empty_response_carries_no_content_headers() {
        let s = response(200, 3, "").format();
        assert!(!s.contains("Content-Length"), "{s}");
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn a_request_formats_with_its_uri_and_body() {
        let s = request(
            "SET_PARAMETER",
            "rtsp://192.168.173.2/wfd1.0",
            9,
            "wfd_idr_request\r\n",
        )
        .format();
        assert!(s.starts_with("SET_PARAMETER rtsp://192.168.173.2/wfd1.0 RTSP/1.0\r\n"));
        assert!(s.contains("CSeq: 9\r\n"));
        assert!(s.ends_with("wfd_idr_request\r\n"));
    }

    #[test]
    fn a_formatted_message_parses_back_identically() {
        let m = response(200, 42, "wfd_audio_codecs: LPCM 00000002 00\r\n");
        let text = m.format();
        let (back, used) = parse(text.as_bytes()).unwrap().unwrap();
        assert_eq!(used, text.len());
        assert_eq!(back.cseq(), Some(42));
        assert_eq!(back.body, m.body);
    }
}

// ---- negotiation ----

use crate::wfd::{capabilities_body, parse_parameter_body, Capabilities};
use std::time::{Duration, Instant};

/// A dead radio is invisible to TCP for minutes, so the keep-alive is the
/// fastest signal the control channel has. Five seconds costs nothing on a
/// link carrying 8 Mbps of video.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(5);
/// Two missed keep-alives, not one: a single loss on a busy link must not end
/// a healthy session.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
/// One IDR request per this interval, however often the decoder asks.
const IDR_MIN_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// CEA table index to resolution. Only the modes we might see are listed; the
/// sink advertises index 5 alone, so anything else is a source error.
fn cea_mode(bit: u32) -> Option<VideoMode> {
    Some(match bit {
        5 => VideoMode {
            width: 1280,
            height: 720,
            fps: 30,
        },
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegState {
    Init,
    Capabilities,
    Ready,
    Playing,
    Done,
}

#[derive(Debug)]
pub enum Action {
    Send(Message),
    /// Media may start flowing; the caller opens its RTP socket.
    Play,
    /// The display asked the source to send at most this many kbps.
    ///
    /// A sink watching its own loss asks for less. Ignoring it means sending
    /// the same rate into a link that is already failing, until the session
    /// drops - which is the difference between degrading and disconnecting.
    Bitrate(u32),
    /// The display asked for a keyframe and the encoder must produce one.
    ///
    /// A sink that joins a stream part-way through a group of pictures has
    /// nothing to decode from and shows black until an IDR arrives. Answering
    /// `wfd_idr_request` with `200 OK` and doing nothing is how a session runs
    /// to completion, exchanging keep-alives, with no picture ever appearing.
    Keyframe,
    Teardown(&'static str),
}

pub struct Negotiation {
    caps: Capabilities,
    session_id: String,
    state: NegState,
    next_cseq: u32,
    presentation_url: String,
    chosen: Option<VideoMode>,
    peer_session: Option<String>,
    last_heard: Option<Instant>,
    last_keepalive: Option<Instant>,
    last_idr: Option<Instant>,
}

impl Negotiation {
    pub fn new(caps: Capabilities, session_id: String) -> Self {
        Self {
            caps,
            session_id,
            state: NegState::Init,
            next_cseq: 100,
            presentation_url: String::new(),
            chosen: None,
            peer_session: None,
            last_heard: None,
            last_keepalive: None,
            last_idr: None,
        }
    }

    pub fn state(&self) -> NegState {
        self.state
    }

    pub fn chosen_video(&self) -> Option<VideoMode> {
        self.chosen
    }

    fn cseq(&mut self) -> u32 {
        let c = self.next_cseq;
        self.next_cseq += 1;
        c
    }

    pub fn on_message(&mut self, m: &Message) -> Vec<Action> {
        self.on_message_at(m, Instant::now())
    }

    /// The clock is injected so the keep-alive and IDR rules are testable
    /// without sleeping.
    pub fn on_message_at(&mut self, m: &Message, now: Instant) -> Vec<Action> {
        self.last_heard = Some(now);
        match &m.start {
            StartLine::Request { method, .. } => {
                let method = method.clone();
                self.on_request(&method, m)
            }
            StartLine::Response { status, .. } => self.on_response(*status, m),
        }
    }

    fn on_request(&mut self, method: &str, m: &Message) -> Vec<Action> {
        let cseq = m.cseq().unwrap_or(0);
        match method {
            "OPTIONS" => {
                let mut ok = response(200, cseq, "");
                ok.headers.push((
                    "Public".into(),
                    "org.wfa.wfd1.0, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER, SET_PARAMETER"
                        .into(),
                ));
                // M2: ask what the source supports, as the exchange requires.
                let c = self.cseq();
                let mut m2 = request("OPTIONS", "*", c, "");
                m2.headers.push(("Require".into(), "org.wfa.wfd1.0".into()));
                vec![Action::Send(ok), Action::Send(m2)]
            }
            "GET_PARAMETER" => {
                // M3, or a keep-alive with an empty body.
                if m.body.trim().is_empty() {
                    return vec![Action::Send(response(200, cseq, ""))];
                }
                self.state = NegState::Capabilities;
                vec![Action::Send(response(
                    200,
                    cseq,
                    &capabilities_body(&self.caps),
                ))]
            }
            "SET_PARAMETER" => self.on_set_parameter(cseq, m),
            "TEARDOWN" => {
                self.state = NegState::Done;
                vec![
                    Action::Send(response(200, cseq, "")),
                    Action::Teardown("source sent TEARDOWN"),
                ]
            }
            _ => vec![Action::Send(response(400, cseq, ""))],
        }
    }

    fn on_set_parameter(&mut self, cseq: u32, m: &Message) -> Vec<Action> {
        let params = parse_parameter_body(&m.body);
        let mut out = Vec::new();
        let mut trigger = None;
        for (name, value) in &params {
            match name.as_str() {
                "wfd_video_formats" => match Self::parse_chosen_video(value) {
                    Some(mode) => self.chosen = Some(mode),
                    None => {
                        // The source picked something we never advertised.
                        return vec![Action::Send(response(400, cseq, ""))];
                    }
                },
                "wfd_presentation_URL" => {
                    self.presentation_url = value
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string();
                }
                "wfd_trigger_method" => trigger = Some(value.trim().to_string()),
                _ => {}
            }
        }
        out.push(Action::Send(response(200, cseq, "")));
        if trigger.as_deref() == Some("SETUP") {
            let c = self.cseq();
            let url = self.uri();
            let mut setup = request("SETUP", &url, c, "");
            setup.headers.push((
                "Transport".into(),
                format!(
                    "RTP/AVP/UDP;unicast;client_port={}",
                    self.caps.ports.rtp_port
                ),
            ));
            self.state = NegState::Ready;
            out.push(Action::Send(setup));
        }
        out
    }

    /// The chosen-format line repeats the capability layout with exactly one
    /// bit set in one table. Accept it only if it is a CEA mode we advertised.
    fn parse_chosen_video(value: &str) -> Option<VideoMode> {
        let f: Vec<&str> = value.split_whitespace().collect();
        if f.len() < 7 {
            return None;
        }
        let cea = u32::from_str_radix(f[4], 16).ok()?;
        let vesa = u32::from_str_radix(f[5], 16).ok()?;
        let hh = u32::from_str_radix(f[6], 16).ok()?;
        if vesa != 0 || hh != 0 || cea.count_ones() != 1 {
            return None;
        }
        cea_mode(cea.trailing_zeros())
    }

    fn on_response(&mut self, status: u16, m: &Message) -> Vec<Action> {
        if status != 200 {
            return vec![Action::Teardown("source refused a request")];
        }
        if self.state == NegState::Ready && self.peer_session.is_none() {
            // M6 answered: take the session id and send M7 PLAY.
            let session = m
                .header("session")
                .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());
            let Some(session) = session else {
                return vec![Action::Teardown("SETUP response carried no Session")];
            };
            self.peer_session = Some(session.clone());
            let c = self.cseq();
            let url = self.uri();
            let mut play = request("PLAY", &url, c, "");
            play.headers.push(("Session".into(), session));
            self.state = NegState::Playing;
            return vec![Action::Send(play), Action::Play];
        }
        Vec::new()
    }

    /// Time-driven work: keep-alive out, and give up on a silent source.
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        if matches!(self.state, NegState::Done) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let since_heard = self.last_heard.map(|t| now.saturating_duration_since(t));
        if since_heard.is_some_and(|d| d > KEEPALIVE_TIMEOUT) {
            self.state = NegState::Done;
            out.push(Action::Teardown("no keep-alive reply for 10 s"));
            return out;
        }
        let due = match self.last_keepalive {
            Some(t) => now.saturating_duration_since(t) >= KEEPALIVE_EVERY,
            None => since_heard.is_some_and(|d| d >= KEEPALIVE_EVERY),
        };
        if due {
            self.last_keepalive = Some(now);
            let c = self.cseq();
            let uri = self.uri();
            let mut ka = request("GET_PARAMETER", &uri, c, "");
            if let Some(s) = &self.peer_session {
                ka.headers.push(("Session".into(), s.clone()));
            }
            out.push(Action::Send(ka));
        }
        out
    }

    /// Asks the source to cap its bitrate. This is a request: the source may
    /// ignore it, which is why the sink never assumes the rate actually fell.
    pub fn request_bitrate(&mut self, kbps: u32, _now: Instant) -> Vec<Action> {
        if !matches!(self.state, NegState::Playing) {
            return Vec::new();
        }
        let c = self.cseq();
        let uri = self.uri();
        let body = format!("microsoft_max_bitrate: {kbps}\r\n");
        let mut m = request("SET_PARAMETER", &uri, c, &body);
        if let Some(s) = &self.peer_session {
            m.headers.push(("Session".into(), s.clone()));
        }
        vec![Action::Send(m)]
    }

    /// Asks the source for a fresh keyframe, at most once per 500 ms.
    pub fn request_idr(&mut self, now: Instant) -> Vec<Action> {
        if self
            .last_idr
            .is_some_and(|t| now.saturating_duration_since(t) < IDR_MIN_INTERVAL)
        {
            return Vec::new();
        }
        self.last_idr = Some(now);
        let c = self.cseq();
        let uri = self.uri();
        let mut m = request("SET_PARAMETER", &uri, c, "wfd_idr_request\r\n");
        if let Some(s) = &self.peer_session {
            m.headers.push(("Session".into(), s.clone()));
        }
        vec![Action::Send(m)]
    }

    fn uri(&self) -> String {
        if self.presentation_url.is_empty() {
            format!("rtsp://localhost/wfd1.0/{}", self.session_id)
        } else {
            self.presentation_url.clone()
        }
    }
}

#[cfg(test)]
mod negotiation_tests {
    use super::*;
    use crate::wfd::{AudioCodecs, Capabilities, ClientPorts, VideoFormats};
    use std::time::{Duration, Instant};

    fn caps() -> Capabilities {
        Capabilities {
            video: VideoFormats::only_720p30(),
            audio: AudioCodecs::lpcm_48k_stereo(),
            ports: ClientPorts { rtp_port: 5000 },
            max_bitrate_kbps: 8000,
            latency_management: true,
            format_change: true,
        }
    }

    fn neg() -> Negotiation {
        Negotiation::new(caps(), "01234567".into())
    }

    fn req(method: &str, cseq: u32, body: &str) -> Message {
        request(method, "rtsp://localhost/wfd1.0", cseq, body)
    }

    fn sent(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(m) => Some(m.format()),
                _ => None,
            })
            .collect()
    }

    /// Drives the exchange to the point just before the SETUP trigger, so the
    /// later tests do not each repeat it.
    fn to_ready(n: &mut Negotiation, t: Instant) {
        n.on_message_at(&req("OPTIONS", 1, ""), t);
        n.on_message_at(&req("GET_PARAMETER", 2, "wfd_video_formats\r\n"), t);
        n.on_message_at(
            &req(
                "SET_PARAMETER",
                3,
                "wfd_video_formats: 00 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
                 wfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n",
            ),
            t,
        );
    }

    #[test]
    fn m1_options_is_answered_with_our_methods_and_then_we_ask_theirs() {
        let mut n = neg();
        let out = n.on_message(&req("OPTIONS", 1, ""));
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 2, "answer M1 and send M2");
        assert!(msgs[0].starts_with("RTSP/1.0 200 OK\r\nCSeq: 1\r\n"));
        assert!(msgs[0].contains(
            "Public: org.wfa.wfd1.0, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER, SET_PARAMETER"
        ));
        assert!(msgs[1].starts_with("OPTIONS * RTSP/1.0\r\n"), "{}", msgs[1]);
        assert_eq!(n.state(), NegState::Init);
    }

    #[test]
    fn m3_get_parameter_is_answered_with_our_capabilities() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        let out = n.on_message(&req(
            "GET_PARAMETER",
            2,
            "wfd_video_formats\r\nwfd_audio_codecs\r\nwfd_client_rtp_ports\r\n",
        ));
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("wfd_video_formats: 40 00 02 04 00000020"));
        assert!(msgs[0].contains("wfd_audio_codecs: LPCM 00000002 00"));
        assert!(msgs[0].contains("microsoft_max_bitrate: 8000"));
        assert_eq!(n.state(), NegState::Capabilities);
    }

    #[test]
    fn m4_set_parameter_records_the_chosen_mode_and_is_acknowledged() {
        let mut n = neg();
        let t = Instant::now();
        n.on_message_at(&req("OPTIONS", 1, ""), t);
        n.on_message_at(&req("GET_PARAMETER", 2, "wfd_video_formats\r\n"), t);
        let out = n.on_message_at(
            &req(
                "SET_PARAMETER",
                3,
                "wfd_video_formats: 00 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
                 wfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n",
            ),
            t,
        );
        assert_eq!(sent(&out).len(), 1);
        assert!(sent(&out)[0].starts_with("RTSP/1.0 200 OK\r\nCSeq: 3\r\n"));
        assert_eq!(
            n.chosen_video(),
            Some(VideoMode {
                width: 1280,
                height: 720,
                fps: 30
            })
        );
    }

    #[test]
    fn a_source_that_picks_an_unadvertised_mode_is_refused() {
        let mut n = neg();
        let t = Instant::now();
        n.on_message_at(&req("OPTIONS", 1, ""), t);
        n.on_message_at(&req("GET_PARAMETER", 2, "wfd_video_formats\r\n"), t);
        // CEA bit 16 (1920x1080p30), which we never advertised.
        let out = n.on_message_at(
            &req(
                "SET_PARAMETER",
                3,
                "wfd_video_formats: 00 00 02 04 00010000 00000000 00000000 00 0000 0000 00 none none\r\n",
            ),
            t,
        );
        let msgs = sent(&out);
        assert!(msgs[0].starts_with("RTSP/1.0 400"), "{}", msgs[0]);
        assert!(n.chosen_video().is_none());
    }

    #[test]
    fn m5_trigger_setup_makes_us_send_setup_then_play() {
        let mut n = neg();
        let t = Instant::now();
        to_ready(&mut n, t);
        let out = n.on_message_at(&req("SET_PARAMETER", 4, "wfd_trigger_method: SETUP\r\n"), t);
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 2, "ack the trigger, then send M6 SETUP");
        assert!(
            msgs[1].starts_with("SETUP rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0\r\n"),
            "{}",
            msgs[1]
        );
        assert!(msgs[1].contains("Transport: RTP/AVP/UDP;unicast;client_port=5000\r\n"));
        assert_eq!(n.state(), NegState::Ready);
    }

    #[test]
    fn a_setup_response_with_a_session_makes_us_play_and_start_media() {
        let mut n = neg();
        let t = Instant::now();
        to_ready(&mut n, t);
        n.on_message_at(&req("SET_PARAMETER", 4, "wfd_trigger_method: SETUP\r\n"), t);
        let mut ok = response(200, 100, "");
        ok.headers
            .push(("Session".into(), "abcdef12;timeout=60".into()));
        let out = n.on_message_at(&ok, t);
        let msgs = sent(&out);
        assert!(
            msgs[0].starts_with("PLAY rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0\r\n"),
            "{}",
            msgs[0]
        );
        assert!(msgs[0].contains("Session: abcdef12\r\n"));
        assert!(
            out.iter().any(|a| matches!(a, Action::Play)),
            "media starts"
        );
        assert_eq!(n.state(), NegState::Playing);
    }

    #[test]
    fn keep_alive_goes_out_every_five_seconds_and_silence_ends_the_session() {
        let mut n = neg();
        let t0 = Instant::now();
        n.on_message_at(&req("OPTIONS", 1, ""), t0);
        assert!(sent(&n.tick(t0 + Duration::from_secs(3))).is_empty());
        let out = n.tick(t0 + Duration::from_secs(6));
        assert!(
            sent(&out)[0].starts_with("GET_PARAMETER "),
            "{:?}",
            sent(&out)
        );
        // No reply for 10 s after the last one heard: tear down.
        let out = n.tick(t0 + Duration::from_secs(11));
        assert!(
            out.iter().any(|a| matches!(a, Action::Teardown(_))),
            "{out:?}"
        );
    }

    #[test]
    fn an_idr_request_is_rate_limited_to_one_per_five_hundred_milliseconds() {
        let mut n = neg();
        let t0 = Instant::now();
        n.on_message_at(&req("OPTIONS", 1, ""), t0);
        assert_eq!(sent(&n.request_idr(t0)).len(), 1);
        assert!(sent(&n.request_idr(t0 + Duration::from_millis(200))).is_empty());
        let out = n.request_idr(t0 + Duration::from_millis(600));
        assert_eq!(sent(&out).len(), 1);
        assert!(
            sent(&out)[0].contains("wfd_idr_request"),
            "{:?}",
            sent(&out)
        );
    }

    #[test]
    fn teardown_from_the_source_ends_the_session() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        let out = n.on_message(&req("TEARDOWN", 9, ""));
        assert!(sent(&out)[0].starts_with("RTSP/1.0 200 OK\r\nCSeq: 9\r\n"));
        assert!(out.iter().any(|a| matches!(a, Action::Teardown(_))));
        assert_eq!(n.state(), NegState::Done);
    }

    #[test]
    fn an_unknown_method_is_refused_without_ending_the_session() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        let out = n.on_message(&req("ANNOUNCE", 5, ""));
        assert!(sent(&out)[0].starts_with("RTSP/1.0 400"));
        assert!(!out.iter().any(|a| matches!(a, Action::Teardown(_))));
    }

    /// A negotiation driven to Playing. Named for the state, not the fixture, so it
    /// does not read as a recursive call to `test_support::negotiation_to_playing`.
    fn playing() -> Negotiation {
        let mut n = Negotiation::new(caps(), "01234567".into());
        for msg in crate::test_support::negotiation_to_playing() {
            let (m, _) = parse(msg.as_bytes()).unwrap().unwrap();
            n.on_message_at(&m, Instant::now());
        }
        n
    }

    #[test]
    fn a_dead_peer_is_noticed_within_ten_seconds() {
        let mut n = playing();
        let t0 = Instant::now();
        // Nine seconds of silence is not yet a dead peer: a single lost keep-alive
        // on a busy link must not end a healthy session.
        let quiet = n.tick(t0 + Duration::from_secs(9));
        assert!(
            !quiet.iter().any(|a| matches!(a, Action::Teardown(_))),
            "{quiet:?}"
        );
        let dead = n.tick(t0 + Duration::from_secs(11));
        assert!(
            dead.iter().any(|a| matches!(a, Action::Teardown(_))),
            "{dead:?}"
        );
    }

    #[test]
    fn a_bitrate_request_names_the_ceiling_and_the_session() {
        let mut n = playing();
        let out = n.request_bitrate(2000, Instant::now());
        let Some(Action::Send(m)) = out.into_iter().next() else {
            panic!("no message");
        };
        let text = m.format();
        assert!(text.starts_with("SET_PARAMETER "), "{text}");
        assert!(text.contains("microsoft_max_bitrate: 2000\r\n"), "{text}");
        assert!(text.contains("Session: "), "the source needs to know which session: {text}");
    }

    #[test]
    fn keep_alives_go_out_every_five_seconds() {
        let mut n = playing();
        let t0 = Instant::now();
        let early = n.tick(t0 + Duration::from_secs(3));
        assert!(early.is_empty(), "too soon: {early:?}");
        let due = n.tick(t0 + Duration::from_secs(6));
        assert_eq!(due.len(), 1, "one keep-alive: {due:?}");
    }
}
