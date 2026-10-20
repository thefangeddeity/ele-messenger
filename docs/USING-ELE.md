# Using ELE

ELE lets the computers in your home send each other messages even when the
internet, the router or the power is out.

## The two parts

- **ELE Mesh**: one group chat shared by every ELE computer within reach. It
  works over Bluetooth and Wi-Fi, with no internet. Everyone in it can read
  every room.
- **ELE servers**: the fuller messenger, with accounts, private messages,
  groups and photos. It works only while the server computer and the home
  network are on.

## Getting started (once on each computer)

1. Install ELE Messenger. On Linux it is the `ele-messenger` package;
   Windows and Mac installers are coming, so for now ask whoever looks
   after your computers.
2. Open the **ELE Messenger** app.
3. It lists the ELE computers it can hear. Type the mesh passphrase and click
   **Join**. Every computer uses the same passphrase. Anyone who has it can
   read your messages, so keep it private.

## Everyday use

- Open the ELE Messenger app, or open **http://127.0.0.1:8740** in a web browser on
  that computer.
- Pick a **Room**. Everyone in the same room sees the same messages; "home"
  is the usual one.
- Type and press Enter. Leave **Name** empty to sign with the computer's name.
- **Around you** lists nearby ELE computers. A green dot means connected
  right now.
- **ELE servers** lists the servers on your network. Click **Open** to use the
  fuller messenger in your browser.

## When something goes wrong

- **Power or internet out:** keep using the app. Computers close together,
  on the same floor or one floor apart, keep talking over Bluetooth. Messages
  written while they were apart arrive when they are back in range.
- **A computer doesn't appear:** check that Bluetooth is on, wait two
  minutes, and make sure both computers use the same passphrase.
- **Two Macs on their own** can't reach each other over Bluetooth yet. Keep a
  Linux computer nearby, or put them on the same Wi-Fi.
- **On Windows,** click Allow when Windows asks about the firewall.
- **The server page asks for a name and PIN:** that server is having
  trouble. Restart that computer, or use the mesh meanwhile.

## Good to know

- Messages are kept for 7 days.
- Nothing goes through the internet. Messages travel directly between your
  computers, scrambled so that only computers with your passphrase can read
  them.
