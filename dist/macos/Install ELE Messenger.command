#!/bin/sh
# Installs ELE Messenger from this disk image for this user: both apps into
# ~/Applications, the mesh service started now and at every login.
set -eu
src=$(cd "$(dirname "$0")" && pwd)
apps="$HOME/Applications"
label=org.ele-messenger.elemesh
agent="$HOME/Library/LaunchAgents/$label.plist"
mkdir -p "$apps" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
for a in "ELE Mesh.app" "ELE Messenger.app"; do
  rm -rf "$apps/$a"
  cp -R "$src/$a" "$apps/"
  xattr -dr com.apple.quarantine "$apps/$a" 2>/dev/null || true
done
cat > "$agent" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>$label</string>
	<key>ProgramArguments</key>
	<array><string>$apps/ELE Mesh.app/Contents/MacOS/elemesh</string><string>run</string></array>
	<key>AssociatedBundleIdentifiers</key><string>$label</string>
	<key>RunAtLoad</key><true/>
	<key>KeepAlive</key><true/>
	<key>StandardOutPath</key><string>$HOME/Library/Logs/elemesh.log</string>
	<key>StandardErrorPath</key><string>$HOME/Library/Logs/elemesh.log</string>
</dict>
</plist>
EOF
launchctl bootstrap "gui/$(id -u)" "$agent"
echo "ELE Messenger is installed in $apps. Open ELE Messenger and enter your mesh passphrase."
echo "The first start asks once for Bluetooth access: allow it."
open "$apps/ELE Messenger.app"
