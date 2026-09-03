//! MPEG-TS demux: enough of the format to pull H.264 access units and LPCM
//! audio out of a Wi-Fi Display stream.
//!
//! Everything here is driven by whole 188-byte packets the caller supplies, so
//! it is testable with synthetic streams and has no I/O of its own. Sections
//! are read from the first packet that carries them; a PAT or PMT split across
//! packets is rare in a Miracast stream (both are far smaller than 184 bytes)
//! and is ignored rather than mis-parsed.

pub const PACKET_LEN: usize = 188;
const SYNC: u8 = 0x47;
/// Stream types we care about: H.264 video and LPCM audio.
const STREAM_TYPE_H264: u8 = 0x1b;
const STREAM_TYPE_LPCM: u8 = 0x83;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Video { data: Vec<u8>, pts_us: Option<u64> },
    Audio { data: Vec<u8>, pts_us: Option<u64> },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DemuxStats {
    pub continuity_errors: u64,
    pub video_pid: Option<u16>,
    pub audio_pid: Option<u16>,
}

#[derive(Default)]
struct Assembly {
    data: Vec<u8>,
    pts_us: Option<u64>,
    damaged: bool,
    open: bool,
    /// Payload length the PES header declared, when it declared one. A
    /// transport packet is a fixed 188 bytes and its tail is padding, so
    /// without this the padding would be appended to the access unit. Video
    /// PES packets are usually unbounded (length zero), and those end at the
    /// next PES start instead.
    expected: Option<usize>,
}

impl Assembly {
    /// Appends payload, stopping at the declared length if there is one.
    fn append(&mut self, bytes: &[u8]) {
        match self.expected {
            Some(want) => {
                let room = want.saturating_sub(self.data.len());
                self.data.extend_from_slice(&bytes[..room.min(bytes.len())]);
            }
            None => self.data.extend_from_slice(bytes),
        }
    }

    /// True once a declared-length unit has all of its bytes.
    fn complete(&self) -> bool {
        self.expected.is_some_and(|want| self.data.len() >= want)
    }
}

#[derive(Default)]
pub struct Demux {
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    video: Assembly,
    audio: Assembly,
    last_cc: std::collections::HashMap<u16, u8>,
    continuity_errors: u64,
}

impl Demux {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> DemuxStats {
        DemuxStats {
            continuity_errors: self.continuity_errors,
            video_pid: self.video_pid,
            audio_pid: self.audio_pid,
        }
    }

