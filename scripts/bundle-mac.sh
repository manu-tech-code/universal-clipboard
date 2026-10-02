#!/bin/sh
# Builds "Universal Clipboard.app": a menu bar app with no Dock icon.
# Ad-hoc signed, which is enough to run on the Mac that built it. Distributing it to other
# Macs needs a Developer ID signature and notarization.
set -e
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build --release -p uclip-menubar -p uclip

APP="target/release/Universal Clipboard.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/release/universal-clipboard "$APP/Contents/MacOS/universal-clipboard"
cp target/release/uclip "$APP/Contents/MacOS/uclip"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>dev.universal-clipboard</string>
  <key>CFBundleName</key><string>Universal Clipboard</string>
  <key>CFBundleExecutable</key><string>universal-clipboard</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Universal Clipboard finds your other devices on the local network to sync your clipboard.</string>
  <key>NSBonjourServices</key><array><string>_uclip._tcp</string></array>
</dict></plist>
PLIST

codesign --force --sign - "$APP"
echo "Built: $APP"
