//! A lost delta fragment is repaired end to end.
//!
//! This is the behaviour the two one-line rule deletions exist to produce, so
//! it is asserted against both halves at once rather than trusting each in
//! isolation.

use castr_net::retransmit::RetransmitBuffer;
use castr_proto::{Packetizer, Reassembler, STREAM_VIDEO};

#[test]
fn a_dropped_delta_fragment_is_repaired_and_the_frame_completes() {
    let mut p = Packetizer::new();
    let mut sender_rtx = RetransmitBuffer::new(500_000);
    let mut receiver = Reassembler::new(500_000);

    let payload = vec![42u8; 4000];
    let frags = p.packetize(STREAM_VIDEO, false, 1_000, &payload, 1200);
    assert!(frags.len() > 2, "the test needs a fragmented frame");
    sender_rtx.record(p.last_frame_number(), false, frags.clone(), 0);

    // Everything except fragment 1 reaches the receiver.
    for (i, f) in frags.iter().enumerate() {
        if i == 1 {
            continue;
        }
        assert!(
            receiver.push(f, 0).unwrap().is_none(),
            "the frame cannot complete while a fragment is missing"
        );
    }

    // The receiver asks, well inside the repair window.
    let nacks = receiver.tick(1_000, 4_000, 150_000);
    assert_eq!(nacks.len(), 1, "{nacks:?}");
    assert_eq!(nacks[0].missing, vec![1]);

    // The sender answers, and the frame completes.
    let resent = sender_rtx.lookup(&nacks[0], 2_000);
    assert_eq!(resent.len(), 1);
    let done = receiver
        .push(&resent[0], 2_000)
        .unwrap()
        .expect("the repaired fragment completes the frame");
    assert_eq!(done.data, payload, "the frame is byte-for-byte intact");
    assert!(!done.keyframe, "and it is still a delta, not a keyframe");
}
