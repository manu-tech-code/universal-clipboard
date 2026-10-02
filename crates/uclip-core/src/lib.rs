//! Universal Clipboard core: identity, pairing, encrypted transport, discovery and clipboard sync.
//! Nothing in here is platform specific except `clipboard::system`.

pub mod clipboard;
pub mod config;
pub mod discovery;
pub mod engine;
pub mod identity;
pub mod pairing;
pub mod peers;
pub mod secure;
pub mod wire;
