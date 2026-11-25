//! Which transfers are running right now, and how far along they are.
//!
//! A transfer that runs holds a lock on a file in its folder for as long as
//! it runs. The system lets go of the lock when the program ends, however it
//! ends, so another Carrick on the same computer (the window while a terminal
//! receives, say) can tell a transfer that is running from one that was cut
//! short, and does not offer to continue what is still going.
//!
//! While it runs, the transfer also writes where it stands to a small file
//! next to its record, about once a second, for that other Carrick to show.

use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions, TryLockError},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    events::{self, Event, EventTx, Route},
    home::{now_secs, Home, Role},
};

/// Held while a transfer runs; dropping it lets go of the transfer.
#[derive(Debug)]
pub struct Claim {
    _lock: File,
}

/// Where a running transfer stands, as another Carrick reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// The program running it: `carrick` (the terminal) or `carrick-gui`.
    pub program: String,
    pub stage: Stage,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub files: u64,
    /// For a receive: verified bytes so far. For a send: those of the
    /// receiver furthest along that does not have everything yet.
    pub have: u64,
    pub total: u64,
    /// For a receive: whether data comes straight from the other computer.
    #[serde(default)]
    pub direct: Option<bool>,
    /// Bytes per second over the last few seconds.
    #[serde(default)]
    pub speed: Option<f64>,
    /// When data started to arrive in this run (seconds since the Unix
    /// epoch), and how much there was then: for the average and time taken.
    #[serde(default)]
    pub since: Option<u64>,
    #[serde(default)]
    pub since_have: u64,
    /// For a send: everyone who connected.
    #[serde(default)]
    pub receivers: Vec<ReceiverProgress>,
    /// When this was written (seconds since the Unix epoch).
    pub updated: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiverProgress {
    pub id: String,
    pub have: u64,
    pub total: u64,
    pub done: bool,
    pub connected: bool,
    #[serde(default)]
    pub direct: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Starting,
    /// Fingerprinting the files before they can be offered.
    Preparing,
    /// Prepared, waiting for its turn behind another send.
    Queued,
    /// Off the network for now: another send goes first.
    Paused,
    /// On offer, waiting for receivers or serving them.
    Offering,
    Looking,
    Checking,
    Receiving,
    /// The connection dropped; trying again by itself.
    Waiting { reason: String },
    /// Waiting for the person to say what to do with files that exist.
    Asking,
    Saving { done: u64, total: u64 },
}

/// How many times taking a transfer is tried before saying it is running
/// elsewhere: `is_running` holds the lock for an instant when it looks.
const CLAIM_TRIES: u32 = 4;

impl Home {
    fn lock_path(&self, id: &str) -> PathBuf {
        self.transfer_dir(id).join("running.lock")
    }

    fn progress_path(&self, id: &str) -> PathBuf {
        self.transfer_dir(id).join("progress.json")
    }

    /// Takes a transfer for this program while it runs. Fails if another
    /// program on this computer is running it.
    pub fn claim(&self, id: &str) -> Result<Claim> {
        let dir = self.transfer_dir(id);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = self.lock_path(id);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        for attempt in 1..=CLAIM_TRIES {
            match file.try_lock() {
                Ok(()) => return Ok(Claim { _lock: file }),
                Err(TryLockError::WouldBlock) if attempt < CLAIM_TRIES => std::thread::sleep(Duration::from_millis(50)),
                Err(TryLockError::WouldBlock) => break,
                Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", path.display())),
            }
        }
        bail!("this transfer is already running in another Carrick on this computer (a window or a terminal)")
    }

