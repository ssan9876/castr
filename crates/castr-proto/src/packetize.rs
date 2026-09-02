use crate::header::*;
use bytes::{Bytes, BytesMut};

#[derive(Debug, Default)]
pub struct Packetizer {
    pub(crate) next_frame: u32,
}

impl Packetizer {
    pub fn new() -> Self {
        Self { next_frame: 0 }
    }

    pub fn last_frame_number(&self) -> u32 {
        self.next_frame.wrapping_sub(1)
    }

    pub fn packetize(
        &mut self,
        stream: u8,
        keyframe: bool,
        timestamp_us: u64,
        data: &[u8],
        max_datagram: usize,
    ) -> Vec<Bytes> {
        if data.is_empty() {
            return Vec::new();
        }
        let budget = max_datagram.saturating_sub(HEADER_LEN).max(1);
        let frame_number = self.next_frame;
        self.next_frame = self.next_frame.wrapping_add(1);
        let chunks: Vec<&[u8]> = data.chunks(budget).collect();
        let count = chunks.len();
        assert!(
            count <= u16::MAX as usize,
            "frame too large for u16 fragment count"
        );
        let mut out = Vec::with_capacity(count);
        for (i, chunk) in chunks.into_iter().enumerate() {
            let mut flags = 0;
            if keyframe {
                flags |= FLAG_KEYFRAME;
            }
            if i + 1 == count {
                flags |= FLAG_END_OF_FRAME;
            }
            let h = DatagramHeader {
                stream,
                flags,
                fragment_index: i as u16,
                fragment_count: count as u16,
                frame_number,
                timestamp_us,
            };
            let mut hdr = [0u8; HEADER_LEN];
            h.encode(&mut hdr);
            let mut buf = BytesMut::with_capacity(HEADER_LEN + chunk.len());
            buf.extend_from_slice(&hdr);
            buf.extend_from_slice(chunk);
            out.push(buf.freeze());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::*;

    #[test]
    fn small_frame_is_one_datagram_with_both_flags() {
        let mut p = Packetizer::new();
        let out = p.packetize(STREAM_VIDEO, true, 42, b"hello", 1200);
        assert_eq!(out.len(), 1);
        let (h, payload) = DatagramHeader::decode(&out[0]).unwrap();
        assert_eq!(payload, b"hello");
        assert_eq!(h.fragment_index, 0);
        assert_eq!(h.fragment_count, 1);
        assert_eq!(h.frame_number, 0);
        assert_eq!(h.timestamp_us, 42);
        assert!(h.is_keyframe());
        assert!(h.is_end());
    }

    #[test]
    fn large_frame_splits_at_payload_budget() {
        let mut p = Packetizer::new();
        let data = vec![7u8; 2500];
        let out = p.packetize(STREAM_VIDEO, false, 0, &data, HEADER_LEN + 1000);
        assert_eq!(out.len(), 3);
        for (i, d) in out.iter().enumerate() {
            let (h, payload) = DatagramHeader::decode(d).unwrap();
            assert!(d.len() <= HEADER_LEN + 1000);
            assert_eq!(h.fragment_index as usize, i);
            assert_eq!(h.fragment_count, 3);
            assert!(!h.is_keyframe());
            assert_eq!(h.is_end(), i == 2);
            let expected_len = if i == 2 { 500 } else { 1000 };
            assert_eq!(payload.len(), expected_len);
        }
    }

    #[test]
    fn frame_number_increments_per_call_and_wraps() {
        let mut p = Packetizer {
            next_frame: u32::MAX,
        };
        let a = p.packetize(STREAM_VIDEO, false, 0, b"a", 100);
        let b = p.packetize(STREAM_VIDEO, false, 0, b"b", 100);
        assert_eq!(
            DatagramHeader::decode(&a[0]).unwrap().0.frame_number,
            u32::MAX
        );
        assert_eq!(DatagramHeader::decode(&b[0]).unwrap().0.frame_number, 0);
        assert_eq!(p.last_frame_number(), 0);
    }

    #[test]
    fn empty_frame_produces_nothing() {
        let mut p = Packetizer::new();
        assert!(p.packetize(STREAM_VIDEO, false, 0, b"", 1200).is_empty());
    }
}
