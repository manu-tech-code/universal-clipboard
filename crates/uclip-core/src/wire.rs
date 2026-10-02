use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_TEXT: usize = 5 * 1024 * 1024;

/// What a clipboard can hold. Only text for now. Images and file promises come next and
/// will be added as new variants.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ClipItem {
    Text(String),
}

impl ClipItem {
    pub fn preview(&self) -> String {
        match self {
            ClipItem::Text(t) => {
                let one_line: String = t.chars().take(60).map(|c| if c.is_control() { ' ' } else { c }).collect();
                if t.chars().count() > 60 { format!("{one_line}\u{2026}") } else { one_line }
            }
        }
    }

    pub fn digest(&self) -> [u8; 8] {
        let ClipItem::Text(t) = self;
        let d = Sha256::digest(t.as_bytes());
        d[..8].try_into().unwrap()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClipMsg {
    pub id: String,
    pub origin: String,
    pub ts_ms: u64,
    pub item: ClipItem,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Msg {
    Hello { name: String, os: String, version: u16, listen_port: u16 },
    Clip(ClipMsg),
    PairConfirm { ok: bool },
    Ping,
    Pong,
}
