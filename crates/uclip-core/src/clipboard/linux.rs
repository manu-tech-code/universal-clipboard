//! Linux clipboard.
//!
//! Preferred path: watch the X11 clipboard with XFixes. XFixes reports every change of clipboard
//! owner without needing window focus, and on a Wayland desktop XWayland mirrors the Wayland
//! clipboard into X11. This is the only way for a background app to follow the clipboard on GNOME,
//! which offers no data-control protocol. It also lets us read the target list and skip anything a
//! password manager flagged as secret.
//!
//! Fallback, when there is no X server at all (pure Wayland without XWayland): poll through `arboard`,
//! which speaks the Wayland data-control protocol on KDE, sway and Hyprland.
//!
//! Writing always goes through `arboard`, which serves the selection for us on either system.

use super::*;
use std::cell::{Cell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};
use x11rb::NONE;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, WindowClass};
use x11rb::rust_connection::RustConnection;

/// Target that KeePassXC, Bitwarden, 1Password and others add to copies that hold a secret.
const SECRET_HINT: &str = "x-kde-passwordManagerHint";

#[derive(Default)]
struct Watched {
    counter: u64,
    text: Option<String>,
}

struct Atoms {
    clipboard: Atom,
    utf8: Atom,
    targets: Atom,
    prop: Atom,
    hint: Atom,
    incr: Atom,
}

/// What to do after learning which formats the clipboard owner offers.
#[derive(Debug, PartialEq)]
enum Next {
    FetchText,
    Skip,
}

fn decide(offered: &[Atom], a: &Atoms) -> Next {
    if offered.contains(&a.hint) || !offered.contains(&a.utf8) { Next::Skip } else { Next::FetchText }
}

fn intern(conn: &RustConnection, name: &str) -> Result<Atom> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

struct XWatcher {
    state: Arc<Mutex<Watched>>,
}

impl XWatcher {
    fn start() -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let win = conn.generate_id()?;
        conn.create_window(0, win, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new())?;
        conn.xfixes_query_version(5, 0)?.reply()?;
        let a = Atoms {
            clipboard: intern(&conn, "CLIPBOARD")?,
            utf8: intern(&conn, "UTF8_STRING")?,
            targets: intern(&conn, "TARGETS")?,
            prop: intern(&conn, "UCLIP_SELECTION")?,
            hint: intern(&conn, SECRET_HINT)?,
            incr: intern(&conn, "INCR")?,
        };
        conn.xfixes_select_selection_input(win, a.clipboard, SelectionEventMask::SET_SELECTION_OWNER)?;
        conn.flush()?;
        let state = Arc::new(Mutex::new(Watched::default()));
        let shared = state.clone();
        std::thread::Builder::new().name("x11-clipboard-watch".into()).spawn(move || watch_loop(conn, win, a, shared))?;
        Ok(Self { state })
    }
}

#[derive(Clone, Copy)]
enum Pending {
    Idle,
    Targets,
    Text,
}

