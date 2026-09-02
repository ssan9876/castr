use crate::transport::Link;
use anyhow::{anyhow, bail, Context};
use castr_proto::ControlMessage;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};

type HmacSha256 = Hmac<Sha256>;
const ID_SENDER: &[u8] = b"castr-sender";
const ID_RECEIVER: &[u8] = b"castr-receiver";

pub fn generate_pin() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

fn proof(key: &[u8], role: &[u8], fp: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(role);
    mac.update(fp);
    mac.finalize().into_bytes().into()
}

fn verify(key: &[u8], role: &[u8], fp: &[u8; 32], got: &[u8; 32]) -> bool {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(role);
    mac.update(fp);
    mac.verify_slice(got).is_ok()
}

async fn fail(link: &Link, why: &str) -> anyhow::Error {
    let _ = link
        .send_control(&ControlMessage::Error {
            code: 3,
            message: why.to_string(),
        })
        .await;
    anyhow!("pairing failed: {why}")
}

/// Sender side. Returns Ok(()) when both proofs verified and PairOk exchanged.
pub async fn pair_as_sender(link: &Link, own_fp: [u8; 32], pin: &str) -> anyhow::Result<()> {
    let (state, msg_a) = Spake2::<Ed25519Group>::start_a(
        &Password::new(pin.as_bytes()),
        &SpakeId::new(ID_SENDER),
        &SpakeId::new(ID_RECEIVER),
    );
    link.send_control(&ControlMessage::PairInit(msg_a)).await?;
    let msg_b = match link.recv_control().await? {
        ControlMessage::PairResp(m) => m,
        ControlMessage::Error { message, .. } => bail!("receiver refused pairing: {message}"),
        other => return Err(fail(link, &format!("unexpected {other:?}")).await),
    };
    let key = state.finish(&msg_b).map_err(|e| anyhow!("spake2: {e:?}"))?;
    link.send_control(&ControlMessage::PairProof(proof(&key, b"sender", &own_fp)))
        .await?;
    match link.recv_control().await? {
        ControlMessage::PairProof(p) if verify(&key, b"receiver", &link.peer_fingerprint(), &p) => {
        }
        ControlMessage::Error { message, .. } => bail!("receiver rejected proof: {message}"),
        _ => return Err(fail(link, "receiver proof mismatch").await),
    }
    link.send_control(&ControlMessage::PairOk).await?;
    match link.recv_control().await? {
        ControlMessage::PairOk => Ok(()),
        other => bail!("expected PairOk, got {other:?}"),
    }
}

/// Receiver side.
pub async fn pair_as_receiver(link: &Link, own_fp: [u8; 32], pin: &str) -> anyhow::Result<()> {
    let msg_a = match link.recv_control().await? {
        ControlMessage::PairInit(m) => m,
        other => return Err(fail(link, &format!("unexpected {other:?}")).await),
    };
    let (state, msg_b) = Spake2::<Ed25519Group>::start_b(
        &Password::new(pin.as_bytes()),
        &SpakeId::new(ID_SENDER),
        &SpakeId::new(ID_RECEIVER),
    );
    let key = state.finish(&msg_a).map_err(|e| anyhow!("spake2: {e:?}"))?;
    link.send_control(&ControlMessage::PairResp(msg_b)).await?;
    match link.recv_control().await? {
        ControlMessage::PairProof(p) if verify(&key, b"sender", &link.peer_fingerprint(), &p) => {}
        ControlMessage::Error { message, .. } => bail!("sender aborted: {message}"),
        _ => return Err(fail(link, "wrong PIN").await),
    }
    link.send_control(&ControlMessage::PairProof(proof(
        &key,
        b"receiver",
        &own_fp,
    )))
    .await?;
    match link.recv_control().await.context("waiting for PairOk")? {
        ControlMessage::PairOk => {}
        ControlMessage::Error { message, .. } => bail!("sender rejected proof: {message}"),
        other => bail!("expected PairOk, got {other:?}"),
    }
    link.send_control(&ControlMessage::PairOk).await?;
    Ok(())
}
