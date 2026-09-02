use castr_net::*;
use std::collections::HashSet;
use std::sync::{Arc, RwLock};

#[tokio::main]
async fn main() {
    let recv_id = Identity::generate().unwrap();
    let send_id = Identity::generate().unwrap();
    let empty = Arc::new(RwLock::new(HashSet::new()));
    let server = Endpoint::server(
        "127.0.0.1:0".parse().unwrap(),
        &recv_id,
        trust_fingerprints(empty),
    )
    .unwrap();
    let client = Endpoint::client("127.0.0.1:0".parse().unwrap(), &send_id, accept_any()).unwrap();
    let addr = server.local_addr().unwrap();
    let accept = tokio::spawn(async move {
        let r = server.accept().await;
        eprintln!("MAIN: accept result is_ok={}", r.is_ok());
    });
    let t0 = std::time::Instant::now();
    let connect = client.connect(addr).await;
    eprintln!(
        "MAIN: connect took {:?}, is_err={}",
        t0.elapsed(),
        connect.is_err()
    );
    if let Ok(link) = &connect {
        let t1 = std::time::Instant::now();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(2), link.closed()).await;
        eprintln!(
            "MAIN: closed() result within 2s: {:?} after {:?}",
            closed.is_ok(),
            t1.elapsed()
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    eprintln!("MAIN: after 500ms sleep");
    accept.abort();
}
