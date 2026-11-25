//! One transfer the window runs: what the core reports about it, folded into
//! what the window shows, and the task that runs it.

use std::{
    collections::VecDeque,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui;
use carrick_core::{
    events::{self, Event, EventTx, Route},
    home::{Existing, Record, Role},
    recv::{self, ReceiveOutcome},
    send::SendOutcome,
    system::{self, Awake},
    CancellationToken,
};

use crate::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Send,
    Receive,
}

/// Where a transfer stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Started; the core has not said anything yet.
    Starting,
    /// Fingerprinting the files before they can be offered.
    Preparing { done: u64, total: u64 },
    /// Prepared, waiting in line behind another send.
    Queued,
    /// Off the network while another send goes first.
    Paused,
    /// On offer, waiting for receivers or serving them.
    Offering,
    /// Looking for the sender.
    Looking { attempt: u32 },
    /// Looking at what the destination already holds.
    Checking { done: u64, total: u64 },
    Receiving,
    /// The connection dropped; the core tries again by itself.
    Waiting { reason: String, seconds: u64 },
    /// Moving finished files into place.
    Saving { done: u64, total: u64 },
    Done,
    /// Stopped from the window; the transfer stays resumable.
    Stopped,
    Failed(String),
}

impl Phase {
    pub fn is_over(&self) -> bool {
        matches!(self, Phase::Done | Phase::Stopped | Phase::Failed(_))
    }
}

/// A receiver, as the sender sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receiver {
    pub id: String,
    pub have: u64,
    pub total: u64,
    pub done: bool,
    pub connected: bool,
    pub route: Option<Route>,
}

/// Recent progress, to work out a speed.
#[derive(Debug, Clone, Default)]
pub struct Speed {
    samples: VecDeque<(Instant, u64)>,
}

impl Speed {
    const WINDOW: Duration = Duration::from_secs(8);

    pub fn add(&mut self, at: Instant, have: u64) {
        if self.samples.back().is_some_and(|(_, last)| have < *last) {
            // Started over (a new attempt counts from what is verified).
            self.samples.clear();
        }
        self.samples.push_back((at, have));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| at.duration_since(*t) > Self::WINDOW) {
            self.samples.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Bytes per second over the last few seconds, once there is enough to say.
    pub fn per_second(&self) -> Option<f64> {
        let (t0, b0) = *self.samples.front()?;
        let (t1, b1) = *self.samples.back()?;
        let seconds = t1.duration_since(t0).as_secs_f64();
        (seconds >= 1.0 && b1 > b0).then(|| (b1 - b0) as f64 / seconds)
    }
}

/// How a transfer is going over time: the speed now, the average since the
/// copy began, the time left at that average, and the time taken so far.
#[derive(Debug, Clone, Default)]
pub struct Pace {
    recent: Speed,
    /// The first verified bytes of this run, and when they were seen.
    first: Option<(Instant, u64)>,
    last: Option<(Instant, u64)>,
    /// When it ended, which stops the clock.
    ended: Option<Instant>,
}

impl Pace {
    pub fn add(&mut self, at: Instant, have: u64) {
        self.recent.add(at, have);
        self.first.get_or_insert((at, have));
        self.last = Some((at, have));
    }

    /// Nothing is arriving for now (a reconnection): no speed "now".
    pub fn pause(&mut self) {
        self.recent.clear();
    }

    pub fn end(&mut self, at: Instant) {
        self.recent.clear();
        if self.first.is_some() {
            self.ended.get_or_insert(at);
        }
    }

    /// Bytes per second over the last few seconds.
    pub fn now(&self) -> Option<f64> {
        self.recent.per_second()
    }

    /// Bytes per second since the copy began, pauses included.
    pub fn average(&self, at: Instant) -> Option<f64> {
        let (t0, b0) = self.first?;
        let (_, b1) = self.last?;
        let seconds = self.ended.unwrap_or(at).duration_since(t0).as_secs_f64();
        (seconds >= 2.0 && b1 > b0).then(|| (b1 - b0) as f64 / seconds)
    }

    /// What is left, at the average speed.
    pub fn left(&self, have: u64, total: u64, at: Instant) -> Option<Duration> {
        let average = self.average(at)?;
        Some(Duration::from_secs_f64(total.saturating_sub(have) as f64 / average))
    }

