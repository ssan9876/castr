//! One sink session: RTSP negotiation, RTP reception, MPEG-TS demux, and the
//! recovery rules, expressed as bytes in and events out. No sockets here, so
//! the whole media path can be replayed from a recording in a test.

use crate::rtp;
use crate::rtsp::{self, Action, Negotiation, VideoMode};
use crate::ts::{self, Unit};
use crate::wfd::Capabilities;
use std::time::Instant;

/// RTP payload type for MPEG-TS, which is what Wi-Fi Display carries.
const PT_MP2T: u8 = 33;
/// Packets held while waiting for a late one.
const REORDER_WINDOW: usize = 32;

#[derive(Debug)]
pub enum SinkEvent {
    /// Formatted RTSP text the caller must write to the control socket.
    SendRtsp(String),
    Video {
        data: Vec<u8>,
        pts_us: Option<u64>,
    },
    Audio {
        data: Vec<u8>,
        pts_us: Option<u64>,
    },
    Started(VideoMode),
    Ended(&'static str),
}

pub struct Session {
    negotiation: Negotiation,
    rtsp_buf: Vec<u8>,
    reorder: rtp::Reorder,
    demux: ts::Demux,
    playing: bool,
    /// Loss seen since the last keyframe request, so a gap can trigger one.
    lost_at_last_check: u64,
}

impl Session {
    pub fn new(caps: Capabilities, session_id: String) -> Self {
        Self {
            negotiation: Negotiation::new(caps, session_id),
            rtsp_buf: Vec::new(),
            reorder: rtp::Reorder::new(REORDER_WINDOW),
            demux: ts::Demux::new(),
            playing: false,
            lost_at_last_check: 0,
        }
    }

    /// Feeds bytes read from the RTSP socket; returns what to send back.
    pub fn on_rtsp_bytes(&mut self, bytes: &[u8]) -> Vec<SinkEvent> {
        self.on_rtsp_bytes_at(bytes, Instant::now())
    }

    /// The clock is injected so the keep-alive and IDR rules stay testable.
    pub fn on_rtsp_bytes_at(&mut self, bytes: &[u8], now: Instant) -> Vec<SinkEvent> {
        self.rtsp_buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            match rtsp::parse(&self.rtsp_buf) {
                Ok(Some((msg, used))) => {
                    self.rtsp_buf.drain(..used);
                    let actions = self.negotiation.on_message_at(&msg, now);
                    self.apply(actions, &mut out);
                }
                Ok(None) => break,
                Err(e) => {
                    self.rtsp_buf.clear();
                    tracing::warn!("miracast: bad RTSP message: {e}");
                    out.push(SinkEvent::Ended("malformed RTSP"));
                    break;
                }
            }
        }
        out
    }

    /// Feeds one UDP datagram from the RTP socket.
    pub fn on_rtp_datagram(&mut self, datagram: &[u8]) -> Vec<SinkEvent> {
        self.on_rtp_datagram_at(datagram, Instant::now())
    }

    pub fn on_rtp_datagram_at(&mut self, datagram: &[u8], now: Instant) -> Vec<SinkEvent> {
        if !self.playing {
            return Vec::new();
        }
        let Some(packet) = rtp::parse(datagram) else {
            return Vec::new();
        };
        if packet.payload_type != PT_MP2T {
            return Vec::new();
        }
        let mut out = Vec::new();
        for p in self.reorder.push(packet) {
            for chunk in p.payload.chunks(ts::PACKET_LEN) {
                for unit in self.demux.push(chunk) {
                    out.push(match unit {
                        Unit::Video { data, pts_us } => SinkEvent::Video { data, pts_us },
                        Unit::Audio { data, pts_us } => SinkEvent::Audio { data, pts_us },
                    });
                }
            }
        }
        // A lost packet or a continuity break damaged a frame: ask for an IDR
        // rather than waiting for the source's next scheduled keyframe.
        let lost = self.reorder.lost() + self.demux.stats().continuity_errors;
        if lost > self.lost_at_last_check {
            self.lost_at_last_check = lost;
            let actions = self.negotiation.request_idr(now);
            self.apply(actions, &mut out);
        }
        out
    }

    /// Time-driven work: keep-alives and the silence timeout.
    pub fn tick(&mut self, now: Instant) -> Vec<SinkEvent> {
        let actions = self.negotiation.tick(now);
        let mut out = Vec::new();
        self.apply(actions, &mut out);
        out
    }

