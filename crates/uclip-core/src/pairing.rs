//! Pairing: two devices connect, prove their keys with a Noise handshake, and each user
//! confirms the same six digits on both screens. Only then are the keys stored.

use crate::discovery;
use crate::identity::{Identity, id_of};
use crate::peers::{Peer, PeerStore};
use crate::secure::{self, Secure};
use crate::wire::{Msg, PROTOCOL_VERSION};
use anyhow::{Result, anyhow, bail};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Asked with the six digits. Returns whether the user saw the same digits on the other device.
pub type Confirm = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Wait for another device to pair with us. `ready` is called with the port once we listen.
pub async fn host(identity: &Identity, store: &PeerStore, daemon_port: u16, wait: Duration, confirm: Confirm, ready: impl FnOnce(u16)) -> Result<Peer> {
    let listener = TcpListener::bind(("0.0.0.0", 0)).await?;
    let port = listener.local_addr()?.port();
    let _advert = discovery::advertise(&identity.id(), &identity.name, port, true).ok();
    ready(port);
    let (stream, _) = timeout(wait, listener.accept()).await.map_err(|_| anyhow!("nobody tried to pair within {} seconds", wait.as_secs()))??;
    exchange(stream, false, identity, store, daemon_port, confirm).await
}

/// Connect to a device that is waiting to pair. Without an address, look for one on the network.
pub async fn join(identity: &Identity, store: &PeerStore, daemon_port: u16, target: Option<SocketAddr>, wait: Duration, confirm: Confirm) -> Result<Peer> {
    let addr = match target {
        Some(a) => a,
        None => find_host(identity, wait).await?,
    };
    let stream = timeout(Duration::from_secs(5), TcpStream::connect(addr)).await.map_err(|_| anyhow!("could not reach {addr}"))??;
    exchange(stream, true, identity, store, daemon_port, confirm).await
}

async fn find_host(identity: &Identity, wait: Duration) -> Result<SocketAddr> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let mut found = discovery::browse(&daemon)?;
    let me = identity.id();
    let search = async {
        while let Some(f) = found.recv().await {
            if f.pairing && f.id != me {
                if let Some(ip) = f.addrs.first() {
                    return Some(SocketAddr::new(*ip, f.port));
                }
            }
        }
        None
    };
    timeout(wait, search).await.ok().flatten().ok_or_else(|| anyhow!("no device open for pairing was found. Run `uclip pair host` on the other device, or pass --addr"))
}

async fn exchange(stream: TcpStream, initiator: bool, identity: &Identity, store: &PeerStore, daemon_port: u16, confirm: Confirm) -> Result<Peer> {
    let remote_ip = stream.peer_addr()?.ip();
    let Secure { mut reader, mut writer, remote_static, handshake_hash } =
        timeout(Duration::from_secs(10), secure::handshake(stream, &identity.private, initiator)).await??;
    writer
        .send(&Msg::Hello { name: identity.name.clone(), os: crate::config::os_name().into(), version: PROTOCOL_VERSION, listen_port: daemon_port })
        .await?;
    let (name, listen_port) = match reader.recv().await? {
        Msg::Hello { name, listen_port, .. } => (name, listen_port),
        _ => bail!("expected Hello"),
    };
    let digits = secure::sas(&handshake_hash);
    let ok = tokio::task::spawn_blocking(move || confirm(&digits)).await?;
    writer.send(&Msg::PairConfirm { ok }).await?;
    let theirs = match reader.recv().await? {
        Msg::PairConfirm { ok } => ok,
        _ => bail!("expected PairConfirm"),
    };
    if !(ok && theirs) {
        bail!("pairing was declined {}", if ok { "by the other device" } else { "here" });
    }
    let peer = Peer { id: id_of(&remote_static), name, public: hex::encode(&remote_static), addr: Some(SocketAddr::new(remote_ip, listen_port).to_string()) };
    store.upsert(peer.clone())?;
    Ok(peer)
}
