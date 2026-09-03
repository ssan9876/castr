//! Replays a synthetic Wi-Fi Display stream through the session and checks
//! that the access units come out whole and in order.
//!
//! This is the proof that the media path works without a radio: negotiation,
//! RTP reordering, transport-stream demux and PES assembly, driven only by
//! bytes. If this passes and the hardware does not, the fault is in the radio
//! layer, not in here.

use castr_miracast::session::{Session, SinkEvent};
use castr_miracast::test_support;
use castr_miracast::wfd::{AudioCodecs, Capabilities, ClientPorts, VideoFormats};

fn caps() -> Capabilities {
    Capabilities {
        video: VideoFormats::only_720p30(),
        audio: AudioCodecs::lpcm_48k_stereo(),
        ports: ClientPorts { rtp_port: 5000 },
        max_bitrate_kbps: 8000,
        latency_management: true,
        format_change: true,
    }
}

fn playing_session() -> Session {
    let mut s = Session::new(caps(), "01234567".into());
    for msg in test_support::negotiation_to_playing() {
        s.on_rtsp_bytes(msg.as_bytes());
    }
    s
}

#[test]
fn a_recorded_stream_replays_into_ordered_access_units() {
    let mut s = playing_session();
    let mut video = Vec::new();
    for datagram in test_support::recorded_stream(24) {
        for e in s.on_rtp_datagram(&datagram) {
            if let SinkEvent::Video { data, pts_us } = e {
                video.push((data, pts_us));
            }
        }
    }
    assert_eq!(video.len(), 24, "every access unit comes back");
    assert!(
        video.iter().all(|(d, _)| d.starts_with(&[0, 0, 0, 1])),
        "each one is an Annex B access unit"
    );
    let ts: Vec<u64> = video.iter().filter_map(|(_, p)| *p).collect();
    assert_eq!(ts.len(), 24, "each one carries its timestamp");
    assert!(
        ts.windows(2).all(|w| w[0] < w[1]),
        "timestamps ascend: {ts:?}"
    );
}

#[test]
fn the_negotiation_reaches_playing_and_reports_the_mode() {
    let mut s = Session::new(caps(), "01234567".into());
    let mut started = None;
    for msg in test_support::negotiation_to_playing() {
        for e in s.on_rtsp_bytes(msg.as_bytes()) {
            if let SinkEvent::Started(mode) = e {
                started = Some(mode);
            }
        }
    }
    let mode = started.expect("the session never reached playing");
    assert_eq!((mode.width, mode.height, mode.fps), (1280, 720, 30));
}

#[test]
fn a_dropped_datagram_costs_one_unit_and_asks_for_a_keyframe() {
    let mut s = playing_session();
    // Long enough that the reordering window fills after the gap: the window
    // is 32 packets, and a packet is only given up on once that many later
    // ones are waiting behind it. A real 720p stream sends about two dozen
    // datagrams per frame, so this is a fraction of a frame; this synthetic
    // one sends a single datagram per frame, so it needs the extra length.
    let stream = test_support::recorded_stream(48);
    let mut video = 0;
    let mut idr_requests = 0;
    for (i, datagram) in stream.into_iter().enumerate() {
        // Drop the fifth datagram, as a lossy 2.4 GHz link would.
        if i == 5 {
            continue;
        }
        for e in s.on_rtp_datagram(&datagram) {
            match e {
                SinkEvent::Video { .. } => video += 1,
                SinkEvent::SendRtsp(text) if text.contains("wfd_idr_request") => idr_requests += 1,
                _ => {}
            }
        }
    }
    // One unit is lost with its datagram; the rest survive, and the last few
    // are still inside the window when the stream ends.
    assert!(
        (40..=47).contains(&video),
        "surviving units keep flowing after the gap: {video}"
    );
    assert!(
        idr_requests >= 1,
        "the loss asks the source for a fresh keyframe"
    );
}
