//! Menu bar app for macOS. It runs the sync engine in the background and gives it a small
//! menu: who is connected, pause, pair, start at login, quit. It has no window and no Dock icon.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tao::event::{Event as TaoEvent, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use uclip_core::clipboard;
use uclip_core::config::{self, DEFAULT_PORT};
use uclip_core::engine::{self, EngineConfig, Event};
use uclip_core::identity::Identity;
use uclip_core::pairing::{self, Confirm};
use uclip_core::peers::PeerStore;

const LABEL: &str = "dev.universal-clipboard.agent";

enum UserEvent {
    Engine(Event),
    Menu(MenuEvent),
}

// ---------- native dialogs through osascript (works from any thread) ----------

fn clean(s: &str) -> String {
    s.replace(['"', '\\'], "")
}

fn ask(message: &str, yes: &str, no: &str) -> bool {
    let script = format!(
        r#"display dialog "{}" with title "Universal Clipboard" buttons {{"{no}", "{yes}"}} default button "{yes}" with icon note"#,
        clean(message)
    );
    match Command::new("osascript").args(["-e", &script]).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).contains(&format!("button returned:{yes}")),
        _ => false,
    }
}

fn notify(message: &str) {
    let script = format!(r#"display notification "{}" with title "Universal Clipboard""#, clean(message));
    let _ = Command::new("osascript").args(["-e", &script]).output();
}

// ---------- start at login ----------

fn plist_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

fn start_at_login_enabled() -> bool {
    plist_path().exists()
}

fn set_start_at_login(on: bool) -> anyhow::Result<()> {
    let uid = String::from_utf8(Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
    let domain = format!("gui/{uid}");
    if on {
        let exe = std::env::current_exe()?;
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{}</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#,
            exe.display()
        );
        std::fs::create_dir_all(plist_path().parent().unwrap())?;
        std::fs::write(plist_path(), plist)?;
        let _ = Command::new("launchctl").args(["bootstrap", &domain]).arg(plist_path()).output();
    } else {
        let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{LABEL}")]).output();
        let _ = std::fs::remove_file(plist_path());
    }
    Ok(())
}

// ---------- icon: a clipboard drawn with distance fields, black on transparent so macOS tints it ----------

fn round_box(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let (qx, qy) = ((px - cx).abs() - hx + r, (py - cy).abs() - hy + r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

fn icon() -> Icon {
    let n = 44u32;
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let board = round_box(px, py, 22.0, 24.0, 13.0, 16.0, 4.0).abs() - 1.6; // outline
            let clip = round_box(px, py, 22.0, 9.0, 6.0, 4.0, 2.0); // filled tab
            let lines = [18.0, 25.0, 32.0].iter().map(|ly| round_box(px, py, 22.0, *ly, 7.0, 1.4, 1.4)).fold(f32::MAX, f32::min);
            let d = board.min(clip).min(lines);
            let a = (0.5 - d).clamp(0.0, 1.0);
            let i = ((y * n + x) * 4) as usize;
            rgba[i + 3] = (a * 255.0) as u8;
        }
    }
    Icon::from_rgba(rgba, n, n).expect("icon")
}

// ---------- app ----------

struct Ui {
    status: MenuItem,
    pause: CheckMenuItem,
    login: CheckMenuItem,
    pair_host: MenuItem,
    pair_join: MenuItem,
    quit: MenuItem,
    _tray: TrayIcon,
}

fn build_ui() -> Ui {
    let title = MenuItem::new("Universal Clipboard", false, None);
    let status = MenuItem::new("Starting\u{2026}", false, None);
    let pause = CheckMenuItem::new("Pause syncing", true, false, None);
    let pair_host = MenuItem::new("Pair: show a code on this Mac\u{2026}", true, None);
    let pair_join = MenuItem::new("Pair: join another device\u{2026}", true, None);
    let login = CheckMenuItem::new("Start at login", true, start_at_login_enabled(), None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &title,
        &status,
        &PredefinedMenuItem::separator(),
        &pause,
        &PredefinedMenuItem::separator(),
        &pair_host,
        &pair_join,
        &PredefinedMenuItem::separator(),
        &login,
        &quit,
    ])
    .expect("menu");
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon_templated(icon())
        .with_tooltip("Universal Clipboard")
        .build()
        .expect("tray icon");
    Ui { status, pause, login, pair_host, pair_join, quit, _tray: tray }
}

