# Testing

How Carrick is tested, and what has been tried beyond the automated tests.

## Automated tests

```
cargo test
```

runs everything that needs neither the Internet nor a graphics adapter, on
Windows and Linux. The CI runs it on Ubuntu on every push, and on Windows by
hand.

- **The core** (`crates/carrick-core/tests/transfer.rs`): real transfers
  between endpoints in one process, with no relay and no Internet. A folder
  arrives identical; a stopped transfer leaves its finished files in place and continues
  from where it stopped; files that already exist are asked about, and are
  skipped or replaced as answered.
- **The window** (`crates/carrick-gui/tests/window.rs`): the window is driven
  from code through its accessibility tree, with [egui_kittest], without being
  shown. Two windows in one process send a folder to each other through their
  buttons, and it arrives identical, with an empty file, an empty folder and
  a non-ASCII name: drop a folder, ask for a ticket, *Start sending*, *Copy*; paste,
  choose a folder, *Start receiving*. Then *Stop*, *Continue* and *Stop* again
  from Transfers. Other tests cover a long name that must not push the buttons
  out, *Forget*, the question about existing files, the light and dark modes,
  and the links in About.

Two tests are ignored by default:

```
cargo test -p carrick-gui --test window -- --ignored
```

sends with a real four-word code over the Internet, through the public lookup
service.

```
CARRICK_SCREENS=<folder> cargo test -p carrick-gui --test screens -- --ignored
```

draws every screen to a PNG from real transfers between windows, in light and
dark. The screenshots in this repository come from it. It needs a graphics
adapter (wgpu takes a software one when there is one).

## Beyond the automated tests

- **Size and interruptions.** 100 GB between two processes on one Windows
  machine, with both sides killed and frozen half way: the files arrived
  identical. A release build moved 4 GiB at about 250 MiB/s, held to 8
  threads.
- **Windows and Linux together.** Transfers between Windows and Linux (Ubuntu
  26.04), in both directions.
- **A simulated network.** Two hosts, each behind its own NAT router, and a
  relay of our own, in Linux network namespaces. Carrick connects directly
  through ordinary routers, survives a 45-second network cut, keeps going when
  the receiver moves to another network, and falls back to the relay when both
  routers remap ports per destination. With those strict routers, one port
  opened on the sender's router (`--port`, `--public-addr`) made the transfer
  go directly, also with no relay at all and only a ticket copied across.
- **The relay container** (`relay/`) was built and run with Docker Compose,
  with its own certificate authority, and a transfer forced through it
  arrived identical.
- **The window on Linux** (X11, on a virtual display): two real windows sent
  a folder to each other, with files dropped from a GTK drag source, the
  ticket copied and pasted through the clipboard, and the destination chosen
  in the system dialog through the XDG portal.
- **The window's ways of drawing.** On Windows it opened on the graphics card,
  with OpenGL and with the software renderer; on Linux, with its graphics
  drivers taken away on purpose, it fell back from wgpu to OpenGL, and with
  none left it showed its message.
- **The installers.** On Windows the setup installs, works from a new terminal
  and from the Start menu, updates an older version in place, and uninstalls
  leaving the user's PATH as it was. `install.ps1`, run as `… | iex` in
  Windows PowerShell 5.1 and in PowerShell 7, downloads the release's
  installer, checks its SHA-256 and installs it; without access to the
  release it stops with a clear message and installs nothing. On Ubuntu the `.deb` installs with apt,
  its window opens, and it is removed cleanly.
- **Real use.** 191 GB in 208 files between two office computers, directly.

## Not tested yet

- A real mobile carrier or corporate network, and UPnP, NAT-PMP or PCP
  opening a port on a real router.
- Continuing after a real reboot, with `carrick autostart on`.
- The window under Wayland, and macOS.
- The relay container's Let's Encrypt mode.

[egui_kittest]: https://github.com/emilk/egui/tree/main/crates/egui_kittest
