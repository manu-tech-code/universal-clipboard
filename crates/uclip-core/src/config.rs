use std::path::PathBuf;

/// Default TCP port the daemon listens on. Pairing stores it so a peer can reconnect
/// without mDNS (for example over Tailscale).
pub const DEFAULT_PORT: u16 = 47474;

/// Where identity and paired peers live. `UCLIP_HOME` overrides it, which the tests rely on.
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("UCLIP_HOME") {
        return PathBuf::from(p);
    }
    directories::ProjectDirs::from("dev", "universal-clipboard", "UniversalClipboard")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".uclip"))
}

pub fn os_name() -> &'static str {
    std::env::consts::OS
}