fn status_text(paused: bool, connected: &BTreeMap<String, String>, paired: usize) -> String {
    if paused {
        return "Paused".into();
    }
    if !connected.is_empty() {
        return format!("Connected: {}", connected.values().cloned().collect::<Vec<_>>().join(", "));
    }
    if paired == 0 { "No devices paired yet".into() } else { "Looking for your devices\u{2026}".into() }
}

fn confirm_dialog() -> Confirm {
    Arc::new(|digits: &str| ask(&format!("Pairing code: {digits}\n\nDoes the other device show the same code?"), "Yes, same code", "No"))
}

pub fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "uclip_core=info".into()))
        .init();
    let dir = config::data_dir();
    let identity = Identity::load_or_create(&dir)?;
    let store = PeerStore::open(&dir);
    let port: u16 = std::env::var("UCLIP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    let paused = Arc::new(AtomicBool::new(false));

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));

    let rt = tokio::runtime::Runtime::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let spec = std::env::var("UCLIP_CLIPBOARD").ok();
    let cfg = EngineConfig { identity: identity.clone(), store: store.clone(), port, clipboard: clipboard::from_spec(spec.as_deref())?, mdns: true, events: Some(tx), paused: paused.clone() };
    rt.spawn(async move {
        if let Err(e) = engine::run(cfg).await {
            tracing::error!("engine stopped: {e}");
        }
    });
    let fwd = proxy.clone();
    rt.spawn(async move {
        while let Some(ev) = rx.recv().await {
            if fwd.send_event(UserEvent::Engine(ev)).is_err() {
                break;
            }
        }
    });

    let mut ui: Option<Ui> = None;
    let mut connected: BTreeMap<String, String> = BTreeMap::new();

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            TaoEvent::NewEvents(StartCause::Init) => {
                let u = build_ui();
                u.status.set_text(status_text(false, &connected, store.list().len()));
                ui = Some(u);
            }
            TaoEvent::UserEvent(UserEvent::Engine(ev)) => {
                match ev {
                    Event::Connected { id, name } => {
                        connected.insert(id, name);
                    }
                    Event::Disconnected { id, .. } => {
                        connected.remove(&id);
                    }
                    _ => return,
                }
                if let Some(u) = &ui {
                    u.status.set_text(status_text(paused.load(Ordering::Relaxed), &connected, store.list().len()));
                }
            }
            TaoEvent::UserEvent(UserEvent::Menu(m)) => {
                let Some(u) = &ui else { return };
                if m.id == *u.quit.id() {
                    *flow = ControlFlow::Exit;
                } else if m.id == *u.pause.id() {
                    paused.store(u.pause.is_checked(), Ordering::Relaxed);
                    u.status.set_text(status_text(u.pause.is_checked(), &connected, store.list().len()));
                } else if m.id == *u.login.id() {
                    if let Err(e) = set_start_at_login(u.login.is_checked()) {
                        notify(&format!("Could not change start at login: {e}"));
                    }
                } else if m.id == *u.pair_host.id() {
                    let (identity, store) = (identity.clone(), store.clone());
                    rt.spawn(async move {
                        let result = pairing::host(&identity, &store, port, Duration::from_secs(120), confirm_dialog(), |_| {
                            notify("This Mac is waiting. On the other device, choose \u{201c}Join another device\u{201d}.")
                        })
                        .await;
                        match result {
                            Ok(p) => notify(&format!("Paired with {}.", p.name)),
                            Err(e) => notify(&format!("Pairing failed: {e}")),
                        }
                    });
                } else if m.id == *u.pair_join.id() {
                    let (identity, store) = (identity.clone(), store.clone());
                    rt.spawn(async move {
                        notify("Looking for a device that is waiting to pair\u{2026}");
                        match pairing::join(&identity, &store, port, None, Duration::from_secs(30), confirm_dialog()).await {
                            Ok(p) => notify(&format!("Paired with {}.", p.name)),
                            Err(e) => notify(&format!("Pairing failed: {e}")),
                        }
                    });
                }
            }
            _ => {}
        }
    });
}
