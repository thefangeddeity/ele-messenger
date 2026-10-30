#!/bin/sh
# Lays out ELE Messenger under a package root, the same for every Linux
# package (the AUR PKGBUILD and the .deb), so they ship identical files.
#   sh dist/linux/install-tree.sh <dir with elemesh and ele> <package root>
# Run from the top of the repository.
set -eu
bin=$1 root=$2
install -Dm755 "$bin/elemesh" "$root/usr/bin/elemesh"
install -Dm755 "$bin/ele" "$root/usr/bin/ele"
sed 's|^ExecStart=.*|ExecStart=/usr/bin/elemesh run|' dist/linux/elemesh.service |
    install -Dm644 /dev/stdin "$root/usr/lib/systemd/user/elemesh.service"
install -Dm644 dist/nginx/elem.conf "$root/etc/nginx/snippets/ele-messenger-mesh.conf"
install -Dm644 client/assets/icon-linux.png "$root/usr/share/icons/hicolor/512x512/apps/ele.png"
install -Dm644 dist/linux/ele.desktop "$root/usr/share/applications/ele.desktop"
install -Dm644 mesh/README.md "$root/usr/share/doc/ele-messenger/README.md"
install -Dm644 docs/USING-ELE.md "$root/usr/share/doc/ele-messenger/USING-ELE.md"
