//! MPEG-TS multiplexing for the source role: access units in, 188-byte
//! transport packets out.
//!
//! The mirror of `ts.rs`, and tested through it: anything our own demuxer
//! cannot read back was not going to survive a television either. Everything
//! is driven by whole access units the caller supplies, so there is no I/O and
//! no clock of its own.

use crate::ts::PACKET_LEN;
use std::collections::HashMap;

pub const PMT_PID: u16 = 0x1000;
pub const VIDEO_PID: u16 = 0x1011;
pub const AUDIO_PID: u16 = 0x1100;
const PROGRAM_NUMBER: u16 = 1;
const STREAM_TYPE_H264: u8 = 0x1b;
const STREAM_TYPE_LPCM: u8 = 0x83;
const STREAM_ID_VIDEO: u8 = 0xe0;
const STREAM_ID_PRIVATE_1: u8 = 0xbd;
/// Program tables are repeated this often, counted in payload packets. A sink
/// that tunes in late, or drops the one copy, must still learn the program
/// rather than give up on the stream.
const TABLE_INTERVAL_PACKETS: u32 = 40;

/// A 33-bit presentation time in 90 kHz units, spread over five bytes with the
/// marker bits the format requires.
fn pts_bytes(pts_90k: u64) -> [u8; 5] {
    [
        0x21 | ((((pts_90k >> 30) & 0x07) as u8) << 1),
        ((pts_90k >> 22) & 0xff) as u8,
        ((((pts_90k >> 15) & 0x7f) as u8) << 1) | 1,
        ((pts_90k >> 7) & 0xff) as u8,
        (((pts_90k & 0x7f) as u8) << 1) | 1,
    ]
}

/// One PES packet.
///
/// Video declares length zero - it is unbounded, and ends at the next PES
/// start - while audio declares its true length, because the demuxer uses that
/// to know where the payload stops and the packet's padding begins.
fn pes(stream_id: u8, payload: &[u8], pts_us: u64, bounded: bool) -> Vec<u8> {
    let header = pts_bytes(pts_us * 9 / 100);
    let declared = if bounded {
        (payload.len() + header.len() + 3) as u16
    } else {
        0
    };
    let mut v = Vec::with_capacity(9 + header.len() + payload.len());
    v.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);
    v.extend_from_slice(&declared.to_be_bytes());
    v.push(0x80); // '10' marker, not scrambled, no priority
    v.push(0x80); // PTS present, no DTS
    v.push(header.len() as u8);
    v.extend_from_slice(&header);
    v.extend_from_slice(payload);
    v
}

/// A section's CRC: the MPEG variant, most significant bit first, no final
/// inversion.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for byte in data {
        crc ^= (*byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Table id, then the syntax indicator and length, then the body and its CRC.
fn section(table_id: u8, body: &[u8]) -> Vec<u8> {
    let mut s = vec![table_id];
    let len = body.len() as u16 + 4; // the body plus the CRC that follows it
    s.extend_from_slice(&(0xb000 | len).to_be_bytes());
    s.extend_from_slice(body);
    let crc = crc32(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

fn pat_section() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes()); // transport stream id
    body.push(0xc1); // version 0, current
    body.push(0x00); // section number
    body.push(0x00); // last section number
    body.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
    body.extend_from_slice(&(0xe000 | PMT_PID).to_be_bytes());
    section(0x00, &body)
}

fn pmt_section() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
    body.push(0xc1);
    body.push(0x00);
    body.push(0x00);
    body.extend_from_slice(&(0xe000 | VIDEO_PID).to_be_bytes()); // the PCR rides on video
    body.extend_from_slice(&0xf000u16.to_be_bytes()); // no program info
    for (stream_type, pid) in [
        (STREAM_TYPE_H264, VIDEO_PID),
        (STREAM_TYPE_LPCM, AUDIO_PID),
    ] {
        body.push(stream_type);
        body.extend_from_slice(&(0xe000 | pid).to_be_bytes());
        body.extend_from_slice(&0xf000u16.to_be_bytes()); // no descriptors
    }
    section(0x02, &body)
}

/// Wraps a section in one transport packet, padded to length with 0xff.
fn section_packet(pid: u16, section: &[u8], cc: u8) -> Vec<u8> {
    let mut p = Vec::with_capacity(PACKET_LEN);
    p.push(0x47);
    p.push(0x40 | ((pid >> 8) as u8 & 0x1f)); // payload unit start
    p.push((pid & 0xff) as u8);
    p.push(0x10 | (cc & 0x0f)); // payload only
    p.push(0x00); // pointer field: the section starts immediately
    p.extend_from_slice(section);
    p.resize(PACKET_LEN, 0xff);
    p
}