    /// The decoder lost its reference; ask the source for a keyframe.
    pub fn note_decode_error(&mut self, now: Instant) -> Vec<SinkEvent> {
        let actions = self.negotiation.request_idr(now);
        let mut out = Vec::new();
        self.apply(actions, &mut out);
        out
    }

    fn apply(&mut self, actions: Vec<Action>, out: &mut Vec<SinkEvent>) {
        for a in actions {
            match a {
                Action::Send(m) => out.push(SinkEvent::SendRtsp(m.format())),
                Action::Play => {
                    self.playing = true;
                    if let Some(mode) = self.negotiation.chosen_video() {
                        out.push(SinkEvent::Started(mode));
                    }
                }
                Action::Teardown(reason) => {
                    self.playing = false;
                    out.push(SinkEvent::Ended(reason));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
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

    fn sess() -> Session {
        Session::new(caps(), "01234567".into())
    }

    fn sent(events: &[SinkEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                SinkEvent::SendRtsp(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn drive_to_playing(s: &mut Session) {
        for msg in test_support::negotiation_to_playing() {
            s.on_rtsp_bytes(msg.as_bytes());
        }
    }

    #[test]
    fn rtsp_bytes_arriving_in_pieces_are_answered_once_complete() {
        let mut s = sess();
        let m1 = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.wfa.wfd1.0\r\n\r\n";
        assert!(
            s.on_rtsp_bytes(&m1[..12]).is_empty(),
            "partial message waits"
        );
        let out = s.on_rtsp_bytes(&m1[12..]);
        assert_eq!(sent(&out).len(), 2, "the M1 answer and our M2");
    }

    #[test]
    fn two_messages_in_one_read_are_both_handled() {
        let mut s = sess();
        let mut buf = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n".to_vec();
        buf.extend_from_slice(
            b"GET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 19\r\n\r\nwfd_video_formats\r\n",
        );
        let out = s.on_rtsp_bytes(&buf);
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 3, "M1 answer, M2, M3 answer");
        assert!(msgs[2].contains("wfd_video_formats: 40 00 02 04 00000020"));
    }

    #[test]
    fn a_video_unit_reaches_the_caller_as_an_event() {
        let mut s = sess();
        drive_to_playing(&mut s);
        let mut got = None;
        for datagram in test_support::recorded_stream(4) {
            for e in s.on_rtp_datagram(&datagram) {
                if let SinkEvent::Video { data, .. } = e {
                    got = Some(data);
                    break;
                }
            }
            if got.is_some() {
                break;
            }
        }
        let data = got.expect("no video unit was emitted");
        assert!(data.starts_with(&[0, 0, 0, 1]), "Annex B access unit");
    }

    #[test]
    fn a_decode_error_asks_the_source_for_a_keyframe_at_most_twice_a_second() {
        let mut s = sess();
        drive_to_playing(&mut s);
        let t0 = Instant::now();
        let first = s.note_decode_error(t0);
        assert!(
            sent(&first)[0].contains("wfd_idr_request"),
            "{:?}",
            sent(&first)
        );
        assert!(sent(&s.note_decode_error(t0 + Duration::from_millis(100))).is_empty());
        assert!(!sent(&s.note_decode_error(t0 + Duration::from_millis(700))).is_empty());
    }

    #[test]
    fn silence_ends_the_session_with_a_reason() {
        let mut s = sess();
        drive_to_playing(&mut s);
        let out = s.tick(Instant::now() + Duration::from_secs(120));
        assert!(
            out.iter().any(|e| matches!(e, SinkEvent::Ended(_))),
            "{out:?}"
        );
    }

    #[test]
    fn media_before_play_is_ignored() {
        let mut s = sess();
        let mut rtp = vec![0x80, 33, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        rtp.extend_from_slice(&[0x47; 188]);
        assert!(s.on_rtp_datagram(&rtp).is_empty());
    }

    #[test]
    fn a_malformed_rtsp_message_ends_the_session_rather_than_looping() {
        let mut s = sess();
        let out = s.on_rtsp_bytes(b"GARBAGE\r\n\r\n");
        assert!(
            out.iter().any(|e| matches!(e, SinkEvent::Ended(_))),
            "{out:?}"
        );
    }
}
