//! RTP parsing and a small reordering window.
//!
//! Wi-Fi Display carries MPEG-TS in RTP payload type 33. The network can
//! deliver packets out of order, so a short window is held before handing them
//! on; anything that never arrives inside the window is counted as lost, which
//! is what drives the sink's keyframe requests.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub sequence: u16,
    pub timestamp: u32,
    pub payload_type: u8,
    pub payload: Vec<u8>,
}

pub fn parse(buf: &[u8]) -> Option<Packet> {
    if buf.len() < 12 || buf[0] >> 6 != 2 {
        return None;
    }
    let csrc = (buf[0] & 0x0f) as usize;
    let extension = buf[0] & 0x10 != 0;
    let mut off = 12 + csrc * 4;
    if extension {
        if buf.len() < off + 4 {
            return None;
        }
        let words = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
        off += 4 + words * 4;
    }
    if buf.len() < off {
        return None;
    }
    Some(Packet {
        sequence: u16::from_be_bytes([buf[2], buf[3]]),
        timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        payload_type: buf[1] & 0x7f,
        payload: buf[off..].to_vec(),
    })
}

/// True when `a` is at or after `b` in sequence-number space, tolerating wrap.
fn seq_ge(a: u16, b: u16) -> bool {
    a.wrapping_sub(b) < 0x8000
}

pub struct Reorder {
    window: usize,
    held: Vec<Packet>,
    next: Option<u16>,
    lost: u64,
}

impl Reorder {
    pub fn new(window: usize) -> Self {
        Self {
            window,
            held: Vec::new(),
            next: None,
            lost: 0,
        }
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }

    pub fn push(&mut self, p: Packet) -> Vec<Packet> {
        let seq = p.sequence;
        if let Some(next) = self.next {
            // Already delivered, or a duplicate of something held.
            if !seq_ge(seq, next) || self.held.iter().any(|h| h.sequence == seq) {
                return Vec::new();
            }
        } else if self.held.iter().any(|h| h.sequence == seq) {
            return Vec::new();
        }
        let pos = self
            .held
            .iter()
            .position(|h| seq_ge(h.sequence, seq))
            .unwrap_or(self.held.len());
        self.held.insert(pos, p);
        let mut out = Vec::new();
        while let Some(head) = self.held.first() {
            let head_seq = head.sequence;
            match self.next {
                // Nothing delivered yet: the first packet sets the sequence.
                None => {}
                Some(next) if head_seq == next => {}
                // A gap, and the window is full: give up on what is missing
                // and resync, counting the packets we will never see.
                Some(next) if self.held.len() > self.window => {
                    self.lost += head_seq.wrapping_sub(next).max(1) as u64;
                }
                Some(_) => break,
            }
            let head = self.held.remove(0);
            self.next = Some(head.sequence.wrapping_add(1));
            out.push(head);
        }
        out
    }

    /// Releases everything still held, in order, at end of stream.
    pub fn flush(&mut self) -> Vec<Packet> {
        let out = std::mem::take(&mut self.held);
        if let Some(last) = out.last() {
            self.next = Some(last.sequence.wrapping_add(1));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(seq: u16, ts: u32, pt: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x80, pt];
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&ts.to_be_bytes());
        v.extend_from_slice(&0x1234_5678u32.to_be_bytes()); // SSRC
        v.extend_from_slice(payload);
        v
    }

    fn pkt(seq: u16) -> Packet {
        Packet {
            sequence: seq,
            timestamp: seq as u32 * 100,
            payload_type: 33,
            payload: vec![seq as u8],
        }
    }

    #[test]
    fn a_packet_parses_into_its_fields() {
        let p = parse(&raw(7, 900, 33, &[1, 2, 3])).expect("parse");
        assert_eq!(p.sequence, 7);
        assert_eq!(p.timestamp, 900);
        assert_eq!(p.payload_type, 33);
        assert_eq!(p.payload, vec![1, 2, 3]);
    }

    #[test]
    fn a_csrc_count_shifts_the_payload() {
        let mut v = raw(1, 0, 33, &[9, 9]);
        v[0] = 0x81; // one CSRC
        v.splice(12..12, [0, 0, 0, 5]);
        let p = parse(&v).expect("parse");
        assert_eq!(p.payload, vec![9, 9]);
    }

    #[test]
    fn a_short_or_wrong_version_packet_is_rejected() {
        assert!(parse(&[0x80, 33, 0]).is_none());
        let mut v = raw(1, 0, 33, &[1]);
        v[0] = 0x40; // version 1
        assert!(parse(&v).is_none());
    }

    #[test]
    fn packets_in_order_pass_straight_through() {
        let mut r = Reorder::new(8);
        let out: Vec<u16> = (1..=4)
            .flat_map(|s| r.push(pkt(s)))
            .map(|p| p.sequence)
            .collect();
        assert_eq!(out, vec![1, 2, 3, 4]);
        assert_eq!(r.lost(), 0);
    }

    #[test]
    fn a_swapped_pair_is_reordered() {
        let mut r = Reorder::new(8);
        let mut got: Vec<u16> = r.push(pkt(1)).into_iter().map(|p| p.sequence).collect();
        got.extend(r.push(pkt(3)).into_iter().map(|p| p.sequence));
        got.extend(r.push(pkt(2)).into_iter().map(|p| p.sequence));
        got.extend(r.flush().into_iter().map(|p| p.sequence));
        assert_eq!(got, vec![1, 2, 3]);
        assert_eq!(r.lost(), 0);
    }

    #[test]
    fn a_gap_wider_than_the_window_is_given_up_on_and_counted() {
        let mut r = Reorder::new(4);
        r.push(pkt(1));
        let mut got = Vec::new();
        for s in 3..=8 {
            got.extend(r.push(pkt(s)).into_iter().map(|p| p.sequence));
        }
        assert!(got.contains(&3), "later packets are released: {got:?}");
        assert_eq!(r.lost(), 1, "packet 2 is counted lost");
    }

    #[test]
    fn a_duplicate_is_dropped() {
        let mut r = Reorder::new(8);
        r.push(pkt(1));
        r.push(pkt(2));
        assert!(r.push(pkt(2)).is_empty());
        assert_eq!(r.flush().len(), 0);
    }

    #[test]
    fn the_sequence_number_wraps_without_declaring_loss() {
        let mut r = Reorder::new(8);
        let mut got = Vec::new();
        for s in [65534u16, 65535, 0, 1] {
            got.extend(r.push(pkt(s)).into_iter().map(|p| p.sequence));
        }
        got.extend(r.flush().into_iter().map(|p| p.sequence));
        assert_eq!(got, vec![65534, 65535, 0, 1]);
        assert_eq!(r.lost(), 0);
    }
}