    /// Whether another program on this computer is running this transfer.
    /// A transfer this program runs counts too: the lock is per open file.
    pub fn is_running(&self, id: &str) -> bool {
        let Ok(file) = OpenOptions::new().read(true).write(true).open(self.lock_path(id)) else {
            return false;
        };
        match file.try_lock() {
            // Nobody holds it; dropping the file lets go at once.
            Ok(()) => false,
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Error(_)) => false,
        }
    }

    /// Where a running transfer stands, if it has said.
    pub fn progress(&self, id: &str) -> Option<Progress> {
        let bytes = std::fs::read(self.progress_path(id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Writes where a transfer stands. A running transfer does it by itself.
    pub fn write_progress(&self, id: &str, progress: &Progress) {
        let path = self.progress_path(id);
        let tmp = path.with_extension("json.tmp");
        let Ok(body) = serde_json::to_vec(progress) else { return };
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// A transfer that ended well, as the history keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finished {
    pub role: Role,
    pub name: String,
    pub files: u64,
    /// The size of the whole transfer.
    pub bytes: u64,
    /// What travelled in the run that finished it (files already in place
    /// do not travel, and a continued transfer starts from what it had).
    pub moved: u64,
    /// When data started to move and when it was all there (seconds since
    /// the Unix epoch).
    pub started: u64,
    pub ended: u64,
    /// For a receive: where the files went.
    #[serde(default)]
    pub dest: Option<PathBuf>,
    /// For a send: how many receivers got everything.
    #[serde(default)]
    pub receivers: u32,
    /// Whether data went straight from one computer to the other.
    #[serde(default)]
    pub direct: Option<bool>,
}

/// How many finished transfers the history keeps.
pub const HISTORY_KEEPS: usize = 3;

impl Home {
    fn history_path(&self) -> PathBuf {
        self.root().join("history.json")
    }

    /// The last transfers that ended well on this computer, newest first.
    pub fn history(&self) -> Vec<Finished> {
        std::fs::read(self.history_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Adds a finished transfer to the history, which keeps only the last few.
    pub fn remember(&self, finished: Finished) {
        let mut list = self.history();
        list.insert(0, finished);
        list.truncate(HISTORY_KEEPS);
        let path = self.history_path();
        let tmp = path.with_extension("json.tmp");
        let Ok(body) = serde_json::to_vec_pretty(&list) else { return };
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// What a running transfer has said so far, kept for its history entry.
#[derive(Clone)]
pub(crate) struct Watch {
    tally: Arc<Mutex<Tally>>,
}

impl Watch {
    /// Writes the history entry of a transfer that just ended well.
    pub(crate) fn remember(&self, home: &Home, id: &str) {
        let Ok(Some(record)) = home.load(id) else { return };
        let tally = self.tally.lock().expect("tally");
        let ended = tally.ended.unwrap_or_else(now_secs);
        let (started, at_start) = tally.since.unwrap_or((ended, tally.moved()));
        let name = if record.name.is_empty() { tally.name.clone() } else { record.name.clone() };
        home.remember(Finished {
            role: record.role,
            name,
            files: if record.files > 0 { record.files } else { tally.files },
            bytes: if record.total_bytes > 0 { record.total_bytes } else { tally.total },
            moved: tally.moved().saturating_sub(at_start),
            started: started.min(ended),
            ended,
            dest: record.dest.clone(),
            receivers: record.finished_receivers.len() as u32,
            direct: if record.role == Role::Receive {
                tally.direct.or(tally.was_direct)
            } else {
                let routes: Vec<bool> = tally.receivers.values().filter(|r| r.done).filter_map(|r| r.direct).collect();
                (!routes.is_empty()).then(|| routes.iter().all(|d| *d))
            },
        });
    }
}

/// Passes the events of a transfer on to `outer`, and keeps its progress
/// file up to date on the way. Ends when the transfer drops the sender.
pub(crate) fn watch(home: &Home, id: &str, outer: EventTx) -> (EventTx, Watch) {
    let (tx, mut rx) = events::channel();
    let home = home.clone();
    let id = id.to_string();
    let tally = Arc::new(Mutex::new(Tally::new()));
    let watching = Watch { tally: tally.clone() };
    tokio::spawn(async move {
        let mut written: Option<Instant> = None;
        while let Some(event) = rx.recv().await {
            let now = Instant::now();
            let progress = {
                let mut tally = tally.lock().expect("tally");
                let turned = tally.apply(&event, now);
                (turned || written.is_none_or(|at| now.duration_since(at) >= WRITE_EVERY)).then(|| tally.progress(now))
            };
            let _ = outer.send(event);
            if let Some(progress) = progress {
                home.write_progress(&id, &progress);
                written = Some(now);
            }
        }
    });
    (tx, watching)
}

const WRITE_EVERY: Duration = Duration::from_secs(1);
const SPEED_WINDOW: Duration = Duration::from_secs(8);

/// Folds events into a `Progress`.
struct Tally {
    stage: Stage,
    name: String,
    files: u64,
    have: u64,
    total: u64,
    direct: Option<bool>,
    /// The route before the last interruption, for the history.
    was_direct: Option<bool>,
    receivers: BTreeMap<String, ReceiverProgress>,
    /// (when, bytes moved): bytes moved is `have` for a receive and the sum
    /// over receivers for a send.
    samples: VecDeque<(Instant, u64)>,
    since: Option<(u64, u64)>,
    /// When everything was there: the receive saved it, or a receiver of
    /// the send got it all (the last one).
    ended: Option<u64>,
}

impl Tally {
    fn new() -> Self {
        Self {
            stage: Stage::Starting,
            name: String::new(),
            files: 0,
            have: 0,
            total: 0,
            direct: None,
            was_direct: None,
            receivers: BTreeMap::new(),
            samples: VecDeque::new(),
            since: None,
            ended: None,
        }
    }

    fn moved(&self) -> u64 {
        if self.receivers.is_empty() {
            self.have
        } else {
            self.receivers.values().map(|r| r.have).sum()
        }
    }

    fn sample(&mut self, now: Instant) {
        let moved = self.moved();
        if self.samples.back().is_some_and(|(_, last)| moved < *last) {
            self.samples.clear();
        }
        self.since.get_or_insert((now_secs(), moved));
        self.samples.push_back((now, moved));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > SPEED_WINDOW) {
            self.samples.pop_front();
        }
    }

    fn receiver(&mut self, id: &str) -> &mut ReceiverProgress {
        self.receivers.entry(id.to_string()).or_insert_with(|| ReceiverProgress {
            id: id.to_string(),
            have: 0,
            total: 0,
            done: false,
            connected: true,
            direct: None,
        })
    }

    /// Takes in one event; true when the stage changed.
    fn apply(&mut self, event: &Event, now: Instant) -> bool {
        let before = std::mem::discriminant(&self.stage);
        match event {
            Event::Hashing { done, total } => {
                self.stage = Stage::Preparing;
                self.have = *done;
                self.total = *total;
            }
            Event::Ready { name, files, total, .. } => {
                self.stage = Stage::Offering;
                self.name = name.clone();
                self.files = *files;
                self.total = *total;
                self.have = 0;
            }
            Event::ReceiverJoined { receiver } => self.receiver(receiver).connected = true,
            Event::ReceiverRoute { receiver, route } => self.receiver(receiver).direct = Some(*route == Route::Direct),
            Event::ReceiverProgress { receiver, have, total } => {
                let r = self.receiver(receiver);
                r.have = *have;
                r.total = *total;
                self.sample(now);
            }
            Event::ReceiverDone { receiver } => {
                let r = self.receiver(receiver);
                r.done = true;
                r.have = r.total;
                self.ended = Some(now_secs());
            }
            Event::ReceiverLeft { receiver } => self.receiver(receiver).connected = false,
            Event::Queued { name, files, total, .. } => {
                self.stage = Stage::Queued;
                self.name = name.clone();
                self.files = *files;
                self.total = *total;
            }
            Event::Paused => {
                self.stage = Stage::Paused;
                self.samples.clear();
            }
            Event::Connecting { .. } => self.stage = Stage::Looking,
            Event::Connected { route } => {
                self.direct = Some(*route == Route::Direct);
                self.was_direct = self.direct;
            }
            Event::Listing { name, files, total } => {
                self.name = name.clone();
                self.files = *files;
                self.total = *total;
            }
            Event::Checking { done, total } => {
                self.stage = Stage::Checking;
                self.have = *done;
                self.total = *total;
            }
            Event::Done { .. } => self.ended = Some(now_secs()),
            Event::AlreadyHave { .. } | Event::Note(_) => {}
            Event::AlreadyExist { .. } => self.stage = Stage::Asking,
            Event::Progress { have, total } => {
                self.stage = Stage::Receiving;
                self.have = *have;
                self.total = *total;
                self.sample(now);
            }
            Event::Waiting { reason, .. } => {
                self.stage = Stage::Waiting { reason: reason.clone() };
                self.direct = None;
                self.samples.clear();
            }
            Event::Saving { done, total } => self.stage = Stage::Saving { done: *done, total: *total },
        }
        before != std::mem::discriminant(&self.stage)
    }

    fn progress(&self, now: Instant) -> Progress {
        let speed = match (self.samples.front(), self.samples.back()) {
            (Some((t0, b0)), Some((t1, b1))) => {
                let seconds = t1.duration_since(*t0).as_secs_f64();
                // Nothing new for a while: there is no speed "now".
                let fresh = now.duration_since(*t1) < Duration::from_secs(5);
                (fresh && seconds >= 1.0 && b1 > b0).then(|| (b1 - b0) as f64 / seconds)
            }
            _ => None,
        };
        let (have, total) = if self.receivers.is_empty() {
            (self.have, self.total)
        } else {
            self.receivers
                .values()
                .filter(|r| !r.done)
                .max_by_key(|r| r.have)
                .map(|r| (r.have, r.total))
                .unwrap_or((self.total, self.total))
        };
        Progress {
            program: program(),
            stage: self.stage.clone(),
            name: self.name.clone(),
            files: self.files,
            have,
            total,
            direct: self.direct,
            speed,
            since: self.since.map(|(at, _)| at),
            since_have: self.since.map(|(_, moved)| moved).unwrap_or(0),
            receivers: self.receivers.values().cloned().collect(),
            updated: now_secs(),
        }
    }
}

/// The name of this program, as the person would recognise it.
fn program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "carrick".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_transfer_is_seen_and_cannot_be_taken_twice() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path()).unwrap();
        assert!(!home.is_running("abc"), "a transfer never started is not running");

        let claim = home.claim("abc").unwrap();
        assert!(home.is_running("abc"));
        let again = home.claim("abc").unwrap_err().to_string();
        assert!(again.contains("already running"), "{again}");
        // Looking does not take it away.
        assert!(home.is_running("abc"));

        drop(claim);
        assert!(!home.is_running("abc"), "a transfer let go of is not running");
        let _taken = home.claim("abc").unwrap();
    }

    #[test]
    fn progress_is_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path()).unwrap();
        assert!(home.progress("abc").is_none());

        let mut tally = Tally::new();
        let start = Instant::now();
        tally.apply(&Event::Connected { route: Route::Direct }, start);
        tally.apply(&Event::Listing { name: "SB#24".into(), files: 21, total: 1000 }, start);
        assert!(tally.apply(&Event::Progress { have: 100, total: 1000 }, start), "the stage changed to receiving");
        assert!(!tally.apply(&Event::Progress { have: 300, total: 1000 }, start + Duration::from_secs(2)));
        std::fs::create_dir_all(home.transfer_dir("abc")).unwrap();
        home.write_progress("abc", &tally.progress(start + Duration::from_secs(2)));

        let read = home.progress("abc").unwrap();
        assert_eq!(read.stage, Stage::Receiving);
        assert_eq!((read.name.as_str(), read.files, read.have, read.total), ("SB#24", 21, 300, 1000));
        assert_eq!(read.direct, Some(true));
        assert_eq!(read.speed, Some(100.0), "200 bytes in 2 seconds");
        assert_eq!(read.since_have, 100);
    }

    #[test]
    fn a_send_reports_the_receiver_still_receiving() {
        let mut tally = Tally::new();
        let now = Instant::now();
        tally.apply(&Event::Ready { id: "x".into(), code: None, ticket: None, name: "a".into(), files: 1, total: 50 }, now);
        tally.apply(&Event::ReceiverProgress { receiver: "r1".into(), have: 50, total: 50 }, now);
        tally.apply(&Event::ReceiverDone { receiver: "r1".into() }, now);
        tally.apply(&Event::ReceiverProgress { receiver: "r2".into(), have: 20, total: 50 }, now);
        let p = tally.progress(now);
        assert_eq!(p.stage, Stage::Offering);
        assert_eq!((p.have, p.total), (20, 50));
        assert_eq!(p.receivers.len(), 2);
    }
}
