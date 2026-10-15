#!/bin/sh
# Installs elemesh as ~/Applications/ELE Mesh.app and starts it at login.
# macOS grants Bluetooth per app, so the daemon lives in a bundle with a
# usage string; the first start asks once for Bluetooth access.
#   sh install.sh path/to/elemesh
# Run `elemesh init --passphrase …` once first.
set -eu
bin=${1:?usage: install.sh path/to/elemesh}
here=$(cd "$(dirname "$0")" && pwd)
app="$HOME/Applications/ELE Mesh.app"
label=org.ele-messenger.elemesh
agent="$HOME/Library/LaunchAgents/$label.plist"

mkdir -p "$app/Contents/MacOS" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
cp "$here/Info.plist" "$app/Contents/Info.plist"
cp "$bin" "$app/Contents/MacOS/elemesh"
# Ad-hoc signatures default to a cdhash requirement, which makes every
# rebuild a new app to macOS privacy and asks for Bluetooth again. Pin the
# requirement to the bundle identifier so the grant survives updates.
codesign --force --sign - --identifier "$label" \
  -r="designated => identifier \"$label\"" "$app"

cat > "$agent" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>$label</string>
	<key>ProgramArguments</key>
	<array><string>$app/Contents/MacOS/elemesh</string><string>run</string></array>
	<key>AssociatedBundleIdentifiers</key><string>$label</string>
	<key>RunAtLoad</key><true/>
	<key>KeepAlive</key><true/>
	<key>StandardOutPath</key><string>$HOME/Library/Logs/elemesh.log</string>
	<key>StandardErrorPath</key><string>$HOME/Library/Logs/elemesh.log</string>
</dict>
</plist>
EOF

launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$agent"
echo "installed; log: ~/Library/Logs/elemesh.log; chat: http://127.0.0.1:8740/"
