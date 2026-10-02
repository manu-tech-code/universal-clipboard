# Universal Clipboard

Copy on one device, paste on another. Mac, Windows, Linux and Android, with the iPhone joining
through the Mac by way of Apple's own Continuity. This repo holds the Rust core and the first
Mac app. The interactive design prototype is `universal-clipboard-prototype.html`: open it in a browser.

## What works today

- Pair two devices with a six digit code shown on both screens (no accounts, no server).
- Encrypted connection between paired devices (Noise XX over TCP, ChaCha20-Poly1305).
- Finds paired devices on the local network with mDNS, reconnects after drops, and falls back to the
  address stored at pairing time (so Tailscale addresses work).
- Plain text clipboard sync both ways, with echo-loop protection.
- Skips anything a password manager marks as concealed or transient.
- macOS menu bar app: status, pause, pair, start at login, quit. No Dock icon.
- Linux clipboard on X11 and Wayland (see below). Linux uses the `uclip` command line and a systemd user service.
- Because it watches the real Mac clipboard, anything the iPhone copies (delivered by Apple
  Continuity) is forwarded to your other devices, and the reverse.

Not built yet: images and files, the Windows clipboard backend, Android, a Linux tray app, the relay,
QUIC transport. See the roadmap on the prototype's "How it works" tab.

## Layout

| Path | What it is |
| --- | --- |
| `crates/uclip-core` | The library: identity, peers, wire format, Noise transport, mDNS, pairing, sync engine, clipboard backends |
| `crates/uclip` | `uclip` command line: `run`, `pair host`, `pair join`, `status`, `forget` |
| `crates/uclip-menubar` | The macOS menu bar app (`universal-clipboard`) |
| `scripts/bundle-mac.sh` | Builds `Universal Clipboard.app` (ad-hoc signed) |
| `scripts/linux-test.sh`, `scripts/linux-wayland-test.sh` | Build and test on Linux inside a Docker container (X11, and sway for Wayland) |
| `contrib/uclip.service` | systemd user service for Linux |
| `spike/` | Throwaway experiments that validated the iPhone bridge |

## Install

**macOS:** `./scripts/make-dmg.sh` builds `target/release/UniversalClipboard.dmg`. Open it and drag the app to
Applications. The app is not notarized yet, so on first launch right-click it and choose Open.
GitHub Actions builds the same disk image when a version tag is pushed (see Releases).

**Linux and Windows:** installers are not built yet. For now see the Linux section below.

## Build and test

Needs Rust (`brew install rustup && rustup default stable`).

```sh
cargo test                 # unit tests plus two-device end-to-end tests over loopback
./scripts/bundle-mac.sh    # builds target/release/Universal Clipboard.app
```

## Try it

On the first Mac (or any machine with the CLI):

```sh
uclip pair host            # shows a code, waits two minutes
```

On the second device:

```sh
uclip pair join            # finds the host automatically, or pass --addr ip:port
```

Both screens show the same six digits. Confirm on both, then start `uclip run` (or the menu bar
app) on each device. Copy on one, paste on the other.

Data lives in `~/Library/Application Support/dev.universal-clipboard.UniversalClipboard`
(set `UCLIP_HOME` to use another folder). The private key is stored there with mode 0600.

For testing without touching the real clipboard, `uclip run --clipboard file:/tmp/clip.txt`
uses a text file as the clipboard.

## Linux

Needs Rust and nothing else (no system libraries). Build and install:

```sh
cargo install --path crates/uclip
```

Then pair as above and run it as a user service so it starts with your desktop session:

```sh
mkdir -p ~/.config/systemd/user && cp contrib/uclip.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now uclip
```

How the clipboard is reached:

- **Watching (preferred):** the app follows the X11 clipboard with XFixes change events, so it only reads
  when the clipboard actually changes and needs no window focus. On a Wayland desktop XWayland mirrors
  the Wayland clipboard into X11, which is the only way for a background app to follow it on GNOME
  (Ubuntu's default). Copies flagged by password managers (`x-kde-passwordManagerHint`) are skipped.
- **Polling (fallback):** with no X server at all, it polls through the `arboard` crate, which speaks
  the Wayland data-control protocol on KDE, sway and Hyprland. The secret flag is not checked on this path.
- **Writing:** always through `arboard`, which uses Wayland data-control or X11 as available.

Tested in a Linux container: unit and end-to-end tests, then a live pair-and-sync run on real X11
(xclip, with unicode and multiline text, through the XFixes watcher) and on sway with X11 switched off
(polling path). Not tested: a real GNOME session, KDE, or a physical machine.

## How it fits together

```
 clipboard thread ──local copy──▶ engine ──Msg::Clip──▶ session (Noise) ──▶ peer
        ▲                           │
        └──────write from peer──────┘        mDNS browse ──▶ connect to paired peers
```

- A device id is the first 8 bytes of SHA-256 of its public key, so an id cannot be claimed without
  the key.
- Only devices whose key is in `peers.json` can connect. Everyone else is dropped after the handshake.
- Both ends may dial at once. They keep the connection opened by the smaller id.
- The clipboard backend sits on its own thread. It polls a cheap change token (macOS: the
  pasteboard change count) and writes incoming items without reporting them back.

## Known limits

- macOS asks for Local Network permission the first time. Allow it, or discovery will not find peers.
- The `.app` is ad-hoc signed. Sharing it with other Macs needs a Developer ID signature and notarization.
- The Linux GNOME route (XWayland watcher) is untested on a real GNOME session.
- The menu bar app's icon and menus have not been checked visually yet, and its pairing dialogs have
  only been compiled, not clicked through. The engine behind them is covered by tests and a live run.
- Still to check with the iPhone bridge: images and files from the iPhone, a locked iPhone,
  a sleeping Mac, and long idle gaps.
