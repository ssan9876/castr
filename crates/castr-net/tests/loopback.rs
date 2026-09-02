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
