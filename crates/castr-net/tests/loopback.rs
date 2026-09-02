use bytes::Bytes;
use castr_net::*;
use castr_proto::*;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn pair() -> (Identity, Identity, Endpoint, Endpoint) {
    let recv_id = Identity::generate().unwrap();
    let send_id = Identity::generate().unwrap();
    let recv_trust = Arc::new(RwLock::new(HashSet::from([send_id.fingerprint])));
    let send_trust = Arc::new(RwLock::new(HashSet::from([recv_id.fingerprint])));
    let server = Endpoint::server(loopback(), &recv_id, trust_fingerprints(recv_trust)).unwrap();
    let client = Endpoint::client(loopback(), &send_id, trust_fingerprints(send_trust)).unwrap();
    (recv_id, send_id, server, client)
}

#[tokio::test]
async fn control_datagram_and_nack_round_trip() {
    let (recv_id, send_id, server, client) = pair();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    let (r, s) = (r.unwrap(), s.unwrap());
    assert_eq!(r.peer_fingerprint(), send_id.fingerprint);
    assert_eq!(s.peer_fingerprint(), recv_id.fingerprint);

    let hello = ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        name: "pc".into(),
        resume_token: None,
    };
    s.send_control(&hello).await.unwrap();
    assert_eq!(r.recv_control().await.unwrap(), hello);
    r.send_control(&ControlMessage::RequestKeyframe)
        .await
        .unwrap();
    assert_eq!(
        s.recv_control().await.unwrap(),
        ControlMessage::RequestKeyframe
    );

    assert!(s.max_datagram_size() >= 1000);
    s.send_datagram(Bytes::from_static(b"video")).unwrap();
    assert_eq!(
        r.recv_datagram().await.unwrap(),
        Bytes::from_static(b"video")
    );

    let mut tx = r.open_nack_stream().await.unwrap();
    let nack = Nack {
        frame_number: 7,
        missing: vec![1, 3],
    };
    tx.send(&nack).await.unwrap();
    let mut rx = s.accept_nack_stream().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), nack);

    s.close("done");
    tokio::time::timeout(std::time::Duration::from_secs(2), r.closed())
        .await
        .expect("receiver sees close");
}

#[tokio::test]
async fn unpaired_client_is_rejected() {
    let recv_id = Identity::generate().unwrap();
    let send_id = Identity::generate().unwrap();
    let empty = Arc::new(RwLock::new(HashSet::new()));
    let server = Endpoint::server(loopback(), &recv_id, trust_fingerprints(empty)).unwrap();
    let client = Endpoint::client(loopback(), &send_id, accept_any()).unwrap();
    let addr = server.local_addr().unwrap();
    let accept = tokio::spawn(async move { server.accept().await.is_ok() });
    let connect = tokio::time::timeout(std::time::Duration::from_secs(5), client.connect(addr))
        .await
        .unwrap();
    assert!(connect.is_err(), "client must fail the handshake");
    accept.abort();
}

#[tokio::test]
async fn send_filter_drops_datagrams() {
    let (_, _, server, client) = pair();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    let (r, s) = (r.unwrap(), s.unwrap());
    s.set_send_filter(Some(Arc::new(|d: &[u8]| d[0] != b'x')));
    s.send_datagram(Bytes::from_static(b"xdrop")).unwrap();
    s.send_datagram(Bytes::from_static(b"keep")).unwrap();
    assert_eq!(
        r.recv_datagram().await.unwrap(),
        Bytes::from_static(b"keep")
    );
}

async fn connected_any() -> (Link, Link, Identity, Identity) {
    let recv_id = Identity::generate().unwrap();
    let send_id = Identity::generate().unwrap();
    let server = Endpoint::server(loopback(), &recv_id, accept_any()).unwrap();
    let client = Endpoint::client(loopback(), &send_id, accept_any()).unwrap();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    std::mem::forget(server);
    std::mem::forget(client);
    (r.unwrap(), s.unwrap(), recv_id, send_id)
}

#[tokio::test]
async fn pairing_succeeds_with_matching_pin() {
    let (r, s, recv_id, send_id) = connected_any().await;
    let pin = generate_pin();
    assert_eq!(pin.len(), 6);
    let (a, b) = tokio::join!(
        pair_as_receiver(&r, recv_id.fingerprint, &pin),
        pair_as_sender(&s, send_id.fingerprint, &pin),
    );
    a.unwrap();
    b.unwrap();
}

#[tokio::test]
async fn pairing_fails_with_wrong_pin() {
    let (r, s, recv_id, send_id) = connected_any().await;
    let (a, b) = tokio::join!(
        pair_as_receiver(&r, recv_id.fingerprint, "111111"),
        pair_as_sender(&s, send_id.fingerprint, "222222"),
    );
    assert!(a.is_err());
    assert!(b.is_err());
}

