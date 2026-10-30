# ELE Messenger

> **ELEMESH**: **E**xtinction **L**evel **E**vent **M**essenger,
> **E**mergency **S**tore-and-**H**old

A household messenger that keeps working when the infrastructure doesn't.
Two laptops on battery, on different floors, after the power and the
internet are gone, can still talk.

There is one chat, the **ELE Mesh**, and no server. Every computer runs the
same small service, `elemesh`, which links to the others over whatever is
left and keeps a copy of every message:

| layer | what carries it | when it's gone |
|---|---|---|
| Bluetooth | nearby machines, no router, on battery | nothing below it |
| Wi-Fi / LAN | machines on the same network, router or not | Bluetooth still works |
| tailnet | machines anywhere, over the internet *(next)* | the house still works |

Each layer only adds reach: losing one never breaks the ones below. A
machine in the middle relays for machines that can't reach each other, and
a machine that was away catches up as soon as it is back in range.

## How it works

- **Store and hold.** Every node keeps the whole log and hands a peer
  whatever it is missing. Nothing is addressed to a server, nothing is lost
  because someone was asleep or out of range.
- **One passphrase.** Machines with the same mesh passphrase link; others
  can't, and can't read anything. Every link is encrypted (Noise XXpsk0).
- **No configuration.** Machines find each other by Bluetooth advertisements
  and Wi-Fi beacons; nothing is hardcoded.
- **Rooms.** Everyone in the mesh reads every room. Messages carry the
  writer's name, or the machine's name when none is given, and expire after
  7 days. You can delete your own messages, clear a room for everyone, or
  clear it just for yourself.

## What is in this repository

| path | what it is |
|---|---|
| `mesh/` | **elemesh**, the mesh service (Rust). Guide: `mesh/README.md` |
| `client/` | **ELE Messenger**, the desktop app (Rust, egui) for Linux, macOS and Windows |
| `docs/` | how to use it (`docs/USING-ELE.md`) and the design (`docs/MESH.md`) |

`mesh/` and `client/` are one Cargo workspace: `cargo build --release` at
the top builds both into `target/release/`.

## Install

One name and one icon on every system: **ELE Messenger**, the app, plus
the mesh service that keeps running when the app is closed. Each release
on GitHub carries the files below.

- **Arch Linux:** `yay -S ele-messenger`, then
  `systemctl --user enable --now elemesh` once.
- **Debian / Ubuntu (24.04 and later):** `sudo apt install ./ele-messenger_<version>_amd64.deb`,
  then `systemctl --user enable --now elemesh` once.
- **macOS:** open `ELE-Messenger-<version>-macos.dmg` and right-click → Open **Install ELE Messenger** (or, from a build, `sh dist/macos/install.sh`); it puts **ELE Messenger**
  and the **ELE Mesh** service in `~/Applications`; the service starts at login.
- **Windows:** unzip `ELE-Messenger-<version>-windows.zip` and run
  `powershell -ExecutionPolicy Bypass -File install.ps1`: ELE Messenger
  appears in the Start menu and the service starts at sign-in.

Every Linux package lays out the same files (`dist/linux/install-tree.sh`);
`dist/linux/Containerfile.deb` and `dist/windows/Containerfile` build the
.deb and the Windows zip on any Linux machine with podman.

Then open ELE Messenger, or http://127.0.0.1:8740 in a browser on that
computer, and enter the mesh passphrase.

## Status

- **Working, on real hardware:** Bluetooth and Wi-Fi links between Linux
  and macOS with no router or internet; offline catch-up; relaying; delete
  and clear; machine names; sharing a machine's mesh page with phones and
  tablets at `/elem/`, behind the passphrase or a one-time invite link.
- **Next:** the tailnet as a mesh transport; private messages and
  password-protected rooms.
- **Not yet:** a Mac can find other machines over Bluetooth but can't be
  found, so two Macs alone need Wi-Fi or a Linux machine between them.
- **Later:** name accounts, built on the mesh.

Until 1.5, this repository also held a separate chat server (`server.py`,
its web client and a Flutter client). It was removed so that there is one
chat; it remains in the history.

## License

GPL-3.0-or-later.
