#!/bin/sh
# Builds ele-messenger_<version>_amd64.deb for Debian and Ubuntu from
# binaries already built (cargo build --release puts them in target/release).
#   sh dist/linux/build-deb.sh [dir with elemesh and ele]
# Run from the top of the repository; needs dpkg-deb.
set -eu
bin=${1:-target/release}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' mesh/Cargo.toml | head -1)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
sh dist/linux/install-tree.sh "$bin" "$root"
mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: ele-messenger
Version: $version
Architecture: amd64
Maintainer: ELE Messenger <noreply@ele-messenger.invalid>
Depends: libc6, libgcc-s1, libdbus-1-3, libxkbcommon0, libgl1, bluez, hicolor-icon-theme
Section: net
Priority: optional
Homepage: https://github.com/thefangeddeity/ele-messenger
Description: Household messenger that keeps working without the internet
 ELE Messenger links the computers in a home over Bluetooth and Wi-Fi into
 one encrypted mesh chat that needs no router, server or internet.
CONTROL
echo /etc/nginx/snippets/ele-messenger-mesh.conf > "$root/DEBIAN/conffiles"
cat > "$root/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
echo "ELE Messenger: start the mesh service for your user (once):"
echo "    systemctl --user enable --now elemesh"
echo "then open ELE Messenger and enter your mesh passphrase."
POSTINST
chmod 755 "$root/DEBIAN/postinst"
out="ele-messenger_${version}_amd64.deb"
dpkg-deb --root-owner-group --build "$root" "$out"
echo "$out"
