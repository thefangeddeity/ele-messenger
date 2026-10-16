# Handoff to the ELEM implementer — 2026-10-16

From the macOS lead. Everything below is committed. **Nothing is pushed.**

**Your instruction: park research.** No new investigation, no Windows port, no
new transports, no refactors. The next session is for testing, and there is
one measurement that matters. It is at the bottom.

---

## What changed in this tree

`52dc249 A message keeps the time it was said`

`POST /v1/rooms/{room}/messages` takes an optional `ts_ms`. Omitted, nothing
changes. Supplied, it is honoured — except a stamp in the future, which is
clamped to now, because `ts_ms` drives both ordering and the retention cutoff
and one far enough ahead would sort above every real message and never be
pruned. A skewed clock should still be heard; it should not pin itself to the
top.

`node.post()` gained a fourth argument, `said_at: Option<u64>`. The store's
own arrival time stays `now_ms()` regardless — that drives the replication
cursor and was never the message's timestamp.

New test: `link::tests::a_supplied_timestamp_is_kept_and_a_future_one_is_clamped`.
13/13 pass on Arch and on macOS. Darwin build swapped into ariana's app
bundle; `~/.local/bin/elemesh.prev` on both Linux nodes is the previous build.

**Why:** a message is addressed by the hash of its content *and its
timestamp*. The mesh re-stamping meant the same message arriving by LAN and
by mesh had two hashes and became two messages. Measured: 3 sent, 8 rows
stored, on every node.

`a06c304` canonicalises the timestamp in `message_id` for the same reason.

## The real bug was never yours

`message-buffer`, the bridge between the messenger's SQLite and the mesh, was
installed on all three nodes and had **never run on any of them** — provable
by the absence of the delivery ledger it writes on every single run. BLE was
delivering the whole time into `messages.db`, and every UI in the suite reads
the messenger's SQLite. The mesh worked, the viewer worked, and nothing
joined them.

Now scheduled: `livecam-buffer.timer` on the Linux nodes, `com.livecam.buffer`
on ariana, every 5s. Steady state verified — 6 rows in every store, 6 in
every mesh, no duplicates, held across twelve ticks.

## Two constraints this puts on you

- **The buffer never publishes a message it did not author.** Each message
  reaches the mesh exactly once, from the node that said it. Do not add
  relay-on-behalf-of behaviour without talking to me — elemesh already
  relays between nodes itself, and a second relayer is what produced the
  duplicates.
- **`canonical_ms` now exists in three places** — `server.py`,
  `tools/sync-peers`, `tools/message-buffer`. They compute the id of the same
  message and **must stay identical**. That is a real seam and it is written
  down in all three.

## Measured facts that affect your design

- **tina has no Bluetooth.** Hardware absent — no bluez, no
  `/sys/class/bluetooth`, no device on any bus. LAN-only barring a dongle,
  and it is the only node serving TLS. Plan off-LAN work around two nodes.
- **The two radios are independent, measured.** During an AP drill tanzania's
  wifi left the network entirely; the ariana↔tanzania **BLE link never
  dropped** — up 38,840s unbroken while the LAN link re-formed at 124s.
- **tanzania can host a wifi AP.** Up in 2s on 2.4 GHz ch1, dnsmasq serving
  `10.42.0.10–254`, 7 minutes stable, clean revert. It cannot be an AP and a
  client at once, but in a blackout there is no uplink to keep.
- **ariana can do IBSS.** `startIBSSModeWithSSID` is deprecated since 10.10
  and still present in the runtime. IBSS is the one mode all three nodes
  have, and unlike BLE it carries IP. `tools/mac-ibss` reports; `--start`
  raises it.
- **Windows stays "Not yet", correctly.** `bluer` is BlueZ-only, so a port
  needs WinRT Bluetooth via `windows-rs`. Not a recompile. Do not start it.

## Debt — real, but not yours to fix now

- `/usr/lib/ele-messenger/server.py` is package-owned (`ele-messenger
  1.3.6-1`). The live copy carries the identity fix; the next upgrade reverts
  it. Needs a version bump and a rebuild.
- This project exists as **three divergent checkouts**: this one has the
  remote and the `mesh/` tree, tina is behind, ariana has no remote at all
  and tracks `ele.db`. Same commit message, three different SHAs.

---

## The one measurement for next session

`link_hosting_proven` is **unknown** on every node. The AP broadcast for a
full 7 minutes and **nothing ever associated** — no DHCP request, no station,
silence until the revert. Nobody had tried to join.

```
sudo systemd-run --unit=hotspot-drill --collect \
  /usr/local/lib/livecam-tools/wifi-hotspot 900 elemesh <8+ chars>
```

Then: join `elemesh` from a phone, load `http://10.42.0.1:8080/llc`, and send
a message from it. That proves or disproves the entire phone story.

The script self-reverts on exit, crash or kill, captures the network to return
to *before* touching anything, and refuses before touching the radio if
dnsmasq is missing. It is safe to run without physical access. Log:
`/var/log/wifi-hotspot-drill.log`.

On the phone, turn off auto-switch to mobile data first — there is no internet
behind the hotspot, and both iOS and Android will quietly prefer cellular.
The viewer will look broken when it is the phone that walked away.

**Second, only if there is time:** messages with tanzania's wifi off. Every
link in that chain is verified except that hop.

Do not treat this document as authority to change anything outside this tree,
and do not push.
