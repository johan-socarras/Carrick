# Changelog

What changes in each version of Carrick. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the versions
follow [Semantic Versioning](https://semver.org/). This repository's history starts at
0.1.5; the versions before it are listed for what they changed.

## [0.2.0] - 2026-11-25

### Added

- Sends wait in line. Starting a send while another is on offer puts the new
  one in line instead of sharing the connection with it; its four-word code
  works at once, and the other computer waits and starts by itself when its
  turn comes. **Send this first** lets one go ahead: the send in front pauses,
  its receivers wait, and it carries on by itself after.
- A history of the last three transfers that ended well on this computer,
  from the window or the terminal, under **Show history** in Transfers: what
  it was, how many files and how large, when, how long it took, the average
  speed, whether it went directly, and where it was saved.
- The window shows the transfers a terminal (or another window) is running on
  the same computer, with their progress, speed and route, instead of
  offering to continue them. A transfer can only run once on a computer:
  `carrick resume` leaves the running ones alone, `carrick cancel` refuses
  them, and `carrick list` marks them `running`.
- **Copy command** next to **Copy** puts the whole `carrick receive …`
  command on the clipboard, for a terminal on the other computer.
- **Save to** opens with the folder used last time, also after a restart, and
  its arrow offers the last five folders; the cross takes one off the list.

### Changed

- The window takes the colours of the icon: its blue-to-cyan on the main
  buttons, the progress bars and the selected tab, the icon itself where the
  name was, the code in a box of its own, and how data travels (directly or
  through a relay) as a coloured label.
- Under Transfers, only what is being copied now; what ended goes to the
  history.
- In the window, the ticket is now called a long code, and its option says
  what it is for: "No Internet? Use a long code instead of four words".

### Fixed

- A send that someone had received in full no longer stays under Unfinished
  once stopped or closed, to be offered again for nobody. A send offered
  until stopped now ends when it is stopped, if anyone has everything.
- The terminal's progress bar stays on one line when the window is resized.
  It used to fill the whole width, and a resize split it and left pieces of
  old lines behind; in the classic Windows console every update also went to
  a new line.
- A folder's hidden `desktop.ini` (and `Thumbs.db`) arrives hidden, instead of
  showing up as a stray file in the destination.
- `CARRICK_NO_NOTIFY=1` is accepted (so are `yes`, `on`, `0` and the rest);
  before, only `true` or `false` were, and the terminal refused to start.

## [0.1.8] - 2026-11-24

### Changed

- A new icon: two arrows, blue and white, tied in one knot on their way across,
  after the carrick bend the program is named after. The window, the Start
  menu, the installer and the Linux menu all use it.
- The `.deb` is built on Ubuntu 24.04, so it installs on 24.04 and newer,
  and it comes with the release.

## [0.1.7] - 2026-11-23

### Fixed

- The links in the window's About screen open in the system's browser. They
  were drawn and could be clicked, but opened nothing.

## 0.1.6 - 2026-11-23

### Added

- The credit for the EFF Large Wordlist for Passphrases (CC BY 3.0 US), the
  source of the words in the codes, in About and at the top of
  `THIRD-PARTY-LICENSES.txt`.

## 0.1.5 - 2026-11-23

### Added

- Light and dark modes, or as the system is, from the buttons at the top right
  of the window. The window remembers the choice.
- An About screen: version, copyright, the MIT and Apache 2.0 licence texts,
  the repository, and a button that opens the third-party licences.
- `carrick --version` gives the copyright, the licence and the repository.

## 0.1.4 - 2026-11-23

### Added

- What to do with files the destination already has, when they differ from
  the incoming ones: replace them, skip them (they are not downloaded either)
  or keep both, saving the incoming one as `name (2).ext`. By default Carrick
  asks, in the window and in the terminal; `--overwrite`, `--skip-existing`
  and `--keep-both` answer in advance. The answer is kept for the rest of the
  transfer, also after a restart.

## 0.1.3 - 2026-11-23

### Added

- The speed now and on average, the time left at the average speed, and the
  time taken, on both sides and for each receiver.
- Forget an unfinished transfer from the Transfers screen, after a
  confirmation; finished ones can be removed from the list.

### Changed

- Each file goes to its folder as soon as it is complete and verified,
  instead of all of them at the end. A stopped transfer keeps every file that
  had arrived whole, and continuing trusts them without reading them again.

### Fixed

- Long names no longer push the buttons of a transfer card out of the window.

## 0.1.2 - 2026-11-23

### Fixed

- The window survives a graphics driver that crashes while it starts: it
  tries each way of drawing in a child process and keeps the first that
  works.

## 0.1.1 - 2026-11-23

### Fixed

- The window opens on computers where the graphics card cannot be used. It
  falls back from the graphics card to OpenGL and then to the software
  renderer, and if nothing works it says so in a message instead of closing
  without a word. It notes how it draws in `carrick-gui.log`.

## 0.1.0 - 2026-11-23

The first version.

### Added

- Send files and folders with a four-word code, or with a ticket that needs no
  lookup service. Directly between the two computers when the networks allow
  it, through a relay when they do not; encrypted end to end and verified
  piece by piece against the transfer's root hash.
- Transfers carry on by themselves after a dropped connection, a closed
  program or a reboot, from the last verified piece; `carrick resume`, and
  `carrick autostart` to run it at sign-in.
- Files the destination already holds, identical, do not travel.
- Several receivers at once, which pass finished files to one another.
- The computer is kept awake during a transfer, and a system notification
  says when it ends.
- A fixed UDP port (`--port`) and a public address in the ticket
  (`--public-addr`), for a direct connection through a router with one port
  open; and a relay of your own in a container (`relay/`).
- A window with Send, Receive and Transfers screens, on the same core as the
  terminal command.
- Installers for Windows (per user, without administrator rights) and a
  `.deb` for Ubuntu and Debian, with the licences of every crate inside.

[0.2.0]: https://github.com/johan-socarras/Carrick/releases/tag/v0.2.0
[0.1.8]: https://github.com/johan-socarras/Carrick/releases/tag/v0.1.8
[0.1.7]: https://github.com/johan-socarras/Carrick/releases/tag/v0.1.7