/// The PCR, as six bytes: a 33-bit 90 kHz base, six reserved bits, then a
/// 9-bit extension we leave at zero.
fn pcr_bytes(pcr_90k: u64) -> [u8; 6] {
    [
        ((pcr_90k >> 25) & 0xff) as u8,
        ((pcr_90k >> 17) & 0xff) as u8,
        ((pcr_90k >> 9) & 0xff) as u8,
        ((pcr_90k >> 1) & 0xff) as u8,
        (((pcr_90k & 1) as u8) << 7) | 0x7e,
        0x00,
    ]
}

#[derive(Default)]
pub struct Muxer {
    cc: HashMap<u16, u8>,
    since_tables: u32,
}

impl Muxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The next continuity counter for a PID. The demuxer reads a gap here as
    /// loss, so it must advance by exactly one per packet carrying payload.
    fn next_cc(&mut self, pid: u16) -> u8 {
        let c = self.cc.entry(pid).or_insert(0x0f);
        *c = (*c + 1) & 0x0f;
        *c
    }

    fn tables_if_due(&mut self) -> Vec<u8> {
        if self.since_tables != 0 && self.since_tables < TABLE_INTERVAL_PACKETS {
            return Vec::new();
        }
        self.since_tables = 0;
        let pat_cc = self.next_cc(0);
        let pmt_cc = self.next_cc(PMT_PID);
        let mut out = section_packet(0, &pat_section(), pat_cc);
        out.extend(section_packet(PMT_PID, &pmt_section(), pmt_cc));
        out
    }

    /// Splits one PES across transport packets.
    ///
    /// The first carries the payload-unit-start indicator, and a short last
    /// packet is padded by growing its adaptation field rather than by stuffing
    /// bytes into the payload, which the demuxer would hand on as data.
    fn packetize(&mut self, pid: u16, pes: &[u8], pcr_90k: Option<u64>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut offset = 0;
        let mut first = true;
        while offset < pes.len() {
            let remaining = pes.len() - offset;
            let mut af: Option<Vec<u8>> = match (first, pcr_90k) {
                (true, Some(pcr)) => {
                    let mut v = vec![0x10]; // PCR present
                    v.extend_from_slice(&pcr_bytes(pcr));
                    Some(v)
                }
                _ => None,
            };
            let capacity = |af: &Option<Vec<u8>>| {
                PACKET_LEN - 4 - af.as_ref().map_or(0, |v| 1 + v.len())
            };
            if remaining < capacity(&af) {
                // Pad to the full packet through the adaptation field. Its
                // content is one flags byte then stuffing, except at exactly
                // one byte of slack, where a zero length says it all.
                let want = PACKET_LEN - 5 - remaining;
                let mut v = af.take().unwrap_or_default();
                if v.is_empty() && want > 0 {
                    v.push(0x00);
                }
                v.resize(want, 0xff);
                af = Some(v);
            }
            let take = remaining.min(capacity(&af));
            let cc = self.next_cc(pid);
            let mut p = Vec::with_capacity(PACKET_LEN);
            p.push(0x47);
            p.push(if first { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1f));
            p.push((pid & 0xff) as u8);
            p.push(if af.is_some() { 0x30 } else { 0x10 } | (cc & 0x0f));
            if let Some(v) = &af {
                p.push(v.len() as u8);
                p.extend_from_slice(v);
            }
            p.extend_from_slice(&pes[offset..offset + take]);
            debug_assert_eq!(p.len(), PACKET_LEN, "a transport packet is a fixed size");
            out.extend(p);
            offset += take;
            first = false;
            self.since_tables += 1;
        }
        out
    }

    /// One access unit, with the program tables ahead of it when they are due.
    pub fn push_video(&mut self, au: &[u8], pts_us: u64) -> Vec<u8> {
        let mut out = self.tables_if_due();
        let pts_90k = pts_us * 9 / 100;
        let pes = pes(STREAM_ID_VIDEO, au, pts_us, false);
        out.extend(self.packetize(VIDEO_PID, &pes, Some(pts_90k)));
        out
    }

    /// Interleaved stereo samples, framed as LPCM.
    pub fn push_audio(&mut self, samples: &[i16], pts_us: u64) -> Vec<u8> {
        let mut out = self.tables_if_due();
        let frame = crate::source::lpcm::frame(samples);
        let pes = pes(STREAM_ID_PRIVATE_1, &frame, pts_us, true);
        out.extend(self.packetize(AUDIO_PID, &pes, None));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::{Demux, Unit};

    /// Feeds a muxed buffer through the sink's demuxer, as the wire would.
    fn round_trip(buf: &[u8]) -> Vec<Unit> {
        let mut d = Demux::new();
        let mut units = Vec::new();
        for p in buf.chunks(PACKET_LEN) {
            units.extend(d.push(p));
        }
        units
    }

    #[test]
    fn every_emitted_buffer_is_whole_packets() {
        let mut m = Muxer::new();
        let out = m.push_video(&[0, 0, 0, 1, 0x65, 0xaa], 0);
        assert!(!out.is_empty());
        assert_eq!(out.len() % PACKET_LEN, 0, "a partial packet is unsendable");
        assert!(out.chunks(PACKET_LEN).all(|p| p[0] == 0x47), "lost sync byte");
    }

    #[test]
    fn an_access_unit_survives_our_own_demuxer() {
        // The strongest cheap check available: the sink already reads a real
        // source's stream, so what it reads back is at least self-consistent.
        let au: Vec<u8> = [0, 0, 0, 1, 0x65]
            .iter()
            .copied()
            .chain(0..200u8)
            .collect();
        let mut m = Muxer::new();
        let mut buf = m.push_video(&au, 1_000);
        buf.extend(m.push_video(&au, 34_000));
        let units = round_trip(&buf);
        assert!(
            units
                .iter()
                .any(|u| matches!(u, Unit::Video { data, .. } if *data == au)),
            "the access unit did not come back intact"
        );
    }

    #[test]
    fn a_units_presentation_time_survives() {
        let mut m = Muxer::new();
        let mut buf = m.push_video(&[0, 0, 0, 1, 0x65, 1], 1_000_000);
        buf.extend(m.push_video(&[0, 0, 0, 1, 0x65, 2], 2_000_000));
        let units = round_trip(&buf);
        match units.first().expect("no unit came back") {
            Unit::Video { pts_us, .. } => assert_eq!(*pts_us, Some(1_000_000)),
            other => panic!("expected video, got {other:?}"),
        }
    }

    #[test]
    fn audio_survives_with_its_header_intact() {
        let mut m = Muxer::new();
        let buf = m.push_audio(&[0x0102; 480], 1_000);
        let units = round_trip(&buf);
        let audio = units
            .iter()
            .find_map(|u| match u {
                Unit::Audio { data, .. } => Some(data.clone()),
                _ => None,
            })
            .expect("no audio unit came back");
        assert_eq!(crate::source::lpcm::payload(&audio).len(), 960);
    }

    #[test]
    fn continuity_counters_do_not_break() {
        let mut m = Muxer::new();
        let mut buf = Vec::new();
        for i in 0..40u64 {
            buf.extend(m.push_video(&[0, 0, 0, 1, 0x41, i as u8], i * 33_000));
        }
        let mut d = Demux::new();
        for p in buf.chunks(PACKET_LEN) {
            d.push(p);
        }
        assert_eq!(
            d.stats().continuity_errors,
            0,
            "the demuxer saw a gap in a stream that was never lossy"
        );
    }

    #[test]
    fn the_program_tables_are_repeated_not_sent_once() {
        // A sink that tunes in late, or drops a packet, must still learn the
        // program rather than give up.
        let mut m = Muxer::new();
        let mut buf = Vec::new();
        for i in 0..60u64 {
            buf.extend(m.push_video(&[0, 0, 0, 1, 0x41, 1], i * 33_000));
        }
        let pat = buf
            .chunks(PACKET_LEN)
            .filter(|p| (((p[1] & 0x1f) as u16) << 8 | p[2] as u16) == 0)
            .count();
        assert!(pat >= 2, "the program association table was sent {pat} times");
    }

    #[test]
    fn the_demuxer_learns_both_streams_from_our_tables() {
        let mut m = Muxer::new();
        let mut buf = m.push_video(&[0, 0, 0, 1, 0x65, 1], 0);
        buf.extend(m.push_audio(&[0; 48], 0));
        let mut d = Demux::new();
        for p in buf.chunks(PACKET_LEN) {
            d.push(p);
        }
        let stats = d.stats();
        assert_eq!(stats.video_pid, Some(VIDEO_PID));
        assert_eq!(stats.audio_pid, Some(AUDIO_PID));
    }

    #[test]
    fn an_access_unit_spanning_many_packets_survives() {
        // A keyframe is far larger than one packet, and the padding rule only
        // shows itself on the last of a long run.
        let au: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut m = Muxer::new();
        let mut buf = m.push_video(&au, 0);
        buf.extend(m.push_video(&[0, 0, 0, 1, 0x41, 9], 33_000));
        let units = round_trip(&buf);
        assert!(
            units
                .iter()
                .any(|u| matches!(u, Unit::Video { data, .. } if *data == au)),
            "a multi-packet access unit came back damaged"
        );
    }
}
