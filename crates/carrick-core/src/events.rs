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
    /// Prepared, and waiting for its turn behind another send: not on the
    /// network yet. A code is already good to give; a ticket comes with
    /// `Ready`, because it carries addresses only known then.
    Queued {
        id: String,
        code: Option<String>,
        name: String,
        files: u64,
        total: u64,
    },
    /// Taken off the network for now, to let another send go first; it
    /// comes back by itself. Receivers wait and then carry on.
    Paused,
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

/// Whether a send may be on the network now. Whoever keeps a line of sends
/// holds the other end; a send without one goes on at once.
pub type Turn = tokio::sync::watch::Receiver<bool>;

pub type EventTx = tokio::sync::mpsc::UnboundedSender<Event>;
pub type EventRx = tokio::sync::mpsc::UnboundedReceiver<Event>;

pub fn channel() -> (EventTx, EventRx) {
    tokio::sync::mpsc::unbounded_channel()
}

/// Sends an event; a closed listener is not an error.
pub(crate) fn emit(tx: &EventTx, event: Event) {
    let _ = tx.send(event);
}
