# Carrick

Send files and folders from one computer to another, wherever they are.

Carrick goes directly from computer to computer when the network allows it,
encrypts everything end to end, checks every piece as it arrives, and **carries
on by itself after a dropped connection, a closed program or a reboot**. That
last part is why it exists: a 100 GB transfer should not need anyone watching
it.

```
$ carrick send ./photos
Ready to send photos — 1203 file(s), 98.4 GiB

On the other computer run:

    carrick receive dumpster-wikipedia-powwow-spectator
```

```
$ carrick receive dumpster-wikipedia-powwow-spectator
Connected directly, computer to computer
Receiving photos — 1203 file(s), 98.4 GiB
Receiving    [==========>              ] 41.2 GiB/98.4 GiB  112 MiB/s  eta 8m
```

## What it does

- **Follows through.** Both sides keep their state on disk. The receiver
  reconnects without limit, notices a stalled connection, and asks only for
  what is missing. Kill either side half way and start it again: it continues
  from the last verified piece.
- **Direct when possible.** Connections are peer to peer with NAT traversal
  ([iroh]), and fall back to a relay when no direct path exists.
- **Encrypted end to end.** QUIC with TLS 1.3 between the two computers. A
  relay only forwards packets it cannot read.
- **Verified piece by piece.** Data travels as BLAKE3 verified streams: each
  16 KiB group is checked against the transfer's root hash before it is
  written. A damaged piece is never saved.
- **Does not repeat itself.** Files the destination already holds, identical,
  do not travel. Send a folder again after changing three files and three
  files move.
- **Several receivers at once**, with no upload limit on their number. They
  also pass finished files to one another, so the sender does not have to
  upload everything to everyone.
- **No size, count or time limits.** Files are fingerprinted where they are,
  not copied; the computer is kept awake while a transfer runs; a system
  notification tells you when it ends.

## Use

There is a window, `carrick-gui`, and a terminal command, `carrick`. Both
run the same core and do the same transfers.

The window draws itself on the graphics card; if that fails it starts again
with OpenGL, then with the software renderer every Windows has (WARP), and if
nothing works it says so in a message instead of closing without a word. It
notes how it draws, and why it could not, in `carrick-gui.log` in its data
folder. `CARRICK_RENDERER=wgpu|glow|software` picks one by hand.

The window has three screens: **Send** (drop or choose files or a folder;
it shows the code in large type, with a button to copy it), **Receive**
(enter the code, choose a folder and what to do with files that already
exist, which by default it asks; follow the progress: the speed now and on
average, the time left and the time taken) and **Transfers** (every transfer
with its bar, whether it goes directly or through a relay, who connected, and
what was left unfinished, to continue or forget it), plus **About** (version,
licence and the licences of the libraries inside). It is light, dark, or as
the system is, from the buttons at the top right, and remembers the choice.
Each file appears in the
destination as soon as it is complete, so a stopped transfer keeps what had
arrived.

```
carrick send <PATH>...           offer files or folders, print the code
carrick receive <CODE> [-o DIR]  receive into DIR (default: current folder)
carrick resume                   continue everything unfinished on this computer
carrick list                     show known transfers
carrick cancel <ID>              forget a transfer
carrick autostart on|off|status  run `resume` at sign-in, to survive a reboot
```

Useful options:

