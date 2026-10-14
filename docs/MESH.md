# elemesh — the independent layer

Status: Accepted (2026-10-13), in part verified on hardware:
- LAN (Linux ↔ Linux, through a home router): link, both directions,
  offline catch-up.
- BLE (Linux peripheral ↔ macOS central): link, both directions, offline
  catch-up, and relay onward over LAN to a third node.
- Not yet: router-less ad-hoc Wi-Fi, macOS peripheral, Windows, two
  Linux nodes over BLE.

## Goal

Two laptops on different floors, mains power gone, no router, no internet,
within Bluetooth range: people can chat. Every other transport (Wi-Fi, the
central server, Tailscale) is an optional layer above this one and must
never be required by it.

## Boundary

`elemesh` is a daemon that runs on every node. It owns:

- the node's identity and the mesh credential,
- a local message store,
- links to nearby nodes over any transport it can open,
- relaying and history sync between those links.

It does **not** own UI, accounts, or the central server. UIs (the web page,
the Flutter app, a CLI) talk to the daemon on the same machine over
`127.0.0.1`. The browser never touches Bluetooth, so browser Bluetooth
limits are irrelevant.

## Interface

Local API, bound to `127.0.0.1` only:

| Call | Meaning |
|------|---------|
| `POST /v1/rooms/{room}/messages` `{"nick","text"}` | append a message authored by this node |
| `GET /v1/rooms/{room}/messages?after={cursor}` | messages in display order after a cursor |
| `GET /v1/rooms` | rooms seen on the mesh |
| `GET /v1/status` | node id, links (transport, peer, age), store size |

The CLI (`elemesh send`, `elemesh tail`, `elemesh status`) wraps the same
calls.

## Data model

Every node is the sole author of an append-only log.

```
Entry { author: NodeId(8 bytes), seq: u64 (1,2,3… per author),
        ts_ms: u64 (author's clock), room: String, nick: String, text: String }
```

`(author, seq)` is the identity of an entry. Entries are immutable, so there
is nothing to merge: a node either has an entry or it doesn't. Display order
is `(ts_ms, author, seq)`; clocks may be wrong, and order is cosmetic, never
used for sync.

Node id = first 8 bytes of SHA-256 of the node's static Noise public key
(same construction as BitChat), generated on first run and kept in the state
directory.

## Sync (anti-entropy, on every new link and every 15 s)

Each side sends a summary: for every author it knows,
`(author, floor, high)`, where `high` is the highest seq it holds with no
gaps below it (down to `floor`), and `floor` is the oldest seq it still
keeps. The receiver answers with every entry it holds that the sender lacks:
`seq > their high`, or authors they don't list. If the receiver's own floor
for an author is above `their high + 1`, the missing range is gone mesh-wide
and the sender jumps its `high` forward instead of asking forever.

This replaces the earlier per-conversation counter, which lost messages
whenever two devices' counters disagreed.

## Live delivery (controlled flood, borrowed from BitChat §4.2)

A new entry, local or received, is pushed to every link except the one it
arrived on. Each carries a TTL starting at 7, decremented per hop; the store's
`(author, seq)` primary key drops duplicates, so only new entries relay. Relays wait a random 10–220 ms
before forwarding so duplicate suppression usually wins. History sync
covers anything the flood missed.

## Credentials: who may link

A mesh has a passphrase. Every link runs a Noise `XXpsk0` handshake
(Curve25519, ChaCha20-Poly1305, SHA-256) with the PSK derived from the
passphrase. The passphrase is mixed into the first handshake message, so a device without it cannot complete a handshake, learn our static key,
read traffic, or inject entries. Two households with different passphrases
share the air but not the mesh. A neighbour joins by being given the
passphrase: no server, no enrolment.

## Transports

All transports carry the same encrypted byte stream; only the link layer
differs. Frames are `u16 length ‖ ciphertext` (Noise caps a message at
65 535 bytes) and are split into chunks no
larger than the link allows (BLE: MTU − 3 bytes, default 182).

1. **BLE**: each node is GATT peripheral and central at once. One service
   with one stream characteristic: the central writes to it, the peripheral
   notifies on it. Advertisement carries the service
   UUID and `ele-<node id hex>` as the local name. If two nodes connect to
   each other twice, the link where the lower node id is central is kept.
   Scanning runs in short windows and stops while any BLE link or dial is
   up: Realtek RTL8821C combo chips time out on "LE set scan enable" while
   connected and reset over USB, dropping every link.
   - Linux: `bluer` (BlueZ) for both roles.
   - macOS: `btleplug` central first; peripheral via CoreBluetooth
     (`objc2-core-bluetooth`) second. Command Line Tools are enough; no Xcode.
   - Windows: `btleplug` central.
2. **LAN** (the Wi-Fi fallback): TCP to peers found by UDP
   broadcast on the local subnet; works on a router-less ad-hoc network
   with link-local addresses.
3. **Server bridge** (later): `server.py` reads and writes rooms through the
   local API of the elemesh on the same host, so the existing web client
   sees mesh traffic when the server is up.

## Failure modes

| Situation | Behaviour |
|-----------|-----------|
| Bluetooth off or absent | daemon runs; local API works; status shows no BLE; LAN still tried |
| Peer vanishes without a disconnect | link dropped after 45 s of silence (peers sync every 15 s) |
| Link drops mid-sync | partial entries are kept (each is self-contained); next link resyncs |
| Wrong passphrase | handshake fails; peer is ignored for 60 s, then retried |
| Clock wrong on a node | display order off for that node's messages; sync unaffected |
| Store full | oldest entries per room are pruned; floors advance; peers stop asking |

## Constants (all assumptions, all configurable)

| Name | Value | Source |
|------|-------|--------|
| TTL | 7 | BitChat |
| relay jitter | 10–220 ms | BitChat |
| sync interval | 15 s | BitChat |
| retention | 7 days, 10 000 entries per room | server.py keeps 7 days |
| max text | 2 KiB | ours |
| BLE chunk | 180 bytes | under the 182-byte default MTU payload |
| BLE scan | 6 s window, 9 s idle, none while linked | ours |
| link idle timeout | 45 s | 3 × sync interval |
| failed-handshake backoff | 60 s | ours |

## Not in this step

Media, private 1:1 encryption beyond the mesh PSK, couriers (BitChat §6.2),
macOS/Windows peripheral role, iOS/Android.

## Licensing

GPL-3.0. Ideas follow BitChat's whitepaper (iOS Unlicense, Android
GPL-3.0); no BitChat code is copied in this step.