    /// Feeds one transport packet. Returns any access units it completed.
    pub fn push(&mut self, packet: &[u8]) -> Vec<Unit> {
        if packet.len() != PACKET_LEN || packet[0] != SYNC {
            return Vec::new();
        }
        let pid = (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16;
        let start = packet[1] & 0x40 != 0;
        let cc = packet[3] & 0x0f;
        let has_payload = packet[3] & 0x10 != 0;
        let has_adaptation = packet[3] & 0x20 != 0;
        if !has_payload {
            return Vec::new();
        }
        let mut off = 4;
        if has_adaptation {
            let len = packet[4] as usize;
            off = 5 + len;
            if off >= PACKET_LEN {
                return Vec::new();
            }
        }
        let payload = &packet[off..];

        // Continuity: the counter increments per packet on a PID. A jump means
        // a packet was lost, so whatever is being assembled is damaged.
        let expected = self.last_cc.get(&pid).map(|c| (c + 1) & 0x0f);
        self.last_cc.insert(pid, cc);
        if let Some(exp) = expected {
            if cc != exp {
                self.continuity_errors += 1;
                if Some(pid) == self.video_pid {
                    self.video.damaged = true;
                } else if Some(pid) == self.audio_pid {
                    self.audio.damaged = true;
                }
            }
        }

        if pid == 0 {
            self.read_pat(payload, start);
            return Vec::new();
        }
        if Some(pid) == self.pmt_pid {
            self.read_pmt(payload, start);
            return Vec::new();
        }
        let is_video = Some(pid) == self.video_pid;
        let is_audio = Some(pid) == self.audio_pid;
        if !is_video && !is_audio {
            return Vec::new();
        }
        let mut out = Vec::new();
        let asm = if is_video {
            &mut self.video
        } else {
            &mut self.audio
        };
        if start {
            // A new PES starts: emit whatever came before, if it is intact.
            if asm.open && !asm.damaged && !asm.data.is_empty() {
                let data = std::mem::take(&mut asm.data);
                let pts = asm.pts_us;
                out.push(if is_video {
                    Unit::Video { data, pts_us: pts }
                } else {
                    Unit::Audio { data, pts_us: pts }
                });
            } else {
                asm.data.clear();
            }
            asm.damaged = false;
            asm.open = true;
            match parse_pes(payload) {
                Some(pes) => {
                    asm.pts_us = pes.pts_us;
                    asm.expected = pes.payload_len;
                    asm.append(pes.body);
                }
                None => {
                    asm.open = false;
                    asm.damaged = true;
                    asm.expected = None;
                }
            }
        } else if asm.open {
            asm.append(payload);
        }
        // A unit whose length the header declared is done the moment those
        // bytes have arrived; one with no declared length waits for the next
        // PES start, handled above.
        if asm.open && asm.complete() && !asm.damaged {
            let data = std::mem::take(&mut asm.data);
            let pts = asm.pts_us;
            asm.open = false;
            asm.expected = None;
            out.push(if is_video {
                Unit::Video { data, pts_us: pts }
            } else {
                Unit::Audio { data, pts_us: pts }
            });
        }
        out
    }

    fn read_pat(&mut self, payload: &[u8], start: bool) {
        if !start || payload.is_empty() {
            return;
        }
        let ptr = payload[0] as usize;
        let Some(s) = payload.get(1 + ptr..) else {
            return;
        };
        // table_id(1) length(2) tsid(2) ver(1) sec(1) last(1) then entries.
        if s.len() < 12 || s[0] != 0x00 {
            return;
        }
        let program = &s[8..12];
        let pid = (((program[2] & 0x1f) as u16) << 8) | program[3] as u16;
        if pid != 0 {
            self.pmt_pid = Some(pid);
        }
    }

    fn read_pmt(&mut self, payload: &[u8], start: bool) {
        if !start || payload.is_empty() {
            return;
        }
        let ptr = payload[0] as usize;
        let Some(s) = payload.get(1 + ptr..) else {
            return;
        };
        if s.len() < 12 || s[0] != 0x02 {
            return;
        }
        let section_len = ((((s[1] & 0x0f) as usize) << 8) | s[2] as usize) + 3;
        if section_len < 16 {
            return;
        }
        let info_len = (((s[10] & 0x0f) as usize) << 8) | s[11] as usize;
        let mut i = 12 + info_len;
        // The last four bytes of the section are its CRC, which the caller's
        // transport already protects; stop before them.
        let end = section_len.saturating_sub(4).min(s.len());
        while i + 5 <= end {
            let stream_type = s[i];
            let pid = (((s[i + 1] & 0x1f) as u16) << 8) | s[i + 2] as u16;
            let es_info = (((s[i + 3] & 0x0f) as usize) << 8) | s[i + 4] as usize;
            match stream_type {
                STREAM_TYPE_H264 => self.video_pid = Some(pid),
                STREAM_TYPE_LPCM => self.audio_pid = Some(pid),
                _ => {}
            }
            i += 5 + es_info;
        }
    }
}

struct Pes<'a> {
    pts_us: Option<u64>,
    /// Payload length the header declared, or `None` when it declared zero,
    /// which means unbounded (the usual case for video).
    payload_len: Option<usize>,
    body: &'a [u8],
}