| Option | What it does |
|---|---|
| `send --ticket` | Give a long ticket instead of a code. A ticket needs no rendezvous server, so it also works on a network without Internet. |
| `send --receivers N` | Stop once N receivers have everything. By default the transfer stays on offer until you stop it. |
| `receive --overwrite`, `--skip-existing`, `--keep-both` | What to do with destination files that differ from the incoming ones: replace them, leave them (and not fetch those), or save the incoming one beside as `name (2).ext`. Without any, Carrick asks when it finds them, or keeps both if nobody is at the terminal. Identical files never travel. |
| `receive --seed` | After finishing, keep passing files to other receivers. |
| `--relay URL` | Use your own relay (`CARRICK_RELAY`). `disabled` turns relays off. |
| `--relay-ca PEM` | Also trust the certificate authorities in this file (`CARRICK_RELAY_CA`), for a relay whose certificate comes from a private authority. |
| `--port PORT` | Listen on a fixed UDP port instead of a random one (`CARRICK_PORT`). Open it on your router once, and the other side connects directly even from a strict network. Pick a port no other program here uses (41641, for example, is Tailscale's); if it is taken, Carrick says so and uses another. iroh also asks the router to open it by itself (UPnP, NAT-PMP, PCP) where the router allows it; that part is not tested yet. |
| `--public-addr IP[:PORT]` | The address this computer is reached at from outside, usually the router's public IP (`CARRICK_PUBLIC_ADDR`). It goes into the ticket and is told to the other side. |
| `--threads N` | Worker threads. Low by default, so a long transfer does not take over the computer. |

## How it works

A transfer is an [iroh-blobs] hash sequence. Its first entry is a manifest
(names, sizes, modification times, empty folders) and the rest are the files,
each identified by its BLAKE3 hash. The root hash therefore pins every name and
every byte.

The **code** is four words from the EFF large word list, about 52 bits. Run
through Argon2id (64 MiB) it gives a seed, and from the seed come the sender's
network identity for this transfer and the key receivers must prove they hold.
The receiver derives the same values, finds the sender by that identity through
iroh's DNS lookup, and presents a keyed BLAKE3 token bound to both endpoints.
Only then does the sender answer requests for data.

A **ticket** carries the sender's identity, a random 256-bit key, the root hash
and the sender's addresses. It is longer, but it needs no lookup service and it
pins the content.

Receivers run the same gate over their own store, which is how they serve one
another. Whatever the source, every piece is verified against the same root.

## What it protects, and what it does not

- Nobody without the code or ticket can find the transfer or ask for data.
- Relay and lookup servers never see content. They do see metadata: IP
  addresses, times and how much is transferred.
- Anyone who has the **code** can receive the files, and could also pose as the
  sender to another receiver. Give a code only to people you would give the
  files to. A **ticket** does not allow posing as the sender.
- Guessing a code blind takes on the order of 2^52 Argon2id computations.
- Carrick does not hide who talks to whom, and does not protect files at rest.

## Limits today

- **Relays.** By default Carrick uses the free public iroh relays, which are
  rate-limited on purpose. When no direct path exists, a transfer through them
  crawls (tens of KiB/s in our measurements). Two free ways around it: open
  one port on the router of either side (`--port`, `--public-addr`), so the
  other side can connect directly; or run a relay of your own on a machine you
  already have, in a container ([relay/README.md](relay/README.md)), and pass
  `--relay`. Neither helps when both sides are behind mobile carriers that
  share one public address among many customers, except your own relay.
- A file that changed travels whole, even if only part of it changed.
- Receivers exchange whole files, and the part of a file another receiver
  already holds; pieces of one large file are not yet spread across receivers.
- The sender has to stay online, or be resumed, until receivers finish.
- Symbolic links are not followed and not sent.
- Windows and Linux. macOS should build but is untested; no phone or web
  client yet.

## Install

There are no published builds yet. To make the installers from the source:

- **Windows**: `pwsh -File packaging\build-windows.ps1` builds
  `carrick-<version>-setup.exe` (with [Inno Setup] 6). It installs for the
  current user without administrator rights, puts `carrick` on that user's
  PATH, adds Carrick to the Start menu, and uninstalls cleanly. It is not
  signed, so Windows warns before running it the first time.
- **Linux**: `sh packaging/build-deb.sh` builds a `.deb` (with [cargo-deb])
  holding `carrick`, `carrick-gui`, a menu entry and the icon.

Both write the licences of every crate Carrick includes
(`THIRD-PARTY-LICENSES.txt`) and leave their output in Cargo's target folder,
under `dist/` and `debian/`.

[Inno Setup]: https://jrsoftware.org/isinfo.php
[cargo-deb]: https://github.com/kornelski/cargo-deb

## Build

```
cargo build --release
cargo test
```

The window's tests drive it from code, without showing it: two windows in
one process send a folder to each other through their buttons. Two more are
ignored by default: one uses a real four-word code over the Internet, and
one draws every screen to a PNG:

```
cargo test -p carrick-gui --test window -- --ignored
CARRICK_SCREENS=<folder> cargo test -p carrick-gui --test screens -- --ignored
```

Requires Rust 1.91 or newer.

## Status

Version 0.1, not yet published.

Tested on Windows with automated tests, with transfers up to 100 GB between
two processes on one machine, and with both sides killed and frozen half way.
The window is tested on Windows by driving it from code, including a transfer
with a real four-word code over the Internet.

Tested on Linux (Ubuntu 26.04) with the automated tests, with transfers
between Windows and Linux in both directions, and in a simulated network: two
hosts, each behind its own NAT router, and a relay of our own. There it
connects directly through ordinary routers, survives a 45-second network cut,
keeps going when the receiver moves to another network, and falls back to the
relay when both routers remap ports per destination. With those same strict
routers, opening one port on the sender's router made the transfer go
directly, also with no relay at all and only the ticket copied across.

The relay container (`relay/`) was built and run with Docker Compose on Linux,
with its own certificate authority, and a transfer forced through it arrived
identical. Its Let's Encrypt mode is not tested.

The window is tested on Linux (X11, on a virtual display): two real
`carrick-gui` windows sent a folder to each other, with files dropped from a
GTK drag source, the ticket copied and pasted through the clipboard, and the
destination chosen in the system dialog through the XDG portal.

The installers: on Windows the setup was installed, used from a new terminal
and from the Start menu (the window opens), and uninstalled leaving the
user's PATH exactly as before; on Ubuntu the `.deb` was installed with apt,
its window opened, and it was removed. A release build moved 4 GiB between
two processes on one Windows machine at about 250 MiB/s, held to 8 threads.
The window opened on Windows on the graphics card (Vulkan), with OpenGL and
with the software renderer; on Linux, with its graphics drivers taken away on
purpose, it fell back from wgpu to OpenGL, and with none left it showed its
message.

Not yet tested through a real mobile carrier or corporate network, nor the
window under Wayland.

## Credits

Built on [iroh] and [iroh-blobs] by n0. The word list is the
[EFF large word list] (CC BY 3.0 US), minus its four hyphenated words.

[iroh]: https://github.com/n0-computer/iroh
[iroh-blobs]: https://github.com/n0-computer/iroh-blobs
[EFF large word list]: https://www.eff.org/dice

## Licence

Carrick is licensed under either of the [MIT licence](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.
