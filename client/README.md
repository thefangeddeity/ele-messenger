# ele — ELE desktop client

One window, the same on Linux, macOS and Windows (egui).

- **ELE Mesh** — one shared group for the ELE machines within reach:
  Bluetooth and Wi-Fi, no internet or server needed, carried further by any
  machine in between. Everyone with the mesh passphrase reads every room.
- The **☰** menu: this machine (rename), the mesh passphrase (join or
  change), this room (clear for me, clear for everyone), appearance, about.
  Right-click one of your own messages to delete it for everyone.

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