    /// Time since the copy began, until it ended.
    pub fn taken(&self, at: Instant) -> Option<Duration> {
        let (t0, _) = self.first?;
        Some(self.ended.unwrap_or(at).duration_since(t0))
    }
}

/// Everything the window shows about one transfer.
#[derive(Debug, Clone)]
pub struct JobView {
    pub kind: Kind,
    /// The core's identifier, once known.
    pub id: Option<String>,
    pub name: String,
    pub files: u64,
    pub total: u64,
    /// For a receive: verified bytes so far.
    pub have: u64,
    pub code: Option<String>,
    pub ticket: Option<String>,
    /// For a receive: how data reaches this computer.
    pub route: Option<Route>,
    /// For a send: everyone who connected, in order of arrival.
    pub receivers: Vec<Receiver>,
    pub phase: Phase,
    /// Files found already at the destination, identical: (files, bytes).
    pub already: Option<(u64, u64)>,
    pub dest: Option<PathBuf>,
    /// Incoming files saved beside a different file of the same name.
    pub kept_aside: Vec<PathBuf>,
    /// The latest remark of the core that is not a state.
    pub note: Option<String>,
    pub pace: Pace,
    /// For a send: how it goes for each receiver, by id.
    pub receiver_pace: std::collections::HashMap<String, Pace>,
    /// Stop was pressed and the transfer is winding down.
    pub stopping: bool,
    /// Files that already exist and differ, waiting for an answer:
    /// (files, bytes, a few of their names).
    pub asking: Option<(u64, u64, Vec<String>)>,
}

impl JobView {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            id: None,
            name: String::new(),
            files: 0,
            total: 0,
            have: 0,
            code: None,
            ticket: None,
            route: None,
            receivers: Vec::new(),
            phase: Phase::Starting,
            already: None,
            dest: None,
            kept_aside: Vec::new(),
            note: None,
            pace: Pace::default(),
            receiver_pace: std::collections::HashMap::new(),
            stopping: false,
            asking: None,
        }
    }

    /// The view of a transfer continued from its record.
    pub fn from_record(record: &Record) -> Self {
        let kind = match record.role {
            Role::Send => Kind::Send,
            Role::Receive => Kind::Receive,
        };
        Self {
            id: Some(record.id.clone()),
            name: record.name.clone(),
            files: record.files,
            total: record.total_bytes,
            code: record.code.clone(),
            dest: record.dest.clone(),
            ..Self::new(kind)
        }
    }

    fn receiver(&mut self, id: &str) -> &mut Receiver {
        if let Some(i) = self.receivers.iter().position(|r| r.id == id) {
            return &mut self.receivers[i];
        }
        self.receivers.push(Receiver { id: id.to_string(), have: 0, total: 0, done: false, connected: true, route: None });
        self.receivers.last_mut().expect("just pushed")
    }

    /// Folds one event of the core into the view.
    pub fn apply(&mut self, event: Event, now: Instant) {
        match event {
            Event::Hashing { done, total } => {
                self.total = total;
                self.phase = Phase::Preparing { done, total };
            }
            Event::Ready { id, code, ticket, name, files, total } => {
                self.id = Some(id);
                self.code = code;
                self.ticket = ticket;
                self.name = name;
                self.files = files;
                self.total = total;
                self.phase = Phase::Offering;
            }
            Event::Queued { id, code, name, files, total } => {
                self.id = Some(id);
                self.code = code;
                self.name = name;
                self.files = files;
                self.total = total;
                self.phase = Phase::Queued;
            }
            Event::Paused => {
                for r in &mut self.receivers {
                    r.connected = false;
                }
                self.receiver_pace.values_mut().for_each(Pace::pause);
                self.phase = Phase::Paused;
            }
            Event::ReceiverJoined { receiver } => {
                let r = self.receiver(&receiver);
                r.connected = true;
            }
            Event::ReceiverRoute { receiver, route } => {
                self.receiver(&receiver).route = Some(route);
            }
            Event::ReceiverProgress { receiver, have, total } => {
                self.receiver_pace.entry(receiver.clone()).or_default().add(now, have);
                let r = self.receiver(&receiver);
                r.have = have;
                r.total = total;
            }
            Event::ReceiverDone { receiver } => {
                self.receiver_pace.entry(receiver.clone()).or_default().end(now);
                let r = self.receiver(&receiver);
                r.done = true;
                r.have = r.total;
            }
            Event::ReceiverLeft { receiver } => {
                // The route stays: it says how the receiver was reached.
                if let Some(pace) = self.receiver_pace.get_mut(&receiver) {
                    pace.pause();
                }
                self.receiver(&receiver).connected = false;
            }
            Event::Connecting { attempt } => {
                self.phase = Phase::Looking { attempt };
            }
            Event::Connected { route } => {
                self.route = Some(route);
            }
            Event::Listing { name, files, total } => {
                self.name = name;
                self.files = files;
                self.total = total;
            }
            Event::Checking { done, total } => {
                self.phase = Phase::Checking { done, total };
            }
            Event::AlreadyHave { files, bytes } => {
                self.already = Some((files, bytes));
            }
            Event::Progress { have, total } => {
                self.have = have;
                self.total = total;
                self.pace.add(now, have);
                self.phase = Phase::Receiving;
            }
            Event::Waiting { reason, seconds } => {
                self.route = None;
                self.pace.pause();
                self.phase = Phase::Waiting { reason, seconds };
            }
            Event::Saving { done, total } => {
                self.phase = Phase::Saving { done, total };
            }
            Event::Done { dest, .. } => {
                self.dest = Some(dest);
            }
            Event::AlreadyExist { files, bytes, examples } => {
                self.asking = Some((files, bytes, examples));
            }
            Event::Note(text) => {
                self.note = Some(text);
            }
        }
    }

    /// Records how the transfer ended.
    pub fn finish(&mut self, outcome: Result<Finish>) {
        self.stopping = false;
        let now = Instant::now();
        self.pace.end(now);
        self.receiver_pace.values_mut().for_each(|p| p.end(now));
        match outcome {
            Ok(Finish::Sent(SendOutcome::Completed { .. })) => self.phase = Phase::Done,
            Ok(Finish::Sent(SendOutcome::Stopped { .. })) | Ok(Finish::Received(ReceiveOutcome::Stopped)) => {
                self.phase = Phase::Stopped;
            }
            Ok(Finish::Received(ReceiveOutcome::Completed { files, bytes, dest, kept_aside })) => {
                self.files = files;
                self.have = self.total.max(bytes);
                self.dest = Some(dest);
                self.kept_aside = kept_aside.into_iter().map(|(_, path)| path).collect();
                self.phase = Phase::Done;
            }
            Err(e) => self.phase = Phase::Failed(format!("{e:#}")),
        }
    }

    /// How far along it is, from 0 to 1, when there is a sensible answer.
    pub fn fraction(&self) -> Option<f32> {
        match &self.phase {
            Phase::Preparing { done, total } | Phase::Checking { done, total } => Some(fmt::fraction(*done, *total)),
            Phase::Receiving | Phase::Waiting { .. } => Some(fmt::fraction(self.have, self.total)),
            Phase::Saving { done, total } => Some(fmt::fraction(*done, *total)),
            Phase::Done if self.kind == Kind::Receive => Some(1.0),
            _ => None,
        }
    }

    /// The name to show, before the core has told it.
    pub fn title(&self) -> String {
        let what = if self.name.is_empty() { "…" } else { &self.name };
        match (self.kind, self.phase == Phase::Done) {
            (Kind::Send, false) => format!("Sending {what}"),
            (Kind::Send, true) => format!("Sent {what}"),
            (Kind::Receive, false) => format!("Receiving {what}"),
            (Kind::Receive, true) => format!("Received {what}"),
        }
    }
}

