//! The source half of the Wi-Fi Display negotiation.
//!
//! Bytes in, actions out, exactly as `rtsp::Negotiation` does for the sink, so
//! a whole M1-M7 exchange replays in a test with no socket - and the two can be
//! driven against each other directly, in one process.
//!
//! The sequence, with who speaks:
//!
//! - **M1** we send `OPTIONS`; the display answers with its `Public` list.
//! - **M2** the display sends `OPTIONS`; we answer. Both ends act as client and
//!   server over one connection, which is why `rtsp` is bidirectional.
//! - **M3** we `GET_PARAMETER` its capabilities.
//! - **M4** we set the one mode we chose, and **M5** asks it to send `SETUP`.
//! - **M6/M7** it sends `SETUP` then `PLAY`; we answer both, and media starts.

use crate::rtsp::{self, Action, Message, StartLine, VideoMode};
use crate::source::caps::{self, SinkCaps};
use castr_media::codec::Mode;
use std::time::{Duration, Instant};

/// Matches the sink's tolerance in `rtsp.rs`: two missed keep-alives, not one,
/// so a single loss on a busy link does not end a healthy session.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(5);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

const PUBLIC: &str =
    "org.wfa.wfd1.0, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER, SET_PARAMETER";
const PRESENTATION_URL: &str = "rtsp://localhost/wfd1.0/streamid=0";

#[derive(Debug, Clone)]
pub struct SourceConfig {
    /// Which way to lean when a display offers several modes: a bigger picture
    /// or a faster one. The same toggle castr's own protocol uses.
    pub mode: Mode,
    /// The port we will send RTP from.
    pub rtp_port: u16,
    /// The session id we hand back when the display sets up the stream.
    pub session_id: String,
    /// What the display's information element said it can carry, if the radio
    /// read one. The capability body may name a ceiling too; the lower wins.
    pub ceiling_mbps: Option<u16>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Quality,
            rtp_port: 5000,
            session_id: "1234567890".to_string(),
            ceiling_mbps: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Init,
    AwaitingCaps,
    Configuring,
    AwaitingSetup,
    Playing,
    Done,
}

/// Why a session ended, in the vocabulary the whole project reports failures
/// in. Every one of these names a stage.
pub type Reason = &'static str;

pub struct SourceSession {
    cfg: SourceConfig,
    state: SourceState,
    next_cseq: u32,
    asked_caps: bool,
    chosen: Option<VideoMode>,
    sink: Option<SinkCaps>,
    last_heard: Option<Instant>,
    last_keepalive: Option<Instant>,
}

impl SourceSession {
    pub fn new(cfg: SourceConfig) -> Self {
        Self {
            cfg,
            state: SourceState::Init,
            next_cseq: 1,
            asked_caps: false,
            chosen: None,
            sink: None,
            last_heard: None,
            last_keepalive: None,
        }
    }

    pub fn state(&self) -> SourceState {
        self.state
    }

    pub fn chosen(&self) -> Option<VideoMode> {
        self.chosen
    }

    /// The port the display wants RTP sent to, once it has told us.
    pub fn sink_rtp_port(&self) -> Option<u16> {
        self.sink.as_ref().map(|c| c.rtp_port)
    }

    /// The ceiling the display asked us to respect, if it named one.
    pub fn max_bitrate_kbps(&self) -> Option<u32> {
        self.sink.as_ref().and_then(|c| c.max_bitrate_kbps)
    }

    fn cseq(&mut self) -> u32 {
        let c = self.next_cseq;
        self.next_cseq += 1;
        c
    }

    /// M1: the first thing on the connection.
    pub fn start(&mut self) -> Vec<Action> {
        let cseq = self.cseq();
        let mut m1 = rtsp::request("OPTIONS", "*", cseq, "");
        m1.headers
            .push(("Require".into(), "org.wfa.wfd1.0".into()));
        self.state = SourceState::AwaitingCaps;
        vec![Action::Send(m1)]
    }

    pub fn on_message(&mut self, m: &Message) -> Vec<Action> {
        self.on_message_at(m, Instant::now())
    }

