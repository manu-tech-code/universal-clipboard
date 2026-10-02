//! The sync engine: listens, finds paired peers, keeps one encrypted session per peer, and
//! moves clipboard changes both ways.

use crate::clipboard::{Clipboard, Factory};
use crate::discovery;
use crate::identity::Identity;
use crate::peers::{Peer, PeerStore};
use crate::secure::{self, Secure};
use crate::wire::{ClipItem, ClipMsg, Msg, PROTOCOL_VERSION};
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinSet;
use tokio::time::{interval, sleep, timeout};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub enum Event {
    Listening { port: u16 },
    Connected { id: String, name: String },
    Disconnected { id: String, name: String },
    LocalCopy { preview: String, sent_to: usize },
    Received { from: String, preview: String },
}

pub struct EngineConfig {
    pub identity: Identity,
    pub store: PeerStore,
    /// Preferred listen port. If it is taken, an ephemeral one is used.
    pub port: u16,
    pub clipboard: Factory,
    pub mdns: bool,
    pub events: Option<UnboundedSender<Event>>,
    /// While true, nothing is sent and incoming copies are ignored. Connections stay up.
    pub paused: Arc<AtomicBool>,
}

struct Session {
    conn: u64,
    tx: mpsc::Sender<Msg>,
    name: String,
}

struct Shared {
    me: String,
    identity: Identity,
    store: PeerStore,
    port: u16,
    sessions: Mutex<HashMap<String, Session>>,
    connecting: Mutex<HashSet<String>>,
    known_addrs: Mutex<HashMap<String, Vec<SocketAddr>>>,
    seen: Mutex<VecDeque<String>>,
    last_applied: Mutex<Option<(ClipItem, Instant)>>,
    to_clipboard: std::sync::mpsc::Sender<ClipItem>,
    events: Option<UnboundedSender<Event>>,
    paused: Arc<AtomicBool>,
    next_conn: AtomicU64,
}

impl Shared {
    fn emit(&self, e: Event) {
        if let Some(tx) = &self.events {
            let _ = tx.send(e);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn random_id() -> String {
    hex::encode(rand::random::<[u8; 8]>())
}

/// Runs until the returned future is dropped or aborted. Everything it spawns stops with it.
pub async fn run(cfg: EngineConfig) -> Result<()> {
    let listener = match TcpListener::bind(("0.0.0.0", cfg.port)).await {
        Ok(l) => l,
        Err(e) => {
            warn!("port {} unavailable ({e}), using a random port", cfg.port);
            TcpListener::bind(("0.0.0.0", 0)).await?
        }
    };
    let port = listener.local_addr()?.port();

    let (to_clipboard, from_engine) = std::sync::mpsc::channel();
    let (local_tx, mut local_rx) = mpsc::unbounded_channel();
    let factory = cfg.clipboard;
    std::thread::spawn(move || clipboard_thread(factory, local_tx, from_engine));

    let sh = Arc::new(Shared {
        me: cfg.identity.id(),
        identity: cfg.identity,
        store: cfg.store,
        port,
        sessions: Mutex::new(HashMap::new()),
        connecting: Mutex::new(HashSet::new()),
        known_addrs: Mutex::new(HashMap::new()),
        seen: Mutex::new(VecDeque::new()),
        last_applied: Mutex::new(None),
        to_clipboard,
        events: cfg.events,
        paused: cfg.paused,
        next_conn: AtomicU64::new(1),
    });
    sh.emit(Event::Listening { port });
    info!("device {} ({}) listening on port {port}", sh.me, sh.identity.name);

    let mut tasks = JoinSet::new();
    let mut _daemon = None;
    if cfg.mdns {
        match discovery::advertise(&sh.me, &sh.identity.name, port, false).and_then(|d| Ok((discovery::browse(&d)?, d))) {
            Ok((mut found, d)) => {
                _daemon = Some(d);
                let sh2 = sh.clone();
                tasks.spawn(async move {
                    while let Some(f) = found.recv().await {
                        if f.id == sh2.me {
                            continue;
                        }
                        let addrs: Vec<SocketAddr> = f.addrs.iter().map(|ip| SocketAddr::new(*ip, f.port)).collect();
                        sh2.known_addrs.lock().unwrap().insert(f.id.clone(), addrs);
                        if let Some(peer) = sh2.store.get(&f.id) {
                            maybe_connect(sh2.clone(), peer);
                        }
                    }
                });
            }
            Err(e) => warn!("mDNS unavailable ({e}); only stored addresses will be used"),
        }
    }

    let sh2 = sh.clone();
    tasks.spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let sh = sh2.clone();
                    tokio::spawn(async move {
                        if let Err(e) = run_session(sh, stream, false, None).await {
                            debug!("inbound connection from {addr} ended: {e}");
                        }
                    });
                }
                Err(e) => warn!("accept failed: {e}"),
            }
        }
    });

    // Reconnect loop and housekeeping: dial paired peers we are not connected to, and drop
    // sessions for peers that were removed from the circle.
    let sh2 = sh.clone();
    tasks.spawn(async move {
        let mut tick = interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            let peers = sh2.store.list();
            let ids: HashSet<&String> = peers.iter().map(|p| &p.id).collect();
            sh2.sessions.lock().unwrap().retain(|id, _| ids.contains(id));
            for p in peers {
                maybe_connect(sh2.clone(), p);
            }
        }
    });

    let sh2 = sh.clone();
    tasks.spawn(async move {
        while let Some(item) = local_rx.recv().await {
            local_copy(&sh2, item);
        }
    });

    while tasks.join_next().await.is_some() {}
    Ok(())
}

