use crate::identity::{fingerprint_of, Identity};
use crate::tls::{client_config, server_config, TrustCheck};
use anyhow::{anyhow, Context};
use bytes::Bytes;
use castr_proto::{decode_len_prefixed, encode_len_prefixed, ControlMessage, Nack};
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::Mutex;

pub const ALPN: &[u8] = b"castr/1";
pub type SendFilter = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

pub struct Endpoint {
    inner: quinn::Endpoint,
}

impl Endpoint {
    pub fn server(bind: SocketAddr, id: &Identity, trust: TrustCheck) -> anyhow::Result<Self> {
        let inner = quinn::Endpoint::server(server_config(id, trust)?, bind)
            .context("bind server endpoint")?;
        Ok(Self { inner })
    }

    pub fn client(bind: SocketAddr, id: &Identity, trust: TrustCheck) -> anyhow::Result<Self> {
        let mut inner = quinn::Endpoint::client(bind).context("bind client endpoint")?;
        inner.set_default_client_config(client_config(id, trust)?);
        Ok(Self { inner })
    }

    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.inner.local_addr()?)
    }

    /// QUIC does not announce a stream to the peer until bytes are written on it, so the
    /// sender writes this preamble right after opening the control stream and the receiver
    /// consumes it before handing the Link out.
    const CONTROL_PREAMBLE: &'static [u8; 4] = b"CTRL";

    pub async fn accept(&self) -> anyhow::Result<Link> {
        loop {
            let incoming = self
                .inner
                .accept()
                .await
                .ok_or_else(|| anyhow!("endpoint closed"))?;
            match incoming.await {
                Ok(conn) => match conn.accept_bi().await {
                    Ok((tx, mut rx)) => {
                        let mut preamble = [0u8; 4];
                        match rx.read_exact(&mut preamble).await {
                            Ok(()) if &preamble == Self::CONTROL_PREAMBLE => {
                                return Link::new(conn, tx, rx)
                            }
                            Ok(()) => tracing::warn!("bad control preamble {preamble:?}"),
                            Err(e) => tracing::warn!("control preamble read failed: {e}"),
                        }
                    }
                    Err(e) => tracing::warn!("control stream not opened: {e}"),
                },
                Err(e) => tracing::warn!("handshake failed: {e}"),
            }
        }
    }

    pub async fn connect(&self, addr: SocketAddr) -> anyhow::Result<Link> {
        let conn = self
            .inner
            .connect(addr, "castr.local")?
            .await
            .context("QUIC handshake")?;
        // A quinn/rustls client-side handshake future resolves once the client has
        // processed the server's flight and generated its own Finished message; it does
        // NOT wait for the server to validate the client's certificate. That check (and
        // any resulting CONNECTION_CLOSE) happens on the server asynchronously afterward,
        // and quinn's background endpoint driver only gets a chance to notice and process
        // it once our task actually yields to the executor. So a bare `connect().await`
        // succeeding is not proof the server accepted the client's identity: open the
        // control stream, then give a rejection a brief window to arrive before handing
        // out a Link that may already be dead.
        let (mut tx, rx) = conn.open_bi().await.context("open control stream")?;
        tx.write_all(Self::CONTROL_PREAMBLE)
            .await
            .context("control preamble")?;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            _ = conn.closed() => {
                return Err(anyhow!(
                    "connection closed during handshake: {:?}",
                    conn.close_reason()
                ));
            }
        }
        Link::new(conn, tx, rx)
    }

    pub fn close(&self) {
        self.inner.close(0u32.into(), b"bye");
    }
}

struct ControlRx {
    stream: quinn::RecvStream,
    buf: Vec<u8>,
}

#[derive(Clone)]
pub struct Link {
    conn: quinn::Connection,
    control_tx: Arc<Mutex<quinn::SendStream>>,
    control_rx: Arc<Mutex<ControlRx>>,
    peer_fp: [u8; 32],
    filter: Arc<StdMutex<Option<SendFilter>>>,
}