    /// The clock is injected so the keep-alive rule is testable without
    /// sleeping.
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
            // M2, and any later OPTIONS the display feels like sending.
            "OPTIONS" => {
                let mut ok = rtsp::response(200, cseq, "");
                ok.headers.push(("Public".into(), PUBLIC.into()));
                vec![Action::Send(ok)]
            }
            // M6. The sink refuses a SETUP response with no Session header, so
            // this is where the session id has to appear.
            "SETUP" => {
                let mut ok = rtsp::response(200, cseq, "");
                ok.headers
                    .push(("Session".into(), self.cfg.session_id.clone()));
                ok.headers.push((
                    "Transport".into(),
                    format!(
                        "RTP/AVP/UDP;unicast;client_port={};server_port={}",
                        self.sink_rtp_port().unwrap_or(5000),
                        self.cfg.rtp_port
                    ),
                ));
                self.state = SourceState::AwaitingSetup;
                vec![Action::Send(ok)]
            }
            // M7: media starts.
            "PLAY" => {
                self.state = SourceState::Playing;
                vec![Action::Send(rtsp::response(200, cseq, "")), Action::Play]
            }
            "TEARDOWN" => {
                self.state = SourceState::Done;
                vec![
                    Action::Send(rtsp::response(200, cseq, "")),
                    Action::Teardown("session: the display ended the session"),
                ]
            }
            // Never fatal: a display may ask us things we have never seen, and
            // refusing them would refuse the display.
            _ => vec![Action::Send(rtsp::response(200, cseq, ""))],
        }
    }

    fn on_response(&mut self, status: u16, m: &Message) -> Vec<Action> {
        if status != 200 {
            return vec![Action::Teardown("negotiation: the display refused a request")];
        }
        if !self.asked_caps {
            // M1 answered. Ask what it can take.
            self.asked_caps = true;
            let cseq = self.cseq();
            let body = "wfd_video_formats\r\n\
                        wfd_audio_codecs\r\n\
                        wfd_content_protection\r\n\
                        wfd_client_rtp_ports\r\n";
            return vec![Action::Send(rtsp::request(
                "GET_PARAMETER",
                PRESENTATION_URL,
                cseq,
                body,
            ))];
        }
        if self.sink.is_none() && !m.body.trim().is_empty() {
            return self.on_capabilities(m);
        }
        // A keep-alive answered, or an acknowledgement of something we set.
        Vec::new()
    }

    fn on_capabilities(&mut self, m: &Message) -> Vec<Action> {
        let sink = match caps::parse(&m.body) {
            Ok(c) => c,
            Err(_) => {
                return vec![Action::Teardown(
                    "negotiation: the display sent capabilities we could not read",
                )]
            }
        };
        // The list of what to propose can only be built now: until M3 we did not
        // know what the display can carry, and offering it more than that is
        // asking for a stream it has already said it cannot take.
        let ceiling = [self.cfg.ceiling_mbps, sink.max_bitrate_kbps.map(|k| (k / 1000) as u16)]
            .into_iter()
            .flatten()
            .min();
        let ours = caps::our_modes(self.cfg.mode, ceiling);
        let chosen = match caps::choose(&sink, &ours) {
            Ok(mode) => mode,
            Err(_) => return vec![Action::Teardown("negotiation: no video format in common")],
        };
        let Some(bit) = caps::mode_bit(chosen) else {
            return vec![Action::Teardown("negotiation: chose a mode with no table entry")];
        };
        self.chosen = Some(chosen);
        self.sink = Some(sink);
        self.state = SourceState::Configuring;

        // M4 then M5, back to back: name the one mode we chose, then ask the
        // display to take over as client and send us SETUP. Exactly one bit is
        // set in exactly one table, which is what a sink will accept.
        let m4 = self.cseq();
        let m5 = self.cseq();
        let set = format!(
            "wfd_video_formats: 00 00 02 04 {bit:08X} 00000000 00000000 00 0000 0000 00 none none\r\n\
             wfd_audio_codecs: LPCM 00000002 00\r\n\
             wfd_presentation_URL: {PRESENTATION_URL} none\r\n\
             wfd_client_rtp_ports: RTP/AVP/UDP;unicast {} 0 mode=play\r\n",
            self.cfg.rtp_port
        );
        vec![
            Action::Send(rtsp::request("SET_PARAMETER", PRESENTATION_URL, m4, &set)),
            Action::Send(rtsp::request(
                "SET_PARAMETER",
                PRESENTATION_URL,
                m5,
                "wfd_trigger_method: SETUP\r\n",
            )),
        ]
    }

    /// Time-driven work: a keep-alive out, and giving up on a silent display.
    ///
    /// RTP is one-way with no feedback, so silence on the media path cannot
    /// tell a still desktop from a dead display. This is the only liveness
    /// signal there is.
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        if self.state != SourceState::Playing {
            return Vec::new();
        }
        if let Some(last) = self.last_heard {
            if now.duration_since(last) > KEEPALIVE_TIMEOUT {
                self.state = SourceState::Done;
                return vec![Action::Teardown("session: the display stopped answering")];
            }
        }
        let due = self
            .last_keepalive
            .is_none_or(|t| now.duration_since(t) >= KEEPALIVE_EVERY);
        if !due {
            return Vec::new();
        }
        self.last_keepalive = Some(now);
        let cseq = self.cseq();
        vec![Action::Send(rtsp::request(
            "GET_PARAMETER",
            PRESENTATION_URL,
            cseq,
            "",
        ))]
    }

    /// The message that ends a session politely. The caller sends this before
    /// closing, always: a display left believing a session is live can refuse
    /// the next one.
    pub fn teardown(&mut self) -> Message {
        let cseq = self.cseq();
        self.state = SourceState::Done;
        let mut m = rtsp::request("TEARDOWN", PRESENTATION_URL, cseq, "");
        m.headers
            .push(("Session".into(), self.cfg.session_id.clone()));
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtsp::{NegState, Negotiation};
    use crate::wfd::{AudioCodecs, Capabilities, ClientPorts, VideoFormats};

    const P720P30: VideoMode = VideoMode {
        width: 1280,
        height: 720,
        fps: 30,
    };
    const P1080P30: VideoMode = VideoMode {
        width: 1920,
        height: 1080,
        fps: 30,
    };
    const P720P60: VideoMode = VideoMode {
        width: 1280,
        height: 720,
        fps: 60,
    };

    fn cfg() -> SourceConfig {
        SourceConfig::default()
    }

    fn sent(actions: &[Action]) -> Vec<Message> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    fn sink() -> Negotiation {
        Negotiation::new(
            Capabilities {
                video: VideoFormats::only_720p30(),
                audio: AudioCodecs::lpcm_48k_stereo(),
                ports: ClientPorts { rtp_port: 5000 },
                max_bitrate_kbps: 20_000,
                latency_management: true,
                format_change: true,
            },
            "1234".to_string(),
        )
    }

    #[test]
    fn a_session_opens_with_m1_options() {
        let mut s = SourceSession::new(cfg());
        let msgs = sent(&s.start());
        assert_eq!(msgs.len(), 1);
        match &msgs[0].start {
            StartLine::Request { method, .. } => assert_eq!(method, "OPTIONS"),
            other => panic!("M1 must be a request, got {other:?}"),
        }
    }

    #[test]
    fn the_displays_options_request_is_answered() {
        // M2 runs the other way down the same connection; a source that only
        // spoke as a client would hang here.
        let mut s = SourceSession::new(cfg());
        s.start();
        let msgs = sent(&s.on_message(&rtsp::request("OPTIONS", "*", 1, "")));
        assert!(
            msgs.iter()
                .any(|m| matches!(m.start, StartLine::Response { status: 200, .. })),
            "the display's OPTIONS went unanswered"
        );
    }

    #[test]
    fn an_unknown_parameter_does_not_end_the_session() {
        let mut s = SourceSession::new(cfg());
        s.start();
        s.on_message(&rtsp::response(200, 1, "")); // M1 answered
        let body = "wfd_video_formats: 40 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
                    wfd_audio_codecs: LPCM 00000002 00\r\n\
                    wfd_client_rtp_ports: RTP/AVP/UDP;unicast 5000 0 mode=play\r\n\
                    some_vendor_extension: whatever\r\n";
        let actions = s.on_message(&rtsp::response(200, 2, body));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Teardown(_))),
            "a vendor parameter must not end the session"
        );
        assert_eq!(s.chosen(), Some(P720P30));
    }

    #[test]
    fn no_common_format_tears_down_rather_than_streaming_blindly() {
        // CEA bit 3 is 720x576p50, a European broadcast mode we never encode.
        let mut s = SourceSession::new(cfg());
        s.start();
        s.on_message(&rtsp::response(200, 1, ""));
        let body = "wfd_video_formats: 40 00 02 04 00000008 00000000 00000000 00 0000 0000 00 none none\r\n";
        let actions = s.on_message(&rtsp::response(200, 2, body));
        let reason = actions.iter().find_map(|a| match a {
            Action::Teardown(r) => Some(*r),
            _ => None,
        });
        assert_eq!(reason, Some("negotiation: no video format in common"));
    }

    #[test]
    fn a_refusal_names_the_stage_rather_than_going_quiet() {
        let mut s = SourceSession::new(cfg());
        s.start();
        let actions = s.on_message(&rtsp::response(400, 1, ""));
        let reason = actions.iter().find_map(|a| match a {
            Action::Teardown(r) => Some(*r),
            _ => None,
        });
        assert_eq!(
            reason,
            Some("negotiation: the display refused a request"),
            "every failure names its stage"
        );
    }

    #[test]
    fn a_silent_display_is_given_up_on() {
        let mut s = SourceSession::new(cfg());
        let start = Instant::now();
        s.state = SourceState::Playing;
        s.last_heard = Some(start);
        assert!(s.tick(start + Duration::from_secs(6)).iter().any(
            |a| matches!(a, Action::Send(m) if matches!(&m.start, StartLine::Request { method, .. } if method == "GET_PARAMETER"))
        ), "a keep-alive was due");
        let actions = s.tick(start + Duration::from_secs(11));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Teardown(r) if r.starts_with("session:"))));
    }

    #[test]
    fn our_source_and_our_sink_negotiate_each_other_to_playing() {
        // Two state machines, no sockets. This does not prove the
        // specification - both could share one wrong reading of it - but a
        // disagreement here is certainly a bug in one of them.
        let mut source = SourceSession::new(cfg());
        let mut sink = sink();

        let mut from_source = source.start();
        let mut source_playing = false;
        for _ in 0..24 {
            let mut from_sink = Vec::new();
            for action in from_source.drain(..) {
                match action {
                    Action::Send(m) => from_sink.extend(sink.on_message(&m)),
                    Action::Play => source_playing = true,
                    Action::Teardown(why) => panic!("the source tore down: {why}"),
                }
            }
            for action in from_sink.drain(..) {
                match action {
                    Action::Send(m) => from_source.extend(source.on_message(&m)),
                    Action::Play => {}
                    Action::Teardown(why) => panic!("the sink tore down: {why}"),
                }
            }
            if from_source.is_empty() {
                break;
            }
        }
        assert!(source_playing, "the source never reached Play");
        assert_eq!(source.state(), SourceState::Playing);
        assert_eq!(sink.state(), NegState::Playing, "the sink never reached Playing");
        assert_eq!(source.chosen(), Some(P720P30));
        assert_eq!(sink.chosen_video(), Some(P720P30), "the two chose differently");
    }

    #[test]
    fn the_toggle_decides_which_of_a_displays_modes_is_taken() {
        // A display offering 720p30, 720p60 and 1080p30 - CEA bits 5, 6 and 7.
        // Quality should take the biggest picture, Game the fastest one, and
        // the choice is made here rather than baked in at construction.
        let body = "wfd_video_formats: 40 00 02 04 000000E0 00000000 00000000 00 0000 0000 00 none none
";
        for (mode, want) in [(Mode::Quality, P1080P30), (Mode::Game, P720P60)] {
            let mut s = SourceSession::new(SourceConfig {
                mode,
                ..SourceConfig::default()
            });
            s.start();
            s.on_message(&rtsp::response(200, 1, ""));
            s.on_message(&rtsp::response(200, 2, body));
            assert_eq!(s.chosen(), Some(want), "{mode:?} chose the wrong mode");
        }
    }

    #[test]
    fn a_ceiling_keeps_us_from_proposing_what_a_display_cannot_carry() {
        // The same display, but it says it can take only 9 Mbit/s. 1080p30 is
        // reckoned at 10, so Quality has to settle for the next thing that
        // fits rather than proposing what the display cannot carry.
        let body = "wfd_video_formats: 40 00 02 04 000000E0 00000000 00000000 00 0000 0000 00 none none
";
        let mut s = SourceSession::new(SourceConfig {
            mode: Mode::Quality,
            ceiling_mbps: Some(9),
            ..SourceConfig::default()
        });
        s.start();
        s.on_message(&rtsp::response(200, 1, ""));
        s.on_message(&rtsp::response(200, 2, body));
        assert_eq!(s.chosen(), Some(P720P60));
    }

    #[test]
    fn a_teardown_carries_the_session() {
        let mut s = SourceSession::new(cfg());
        let m = s.teardown();
        assert!(matches!(&m.start, StartLine::Request { method, .. } if method == "TEARDOWN"));
        assert_eq!(m.header("session"), Some("1234567890"));
        assert_eq!(s.state(), SourceState::Done);
    }
}
