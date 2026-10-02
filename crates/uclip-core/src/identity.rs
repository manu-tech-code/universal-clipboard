use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

#[derive(Serialize, Deserialize)]
struct Stored {
    name: String,
    private: String,
    public: String,
}

/// This device's long-term key. The device id is derived from the public key, so an id
/// cannot be claimed without holding the matching private key.
#[derive(Clone)]
pub struct Identity {
    pub name: String,
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

pub fn id_of(public: &[u8]) -> String {
    hex::encode(&Sha256::digest(public)[..8])
}

impl Identity {
    pub fn id(&self) -> String {
        id_of(&self.public)
    }

    pub fn load_or_create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("identity.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let s: Stored = serde_json::from_str(&text).context("reading identity.json")?;
            return Ok(Self { name: s.name, private: hex::decode(s.private)?, public: hex::decode(s.public)? });
        }
        let kp = snow::Builder::new(NOISE_PARAMS.parse()?).generate_keypair()?;
        let name = gethostname::gethostname().to_string_lossy().trim_end_matches(".local").to_string();
        let stored = Stored { name: name.clone(), private: hex::encode(&kp.private), public: hex::encode(&kp.public) };
        write_private(&path, &serde_json::to_vec_pretty(&stored)?)?;
        Ok(Self { name, private: kp.private, public: kp.public })
    }
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)?;
    Ok(())
}