impl Link {
    fn new(
        conn: quinn::Connection,
        tx: quinn::SendStream,
        rx: quinn::RecvStream,
    ) -> anyhow::Result<Self> {
        let peer_fp = peer_fingerprint(&conn)?;
        Ok(Self {
            conn,
            control_tx: Arc::new(Mutex::new(tx)),
            control_rx: Arc::new(Mutex::new(ControlRx {
                stream: rx,
                buf: Vec::new(),
            })),
            peer_fp,
            filter: Arc::new(StdMutex::new(None)),
        })
    }

    pub fn peer_fingerprint(&self) -> [u8; 32] {
        self.peer_fp
    }
    pub fn remote_addr(&self) -> SocketAddr {
        self.conn.remote_address()
    }
    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }
    pub fn max_datagram_size(&self) -> usize {
        self.conn.max_datagram_size().unwrap_or(1200)
    }
    pub fn set_send_filter(&self, f: Option<SendFilter>) {
        *self.filter.lock().unwrap() = f;
    }
    pub fn close(&self, reason: &str) {
        self.conn.close(0u32.into(), reason.as_bytes());
    }
    pub async fn closed(&self) {
        let _ = self.conn.closed().await;
    }

    pub async fn send_control(&self, msg: &ControlMessage) -> anyhow::Result<()> {
        let bytes = encode_len_prefixed(msg);
        let mut tx = self.control_tx.lock().await;
        tx.write_all(&bytes).await.context("control write")?;
        Ok(())
    }

    pub async fn recv_control(&self) -> anyhow::Result<ControlMessage> {
        let mut rx = self.control_rx.lock().await;
        loop {
            if let Some((msg, used)) = decode_len_prefixed::<ControlMessage>(&rx.buf)? {
                rx.buf.drain(..used);
                return Ok(msg);
            }
            let mut chunk = [0u8; 4096];
            let n = rx
                .stream
                .read(&mut chunk)
                .await
                .context("control read")?
                .ok_or_else(|| anyhow!("control stream closed"))?;
            rx.buf.extend_from_slice(&chunk[..n]);
        }
    }

    pub fn send_datagram(&self, d: Bytes) -> anyhow::Result<()> {
        if let Some(f) = self.filter.lock().unwrap().as_ref() {
            if !f(&d) {
                return Ok(());
            }
        }
        self.conn.send_datagram(d).context("send datagram")
    }

    pub async fn recv_datagram(&self) -> anyhow::Result<Bytes> {
        self.conn.read_datagram().await.context("read datagram")
    }

    pub async fn open_nack_stream(&self) -> anyhow::Result<NackSender> {
        Ok(NackSender(
            self.conn.open_uni().await.context("open nack stream")?,
        ))
    }

    pub async fn accept_nack_stream(&self) -> anyhow::Result<NackReceiver> {
        Ok(NackReceiver {
            stream: self.conn.accept_uni().await.context("accept nack stream")?,
            buf: Vec::new(),
        })
    }
}

fn peer_fingerprint(conn: &quinn::Connection) -> anyhow::Result<[u8; 32]> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| anyhow!("peer presented no certificate"))?;
    let certs = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| anyhow!("unexpected peer identity type"))?;
    let first = certs
        .first()
        .ok_or_else(|| anyhow!("empty peer certificate chain"))?;
    Ok(fingerprint_of(first.as_ref()))
}

pub struct NackSender(quinn::SendStream);

impl NackSender {
    pub async fn send(&mut self, nack: &Nack) -> anyhow::Result<()> {
        self.0
            .write_all(&encode_len_prefixed(nack))
            .await
            .context("nack write")
    }
}

pub struct NackReceiver {
    stream: quinn::RecvStream,
    buf: Vec<u8>,
}

impl NackReceiver {
    pub async fn recv(&mut self) -> anyhow::Result<Nack> {
        loop {
            if let Some((nack, used)) = decode_len_prefixed::<Nack>(&self.buf)? {
                self.buf.drain(..used);
                return Ok(nack);
            }
            let mut chunk = [0u8; 1024];
            let n = self
                .stream
                .read(&mut chunk)
                .await
                .context("nack read")?
                .ok_or_else(|| anyhow!("nack stream closed"))?;
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}