fn local_copy(sh: &Arc<Shared>, item: ClipItem) {
    if sh.paused.load(Ordering::Relaxed) {
        return;
    }
    if let Some((applied, at)) = &*sh.last_applied.lock().unwrap() {
        if *applied == item && at.elapsed() < Duration::from_secs(5) {
            return; // the OS reporting our own write back to us
        }
    }
    let msg = ClipMsg { id: random_id(), origin: sh.me.clone(), ts_ms: now_ms(), item: item.clone() };
    let sessions = sh.sessions.lock().unwrap();
    let mut sent = 0;
    for s in sessions.values() {
        if s.tx.try_send(Msg::Clip(msg.clone())).is_ok() {
            sent += 1;
        }
    }
    drop(sessions);
    info!("copied {:?} -> sent to {sent} device(s)", item.preview());
    sh.emit(Event::LocalCopy { preview: item.preview(), sent_to: sent });
}

fn remote_copy(sh: &Arc<Shared>, from: &str, msg: ClipMsg) {
    if sh.paused.load(Ordering::Relaxed) {
        return;
    }
    {
        let mut seen = sh.seen.lock().unwrap();
        if seen.contains(&msg.id) {
            return;
        }
        seen.push_back(msg.id.clone());
        if seen.len() > 64 {
            seen.pop_front();
        }
    }
    *sh.last_applied.lock().unwrap() = Some((msg.item.clone(), Instant::now()));
    let _ = sh.to_clipboard.send(msg.item.clone());
    info!("received {:?} from {from}", msg.item.preview());
    sh.emit(Event::Received { from: from.to_string(), preview: msg.item.preview() });
}

fn candidates(sh: &Shared, peer: &Peer) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = sh.known_addrs.lock().unwrap().get(&peer.id).cloned().unwrap_or_default();
    if let Some(a) = peer.addr.as_deref().and_then(|a| a.parse().ok()) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

fn maybe_connect(sh: Arc<Shared>, peer: Peer) {
    if peer.id == sh.me || sh.sessions.lock().unwrap().contains_key(&peer.id) {
        return;
    }
    if !sh.connecting.lock().unwrap().insert(peer.id.clone()) {
        return;
    }
    tokio::spawn(async move {
        for addr in candidates(&sh, &peer) {
            if let Ok(Ok(stream)) = timeout(Duration::from_secs(3), TcpStream::connect(addr)).await {
                if let Err(e) = run_session(sh.clone(), stream, true, Some(peer.clone())).await {
                    debug!("session with {} ended: {e}", peer.name);
                }
                break;
            }
        }
        sh.connecting.lock().unwrap().remove(&peer.id);
    });
}

