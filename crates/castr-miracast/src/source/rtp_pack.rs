//! RTP packetization for the source role.
//!
//! Wi-Fi Display carries MPEG-TS in payload type 33, seven transport packets to
//! a datagram: 1316 bytes of payload, which with headers stays inside an
//! ordinary 1500-byte path without fragmenting.
//!
//! The mirror of `rtp.rs`, and tested through it.

use crate::ts::PACKET_LEN;

pub const PAYLOAD_TYPE: u8 = 33;
pub const PACKETS_PER_DATAGRAM: usize = 7;
/// RTP's fixed header, with no CSRC list and no extension.
const HEADER_LEN: usize = 12;

pub struct Packetizer {
    ssrc: u32,
    sequence: u16,
}

impl Packetizer {
    pub fn new(ssrc: u32) -> Self {
        Self { ssrc, sequence: 0 }
    }

    /// Splits whole transport packets into datagrams, all stamped with the same
    /// presentation time: they belong to one access unit.
    ///
    /// A short tail is sent rather than held. Waiting for seven would delay the
    /// end of every frame, and the sink reassembles from the transport stream
    /// regardless of how the datagrams were cut.
    pub fn push(&mut self, ts_packets: &[u8], timestamp_90k: u32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for group in ts_packets.chunks(PACKETS_PER_DATAGRAM * PACKET_LEN) {
            let mut d = Vec::with_capacity(HEADER_LEN + group.len());
            d.push(0x80); // version 2, no padding, no extension, no CSRC
            d.push(PAYLOAD_TYPE);
            d.extend_from_slice(&self.sequence.to_be_bytes());
            d.extend_from_slice(&timestamp_90k.to_be_bytes());
            d.extend_from_slice(&self.ssrc.to_be_bytes());
            d.extend_from_slice(group);
            self.sequence = self.sequence.wrapping_add(1);
            out.push(d);
        }
        out
    }

    #[cfg(test)]
    fn set_sequence_for_test(&mut self, seq: u16) {
        self.sequence = seq;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp;

    fn ts_packets(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            let mut p = [0u8; PACKET_LEN];
            p[0] = 0x47;
            p[4] = i as u8;
            v.extend_from_slice(&p);
        }
        v
    }

    #[test]
    fn seven_transport_packets_make_one_datagram() {
        let mut p = Packetizer::new(1);
        let out = p.push(&ts_packets(7), 0);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].len(),
            HEADER_LEN + 7 * PACKET_LEN,
            "header plus 1316 bytes"
        );
    }

    #[test]
    fn our_own_parser_reads_what_we_wrote() {
        let mut p = Packetizer::new(0xdead_beef);
        let out = p.push(&ts_packets(14), 900);
        assert_eq!(out.len(), 2);
        let first = rtp::parse(&out[0]).expect("unparseable datagram");
        assert_eq!(first.payload_type, PAYLOAD_TYPE);
        assert_eq!(first.timestamp, 900);
        assert_eq!(first.payload.len(), 7 * PACKET_LEN);
        let second = rtp::parse(&out[1]).expect("unparseable datagram");
        assert_eq!(
            second.sequence,
            first.sequence.wrapping_add(1),
            "sequence numbers must advance by one"
        );
    }

    #[test]
    fn a_short_tail_is_still_sent() {
        // Holding it back would delay the end of every frame.
        let mut p = Packetizer::new(1);
        let out = p.push(&ts_packets(9), 0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].len(), HEADER_LEN + 2 * PACKET_LEN);
    }

    #[test]
    fn sequence_numbers_wrap_rather_than_overflow() {
        let mut p = Packetizer::new(1);
        p.set_sequence_for_test(u16::MAX);
        let out = p.push(&ts_packets(14), 0);
        assert_eq!(rtp::parse(&out[0]).unwrap().sequence, u16::MAX);
        assert_eq!(rtp::parse(&out[1]).unwrap().sequence, 0);
    }

    #[test]
    fn the_payload_is_the_transport_stream_unchanged() {
        // Anything added or dropped here would desynchronise the demuxer.
        let mut p = Packetizer::new(1);
        let ts = ts_packets(7);
        let out = p.push(&ts, 0);
        assert_eq!(rtp::parse(&out[0]).unwrap().payload, ts);
    }

    #[test]
    fn nothing_in_makes_nothing_out() {
        let mut p = Packetizer::new(1);
        assert!(p.push(&[], 0).is_empty());
    }

    #[test]
    fn a_whole_frame_survives_the_reordering_window() {
        // The sink holds packets briefly before handing them on; what we send
        // must come out of that window in the order it went in.
        let mut p = Packetizer::new(1);
        let out = p.push(&ts_packets(21), 4_500);
        let mut window = rtp::Reorder::new(8);
        let mut seen = Vec::new();
        for d in &out {
            seen.extend(window.push(rtp::parse(d).expect("unparseable")));
        }
        seen.extend(window.flush());
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].sequence, 0);
        assert_eq!(seen[2].sequence, 2);
        assert_eq!(window.lost(), 0, "nothing was lost, so nothing may be counted");
    }
}
