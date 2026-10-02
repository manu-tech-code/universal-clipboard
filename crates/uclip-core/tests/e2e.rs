//! Two devices on loopback: pair them, then check that a copy on one lands on the other and
//! that nothing echoes back. Uses file-backed clipboards and stored addresses instead of mDNS.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uclip_core::clipboard;
use uclip_core::engine::{self, EngineConfig, Event};
use uclip_core::identity::Identity;
use uclip_core::pairing::{self, Confirm};
use uclip_core::peers::PeerStore;

struct Device {
    identity: Identity,
    store: PeerStore,
    clip: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn device() -> Device {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::load_or_create(dir.path()).unwrap();
    let store = PeerStore::open(dir.path());
    let clip = dir.path().join("clipboard.txt");
    Device { identity, store, clip, _dir: dir }
}

fn start(d: &Device, port: u16) -> (tokio::task::JoinHandle<anyhow::Result<()>>, mpsc::UnboundedReceiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cfg = EngineConfig {
        identity: d.identity.clone(),
        store: d.store.clone(),
        port,
        clipboard: clipboard::from_spec(Some(&format!("file:{}", d.clip.display()))).unwrap(),
        mdns: false,
        events: Some(tx),
        paused: Default::default(),
    };
    (tokio::spawn(engine::run(cfg)), rx)
}

async fn eventually(path: &Path, want: &str) -> bool {
    for _ in 0..60 {
        if std::fs::read_to_string(path).ok().as_deref() == Some(want) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_then_sync_both_ways_without_echo() {
    let (a, b) = (device(), device());
    let yes: Confirm = Arc::new(|_| true);

    // Pair: A hosts, B joins, both confirm.
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    let (ai, ast) = (a.identity.clone(), a.store.clone());
    let host = tokio::spawn({
        let yes = yes.clone();
        async move { pairing::host(&ai, &ast, 47999, Duration::from_secs(10), yes, move |p| { let _ = port_tx.send(p); }).await }
    });
    let port = port_rx.await.unwrap();
    let joined = pairing::join(&b.identity, &b.store, 0, Some(format!("127.0.0.1:{port}").parse().unwrap()), Duration::from_secs(10), yes).await.unwrap();
    let hosted = host.await.unwrap().unwrap();
    assert_eq!(joined.id, a.identity.id());
    assert_eq!(hosted.id, b.identity.id());

    // Start both engines. A listens on a fixed port that B's stored address points at.
    let mut p = b.store.get(&a.identity.id()).unwrap();
    p.addr = Some("127.0.0.1:47998".into());
    b.store.upsert(p).unwrap();
    let (ta, mut ea) = start(&a, 47998);
    let (tb, mut eb) = start(&b, 0);

    for rx in [&mut ea, &mut eb] {
        loop {
            match tokio::time::timeout(Duration::from_secs(15), rx.recv()).await.expect("peers never connected") {
                Some(Event::Connected { .. }) => break,
                Some(_) => continue,
                None => panic!("event channel closed"),
            }
        }
    }

    std::fs::write(&a.clip, "hello from A").unwrap();
    assert!(eventually(&b.clip, "hello from A").await, "A to B did not sync");

    std::fs::write(&b.clip, "reply from B").unwrap();
    assert!(eventually(&a.clip, "reply from B").await, "B to A did not sync");

    // No echo: B received A's text and must not have sent it back as a new copy.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let mut local_copies_b = 0;
    while let Ok(ev) = eb.try_recv() {
        if matches!(ev, Event::LocalCopy { .. }) {
            local_copies_b += 1;
        }
    }
    assert_eq!(local_copies_b, 1, "B should report exactly one local copy (its own reply)");

    // An unpaired device is refused.
    let c = device();
    let stream = tokio::net::TcpStream::connect("127.0.0.1:47998").await.unwrap();
    let hs = uclip_core::secure::handshake(stream, &c.identity.private, true).await;
    if let Ok(mut s) = hs {
        assert!(s.reader.recv().await.is_err(), "unpaired device must be dropped");
    }

    ta.abort();
    tb.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_fails_when_one_side_declines() {
    let (a, b) = (device(), device());
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    let (ai, ast) = (a.identity.clone(), a.store.clone());
    let host = tokio::spawn(async move {
        let no: Confirm = Arc::new(|_| false);
        pairing::host(&ai, &ast, 0, Duration::from_secs(10), no, move |p| { let _ = port_tx.send(p); }).await
    });
    let port = port_rx.await.unwrap();
    let yes: Confirm = Arc::new(|_| true);
    let r = pairing::join(&b.identity, &b.store, 0, Some(format!("127.0.0.1:{port}").parse().unwrap()), Duration::from_secs(10), yes).await;
    assert!(r.is_err());
    assert!(host.await.unwrap().is_err());
    assert!(a.store.list().is_empty() && b.store.list().is_empty());
}
