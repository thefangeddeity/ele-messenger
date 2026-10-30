#!/bin/sh
# Installs ELE Messenger for this user:
# - ~/Applications/ELE Mesh.app, the mesh service, started at login. macOS
#   grants Bluetooth per app, so it lives in a bundle with a usage string;
#   its first start asks once for Bluetooth access.
# - ~/Applications/ELE Messenger.app, the app, with its icon.
#   sh dist/macos/install.sh [dir with elemesh and ele]   (default target/release)
# Run from the top of the repository.
set -eu
arg=${1:-target/release}
# Older usage passed the elemesh binary itself.
case $arg in */elemesh) bin=$(dirname "$arg") ;; *) bin=$arg ;; esac
here=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../mesh/Cargo.toml" | head -1)
app="$HOME/Applications/ELE Mesh.app"
label=org.ele-messenger.elemesh
agent="$HOME/Library/LaunchAgents/$label.plist"

mkdir -p "$app/Contents/MacOS" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
sed "s/@VERSION@/$version/" "$here/Info.plist" > "$app/Contents/Info.plist"
cp "$bin/elemesh" "$app/Contents/MacOS/elemesh"
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

# The app: same ad-hoc signing, so a rebuild keeps its Bluetooth grant too.
if [ -x "$bin/ele" ]; then
  ui="$HOME/Applications/ELE Messenger.app"
  mkdir -p "$ui/Contents/MacOS" "$ui/Contents/Resources"
  sed "s/@VERSION@/$version/" "$here/Messenger.Info.plist" > "$ui/Contents/Info.plist"
  cp "$bin/ele" "$ui/Contents/MacOS/ele"
  iconset=$(mktemp -d)/ele.iconset
  mkdir -p "$iconset"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$here/../../client/assets/icon.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2)); [ $d -le 1024 ] && sips -z $d $d "$here/../../client/assets/icon.png" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$ui/Contents/Resources/ele.icns"
  codesign --force --sign - --identifier org.ele-messenger.app     -r="designated => identifier \"org.ele-messenger.app\"" "$ui"
  touch "$ui"
fi
echo "installed $version; log: ~/Library/Logs/elemesh.log; chat: http://127.0.0.1:8740/"
