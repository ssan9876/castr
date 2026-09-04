//! Wi-Fi Display LPCM framing.
//!
//! The audio a sink expects is not plain PCM: each payload carries a short
//! header declaring its size and format, and the samples that follow are
//! big-endian. Getting either wrong produces static rather than silence, so
//! both are pinned by tests against literal bytes.
//!
//! `payload` strips a header only when one is demonstrably there - when the
//! declared size matches the bytes that follow. The alternative, stripping
//! unconditionally, would corrupt audio from any source that sends none, and
//! the sink already receives from a real Windows source today. Reading four
//! bytes of samples as a header is the cheaper mistake to make than the
//! reverse, and this way neither is made.

/// Bytes of audio data header ahead of the samples.
pub const HEADER_LEN: usize = 4;

/// 48 kHz, 16-bit, two channels - the only mode the sink offers.
const FORMAT_BYTE: u8 = 0x11;

/// Wraps interleaved stereo samples as one LPCM audio frame.
pub fn frame(samples: &[i16]) -> Vec<u8> {
    let payload_len = samples.len() * 2;
    let mut out = Vec::with_capacity(HEADER_LEN + payload_len);
    out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    out.push(FORMAT_BYTE);
    out.push(0);
    for s in samples {
        out.extend_from_slice(&s.to_be_bytes());
    }
    out
}

/// The samples of a frame, without the header if it has one.
pub fn payload(frame: &[u8]) -> &[u8] {
    if frame.len() < HEADER_LEN {
        return &[];
    }
    let declared = u16::from_be_bytes([frame[0], frame[1]]) as usize;
    if declared == frame.len() - HEADER_LEN {
        &frame[HEADER_LEN..]
    } else {
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_a_header_then_big_endian_samples() {
        // Big-endian is not the machine's order, and getting it wrong yields
        // loud static rather than silence - so it is pinned to literal bytes.
        let out = frame(&[0x0102, -2]);
        assert_eq!(out.len(), HEADER_LEN + 4);
        assert_eq!(&out[HEADER_LEN..], &[0x01, 0x02, 0xff, 0xfe]);
    }

    #[test]
    fn the_header_declares_the_payload_size() {
        let out = frame(&[0; 480]);
        let declared = u16::from_be_bytes([out[0], out[1]]) as usize;
        assert_eq!(declared, 960, "480 samples are 960 bytes");
    }

    #[test]
    fn payload_skips_a_header_that_is_really_there() {
        let out = frame(&[0x1234]);
        assert_eq!(payload(&out), &[0x12, 0x34]);
    }

    #[test]
    fn payload_keeps_everything_when_there_is_no_header() {
        // A source that sends bare samples must not lose the first two of
        // them. The declared length is what tells the two cases apart.
        let bare = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66];
        assert_eq!(payload(&bare), &bare);
    }

    #[test]
    fn a_runt_frame_yields_nothing_rather_than_panicking() {
        // A damaged stream must not take the process down.
        assert!(payload(&[0, 0]).is_empty());
        assert!(payload(&[]).is_empty());
    }

    #[test]
    fn a_frame_round_trips_through_payload() {
        let samples = [1i16, -1, 32767, -32768, 0];
        let f = frame(&samples);
        let got: Vec<i16> = payload(&f)
            .chunks_exact(2)
            .map(|b| i16::from_be_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(got, samples);
    }
}
