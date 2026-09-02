pub const HEADER_LEN: usize = 20;
pub const STREAM_VIDEO: u8 = 0;
pub const STREAM_AUDIO: u8 = 1;
pub const FLAG_KEYFRAME: u8 = 0b01;
pub const FLAG_END_OF_FRAME: u8 = 0b10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatagramHeader {
    pub stream: u8,
    pub flags: u8,
    pub fragment_index: u16,
    pub fragment_count: u16,
    pub frame_number: u32,
    pub timestamp_us: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("datagram shorter than header")]
    TooShort,
    #[error("unknown stream id {0}")]
    UnknownStream(u8),
}

impl DatagramHeader {
    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[0] = self.stream;
        out[1] = self.flags;
        out[2..4].copy_from_slice(&self.fragment_index.to_le_bytes());
        out[4..6].copy_from_slice(&self.fragment_count.to_le_bytes());
        out[6..8].copy_from_slice(&[0, 0]);
        out[8..12].copy_from_slice(&self.frame_number.to_le_bytes());
        out[12..20].copy_from_slice(&self.timestamp_us.to_le_bytes());
    }

    pub fn decode(buf: &[u8]) -> Result<(DatagramHeader, &[u8]), HeaderError> {
        if buf.len() < HEADER_LEN {
            return Err(HeaderError::TooShort);
        }
        let stream = buf[0];
        if stream != STREAM_VIDEO && stream != STREAM_AUDIO {
            return Err(HeaderError::UnknownStream(stream));
        }
        let h = DatagramHeader {
            stream,
            flags: buf[1],
            fragment_index: u16::from_le_bytes([buf[2], buf[3]]),
            fragment_count: u16::from_le_bytes([buf[4], buf[5]]),
            frame_number: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            timestamp_us: u64::from_le_bytes(buf[12..20].try_into().unwrap()),
        };
        Ok((h, &buf[HEADER_LEN..]))
    }

    pub fn is_keyframe(&self) -> bool {
        self.flags & FLAG_KEYFRAME != 0
    }
    pub fn is_end(&self) -> bool {
        self.flags & FLAG_END_OF_FRAME != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_all_fields() {
        let h = DatagramHeader {
            stream: STREAM_VIDEO,
            flags: FLAG_KEYFRAME | FLAG_END_OF_FRAME,
            fragment_index: 3,
            fragment_count: 7,
            frame_number: 0xDEADBEEF,
            timestamp_us: 1_234_567_890_123,
        };
        let mut buf = [0u8; HEADER_LEN];
        h.encode(&mut buf);
        let (decoded, payload) = DatagramHeader::decode(&buf).unwrap();
        assert_eq!(decoded, h);
        assert!(payload.is_empty());
    }

    #[test]
    fn layout_matches_spec_little_endian() {
        let h = DatagramHeader {
            stream: STREAM_AUDIO,
            flags: 0,
            fragment_index: 0x0102,
            fragment_count: 0x0304,
            frame_number: 0x05060708,
            timestamp_us: 0x090A0B0C0D0E0F10,
        };
        let mut buf = [0u8; HEADER_LEN];
        h.encode(&mut buf);
        assert_eq!(buf[0], 1);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..4], &[0x02, 0x01]);
        assert_eq!(&buf[4..6], &[0x04, 0x03]);
        assert_eq!(&buf[6..8], &[0, 0]);
        assert_eq!(&buf[8..12], &[0x08, 0x07, 0x06, 0x05]);
        assert_eq!(
            &buf[12..20],
            &[0x10, 0x0F, 0x0E, 0x0D, 0x0C, 0x0B, 0x0A, 0x09]
        );
    }

    #[test]
    fn decode_returns_payload_after_header() {
        let mut buf = vec![0u8; HEADER_LEN];
        buf.extend_from_slice(b"abc");
        let (_, payload) = DatagramHeader::decode(&buf).unwrap();
        assert_eq!(payload, b"abc");
    }

    #[test]
    fn decode_rejects_short_and_unknown_stream() {
        assert_eq!(
            DatagramHeader::decode(&[0u8; 19]).unwrap_err(),
            HeaderError::TooShort
        );
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = 9;
        assert_eq!(
            DatagramHeader::decode(&buf).unwrap_err(),
            HeaderError::UnknownStream(9)
        );
    }
}