/// Reads a PES header: its timestamp, its declared payload length, and the
/// payload that follows.
fn parse_pes(p: &[u8]) -> Option<Pes<'_>> {
    if p.len() < 9 || p[0] != 0 || p[1] != 0 || p[2] != 1 {
        return None;
    }
    // Bytes 4-5 count everything after them: the three header bytes, the
    // optional-header extension, and the payload.
    let packet_len = ((p[4] as usize) << 8) | p[5] as usize;
    let flags = p[7];
    let header_len = p[8] as usize;
    let body_start = 9 + header_len;
    if p.len() < body_start {
        return None;
    }
    let payload_len = packet_len
        .checked_sub(3 + header_len)
        .filter(|_| packet_len != 0);
    let pts = if flags & 0x80 != 0 && header_len >= 5 {
        let b = &p[9..14];
        let v = (((b[0] as u64) >> 1) & 0x07) << 30
            | (b[1] as u64) << 22
            | (((b[2] as u64) >> 1) & 0x7f) << 15
            | (b[3] as u64) << 7
            | ((b[4] as u64) >> 1);
        // 90 kHz ticks to microseconds.
        Some(v * 1_000_000 / 90_000)
    } else {
        None
    };
    Some(Pes {
        pts_us: pts,
        payload_len,
        body: &p[body_start..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO_PID: u16 = 0x1011;
    const AUDIO_PID: u16 = 0x1100;

    /// One 188-byte TS packet: sync byte, flags, PID, continuity, payload.
    fn ts_packet(pid: u16, start: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 188];
        p[0] = 0x47;
        p[1] = ((start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
        p[2] = (pid & 0xff) as u8;
        p[3] = 0x10 | (cc & 0x0f); // payload only
        let n = payload.len().min(184);
        p[4..4 + n].copy_from_slice(&payload[..n]);
        p
    }

    /// PAT naming one program whose PMT lives on `pmt_pid`.
    fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut section = vec![0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00];
        section.extend_from_slice(&[0x00, 0x01]); // program number 1
        section.extend_from_slice(&[(0xe0 | (pmt_pid >> 8) as u8), (pmt_pid & 0xff) as u8]);
        section.extend_from_slice(&[0, 0, 0, 0]); // CRC, unchecked by the demux
        let mut payload = vec![0x00]; // pointer field
        payload.extend_from_slice(&section);
        ts_packet(0x0000, true, 0, &payload)
    }

    /// PMT naming an H.264 video stream and an LPCM audio stream.
    fn pmt(pmt_pid: u16) -> Vec<u8> {
        let mut es = Vec::new();
        es.extend_from_slice(&[
            0x1b,
            (0xe0 | (VIDEO_PID >> 8) as u8),
            (VIDEO_PID & 0xff) as u8,
            0xf0,
            0x00,
        ]);
        es.extend_from_slice(&[
            0x83,
            (0xe0 | (AUDIO_PID >> 8) as u8),
            (AUDIO_PID & 0xff) as u8,
            0xf0,
            0x00,
        ]);
        let section_len = 9 + es.len() + 4;
        let mut section = vec![
            0x02,
            0xb0 | ((section_len >> 8) as u8 & 0x0f),
            (section_len & 0xff) as u8,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0xe0 | (VIDEO_PID >> 8) as u8,
            (VIDEO_PID & 0xff) as u8,
            0xf0,
            0x00,
        ];
        section.extend_from_slice(&es);
        section.extend_from_slice(&[0, 0, 0, 0]);
        let mut payload = vec![0x00];
        payload.extend_from_slice(&section);
        ts_packet(pmt_pid, true, 0, &payload)
    }

    /// A PES header carrying `pts` (90 kHz) and `payload`.
    fn pes(stream_id: u8, pts_90k: Option<u64>, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x00, 0x00, 0x01, stream_id];
        let (flags, hdr_len, pts_bytes) = match pts_90k {
            Some(pts) => {
                // The 33-bit timestamp is split across five bytes with marker
                // bits, which is why it looks like this.
                let b = vec![
                    0x21 | ((((pts >> 30) & 0x07) as u8) << 1),
                    ((pts >> 22) & 0xff) as u8,
                    ((((pts >> 15) & 0x7f) as u8) << 1) | 1,
                    ((pts >> 7) & 0xff) as u8,
                    (((pts & 0x7f) as u8) << 1) | 1,
                ];
                (0x80u8, 5u8, b)
            }
            None => (0x00, 0, Vec::new()),
        };
        let len = 3 + hdr_len as usize + payload.len();
        p.push((len >> 8) as u8);
        p.push((len & 0xff) as u8);
        p.push(0x80);
        p.push(flags);
        p.push(hdr_len);
        p.extend_from_slice(&pts_bytes);
        p.extend_from_slice(payload);
        p
    }

    fn feed(d: &mut Demux, packets: Vec<Vec<u8>>) -> Vec<Unit> {
        packets.into_iter().flat_map(|p| d.push(&p)).collect()
    }

    #[test]
    fn the_tables_teach_it_which_pids_carry_what() {
        let mut d = Demux::new();
        feed(&mut d, vec![pat(0x1000), pmt(0x1000)]);
        assert_eq!(d.stats().video_pid, Some(VIDEO_PID));
        assert_eq!(d.stats().audio_pid, Some(AUDIO_PID));
    }

    #[test]
    fn a_video_access_unit_comes_out_with_its_timestamp() {
        let mut d = Demux::new();
        let au = [0u8, 0, 0, 1, 0x65, 1, 2, 3];
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(VIDEO_PID, true, 0, &pes(0xe0, Some(90_000), &au)));
        // A second PES start flushes the first.
        packets.push(ts_packet(VIDEO_PID, true, 1, &pes(0xe0, Some(93_000), &au)));
        let units = feed(&mut d, packets);
        match &units[0] {
            Unit::Video { data, pts_us } => {
                assert_eq!(data, &au);
                assert_eq!(*pts_us, Some(1_000_000), "90 kHz to microseconds");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_unit_split_across_packets_is_reassembled() {
        let mut d = Demux::new();
        let big: Vec<u8> = (0..400u32).map(|i| (i % 251) as u8).collect();
        let payload = pes(0xe0, Some(0), &big);
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(VIDEO_PID, true, 0, &payload[..184]));
        packets.push(ts_packet(VIDEO_PID, false, 1, &payload[184..368]));
        packets.push(ts_packet(VIDEO_PID, false, 2, &payload[368..]));
        packets.push(ts_packet(VIDEO_PID, true, 3, &pes(0xe0, Some(3000), &[9])));
        let units = feed(&mut d, packets);
        match &units[0] {
            Unit::Video { data, .. } => assert_eq!(data, &big),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn audio_is_separated_from_video() {
        let mut d = Demux::new();
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(
            AUDIO_PID,
            true,
            0,
            &pes(0xbd, Some(90_000), &[1, 2, 3, 4]),
        ));
        packets.push(ts_packet(
            AUDIO_PID,
            true,
            1,
            &pes(0xbd, Some(91_000), &[5]),
        ));
        let units = feed(&mut d, packets);
        assert!(matches!(&units[0], Unit::Audio { data, .. } if data == &[1, 2, 3, 4]));
    }

    #[test]
    fn a_continuity_break_is_counted_and_drops_the_damaged_unit() {
        let mut d = Demux::new();
        let payload = pes(0xe0, Some(0), &[7u8; 300]);
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(VIDEO_PID, true, 0, &payload[..184]));
        // cc jumps from 0 to 5: a packet was lost.
        packets.push(ts_packet(VIDEO_PID, false, 5, &payload[184..]));
        packets.push(ts_packet(VIDEO_PID, true, 6, &pes(0xe0, Some(3000), &[1])));
        let units = feed(&mut d, packets);
        assert_eq!(d.stats().continuity_errors, 1);
        assert!(
            !units
                .iter()
                .any(|u| matches!(u, Unit::Video { data, .. } if data.len() > 200)),
            "the damaged unit is not emitted"
        );
    }

    #[test]
    fn packets_before_the_tables_are_ignored_without_panicking() {
        let mut d = Demux::new();
        let units = d.push(&ts_packet(
            VIDEO_PID,
            true,
            0,
            &pes(0xe0, Some(0), &[1, 2, 3]),
        ));
        assert!(units.is_empty());
    }

    #[test]
    fn a_packet_with_no_sync_byte_is_rejected() {
        let mut d = Demux::new();
        let mut bad = ts_packet(VIDEO_PID, true, 0, &[1, 2, 3]);
        bad[0] = 0x00;
        assert!(d.push(&bad).is_empty());
    }

    #[test]
    fn an_adaptation_field_is_skipped() {
        let mut d = Demux::new();
        feed(&mut d, vec![pat(0x1000), pmt(0x1000)]);
        let inner = pes(0xe0, Some(0), &[4, 5, 6]);
        let mut p = vec![0u8; 188];
        p[0] = 0x47;
        p[1] = 0x40 | ((VIDEO_PID >> 8) as u8 & 0x1f);
        p[2] = (VIDEO_PID & 0xff) as u8;
        p[3] = 0x30; // adaptation field and payload
        p[4] = 7; // adaptation length
        p[12..12 + inner.len()].copy_from_slice(&inner);
        // The header declares its payload length, so the unit is complete
        // within this one packet: the adaptation field was skipped correctly
        // and none of it reached the payload.
        let units = d.push(&p);
        assert!(matches!(&units[0], Unit::Video { data, .. } if data == &[4, 5, 6]));
    }

    #[test]
    fn a_unit_with_a_declared_length_is_emitted_without_waiting_for_the_next() {
        let mut d = Demux::new();
        feed(&mut d, vec![pat(0x1000), pmt(0x1000)]);
        // One packet, one complete access unit: nothing is held back, which is
        // what keeps a frame from waiting on its successor.
        let units = d.push(&ts_packet(
            VIDEO_PID,
            true,
            0,
            &pes(0xe0, Some(90_000), &[0, 0, 0, 1, 0x65, 42]),
        ));
        assert_eq!(units.len(), 1);
        assert!(matches!(&units[0], Unit::Video { data, .. } if data == &[0, 0, 0, 1, 0x65, 42]));
    }

    #[test]
    fn an_unbounded_unit_still_waits_for_the_next_pes_start() {
        let mut d = Demux::new();
        feed(&mut d, vec![pat(0x1000), pmt(0x1000)]);
        // PES length zero means unbounded, which is what a source sends for
        // video it is still producing.
        let mut unbounded = pes(0xe0, Some(0), &[1, 2, 3]);
        unbounded[4] = 0;
        unbounded[5] = 0;
        assert!(d
            .push(&ts_packet(VIDEO_PID, true, 0, &unbounded))
            .is_empty());
        // The new start flushes the unbounded one, and the new unit declares
        // its own length, so it completes in the same packet: two units out.
        let units = d.push(&ts_packet(VIDEO_PID, true, 1, &pes(0xe0, Some(3000), &[9])));
        assert_eq!(units.len(), 2, "{units:?}");
        // The unbounded unit keeps whatever filled the rest of its transport
        // packet: with no declared length there is nothing to mark the end.
        // Real senders stuff the tail with an adaptation field instead of
        // padding the payload, so this only shows up with synthetic packets.
        assert!(matches!(&units[0], Unit::Video { data, .. } if data.starts_with(&[1, 2, 3])));
        assert!(matches!(&units[1], Unit::Video { data, .. } if data == &[9]));
    }
}
