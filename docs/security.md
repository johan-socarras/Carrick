# Security

What Carrick protects, how, and what it leaves to you.

## How a transfer is protected

- **Encrypted end to end.** Every connection is QUIC with TLS 1.3, between
  the two computers' own keys ([iroh]). A relay, when one is needed, forwards
  packets it cannot read.
- **Only someone with the code or ticket gets in.** The sender answers no
  request for data until the other side proves it holds the transfer's key,
  with a keyed BLAKE3 token bound to both endpoints. Without it, the sender
  cannot even be found.
- **Every piece is verified before it is written.** A transfer is an
  [iroh-blobs] hash sequence: a manifest (names, sizes, modification times,
  empty folders) and the files, each identified by its BLAKE3 hash. Data
  travels as verified streams, and each 16 KiB group is checked against the
  root hash on arrival. A damaged or forged piece is never saved, whoever sent
  it: the sender, another receiver or a relay.
- **The receiver does not trust the sender with its disk or its memory.** The
  file list and the manifest have size limits that are checked before they
  are downloaded; names are checked so nothing can be written outside the
  destination folder; and on Windows, names the system reserves (`CON`,
  `NUL`, `COM1`…) are saved with a `_` after them.

## The code

A code is four words from the [EFF large word list], about 52 bits. Run
through Argon2id (64 MiB) it gives a seed, and from the seed come the
sender's network identity for this transfer and the key receivers must prove
they hold. The receiver derives the same values, finds the sender by that
identity through iroh's DNS lookup service, and proves the key.

Guessing a code blind takes on the order of 2^52 Argon2id computations, each
of them slow on purpose.

## The ticket

A ticket (`carrick send --ticket`) carries the sender's identity, a random
256-bit key, the root hash of the content and the sender's addresses. It is
longer than a code, but it needs no lookup service, it pins the content, and
it cannot be used to pose as the sender.

## What it does not protect

- **Anyone with the code can receive the files**, and could pose as the
  sender to another receiver of the same code. Give a code only to people you
  would give the files to; use a ticket when that matters.
- **Relays and the lookup service see metadata**: IP addresses, times and
  how much is transferred. Never content. Run your own relay
  ([relay/README.md](../relay/README.md)) to keep that metadata on your side.
- Carrick does not hide who talks to whom.
- Carrick does not protect files at rest, on either computer. While a
  transfer is unfinished, its verified pieces wait in a hidden
  `.carrick-<id>` folder inside the destination.

## Reporting a vulnerability

Please do not open a public issue. Write to the owner of this repository
through GitHub, with the version (`carrick --version`) and the steps to
reproduce it.

[iroh]: https://github.com/n0-computer/iroh
[iroh-blobs]: https://github.com/n0-computer/iroh-blobs
[EFF large word list]: https://www.eff.org/dice
