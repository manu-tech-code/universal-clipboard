#!/bin/sh
# Wraps the app bundle in a drag-to-Applications disk image: target/release/UniversalClipboard.dmg
set -e
cd "$(dirname "$0")/.."
./scripts/bundle-mac.sh
STAGE=$(mktemp -d)
cp -R "target/release/Universal Clipboard.app" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f target/release/UniversalClipboard.dmg
hdiutil create -quiet -volname "Universal Clipboard" -srcfolder "$STAGE" -ov -format UDZO target/release/UniversalClipboard.dmg
rm -rf "$STAGE"
echo "Built: target/release/UniversalClipboard.dmg"
