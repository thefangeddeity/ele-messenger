# ele — ELE desktop client

One window, the same on Linux, macOS and Windows (egui).

- **ELE Mesh** — one shared group for the ELE machines within reach:
  Bluetooth and Wi-Fi, no internet or server needed, carried further by any
  machine in between. Everyone with the mesh passphrase reads every room.
- **ELE servers** — the full chat platform (accounts, direct messages,
  groups, images), found on the network over mDNS (`_ele-messenger._tcp`).
  *Open* shows a server's page in your browser.

## How it connects

1. If an elemesh daemon answers on `127.0.0.1:8740`, the client uses it.
2. Otherwise it runs its own node, with the same state directory and API as
   the daemon. Without a passphrase that node only *scouts*: it lists the ELE
   machines it hears but never advertises, dials or links. Entering the
   passphrase joins the mesh.

## Build and run

From the top of the repository:

```sh
cargo build --release -p ele-client   # binary: target/release/ele (ele.exe on Windows)
./target/release/ele
```

Linux needs the usual GUI libraries (X11 or Wayland, OpenGL). On Windows,
allow `ele.exe` through the firewall when asked, or the mesh can't hear other
machines over Wi-Fi.
