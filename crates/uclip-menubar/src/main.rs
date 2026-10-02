//! The menu bar app exists for macOS only. Other platforms get the `uclip` command line for now.

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("universal-clipboard (menu bar app) is macOS only. Use `uclip run` on this platform.");
}
