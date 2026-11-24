# Changelog

What changes in each version of Carrick. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the versions
follow [Semantic Versioning](https://semver.org/). This repository's history starts at
0.1.5; the versions before it are listed for what they changed.

## [0.1.8] - 2026-11-24

### Changed

- A new icon: two arrows, blue and white, tied in one knot on their way across,
  after the carrick bend the program is named after. The window, the Start
  menu, the installer and the Linux menu all use it.

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

[0.1.8]: https://github.com/johan-socarras/Carrick/releases/tag/v0.1.8
[0.1.7]: https://github.com/johan-socarras/Carrick/releases/tag/v0.1.7
