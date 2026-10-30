#!/bin/sh
# Builds ELE-Messenger-<version>-macos.dmg from a build (target/release):
# ELE Messenger.app, ELE Mesh.app (the service) and an installer script that
# puts both in ~/Applications and starts the service at login.
#   sh dist/macos/build-dmg.sh [dir with elemesh and ele]
# Run from the top of the repository on a Mac.
set -eu
bin=${1:-target/release}
here=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' mesh/Cargo.toml | head -1)
stage=$(mktemp -d)/ELE-Messenger
mkdir -p "$stage"
# install.sh lays the bundles out and signs them; ELE_STAGE makes it build
# into the image instead of installing.
ELE_STAGE="$stage" sh "$here/install.sh" "$bin"
cp "$here/Install ELE Messenger.command" "$stage/"
chmod 755 "$stage/Install ELE Messenger.command"
out="ELE-Messenger-$version-macos.dmg"
rm -f "$out"
hdiutil create -volname "ELE Messenger $version" -srcfolder "$stage" -format UDZO -ov "$out" >/dev/null
echo "$out"
