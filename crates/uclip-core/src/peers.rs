use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Peer {
    pub id: String,
    pub name: String,
    /// Hex of the peer's static public key. A connection is only trusted if it proves this key.
    pub public: String,
    /// Last known `ip:port`, used when mDNS is unavailable (Tailscale, other subnets).
    #[serde(default)]
    pub addr: Option<String>,
}

/// The circle of paired devices, kept in a small JSON file. Other processes (the pairing CLI)
/// edit it while the daemon runs, so every call re-reads the file.
#[derive(Clone)]
pub struct PeerStore {
    path: PathBuf,
}

impl PeerStore {
    pub fn open(dir: &Path) -> Self {
        Self { path: dir.join("peers.json") }
    }

    pub fn list(&self) -> Vec<Peer> {
        std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<Peer> {
        self.list().into_iter().find(|p| p.id == id)
    }

    pub fn find_by_public(&self, public: &[u8]) -> Option<Peer> {
        let want = hex::encode(public);
        self.list().into_iter().find(|p| p.public == want)
    }

    pub fn upsert(&self, peer: Peer) -> Result<()> {
        let mut all = self.list();
        all.retain(|p| p.id != peer.id);
        all.push(peer);
        self.save(&all)
    }

    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut all = self.list();
        let before = all.len();
        all.retain(|p| p.id != id);
        self.save(&all)?;
        Ok(all.len() != before)
    }

    fn save(&self, all: &[Peer]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }
}
