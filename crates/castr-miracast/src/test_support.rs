//! Builders for synthetic Wi-Fi Display traffic.
//!
//! These are `pub` rather than `#[cfg(test)]` because the replay test in
//! `tests/` is a separate crate and cannot see test-only items. They exist so
//! the whole media path can be exercised without a radio: if a replay passes
//! and the hardware does not, the fault is in the radio layer.
#![doc(hidden)]

pub const VIDEO_PID: u16 = 0x1011;
pub const AUDIO_PID: u16 = 0x1100;
pub const PMT_PID: u16 = 0x1000;

/// One 188-byte transport packet: sync byte, flags, PID, continuity, payload.
pub fn ts_packet(pid: u16, start: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 188];
    p[0] = 0x47;
    p[1] = ((start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
    p[2] = (pid & 0xff) as u8;
    p[3] = 0x10 | (cc & 0x0f); // payload only
    let n = payload.len().min(184);
    p[4..4 + n].copy_from_slice(&payload[..n]);
    p
}

/// A program association table naming one program, whose map lives on `PMT_PID`.
pub fn pat() -> Vec<u8> {
    let mut section = vec![0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00];
    section.extend_from_slice(&[0x00, 0x01]); // program number 1
    section.extend_from_slice(&[(0xe0 | (PMT_PID >> 8) as u8), (PMT_PID & 0xff) as u8]);
    section.extend_from_slice(&[0, 0, 0, 0]); // CRC, unchecked by the demux
    let mut payload = vec![0x00]; // pointer field
    payload.extend_from_slice(&section);
    ts_packet(0x0000, true, 0, &payload)
}

/// A program map naming an H.264 video stream and an LPCM audio stream.
pub fn pmt() -> Vec<u8> {
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
    ts_packet(PMT_PID, true, 0, &payload)
}

/// A PES packet carrying `pts` (90 kHz ticks) and `payload`.
pub fn pes(stream_id: u8, pts_90k: Option<u64>, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0x00, 0x00, 0x01, stream_id];
    let (flags, hdr_len, pts_bytes) = match pts_90k {
        Some(pts) => {
            // The 33-bit timestamp is split across five bytes with marker bits.
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

/// Wraps transport packets in an RTP header, seven to a datagram, which is
/// what a real source sends.
pub fn rtp_datagram(sequence: u16, timestamp: u32, packets: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0x80, 33];
    v.extend_from_slice(&sequence.to_be_bytes());
    v.extend_from_slice(&timestamp.to_be_bytes());
    v.extend_from_slice(&0x1234_5678u32.to_be_bytes()); // SSRC
    for p in packets {
        v.extend_from_slice(p);
    }
    v
}

/// The RTSP messages a source sends to drive a sink from idle to playing,
/// in order, each one complete.
pub fn negotiation_to_playing() -> Vec<String> {
    let choose =
        "wfd_video_formats: 00 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
                  wfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n";
    let trigger = "wfd_trigger_method: SETUP\r\n";
    let ask = "wfd_video_formats\r\n";
    vec![
        "OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.wfa.wfd1.0\r\n\r\n".to_string(),
        format!(
            "GET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 2\r\nContent-Length: {}\r\n\r\n{}",
            ask.len(),
            ask
        ),
        format!(
            "SET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 3\r\nContent-Length: {}\r\n\r\n{}",
            choose.len(),
            choose
        ),
        format!(
            "SET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 4\r\nContent-Length: {}\r\n\r\n{}",
            trigger.len(),
            trigger
        ),
        "RTSP/1.0 200 OK\r\nCSeq: 100\r\nSession: abcdef12;timeout=60\r\n\r\n".to_string(),
    ]
}

/// A recording of `units` video access units with ascending timestamps, as
/// the datagrams a source would send: the tables first, then the units.
pub fn recorded_stream(units: u32) -> Vec<Vec<u8>> {
    let mut out = vec![rtp_datagram(0, 0, &[pat(), pmt()])];
    let mut cc = 0u8;
    for i in 0..units {
        // An Annex B access unit whose payload is distinguishable per frame.
        let mut au = vec![0u8, 0, 0, 1, 0x65];
        au.extend((0..20u32).map(|b| ((i * 7 + b) % 251) as u8));
        let pts = 90_000 + i as u64 * 3000;
        let packet = ts_packet(VIDEO_PID, true, cc, &pes(0xe0, Some(pts), &au));
        cc = (cc + 1) & 0x0f;
        out.push(rtp_datagram((i + 1) as u16, pts as u32, &[packet]));
    }
    out
}
