# ele-messenger

> **ELEMESH** — **E**xtinction **L**evel **E**vent **M**essenger,
> **E**mergency **S**tore-and-**H**old

A household messenger that keeps working when the infrastructure does not.

The second half of the name is the mechanism, not a slogan. Every node keeps
the whole log and **holds** it; when a peer comes within reach it hands over
whatever that peer is missing. Nothing is addressed at a server, nothing
expires because a recipient was asleep, and a node switched off for a week
catches up by being switched on near another one.

`elemesh` is also, literally, the name of the daemon that does it.

It is built for the case where the internet is gone and possibly the router
with it: a small server on a machine in the house, a browser client, and
peer-to-peer replication between machines over the local network. No account
with anyone, no phone number, no relay you do not own.

---

## Part of the `laptop-comms` suite

ele-messenger is one of two packages built to run together on the same
machine:

| package | what it is |
|---|---|
| **ele-messenger** | this one — the household messenger |
| **[laptop-livecam](https://github.com/thefangeddeity/laptop-livecam)** | a camera appliance for old laptops and webcams |

Either installs and runs alone. Installed together on one node they share an
origin and a set of names, so nothing needs configuring to make them
cooperate:

| path | what answers |
|---|---|
| `/` | a home page naming the node, with a door to each app |
| `/llc` | the camera viewer |
| `/elem` | this messenger |

Because they share an origin, the camera viewer grows a **Messages card** that
reads and sends through this server directly, and both pages share a theme and
one light/dark switch.

---

## How it degrades

The point of the project is what happens as each layer fails.

| layer | carries | when it goes |
|---|---|---|
| internet | everything | the tailnet and the LAN still work |
| tailnet | everything, off-site | the LAN still works |
| **LAN / Wi-Fi** | **everything, in the house** | a node can host its own access point |
| mesh / BLE | text | two laptops on battery, in Bluetooth range |

Nodes find each other over **mDNS** — no addresses are configured anywhere —
and a node's view of the others degrades to whatever transport is left. With
the internet removed entirely, discovery and delivery keep working on the LAN.

## Identity: the node first, the person optionally

A browser opened on a node is already somebody: that machine. The server
issues a **node session** to callers on the local network, so the messenger
works with nobody logged in — which is the state it will be in during the
event it exists for. Nobody should be typing a PIN into a wall display to
report that a stairwell is blocked.

Logging in as a person is a feature on top, not a gate. It records *who* is
writing, which the words can also say; the node identity records *which
machine is still talking*, which the words cannot. Every message carries the
node it was written on.

## Replication

Every node keeps the whole log and asks its peers for what it lacks. Routing a
message toward "the node its recipient lives on" would need a directory of who
is where, kept correct while machines come and go. Replication needs none of
that, and it is idempotent — running a sync twice changes nothing the second
time.

A message's identity is a hash of its own content, so two nodes that
independently hold the same message agree on that with no coordinator and no
id allocator. Sync is **pull-only**: a node decides what enters its own store,
and nothing a peer says can make it delete or overwrite anything.

Measured, between two laptops with the internet and the tailnet both cut:
**6 seconds** over the LAN path, and **32 milliseconds** over the mesh daemon,
which pushes over a held link instead of polling.

## API

| endpoint | |
|---|---|
| `POST /api/register`, `POST /api/login` | username + 4-digit PIN |
| `POST /api/node-login` | a session as this machine; local network only |
| `GET /api/history` | recent messages |
| `POST /api/send` | send without holding a websocket open |
| `GET /api/sync/export` | the local store, for a peer to pull; fleet-token gated |
| `WS /ws/{username}` | live delivery |

`POST /api/send` exists because the websocket registry holds exactly one
socket per user: a second connection as the same person replaces the first,
and closing it drops the original client's delivery. Anything that wants to
send one line can now do so without evicting the chat app someone is using.

## What is in this repository

| path | what it is |
|---|---|
| `mesh/` | **elemesh**, the per-machine mesh daemon (Rust): Bluetooth and Wi-Fi, no infrastructure. Guide: `mesh/README.md` |
| `client/` | **ele**, the desktop client (Rust, egui) for Linux, macOS and Windows. Uses the local elemesh, or runs its own node |
| `server.py`, `index.html` | the **server**: accounts, direct messages, groups, images, and the web client it serves |
| `flutter/` | the Flutter client for the server |
| `pkg/`, `debian/`, `pkg-client/`, `debian-client/` | packaging |
| `docs/` | design notes (`docs/MESH.md`) and hand-offs |

`mesh/` and `client/` are one Cargo workspace: `cargo build --release` at the
top builds both into `target/release/`.

## Install

Packages are published for Debian/Ubuntu and Arch (AUR). The server is Python
(FastAPI + uvicorn) over SQLite; its client is a web app, with a Flutter
client in `flutter/`.

## Status, honestly

- **Text across nodes with no internet: working**, over mDNS discovery and
  LAN replication.
- **Voice and video: not yet.** Both need a secure context for microphone
  access, so they need a certificate valid on the local network — which the
  text path does not.
- **Mesh over Bluetooth and Wi-Fi: working** between Linux and macOS, with no
  router, server or internet: links, offline catch-up and relaying verified
  on hardware. A Mac can find others but cannot yet be found, so two Macs
  alone need Wi-Fi or a Linux machine between them. See `mesh/README.md`.
- Sessions are held in memory, so restarting the server signs everyone out.
- Encrypted ("secret") messages exist, and have not had an external review.
  Do not rely on them for anything that matters.
