# elemesh — running the mesh daemon

elemesh is the part of ELE Messenger that works with no internet, no router
and no server. Every laptop runs one copy. Copies that share the same mesh
passphrase find each other over Bluetooth and over any shared Wi-Fi/LAN,
link up by themselves, and keep each other's messages in sync. Messages
written while two machines were apart are exchanged when they meet again,
and a machine in the middle relays for machines that can't reach each other.

You chat in a browser on your own machine at **http://127.0.0.1:8740/**.
That page only talks to the local daemon; nothing leaves the machine
except through the encrypted mesh links.

| | Linux | macOS |
|---|---|---|
| Program | `~/.local/bin/elemesh` | `~/Applications/ELE Mesh.app` (CLI copy in `~/.local/bin/elemesh`) |
| Data (identity, passphrase, messages) | `~/.local/state/elemesh/` | `~/Library/Application Support/elemesh/` |
| Log | `journalctl --user -u elemesh` | `~/Library/Logs/elemesh.log` |
| Starts | at login (systemd user service) | at login (LaunchAgent `org.ele-messenger.elemesh`) |
| Bluetooth | finds others and can be found | finds others (cannot yet be found) |

At least one machine in each Bluetooth pair must be able to be found, so
two Macs need a Linux machine (or Wi-Fi) between them for now.

## 1. Build

Needs Rust (`rustup`); on macOS the Command Line Tools are enough, no Xcode.
On Linux, BlueZ (`bluetoothd`) must be running.

From the top of the repository (mesh and the desktop client share one Cargo
workspace and one `target/` directory):

```sh
cargo build --release -p elemesh   # binary: target/release/elemesh
cargo test --release -p elemesh    # optional
```

The commands below run from the same place.

## 2. Install

**Linux**

```sh
install -Dm755 target/release/elemesh ~/.local/bin/elemesh
install -Dm644 mesh/dist/linux/elemesh.service ~/.config/systemd/user/elemesh.service
systemctl --user daemon-reload
```

**macOS**

```sh
mkdir -p ~/.local/bin && cp target/release/elemesh ~/.local/bin/   # command-line copy
```

Add `~/.local/bin` to your `PATH` if it isn't already (both systems), or
type the full path `~/.local/bin/elemesh` in the commands below.

The app itself is installed in step 4.

## 3. Choose the mesh passphrase (once per machine)

Every machine that should talk to yours needs the **same** passphrase.
Pick something long; anyone who has it can join and read the mesh.

```sh
elemesh init --passphrase 'the same long phrase on every machine'
```

This prints the machine's node id (16 hex characters). Running `init`
again changes the passphrase and keeps the node id and the messages.

## 4. Start it

**Linux**

```sh
systemctl --user enable --now elemesh
```

To have it run even before anyone logs in (for example a machine that
boots after a power cut and nobody touches it):

```sh
sudo loginctl enable-linger $USER
```

**macOS**

```sh
sh mesh/dist/macos/install.sh target/release/elemesh
```

The first time, macOS asks whether **ELE Mesh** may use Bluetooth: click
**Allow**. The app is signed so that updating it later does not ask again.
If you missed the prompt: System Settings → Privacy & Security →
Bluetooth → turn on ELE Mesh, then restart it (section 5).

## 5. Everyday control

| What | Linux | macOS |
|---|---|---|
| Is it running? | `systemctl --user status elemesh` | `launchctl print gui/$(id -u)/org.ele-messenger.elemesh \| grep state` |
| Stop | `systemctl --user stop elemesh` | `launchctl bootout gui/$(id -u)/org.ele-messenger.elemesh` |
| Start | `systemctl --user start elemesh` | `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/org.ele-messenger.elemesh.plist` |
| Restart | `systemctl --user restart elemesh` | `launchctl kickstart -k gui/$(id -u)/org.ele-messenger.elemesh` |
| Don't start at login | `systemctl --user disable elemesh` | stop it, then delete the plist in `~/Library/LaunchAgents/` |
| Watch the log | `journalctl --user -u elemesh -f` | `tail -f ~/Library/Logs/elemesh.log` |

Stopping loses nothing: messages are on disk and the others catch you up
when you start again.

## 6. Chat

Open the **ELE Messenger** app, or **http://127.0.0.1:8740/** in any browser
on the same machine.

- **Room**: everyone who types the same room name is in the same chat
  (default `home`).
- **Name**: what others see next to your messages. Left empty, messages carry
  this machine's name.
- **Around you** lists the ELE machines heard nearby; a green dot means
  linked right now. With none in range, messages are kept and go out when
  someone appears.
- The **☰** menu: rename this machine, join or change the mesh passphrase
  (the service restarts itself with it), clear this room for everyone or just
  here, light or dark, copy an invite link (section 6a). Delete one of your
  own messages by right-clicking it in the app, or hovering over it on the
  page.

From a terminal (handy over SSH):

```sh
elemesh send home "on the second floor, all fine" --nick alex
elemesh tail home          # print the room and follow new messages; Ctrl-C to quit
elemesh status             # this node, its links and who it hears
elemesh name "Kitchen laptop"   # rename this machine ("" = its host name)
```