/// How the core said a transfer ended.
#[derive(Debug)]
pub enum Finish {
    Sent(SendOutcome),
    Received(ReceiveOutcome),
}

/// A transfer started from this window.
pub struct Job {
    pub view: Arc<Mutex<JobView>>,
    stop: CancellationToken,
    /// Tells whether a record on disk is this transfer, so the list of
    /// unfinished transfers does not offer to continue it twice.
    owns: Box<dyn Fn(&Record, &JobView) -> bool + Send>,
    /// Where the answer goes when a receive asks about files that exist.
    pub answer: Option<recv::AnswerTx>,
    /// For a send: whether it may be on the network now, and its place in
    /// the line of sends (lower goes first).
    pub turn: Option<tokio::sync::watch::Sender<bool>>,
    pub place: i64,
}

impl Job {
    /// Starts `work` on `runtime`; its events update the view as they come.
    pub fn start<F, Fut>(
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
        view: JobView,
        notify: bool,
        owns: impl Fn(&Record, &JobView) -> bool + Send + 'static,
        work: F,
    ) -> Self
    where
        F: FnOnce(EventTx, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Finish>> + Send + 'static,
    {
        let view = Arc::new(Mutex::new(view));
        let stop = CancellationToken::new();
        let (tx, mut rx) = events::channel();
        let shown = view.clone();
        let repaint = ctx.clone();
        let listener = runtime.spawn(async move {
            while let Some(event) = rx.recv().await {
                if notify {
                    if let Event::AlreadyExist { files, .. } = &event {
                        system::notify(
                            "Carrick is waiting for you",
                            &format!("{files} file(s) already exist with different content: replace, skip or keep both?"),
                        );
                    }
                    if let Event::ReceiverDone { receiver } = &event {
                        let name = shown.lock().expect("view").name.clone();
                        system::notify("Carrick: a receiver has everything", &format!("{name} arrived complete at {receiver}"));
                    }
                }
                shown.lock().expect("view").apply(event, Instant::now());
                repaint.request_repaint();
            }
        });
        let shown = view.clone();
        let repaint = ctx.clone();
        let token = stop.clone();
        runtime.spawn(async move {
            let _awake = Awake::hold();
            let outcome = work(tx, token).await;
            // Let the last events in before the outcome; the core closes the
            // channel when it is done with it.
            let _ = tokio::time::timeout(Duration::from_secs(5), listener).await;
            if notify {
                tell(&shown.lock().expect("view"), &outcome);
            }
            shown.lock().expect("view").finish(outcome);
            repaint.request_repaint();
        });
        Self { view, stop, owns: Box::new(owns), answer: None, turn: None, place: 0 }
    }

    /// Answers the receive's question about files that already exist.
    pub fn answer(&self, choice: Existing) {
        if let Some(tx) = &self.answer {
            let _ = tx.send(choice);
        }
        self.view.lock().expect("view").asking = None;
    }

    pub fn stop(&self) {
        self.view.lock().expect("view").stopping = true;
        self.stop.cancel();
    }

    pub fn is_running(&self) -> bool {
        !self.view.lock().expect("view").phase.is_over()
    }

    pub fn owns(&self, record: &Record) -> bool {
        (self.owns)(record, &self.view.lock().expect("view"))
    }
}

/// The system notification for an ending, as the terminal shows it.
fn tell(view: &JobView, outcome: &Result<Finish>) {
    match outcome {
        Ok(Finish::Received(ReceiveOutcome::Completed { files, bytes, dest, .. })) => system::notify(
            "Carrick: transfer received",
            &format!("{}, {}, saved in {}", fmt::files(*files), fmt::bytes(*bytes), dest.display()),
        ),
        Err(e) => {
            let what = match view.kind {
                Kind::Send => "sending",
                Kind::Receive => "receiving",
            };
            system::notify(&format!("Carrick: {what} failed"), &format!("{e:#}"));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_follows_its_receivers() {
        let now = Instant::now();
        let mut v = JobView::new(Kind::Send);
        v.apply(Event::Hashing { done: 5, total: 10 }, now);
        assert_eq!(v.phase, Phase::Preparing { done: 5, total: 10 });
        assert_eq!(v.fraction(), Some(0.5));
        v.apply(
            Event::Ready {
                id: "abc".into(),
                code: Some("one-two-three-four".into()),
                ticket: None,
                name: "photos".into(),
                files: 3,
                total: 10,
            },
            now,
        );
        assert_eq!(v.phase, Phase::Offering);
        assert_eq!(v.code.as_deref(), Some("one-two-three-four"));
        assert_eq!(v.title(), "Sending photos");

        v.apply(Event::ReceiverJoined { receiver: "r1".into() }, now);
        v.apply(Event::ReceiverRoute { receiver: "r1".into(), route: Route::Relay }, now);
        v.apply(Event::ReceiverProgress { receiver: "r1".into(), have: 4, total: 10 }, now);
        v.apply(Event::ReceiverJoined { receiver: "r2".into() }, now);
        v.apply(Event::ReceiverRoute { receiver: "r1".into(), route: Route::Direct }, now);
        v.apply(Event::ReceiverDone { receiver: "r1".into() }, now);
        v.apply(Event::ReceiverLeft { receiver: "r2".into() }, now);
        assert_eq!(v.receivers.len(), 2);
        assert_eq!(v.receivers[0], Receiver { id: "r1".into(), have: 10, total: 10, done: true, connected: true, route: Some(Route::Direct) });
        assert!(!v.receivers[1].connected);

        v.finish(Ok(Finish::Sent(SendOutcome::Completed { receivers: 1 })));
        assert_eq!(v.phase, Phase::Done);
        assert!(v.phase.is_over());
        assert_eq!(v.title(), "Sent photos");
    }

    #[test]
    fn a_receive_shows_route_progress_and_retries() {
        let t0 = Instant::now();
        let mut v = JobView::new(Kind::Receive);
        v.apply(Event::Connecting { attempt: 1 }, t0);
        assert_eq!(v.phase, Phase::Looking { attempt: 1 });
        v.apply(Event::Connected { route: Route::Direct }, t0);
        v.apply(Event::Listing { name: "video.bin".into(), files: 1, total: 1000 }, t0);
        v.apply(Event::AlreadyHave { files: 0, bytes: 0 }, t0);
        v.apply(Event::Progress { have: 100, total: 1000 }, t0);
        v.apply(Event::Progress { have: 600, total: 1000 }, t0 + Duration::from_secs(2));
        assert_eq!(v.route, Some(Route::Direct));
        assert_eq!(v.fraction(), Some(0.6));
        assert_eq!(v.pace.now(), Some(250.0));
        assert_eq!(v.pace.average(t0 + Duration::from_secs(2)), Some(250.0));
        assert_eq!(v.pace.left(600, 1000, t0 + Duration::from_secs(2)), Some(Duration::from_secs_f64(1.6)));
        assert_eq!(v.pace.taken(t0 + Duration::from_secs(5)), Some(Duration::from_secs(5)));

        v.apply(Event::Waiting { reason: "the connection dropped".into(), seconds: 2 }, t0);
        assert_eq!(v.route, None);
        assert_eq!(v.pace.now(), None, "no speed now while waiting");
        assert!(v.pace.average(t0 + Duration::from_secs(4)).is_some(), "the average keeps counting");
        assert_eq!(v.fraction(), Some(0.6), "what arrived is kept while waiting");
        // After the wait the core looks again.
        v.apply(Event::Connecting { attempt: 2 }, t0);
        assert_eq!(v.phase, Phase::Looking { attempt: 2 });

        v.apply(Event::Saving { done: 1, total: 1 }, t0);
        v.finish(Ok(Finish::Received(ReceiveOutcome::Completed {
            files: 1,
            bytes: 1000,
            dest: PathBuf::from("dest"),
            kept_aside: vec![("video.bin".into(), PathBuf::from("dest/video (2).bin"))],
        })));
        assert_eq!(v.phase, Phase::Done);
        assert_eq!(v.fraction(), Some(1.0));
        assert_eq!(v.kept_aside, vec![PathBuf::from("dest/video (2).bin")]);
    }

    #[test]
    fn errors_and_stops_end_a_transfer() {
        let mut v = JobView::new(Kind::Receive);
        v.finish(Err(anyhow::anyhow!("refused: wrong code or ticket")));
        assert_eq!(v.phase, Phase::Failed("refused: wrong code or ticket".into()));
        let mut v = JobView::new(Kind::Send);
        v.stopping = true;
        v.finish(Ok(Finish::Sent(SendOutcome::Stopped { receivers: 0 })));
        assert_eq!(v.phase, Phase::Stopped);
        assert!(!v.stopping);
    }
}
