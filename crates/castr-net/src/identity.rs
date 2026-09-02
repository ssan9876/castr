use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: [u8; 32],
}

pub fn fingerprint_of(cert_der: &[u8]) -> [u8; 32] {
    Sha256::digest(cert_der).into()
}

pub fn parse_fingerprint(hex_str: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_str).ok()?;
    bytes.try_into().ok()
}

impl Identity {
    pub fn generate() -> anyhow::Result<Self> {
        let ck =
            rcgen::generate_simple_self_signed(vec!["castr.local".to_string()]).context("rcgen")?;
        let cert_der = ck.cert.der().to_vec();
        let key_der = ck.key_pair.serialize_der();
        let fingerprint = fingerprint_of(&cert_der);
        Ok(Self {
            cert_der,
            key_der,
            fingerprint,
        })
    }

    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let cert_path = dir.join("identity.crt");
        let key_path = dir.join("identity.key");
        if cert_path.exists() && key_path.exists() {
            let cert_der = std::fs::read(&cert_path)?;
            let key_der = std::fs::read(&key_path)?;
            let fingerprint = fingerprint_of(&cert_der);
            return Ok(Self {
                cert_der,
                key_der,
                fingerprint,
            });
        }
        let id = Self::generate()?;
        std::fs::write(&cert_path, &id.cert_der)?;
        std::fs::write(&key_path, &id.key_der)?;
        Ok(id)
    }

    pub fn fingerprint_hex(&self) -> String {
        hex::encode(self.fingerprint)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedPeer {
    pub name: String,
    pub paired_at_unix: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    peers: BTreeMap<String, PairedPeer>,
}

pub struct PairedStore {
    path: PathBuf,
    file: StoreFile,
}

impl PairedStore {
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let file = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path)?).context("parse paired.toml")?
        } else {
            StoreFile::default()
        };
        Ok(Self { path, file })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, toml::to_string_pretty(&self.file)?)?;
        Ok(())
    }

    pub fn is_paired(&self, fp: &[u8; 32]) -> bool {
        self.file.peers.contains_key(&hex::encode(fp))
    }

    pub fn add(&mut self, fp: [u8; 32], name: String) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.file.peers.insert(
            hex::encode(fp),
            PairedPeer {
                name,
                paired_at_unix: now,
            },
        );
    }

    pub fn remove(&mut self, fp: &[u8; 32]) -> bool {
        self.file.peers.remove(&hex::encode(fp)).is_some()
    }

    pub fn list(&self) -> Vec<([u8; 32], PairedPeer)> {
        self.file
            .peers
            .iter()
            .filter_map(|(k, v)| parse_fingerprint(k).map(|fp| (fp, v.clone())))
            .collect()
    }

    pub fn find_by_name(&self, name: &str) -> Option<[u8; 32]> {
        self.list()
            .into_iter()
            .find(|(_, p)| p.name == name)
            .map(|(fp, _)| fp)
    }
}

pub fn config_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("castr");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_yields_cert_key_and_stable_fingerprint() {
        let id = Identity::generate().unwrap();
        assert!(!id.cert_der.is_empty() && !id.key_der.is_empty());
        assert_eq!(id.fingerprint, fingerprint_of(&id.cert_der));
        assert_eq!(id.fingerprint_hex().len(), 64);
        assert_eq!(
            parse_fingerprint(&id.fingerprint_hex()),
            Some(id.fingerprint)
        );
        assert_eq!(parse_fingerprint("zz"), None);
    }

    #[test]
    fn load_or_create_persists_and_reloads_same_identity() {
        let dir = std::env::temp_dir().join(format!("castr-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert!(dir.join("identity.crt").exists() && dir.join("identity.key").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paired_store_round_trips_through_toml() {
        let path = std::env::temp_dir().join(format!("castr-paired-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut s = PairedStore::load(path.clone()).unwrap();
        assert!(s.list().is_empty());
        let fp = [0xABu8; 32];
        assert!(!s.is_paired(&fp));
        s.add(fp, "living room".into());
        s.save().unwrap();
        let s2 = PairedStore::load(path.clone()).unwrap();
        assert!(s2.is_paired(&fp));
        assert_eq!(s2.find_by_name("living room"), Some(fp));
        assert_eq!(s2.list()[0].1.name, "living room");
        let mut s3 = s2;
        assert!(s3.remove(&fp));
        assert!(!s3.remove(&fp));
        std::fs::remove_file(&path).unwrap();
    }
}