## 6a. Share with phones and tablets

A phone can't run the mesh, but it can use a computer's: the computer shares
its mesh page as **/elem/** through its web server, behind the passphrase.

- **Linux with nginx:** the package installs
  `/etc/nginx/snippets/ele-messenger-mesh.conf`; include it in the server
  block that serves this machine's pages (the camera suite already does).
- **macOS, or any machine on the tailnet:**
  `tailscale serve --bg --set-path /elem http://127.0.0.1:8740`.
- On the phone, open `https://<machine>.<tailnet>.ts.net/elem/` (or the
  machine's LAN address with `/elem/`) and enter the mesh passphrase once.
- Or send an **invite link** instead (☰ → Copy invite link): it lets one
  device in, once, within a day, without giving out the passphrase.

A visitor reads, posts, deletes and clears for everyone, but can't rename the
machine, change its passphrase or clear just its view. elemesh itself only
ever listens on 127.0.0.1; the web server is the one way in.

## 7. Updating

Rebuild, then:

- **Linux**: `install -Dm755 target/release/elemesh ~/.local/bin/elemesh && systemctl --user restart elemesh`
- **macOS**: `sh mesh/dist/macos/install.sh target/release/elemesh && cp target/release/elemesh ~/.local/bin/`
  (no new Bluetooth prompt)

## 8. Troubleshooting

Start with `elemesh status`. Each link shows:

- `transport`: `ble` (Bluetooth) or `lan` (Wi-Fi/Ethernet). The same peer can
  show up once per transport; that's normal and gives a spare path.
- `up_s`: seconds the link has been up.
- `heard_s`: seconds since that peer last spoke. Peers check in every 15 s;
  a link silent for 45 s is dropped and redialled.

**`daemon not running at 127.0.0.1:8740`** — it isn't running: start it
(section 5) and read the log for the reason.

**`no mesh passphrase`** in the log — run `elemesh init` (section 3), then
restart.

**Two machines never link**
1. Both running? `elemesh status` on each.
2. Same passphrase? A mismatch shows up in the log as *handshake with …
   failed*, about once a minute (one side adds *wrong mesh passphrase?*;
   the other usually just sees the connection close). Run `init` again on one of them with the
   right phrase and restart it.
3. Bluetooth on? Linux: `busctl get-property org.bluez /org/bluez/hci0 org.bluez.Adapter1 Powered`
   should say `b true`. macOS: Bluetooth on in Control Center, and ELE Mesh
   allowed in Privacy & Security → Bluetooth.
4. Two Macs alone can't link over Bluetooth yet (neither can be found);
   put them on the same Wi-Fi, or have a Linux machine nearby.
5. Range: Bluetooth through floors is short; move closer while testing.
   Allow up to two minutes after a restart for Bluetooth to reconnect.

**Linux log says *ble transport down … no Bluetooth adapter*** — the
machine has no Bluetooth, or it is off. Wi-Fi/LAN still works. The warning
appears once; the daemon checks again every minute.

**Bluetooth keeps dropping on a Linux machine** — check the kernel log:
`journalctl -k | grep -i bluetooth`. *"command 0x200c tx timeout"* followed by
*"Resetting usb device"* is a Realtek RTL8821C firmware hang. elemesh avoids
the commands that trigger it and recovers by itself when it happens anyway.

**Wi-Fi/LAN links don't form** — both machines must be on the same network
segment, and UDP and TCP port **47474** must be open (beacons are UDP
broadcast). Check a firewall: `sudo ufw status`, `sudo nft list ruleset`, or
macOS System Settings → Network → Firewall.

**macOS: no `ble` lines in the log at all** — Bluetooth permission is
missing or still waiting on a prompt nobody saw (the screen may be
asleep). Wake the screen, look for the prompt, or grant it in Privacy &
Security → Bluetooth.

**More detail in the log**
- Linux: `systemctl --user edit elemesh`, add `[Service]` /
  `Environment=RUST_LOG=elemesh=debug`, then restart.
- macOS: `/usr/libexec/PlistBuddy -c "Add :EnvironmentVariables dict" -c "Add :EnvironmentVariables:RUST_LOG string elemesh=debug" ~/Library/LaunchAgents/org.ele-messenger.elemesh.plist`,
  then stop and start (a plain restart doesn't reread the plist). Remove it
  afterwards with `-c "Delete :EnvironmentVariables"`.

**Start over on one machine** — stop it and delete its data folder (see
the table at the top). This erases its identity and the messages stored
there, but the others still hold everything they synced and will refill
it after you `init` again.

## Ports and privacy

- `127.0.0.1:8740`: the chat page and local API. elemesh only listens here;
  other devices reach it through this machine's web server at `/elem/`
  (section 6a), behind the passphrase or an invite.
- `47474` TCP and UDP: mesh links and discovery beacons on the local network.
- Bluetooth service `e1e5a3c0-7d1b-4f6a-9c1e-6d2b8a4f0e11`, advertised as
  `ele-<node id>`.

Everything between machines is encrypted (Noise XXpsk0). Without the
passphrase a device can't link, read messages, or learn who you are. Design
details are in [docs/MESH.md](../docs/MESH.md).
