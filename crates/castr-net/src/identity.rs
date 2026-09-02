use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Writes `data` to `path` atomically: writes to a `.tmp` sibling in the same directory,
/// then renames it over `path`. On Windows, `rename` can fail with `AlreadyExists` if the
/// destination already exists, so fall back to removing it first and retrying. When
/// `secret` is true and we're on unix, the temp file is created with mode 0o600 before any
/// data is written to it, so the private key is never briefly world-readable.
fn write_atomic(path: &Path, data: &[u8], secret: bool) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    let tmp_path = dir.join(format!(
        "{}.tmp",
        path.file_name()
            .ok_or_else(|| anyhow::anyhow!("{} has no file name", path.display()))?
            .to_string_lossy()
    ));

    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if secret {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = secret;
        let mut file = opts
            .open(&tmp_path)
            .with_context(|| format!("create {}", tmp_path.display()))?;
        std::io::Write::write_all(&mut file, data)
            .with_context(|| format!("write {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", tmp_path.display()))?;
    }

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp_path, path)
                .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))?;
        } else {
            return Err(e)
                .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()));
        }
    }
    Ok(())
}

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
        write_atomic(&cert_path, &id.cert_der, false)?;
        write_atomic(&key_path, &id.key_der, true)?;
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
        write_atomic(
            &self.path,
            toml::to_string_pretty(&self.file)?.as_bytes(),
            false,
        )
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
    fn key_file_is_written_atomically_and_reloads() {
        let dir = std::env::temp_dir().join(format!("castr-id-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Identity::load_or_create(&dir).unwrap();
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !entries.iter().any(|n| n.ends_with(".tmp")),
            "no .tmp files should remain: {entries:?}"
        );
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("identity.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
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
