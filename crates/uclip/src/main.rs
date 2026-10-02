use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uclip_core::config::{self, DEFAULT_PORT};
use uclip_core::engine::{self, EngineConfig, Event};
use uclip_core::identity::Identity;
use uclip_core::pairing::{self, Confirm};
use uclip_core::peers::PeerStore;
use uclip_core::clipboard;

#[derive(Parser)]
#[command(name = "uclip", version, about = "Universal clipboard: copy on one device, paste on another")]
struct Cli {
    /// Port the background service listens on
    #[arg(long, global = true, env = "UCLIP_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the background service that syncs your clipboard
    Run {
        /// Use a text file as the clipboard instead of the system one (for testing)
        #[arg(long, env = "UCLIP_CLIPBOARD")]
        clipboard: Option<String>,
        /// Turn off local network discovery and rely on stored addresses
        #[arg(long)]
        no_mdns: bool,
    },
    /// Pair with another device
    Pair {
        #[command(subcommand)]
        role: Role,
    },
    /// Show this device and the devices paired with it
    Status,
    /// Remove a paired device by id
    Forget { id: String },
}

#[derive(Subcommand)]
enum Role {
    /// Wait here for the other device to join
    Host,
    /// Join a device that is waiting. Finds it automatically unless --addr is given
    Join {
        #[arg(long)]
        addr: Option<SocketAddr>,
    },
}

fn confirm_on_terminal() -> Confirm {
    Arc::new(|digits: &str| {
        println!("\n    Pairing code:  {digits}\n");
        print!("Does the other device show the same code? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "uclip=info,uclip_core=info".into()))
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let dir = config::data_dir();
    let identity = Identity::load_or_create(&dir)?;
    let store = PeerStore::open(&dir);

    match cli.command {
        Cmd::Status => {
            println!("This device:  {} ({})", identity.name, identity.id());
            println!("Data folder:  {}", dir.display());
            let peers = store.list();
            if peers.is_empty() {
                println!("\nNo paired devices yet. Run `uclip pair host` here and `uclip pair join` on the other device.");
            }
            for p in peers {
                println!("\nPaired:       {} ({})\n  last address {}", p.name, p.id, p.addr.as_deref().unwrap_or("unknown"));
            }
        }
        Cmd::Forget { id } => {
            if store.remove(&id)? {
                println!("Removed {id}. A running service drops the connection within a few seconds.");
            } else {
                bail!("no paired device with id {id}");
            }
        }
        Cmd::Pair { role } => {
            let peer = match role {
                Role::Host => {
                    println!("Waiting for another device to run `uclip pair join` (2 minutes)...");
                    pairing::host(&identity, &store, cli.port, Duration::from_secs(120), confirm_on_terminal(), |p| println!("Listening on port {p}. This device: {}", identity.name)).await?
                }
                Role::Join { addr } => {
                    println!("Looking for a device that is waiting to pair...");
                    pairing::join(&identity, &store, cli.port, addr, Duration::from_secs(30), confirm_on_terminal()).await?
                }
            };
            println!("Paired with {} ({}). Start the service with `uclip run` on both devices.", peer.name, peer.id);
        }
        Cmd::Run { clipboard: spec, no_mdns } => {
            let factory = clipboard::from_spec(spec.as_deref()).context("opening the clipboard")?;
            let (tx, mut rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    match ev {
                        Event::Listening { port } => println!("Listening on port {port}"),
                        Event::Connected { name, .. } => println!("Connected to {name}"),
                        Event::Disconnected { name, .. } => println!("Lost {name}"),
                        Event::LocalCopy { preview, sent_to } => println!("Copied {preview:?}, sent to {sent_to} device(s)"),
                        Event::Received { from, preview } => println!("Received {preview:?} from {from}"),
                    }
                }
            });
            let cfg = EngineConfig { identity, store, port: cli.port, clipboard: factory, mdns: !no_mdns, events: Some(tx), paused: Default::default() };
            tokio::select! {
                r = engine::run(cfg) => r?,
                _ = tokio::signal::ctrl_c() => println!("\nStopped."),
            }
        }
    }
    Ok(())
}
