# Using ELE

ELE lets the computers in your home send each other messages even when the
internet, the router or the power is out.

It is one group chat, the **ELE Mesh**, shared by every ELE computer within
reach, over Bluetooth and Wi-Fi, with no internet and no server. Everyone in
it can read every room.

## Getting started (once on each computer)

1. Install ELE Messenger. On Linux it is the `ele-messenger` package;
   Windows and Mac installers are coming, so for now ask whoever looks
   after your computers.
2. Open the **ELE Messenger** app.
3. Type the mesh passphrase (at least 8 characters; capitals count) and
   click **Join**. Every computer uses the same one; keep it private.

## Everyday use

- Open the ELE Messenger app, or **http://127.0.0.1:8740** in a browser on
  that computer.
- Pick a **Room**. Everyone in the same room sees the same messages; "home"
  is the usual one.
- Type and press Enter. Leave **Name** empty to sign with the computer's name.
- **Around you** groups nearby ELE computers by connection: full, partial,
  internet only, then heard but not linked (a different passphrase, or
  still connecting).
- The **☰** menu renames the computer, changes its passphrase (this
  computer only: do it on each one), clears a room and switches light or
  dark. To delete your own message, right-click it in the app, or hover over
  it on the page.

## On a phone or tablet

Open the computer's address with `/elem/` on the end, e.g.
`https://<computer>.<your tailnet>.ts.net/elem/`, and type the mesh passphrase.
Or, on the computer, use ☰ → **Copy invite link** and send that to the
phone: it works once, for a day, without giving out the passphrase.

## When something goes wrong

- **Power or internet out:** keep using the app. Computers on the same floor
  or one floor apart keep talking over Bluetooth. Messages written while
  apart arrive when they are back in range.
- **A computer doesn't appear:** check that Bluetooth is on, wait two
  minutes, and make sure both use the same passphrase.
- **Two Macs on their own** can't reach each other over Bluetooth yet. Keep a
  Linux computer nearby, or put them on the same Wi-Fi.
- **On Windows,** allow ELE through the firewall when asked.

## Good to know

- Messages are kept for 7 days.
- Messages travel directly between your computers, readable only with your
  passphrase.
