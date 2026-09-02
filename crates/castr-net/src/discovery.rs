use anyhow::Context;
use castr_proto::PROTOCOL_VERSION;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

pub const SERVICE_TYPE: &str = "_castr._udp.local.";
pub const PROBE_PORT: u16 = 7331;
pub const PROBE_MAGIC: &[u8] = b"CASTR?";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverInfo {
    pub name: String,
    pub fingerprint: [u8; 32],
    pub addr: SocketAddr,
    pub version: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Beacon {
    pub name: String,
    pub fp: [u8; 32],
    pub port: u16,
    pub version: u16,
}

pub struct Advertiser {
    mdns: Option<ServiceDaemon>,
    fullname: String,
    probe_port: u16,
    responder: tokio::task::JoinHandle<()>,
}

impl Advertiser {
    /// `quic_port` is the receiver's QUIC port. `probe_port` is normally PROBE_PORT; tests pass 0 and read it back.
    pub async fn start(
        name: &str,
        fp: [u8; 32],
        quic_port: u16,
        probe_port: u16,
    ) -> anyhow::Result<Self> {
        let beacon = Beacon {
            name: name.to_string(),
            fp,
            port: quic_port,
            version: PROTOCOL_VERSION,
        };
        let reply = postcard::to_allocvec(&beacon)?;
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, probe_port))
            .await
            .context("bind probe port")?;
        sock.set_broadcast(true)?;
        let probe_port = sock.local_addr()?.port();
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                    break;
                };
                if n == PROBE_MAGIC.len() + 1 && &buf[..PROBE_MAGIC.len()] == PROBE_MAGIC {
                    let _ = sock.send_to(&reply, from).await;
                }
            }
        });

        let (mdns, fullname) = match ServiceDaemon::new() {
            Ok(daemon) => {
                let host = format!("castr-{}.local.", &hex::encode(fp)[..12]);
                let props: HashMap<String, String> = HashMap::from([
                    ("name".to_string(), name.to_string()),
                    ("fp".to_string(), hex::encode(fp)),
                    ("ver".to_string(), PROTOCOL_VERSION.to_string()),
                ]);
                match ServiceInfo::new(SERVICE_TYPE, name, &host, "", quic_port, props)
                    .context("mdns service info")
                {
                    Ok(info) => {
                        let info = info.enable_addr_auto();
                        let fullname = info.get_fullname().to_string();
                        match daemon.register(info) {
                            Ok(()) => (Some(daemon), fullname),
                            Err(e) => {
                                tracing::warn!("mDNS register failed: {e}");
                                (None, String::new())
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("mDNS service info failed: {e}");
                        (None, String::new())
                    }
                }
            }
            Err(e) => {
                tracing::warn!("mDNS unavailable: {e}");
                (None, String::new())
            }
        };
        Ok(Self {
            mdns,
            fullname,
            probe_port,
            responder,
        })
    }

    pub fn probe_port(&self) -> u16 {
        self.probe_port
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        self.responder.abort();
        if let Some(d) = self.mdns.take() {
            let _ = d.unregister(&self.fullname);
            let _ = d.shutdown();
        }
    }
}

async fn probe_udp(
    timeout: Duration,
    probe_port: u16,
    out: &mut HashMap<[u8; 32], ReceiverInfo>,
) -> anyhow::Result<()> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    sock.set_broadcast(true)?;
    let mut probe = PROBE_MAGIC.to_vec();
    probe.push(PROTOCOL_VERSION as u8);
    for target in [
        IpAddr::V4(Ipv4Addr::BROADCAST),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    ] {
        let _ = sock.send_to(&probe, (target, probe_port)).await;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 512];
    loop {
        let Ok(res) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await else {
            break;
        };
        let Ok((n, from)) = res else { break };
        if let Ok(b) = postcard::from_bytes::<Beacon>(&buf[..n]) {
            out.entry(b.fp).or_insert(ReceiverInfo {
                name: b.name,
                fingerprint: b.fp,
                addr: SocketAddr::new(from.ip(), b.port),
                version: b.version,
            });
        }
    }
    Ok(())
}

async fn probe_mdns(timeout: Duration, out: &mut HashMap<[u8; 32], ReceiverInfo>) {
    let Ok(daemon) = ServiceDaemon::new() else {
        return;
    };
    let Ok(rx) = daemon.browse(SERVICE_TYPE) else {
        return;
    };
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let Ok(ev) = tokio::time::timeout_at(deadline, rx.recv_async()).await else {
            break;
        };
        let Ok(ev) = ev else { break };
        if let ServiceEvent::ServiceResolved(info) = ev {
            let fp = info
                .get_property_val_str("fp")
                .and_then(crate::identity::parse_fingerprint);
            let name = info.get_property_val_str("name").unwrap_or("").to_string();
            let version = info
                .get_property_val_str("ver")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let ip = info.get_addresses().iter().find(|a| a.is_ipv4()).copied();
            if let (Some(fp), Some(ip)) = (fp, ip) {
                out.entry(fp).or_insert(ReceiverInfo {
                    name,
                    fingerprint: fp,
                    addr: SocketAddr::new(ip, info.get_port()),
                    version,
                });
            }
        }
    }
    let _ = daemon.shutdown();
}

/// Runs mDNS browse and a UDP probe (to 255.255.255.255:probe_port and 127.0.0.1:probe_port) in parallel for `timeout`, merged by fingerprint.
pub async fn browse(timeout: Duration, probe_port: u16) -> anyhow::Result<Vec<ReceiverInfo>> {
    let mut udp = HashMap::new();
    let mut mdns = HashMap::new();
    let (u, _) = tokio::join!(
        probe_udp(timeout, probe_port, &mut udp),
        probe_mdns(timeout, &mut mdns)
    );
    u?;
    for (fp, info) in mdns {
        udp.entry(fp).or_insert(info);
    }
    let mut v: Vec<_> = udp.into_values().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}
