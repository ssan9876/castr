//! Wi-Fi Display LPCM framing.
//!
//! The audio a sink expects is not plain PCM: each payload carries a short
//! header declaring its size and format, and the samples that follow are
//! big-endian. Getting either wrong produces static rather than silence, so
//! both are pinned by tests against literal bytes.
//!
//! `payload` strips a header only when the WFD LPCM substream id and non-zero
//! access-unit count are present. The alternative, stripping
//! unconditionally, would corrupt audio from any source that sends none, and
//! the sink already receives from a real Windows source today. Reading four
//! bytes of samples as a header is the cheaper mistake to make than the
//! reverse, and this way neither is made.

/// Bytes of the WFD LPCM private header ahead of the samples.
pub const HEADER_LEN: usize = 4;

/// One WFD LPCM access unit is 80 samples per channel. Six access units make
/// the required ten-millisecond 48 kHz stereo PES payload.
pub const SAMPLES_PER_AU_PER_CHANNEL: usize = 80;
pub const CHANNELS: usize = 2;
pub const AUS_PER_FRAME: usize = 6;
pub const SAMPLES_PER_FRAME: usize = SAMPLES_PER_AU_PER_CHANNEL * CHANNELS * AUS_PER_FRAME;

const SUBSTREAM_ID: u8 = 0xa0;
/// 16-bit (00), 48 kHz (010), stereo/channels-minus-one (001).
const FORMAT_BYTE: u8 = 0x11;

/// Wraps interleaved stereo samples as one LPCM audio frame.
pub fn frame(samples: &[i16]) -> Vec<u8> {
    assert_eq!(
        samples.len(),
        SAMPLES_PER_FRAME,
        "a WFD LPCM PES carries six 80-sample stereo access units"
    );
    let payload_len = samples.len() * 2;
    let mut out = Vec::with_capacity(HEADER_LEN + payload_len);
    out.push(SUBSTREAM_ID);
    out.push(AUS_PER_FRAME as u8);
    out.push(0); // reserved and audio_emphasis_flag=0
    out.push(FORMAT_BYTE);
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
    if frame[0] == SUBSTREAM_ID && frame[1] > 0 {
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
        let mut samples = vec![0; SAMPLES_PER_FRAME];
        samples[0] = 0x0102;
        samples[1] = -2;
        let out = frame(&samples);
        assert_eq!(&out[..HEADER_LEN], &[0xa0, 0x06, 0x00, 0x11]);
        assert_eq!(&out[HEADER_LEN..HEADER_LEN + 4], &[0x01, 0x02, 0xff, 0xfe]);
    }

    #[test]
    fn the_frame_has_the_specified_ten_milliseconds_of_audio() {
        let out = frame(&vec![0; SAMPLES_PER_FRAME]);
        assert_eq!(out.len(), HEADER_LEN + 1920);
    }

    #[test]
    fn payload_skips_a_header_that_is_really_there() {
        let mut samples = vec![0; SAMPLES_PER_FRAME];
        samples[0] = 0x1234;
        let out = frame(&samples);
        assert_eq!(&payload(&out)[..2], &[0x12, 0x34]);
    }

    #[test]
    fn payload_keeps_everything_when_there_is_no_header() {
        // A source that sends bare samples must not lose the first two of
        // them. The WFD substream id tells the two cases apart.
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
        let mut samples = vec![0i16; SAMPLES_PER_FRAME];
        samples[..5].copy_from_slice(&[1, -1, 32767, -32768, 0]);
        let f = frame(&samples);
        let got: Vec<i16> = payload(&f)
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|b| i16::from_be_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(got, samples);
    }
}
