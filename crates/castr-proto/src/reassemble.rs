use crate::header::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteFrame {
    pub stream: u8,
    pub frame_number: u32,
    pub timestamp_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Nack {
    pub frame_number: u32,
    pub missing: Vec<u16>,
}

struct Partial {
    stream: u8,
    timestamp_us: u64,
    keyframe: bool,
    first_seen_us: u64,
    parts: Vec<Option<Vec<u8>>>,
    received: usize,
}

pub struct Reassembler {
    max_age_us: u64,
    partial: BTreeMap<u32, Partial>,
    /// Frame numbers completed recently, so late duplicates are dropped.
    completed: std::collections::VecDeque<u32>,
    lost: u64,
}

/// True if `a` is newer than or equal to `b` under wrapping arithmetic.
pub fn frame_newer_or_eq(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) < u32::MAX / 2
}

impl Reassembler {
    pub fn new(max_age_us: u64) -> Self {
        Self {
            max_age_us,
            partial: BTreeMap::new(),
            completed: std::collections::VecDeque::new(),
            lost: 0,
        }
    }

    pub fn pending(&self) -> usize {
        self.partial.len()
    }

    pub fn fragments_lost(&mut self) -> u64 {
        std::mem::take(&mut self.lost)
    }

    pub fn push(
        &mut self,
        datagram: &[u8],
        now_us: u64,
    ) -> Result<Option<CompleteFrame>, HeaderError> {
        let (h, payload) = DatagramHeader::decode(datagram)?;
        if self.completed.contains(&h.frame_number) {
            return Ok(None);
        }
        let count = h.fragment_count.max(1) as usize;
        let entry = self
            .partial
            .entry(h.frame_number)
            .or_insert_with(|| Partial {
                stream: h.stream,
                timestamp_us: h.timestamp_us,
                keyframe: h.is_keyframe(),
                first_seen_us: now_us,
                parts: vec![None; count],
                received: 0,
            });
        let idx = h.fragment_index as usize;
        if idx >= entry.parts.len() || entry.parts[idx].is_some() {
            return Ok(None);
        }
        entry.parts[idx] = Some(payload.to_vec());
        entry.received += 1;
        if entry.received < entry.parts.len() {
            return Ok(None);
        }
        let done = self.partial.remove(&h.frame_number).unwrap();
        self.remember_completed(h.frame_number);
        let mut data = Vec::new();
        for p in done.parts.into_iter() {
            data.extend_from_slice(&p.unwrap());
        }
        Ok(Some(CompleteFrame {
            stream: done.stream,
            frame_number: h.frame_number,
            timestamp_us: done.timestamp_us,
            keyframe: done.keyframe,
            data,
        }))
    }

    fn remember_completed(&mut self, n: u32) {
        self.completed.push_back(n);
        while self.completed.len() > 64 {
            self.completed.pop_front();
        }
    }

    pub fn tick(&mut self, now_us: u64) -> Vec<Nack> {
        let mut nacks = Vec::new();
        let mut expired = Vec::new();
        for (&fnum, p) in self.partial.iter() {
            let age = now_us.saturating_sub(p.first_seen_us);
            if age > self.max_age_us {
                expired.push(fnum);
                self.lost += (p.parts.len() - p.received) as u64;
            } else if p.keyframe {
                let missing: Vec<u16> = p
                    .parts
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| x.is_none())
                    .map(|(i, _)| i as u16)
                    .collect();
                nacks.push(Nack {
                    frame_number: fnum,
                    missing,
                });
            }
        }
        for f in expired {
            self.partial.remove(&f);
        }
        nacks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::*;
    use crate::packetize::Packetizer;

    fn frames(p: &mut Packetizer, keyframe: bool, data: &[u8]) -> Vec<bytes::Bytes> {
        p.packetize(STREAM_VIDEO, keyframe, 5, data, HEADER_LEN + 100)
    }

    #[test]
    fn in_order_fragments_complete_a_frame() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let data: Vec<u8> = (0..250).map(|i| i as u8).collect();
        let dgs = frames(&mut p, true, &data);
        assert_eq!(dgs.len(), 3);
        assert_eq!(r.push(&dgs[0], 0).unwrap(), None);
        assert_eq!(r.push(&dgs[1], 0).unwrap(), None);
        let f = r.push(&dgs[2], 0).unwrap().unwrap();
        assert_eq!(
            f,
            CompleteFrame {
                stream: STREAM_VIDEO,
                frame_number: 0,
                timestamp_us: 5,
                keyframe: true,
                data
            }
        );
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn out_of_order_and_duplicate_fragments_still_complete() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let data: Vec<u8> = (0..250).map(|i| i as u8).collect();
        let dgs = frames(&mut p, false, &data);
        assert_eq!(r.push(&dgs[2], 0).unwrap(), None);
        assert_eq!(r.push(&dgs[2], 0).unwrap(), None);
        assert_eq!(r.push(&dgs[0], 0).unwrap(), None);
        let f = r.push(&dgs[1], 0).unwrap().unwrap();
        assert_eq!(f.data, data);
    }

    #[test]
    fn tick_nacks_incomplete_keyframe_and_expires_old_frames() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let data = vec![1u8; 350];
        let dgs = frames(&mut p, true, &data);
        r.push(&dgs[0], 0).unwrap();
        r.push(&dgs[3], 0).unwrap();
        let nacks = r.tick(100_000);
        assert_eq!(
            nacks,
            vec![Nack {
                frame_number: 0,
                missing: vec![1, 2]
            }]
        );
        assert_eq!(r.pending(), 1);
        assert!(r.tick(600_001).is_empty());
        assert_eq!(r.pending(), 0);
        assert_eq!(r.fragments_lost(), 2);
        assert_eq!(r.fragments_lost(), 0);
    }

    #[test]
    fn tick_does_not_nack_delta_frames() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let dgs = frames(&mut p, false, &[2u8; 350]);
        r.push(&dgs[0], 0).unwrap();
        assert!(r.tick(100_000).is_empty());
    }

    #[test]
    fn late_fragment_for_already_completed_frame_is_ignored() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let dgs = frames(&mut p, false, &[3u8; 150]);
        r.push(&dgs[0], 0).unwrap();
        assert!(r.push(&dgs[1], 0).unwrap().is_some());
        assert_eq!(r.push(&dgs[1], 0).unwrap(), None);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn frame_numbers_wrap_without_confusing_age() {
        let mut p = Packetizer {
            next_frame: u32::MAX,
        };
        let mut r = Reassembler::new(500_000);
        let a = frames(&mut p, false, b"a");
        let b = frames(&mut p, false, b"b");
        assert_eq!(r.push(&a[0], 0).unwrap().unwrap().frame_number, u32::MAX);
        assert_eq!(r.push(&b[0], 0).unwrap().unwrap().frame_number, 0);
    }

    #[test]
    fn rejects_bad_header() {
        let mut r = Reassembler::new(1);
        assert_eq!(r.push(&[0u8; 3], 0).unwrap_err(), HeaderError::TooShort);
    }
}
