//! What the core tells whoever is showing the transfer to a person.

use std::path::PathBuf;

/// How data is reaching the other side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Straight from one computer to the other.
    Direct,
    /// Through a relay server, still encrypted end to end.
    Relay,
}

#[derive(Debug, Clone)]
pub enum Event {
    // ---- sending ----
    /// Fingerprinting the files before they can be offered.
    Hashing { done: u64, total: u64 },
    /// The transfer is on offer.
    Ready {
        id: String,
        code: Option<String>,
        ticket: Option<String>,
        name: String,
        files: u64,
        total: u64,
    },
    ReceiverJoined { receiver: String },
    /// How data reaches this receiver; sent again whenever it changes.
    ReceiverRoute { receiver: String, route: Route },
    ReceiverProgress { receiver: String, have: u64, total: u64 },
    ReceiverDone { receiver: String },
    ReceiverLeft { receiver: String },

    // ---- receiving ----
    Connecting { attempt: u32 },
    Connected { route: Route },
    /// The list of files arrived.
    Listing { name: String, files: u64, total: u64 },
    /// Looking at what the destination already holds.
    Checking { done: u64, total: u64 },
    /// Files found already present and identical; they will not travel.
    AlreadyHave { files: u64, bytes: u64 },
    /// Files whose place in the destination is taken by different files.
    /// The receive waits for an answer (see `recv::Answers`).
    AlreadyExist { files: u64, bytes: u64, examples: Vec<String> },
    Progress { have: u64, total: u64 },
    /// The connection dropped or stalled; trying again by itself.
    Waiting { reason: String, seconds: u64 },
    /// Moving finished files into place.
    Saving { done: u64, total: u64 },
    Done { files: u64, bytes: u64, dest: PathBuf },

    /// Anything else worth a line.
    Note(String),
}

pub type EventTx = tokio::sync::mpsc::UnboundedSender<Event>;
pub type EventRx = tokio::sync::mpsc::UnboundedReceiver<Event>;

pub fn channel() -> (EventTx, EventRx) {
    tokio::sync::mpsc::unbounded_channel()
}

/// Sends an event; a closed listener is not an error.
pub(crate) fn emit(tx: &EventTx, event: Event) {
    let _ = tx.send(event);
}