fn watch_loop(conn: RustConnection, win: u32, a: Atoms, state: Arc<Mutex<Watched>>) {
    let publish = |text: Option<String>| {
        let mut s = state.lock().unwrap();
        s.text = text;
        s.counter += 1;
    };
    let mut pending = Pending::Idle;
    loop {
        let event = match conn.wait_for_event() {
            Ok(e) => e,
            Err(e) => {
                warn!("lost the X11 connection, clipboard sync from this device stops: {e}");
                return;
            }
        };
        let result: Result<()> = (|| {
            match event {
                Event::XfixesSelectionNotify(e) if e.selection == a.clipboard => {
                    if e.owner == NONE {
                        pending = Pending::Idle;
                        publish(None); // clipboard cleared
                    } else {
                        pending = Pending::Targets;
                        conn.convert_selection(win, a.clipboard, a.targets, a.prop, e.timestamp)?;
                        conn.flush()?;
                    }
                }
                Event::SelectionNotify(e) if e.requestor == win => match pending {
                    Pending::Targets => {
                        let offered: Vec<Atom> = if e.property == NONE {
                            vec![a.utf8] // owner does not answer TARGETS: just try for text
                        } else {
                            let r = conn.get_property(true, win, a.prop, AtomEnum::ATOM, 0, 1024)?.reply()?;
                            r.value32().map(|v| v.collect()).unwrap_or_default()
                        };
                        match decide(&offered, &a) {
                            Next::FetchText => {
                                pending = Pending::Text;
                                conn.convert_selection(win, a.clipboard, a.utf8, a.prop, e.time)?;
                                conn.flush()?;
                            }
                            Next::Skip => {
                                pending = Pending::Idle;
                                publish(None);
                            }
                        }
                    }
                    Pending::Text => {
                        pending = Pending::Idle;
                        if e.property == NONE {
                            publish(None);
                        } else {
                            let r = conn.get_property(true, win, a.prop, AtomEnum::ANY, 0, (MAX_TEXT / 4 + 1) as u32)?.reply()?;
                            // INCR (chunked transfer) and oversize values are skipped.
                            let ok = r.type_ != a.incr && r.bytes_after == 0;
                            publish(if ok { String::from_utf8(r.value).ok() } else { None });
                        }
                    }
                    Pending::Idle => {}
                },
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = result {
            debug!("clipboard event handling failed: {e}");
            pending = Pending::Idle;
        }
    }
}

// ---------- polling fallback ----------

#[derive(Default)]
struct PollState {
    hash: Option<u64>,
    text: Option<String>,
    counter: u64,
}

fn hash_of(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

enum Backend {
    Watch(XWatcher),
    Poll,
}

pub struct LinuxClipboard {
    backend: RefCell<Option<Backend>>,
    poll: RefCell<PollState>,
    arboard: RefCell<Option<arboard::Clipboard>>,
    warned: Cell<bool>,
}

impl LinuxClipboard {
    pub fn new() -> Self {
        Self { backend: RefCell::new(None), poll: RefCell::new(PollState::default()), arboard: RefCell::new(None), warned: Cell::new(false) }
    }

    /// Connects lazily and retries on every call, so a service that starts before the desktop
    /// session is up begins working as soon as the display appears.
    fn with_arboard<R>(&self, f: impl FnOnce(&mut arboard::Clipboard) -> R) -> Option<R> {
        let mut inner = self.arboard.borrow_mut();
        if inner.is_none() {
            *inner = Some(arboard::Clipboard::new().ok()?);
        }
        inner.as_mut().map(f)
    }

    fn ensure(&self) -> bool {
        if self.backend.borrow().is_some() {
            return true;
        }
        if std::env::var_os("DISPLAY").is_some() {
            match XWatcher::start() {
                Ok(w) => {
                    info!("following the clipboard through X11 (XWayland on Wayland desktops)");
                    *self.backend.borrow_mut() = Some(Backend::Watch(w));
                    return true;
                }
                Err(e) => debug!("no X server to watch: {e}"),
            }
        }
        if self.with_arboard(|_| ()).is_some() {
            info!("following the clipboard by polling (Wayland data-control)");
            *self.backend.borrow_mut() = Some(Backend::Poll);
            return true;
        }
        if !self.warned.replace(true) {
            warn!("no clipboard available yet. Is a desktop session running? Will keep retrying.");
        }
        false
    }

    fn poll_token(&self) -> u64 {
        let polled = self.with_arboard(|c| c.get_text());
        let text = match polled {
            Some(Ok(t)) => Some(t),
            Some(Err(arboard::Error::ContentNotAvailable)) => None, // empty, or not text
            _ => return self.poll.borrow().counter,                 // transient failure: learn nothing
        };
        let mut st = self.poll.borrow_mut();
        let hash = text.as_deref().map(hash_of);
        if hash != st.hash {
            st.hash = hash;
            st.text = text;
            st.counter += 1;
        }
        st.counter
    }
}

fn usable(text: &str) -> Option<ClipItem> {
    (!text.is_empty() && text.len() <= MAX_TEXT).then(|| ClipItem::Text(text.to_string()))
}

impl Clipboard for LinuxClipboard {
    fn token(&self) -> u64 {
        if !self.ensure() {
            return 0;
        }
        match &*self.backend.borrow() {
            Some(Backend::Watch(w)) => w.state.lock().unwrap().counter,
            Some(Backend::Poll) => self.poll_token(),
            None => 0,
        }
    }

    fn read(&self) -> Option<ClipItem> {
        match &*self.backend.borrow() {
            Some(Backend::Watch(w)) => usable(w.state.lock().unwrap().text.as_deref()?),
            Some(Backend::Poll) => usable(self.poll.borrow().text.as_deref()?),
            None => None,
        }
    }

    fn write(&self, item: &ClipItem) -> Result<()> {
        let ClipItem::Text(text) = item;
        match self.with_arboard(|c| c.set_text(text.clone())) {
            Some(r) => Ok(r?),
            None => bail!("no clipboard available"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms() -> Atoms {
        Atoms { clipboard: 1, utf8: 2, targets: 3, prop: 4, hint: 5, incr: 6 }
    }

    #[test]
    fn secrets_and_non_text_are_skipped() {
        let a = atoms();
        assert_eq!(decide(&[2, 9], &a), Next::FetchText);
        assert_eq!(decide(&[2, 5], &a), Next::Skip, "password manager hint");
        assert_eq!(decide(&[9, 10], &a), Next::Skip, "no text on offer");
    }

    /// Needs a display (run under Xvfb). Skips quietly without one.
    #[test]
    fn set_then_read_back_and_token_changes() {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            eprintln!("no display, skipping");
            return;
        }
        let cb = LinuxClipboard::new();
        let before = cb.token();
        cb.write(&ClipItem::Text("hello linux".into())).unwrap();
        let mut after = before;
        for _ in 0..40 {
            after = cb.token();
            if after != before {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_ne!(before, after, "token must change after a write");
        assert_eq!(cb.read(), Some(ClipItem::Text("hello linux".into())));
        assert_eq!(cb.token(), after, "no change, no new token");
    }
}