#[tokio::test]
async fn accept_survives_peer_that_never_opens_control_stream() {
    let recv_id = Identity::generate().unwrap();
    let stalling_id = Identity::generate().unwrap();
    let good_id = Identity::generate().unwrap();

    let server = Endpoint::server(loopback(), &recv_id, accept_any()).unwrap();
    let addr = server.local_addr().unwrap();

    // A raw quinn client that completes the QUIC handshake but never opens any stream,
    // simulating a peer that stalls the receiver's accept loop. `quinn::Endpoint::connect`
    // only initiates the handshake; it must be polled concurrently with `server.accept()`
    // for that handshake to actually be driven to completion (a not-yet-`accept()`-ed
    // incoming connection does no protocol work on its own), so we start it here and
    // await it in the same `tokio::join!` as everything else below.
    let mut stalling = quinn::Endpoint::client(loopback()).unwrap();
    stalling.set_default_client_config(client_config(&stalling_id, accept_any()).unwrap());
    let stalling_connecting = stalling.connect(addr, "castr.local").unwrap();

    let good_client = Endpoint::client(loopback(), &good_id, accept_any()).unwrap();

    let (accept_res, stalling_res, good_res) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                server.accept(),
                stalling_connecting,
                good_client.connect(addr)
            )
        })
        .await
        .expect("well-behaved peer accepted within 10s");

    let stalling_conn = stalling_res.expect("stalling peer completed its handshake");
    let r = accept_res.expect("accept succeeded");
    let s = good_res.expect("connect succeeded");

    assert_eq!(r.peer_fingerprint(), good_id.fingerprint);
    assert_eq!(s.peer_fingerprint(), recv_id.fingerprint);

    stalling_conn.close(0u32.into(), b"bye");
    stalling.close(0u32.into(), b"bye");
}

#[tokio::test]
async fn udp_probe_finds_advertiser_on_loopback() {
    let fp = [0x42u8; 32];
    let adv = Advertiser::start("Test Receiver", fp, 5555, 0)
        .await
        .unwrap();
    let found = browse(std::time::Duration::from_millis(800), adv.probe_port())
        .await
        .unwrap();
    let hit = found
        .iter()
        .find(|r| r.fingerprint == fp)
        .expect("advertiser discovered");
    assert_eq!(hit.name, "Test Receiver");
    assert_eq!(hit.addr.port(), 5555);
    assert_eq!(hit.version, PROTOCOL_VERSION);
}

#[tokio::test]
async fn browse_with_nothing_advertised_returns_empty() {
    let found = browse(std::time::Duration::from_millis(300), 1)
        .await
        .unwrap();
    assert!(found.iter().all(|r| r.fingerprint != [0x42u8; 32]));
}

#[tokio::test]
async fn nack_recovers_dropped_keyframe_fragment() {
    let (_, _, server, client) = pair();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    let (r, s) = (r.unwrap(), s.unwrap());

    // Drop fragment index 1 of frame 0 the first time it is sent.
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d2 = dropped.clone();
    s.set_send_filter(Some(Arc::new(move |d: &[u8]| {
        let (h, _) = DatagramHeader::decode(d).unwrap();
        if h.frame_number == 0
            && h.fragment_index == 1
            && !d2.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }
        true
    })));

    let mut packetizer = Packetizer::new();
    let mut rtx = RetransmitBuffer::new(500_000);
    let data: Vec<u8> = (0..3000).map(|i| (i % 251) as u8).collect();
    let frags = packetizer.packetize(STREAM_VIDEO, true, 0, &data, 1200);
    rtx.record(0, true, frags.clone(), 0);
    for f in &frags {
        s.send_datagram(f.clone()).unwrap();
    }

    let mut reasm = Reassembler::new(500_000);
    let mut nack_tx = r.open_nack_stream().await.unwrap();
    let recv_task = async {
        loop {
            let d = tokio::time::timeout(std::time::Duration::from_millis(200), r.recv_datagram())
                .await;
            match d {
                Ok(Ok(d)) => {
                    if let Some(f) = reasm.push(&d, 0).unwrap() {
                        return f;
                    }
                }
                _ => {
                    for n in reasm.tick(100_000) {
                        nack_tx.send(&n).await.unwrap();
                    }
                }
            }
        }
    };
    let sender_task = async {
        let mut nack_rx = s.accept_nack_stream().await.unwrap();
        let n = nack_rx.recv().await.unwrap();
        assert_eq!(
            n,
            Nack {
                frame_number: 0,
                missing: vec![1]
            }
        );
        for f in rtx.lookup(&n, 10_000, 33_333) {
            s.send_datagram(f).unwrap();
        }
    };
    let (frame, _) = tokio::join!(recv_task, sender_task);
    assert_eq!(frame.data, data);
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}