async fn run_session(sh: Arc<Shared>, stream: TcpStream, initiator: bool, expect: Option<Peer>) -> Result<()> {
    let Secure { mut reader, mut writer, remote_static, .. } =
        timeout(Duration::from_secs(10), secure::handshake(stream, &sh.identity.private, initiator)).await??;
    let Some(peer) = sh.store.find_by_public(&remote_static) else { bail!("not a paired device") };
    if let Some(e) = &expect {
        if e.id != peer.id {
            bail!("connected to {} but expected {}", peer.id, e.id);
        }
    }
    writer
        .send(&Msg::Hello { name: sh.identity.name.clone(), os: crate::config::os_name().into(), version: PROTOCOL_VERSION, listen_port: sh.port })
        .await?;
    match timeout(Duration::from_secs(10), reader.recv()).await?? {
        Msg::Hello { .. } => {}
        _ => bail!("expected Hello"),
    }

    let (tx, mut rx) = mpsc::channel::<Msg>(64);
    let conn = sh.next_conn.fetch_add(1, Ordering::Relaxed);
    let replaced;
    let initiator_id = if initiator { sh.me.clone() } else { peer.id.clone() };
    {
        // Both sides may dial at once. Keep the connection opened by the smaller id; both
        // ends apply the same rule, so they agree on which one survives.
        let mut sessions = sh.sessions.lock().unwrap();
        if sessions.contains_key(&peer.id) && initiator_id != sh.me.clone().min(peer.id.clone()) {
            return Ok(());
        }
        replaced = sessions.insert(peer.id.clone(), Session { conn, tx: tx.clone(), name: peer.name.clone() }).is_some();
    }
    if !replaced {
        info!("connected to {} ({})", peer.name, peer.id);
        sh.emit(Event::Connected { id: peer.id.clone(), name: peer.name.clone() });
    }

    let writer_task = tokio::spawn(async move {
        let mut ping = interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => if writer.send(&m).await.is_err() { break },
                    None => break,
                },
                _ = ping.tick() => if writer.send(&Msg::Ping).await.is_err() { break },
            }
        }
    });

    let last_heard = Arc::new(AtomicU64::new(now_ms()));
    let (sh2, lh, from, ptx) = (sh.clone(), last_heard.clone(), peer.name.clone(), tx.clone());
    let mut reader_task = tokio::spawn(async move {
        while let Ok(msg) = reader.recv().await {
            lh.store(now_ms(), Ordering::Relaxed);
            match msg {
                Msg::Clip(c) => remote_copy(&sh2, &from, c),
                Msg::Ping => {
                    let _ = ptx.try_send(Msg::Pong);
                }
                _ => {}
            }
        }
    });
    drop(tx);

    loop {
        tokio::select! {
            _ = &mut reader_task => break,
            _ = sleep(Duration::from_secs(5)) => {
                if now_ms().saturating_sub(last_heard.load(Ordering::Relaxed)) > 30_000 {
                    reader_task.abort();
                    break;
                }
            }
        }
    }
    writer_task.abort();
    let mut sessions = sh.sessions.lock().unwrap();
    if sessions.get(&peer.id).map(|s| s.conn) == Some(conn) {
        let s = sessions.remove(&peer.id).unwrap();
        drop(sessions);
        info!("disconnected from {}", s.name);
        sh.emit(Event::Disconnected { id: peer.id, name: s.name });
    }
    Ok(())
}

/// Owns the clipboard on its own thread: writes what peers send, reports what the user copies.
fn clipboard_thread(factory: Factory, local: UnboundedSender<ClipItem>, incoming: std::sync::mpsc::Receiver<ClipItem>) {
    let cb: Box<dyn Clipboard> = factory();
    let mut last = cb.token();
    loop {
        loop {
            match incoming.try_recv() {
                Ok(item) => {
                    if let Err(e) = cb.write(&item) {
                        warn!("could not write the clipboard: {e}");
                    }
                    last = cb.token(); // our own write is not a new copy
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }
        let token = cb.token();
        if token != last {
            last = token;
            if let Some(item) = cb.read() {
                if local.send(item).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}
