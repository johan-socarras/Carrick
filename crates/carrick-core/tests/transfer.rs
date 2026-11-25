//! Whole transfers between endpoints in this process.
//!
//! They use tickets and no relay, so they need no server and no Internet:
//! the endpoints find each other by the addresses in the ticket.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use carrick_core::{
    access::Ticket,
    events::{self, Event, EventRx, Route},
    home::{Existing, Home, Role, State},
    net::{NetOptions, RelayChoice},
    recv::{self, ReceiveOptions, ReceiveOutcome},
    send::{self, SendOptions, SendOutcome},
    CancellationToken,
};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(120);

fn offline() -> NetOptions {
    NetOptions { relay: RelayChoice::Disabled, ..Default::default() }
}

/// Repeatable bytes that do not compress, without a dependency.
fn fill(path: &Path, size: usize, seed: u64) {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut data = Vec::with_capacity(size + 8);
    while data.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(&state.to_le_bytes());
    }
    data.truncate(size);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// Every file under `root`, as (relative path, content), sorted.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with(".carrick-") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

struct Sending {
    ticket: String,
    task: JoinHandle<anyhow::Result<SendOutcome>>,
    stop: CancellationToken,
    /// What the sender reported after it was ready.
    seen: Arc<Mutex<Vec<Event>>>,
}

/// Starts a sender and waits until it has a ticket to give.
async fn start_sender(home: &Home, paths: Vec<PathBuf>, receivers: Option<u32>) -> Sending {
    let (tx, mut rx) = events::channel();
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let home = home.clone();
        let stop = stop.clone();
        async move {
            let opts = SendOptions { paths, ticket: true, receivers, net: offline(), turn: None };
            send::send(&home, opts, tx, stop).await
        }
    });
    let ticket = loop {
        match tokio::time::timeout(WAIT, rx.recv()).await.expect("sender got ready in time") {
            Some(Event::Ready { ticket: Some(ticket), .. }) => break ticket,
            Some(_) => continue,
            None => panic!("the sender ended before it was ready: {:?}", task.await),
        }
    };
    let seen = collect(rx);
    Sending { ticket, task, stop, seen }
}

/// Collects events while a receive runs.
fn collect(mut rx: EventRx) -> Arc<Mutex<Vec<Event>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            sink.lock().unwrap().push(event);
        }
    });
    seen
}

async fn receive_all(home: &Home, key: &str, dest: &Path, overwrite: bool) -> (anyhow::Result<ReceiveOutcome>, Arc<Mutex<Vec<Event>>>) {
    let (tx, rx) = events::channel();
    let seen = collect(rx);
    let opts = ReceiveOptions {
        key: key.to_string(),
        dest: dest.to_path_buf(),
        existing: if overwrite { Existing::Replace } else { Existing::KeepBoth },
        answers: None,
        seed: false,
        net: offline(),
    };
    let outcome = tokio::time::timeout(WAIT, recv::receive(home, opts, tx, CancellationToken::new()))
        .await
        .expect("receive finished in time");
    (outcome, seen)
}

fn sample_tree(root: &Path) -> PathBuf {
    let top = root.join("photos");
    fill(&top.join("big.bin"), 9 * 1024 * 1024 + 123, 1);
    fill(&top.join("nested/deep/small.bin"), 4_321, 2);
    fill(&top.join("nested/twin-a.bin"), 300_000, 3);
    fill(&top.join("nested/twin-b.bin"), 300_000, 3); // same content twice
    std::fs::write(top.join("empty.txt"), b"").unwrap();
    std::fs::write(top.join("with space and ñ.txt"), "hola").unwrap();
    std::fs::create_dir_all(top.join("empty-folder")).unwrap();
    top
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_arrives_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");

    let sending = start_sender(&sender_home, vec![source], Some(1)).await;
    let (outcome, _) = receive_all(&receiver_home, &sending.ticket, &dest, false).await;
    match outcome.unwrap() {
        ReceiveOutcome::Completed { files, kept_aside, .. } => {
            assert_eq!(files, 6);
            assert!(kept_aside.is_empty());
        }
        other => panic!("unexpected outcome {other:?}"),
    }
    assert_eq!(snapshot(&tmp.path().join("source")), snapshot(&dest));
    assert!(dest.join("photos/empty-folder").is_dir());
    // Nothing of the working store is left next to the files.
    assert!(std::fs::read_dir(&dest).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".carrick-")));

    let sent = tokio::time::timeout(WAIT, sending.task).await.unwrap().unwrap().unwrap();
    assert_eq!(sent, SendOutcome::Completed { receivers: 1 });
    // Without a relay the only way is direct, and the sender says so.
    let routes: Vec<Route> = sending
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| if let Event::ReceiverRoute { route, .. } = e { Some(*route) } else { None })
        .collect();
    assert_eq!(routes, vec![Route::Direct]);
    assert!(sender_home.list().unwrap().iter().all(|r| r.state == State::Done));
    assert!(receiver_home.list().unwrap().iter().all(|r| r.state == State::Done));
}

/// On offer to anyone (no number of receivers), as the window sends: once
/// someone has everything, stopping ends the send instead of leaving it to
/// be offered again for nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_a_send_someone_received_ends_it() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");

    let sending = start_sender(&sender_home, vec![source], None).await;
    let id = sender_home.list().unwrap()[0].id.clone();
    assert!(sender_home.is_running(&id), "a send on offer is running");
    let (outcome, _) = receive_all(&receiver_home, &sending.ticket, &dest, false).await;
    assert!(matches!(outcome.unwrap(), ReceiveOutcome::Completed { .. }));
    let receive_id = receiver_home.list().unwrap()[0].id.clone();
    assert!(!receiver_home.is_running(&receive_id), "a finished receive lets go");

    // Stopped right away, maybe before the sender has counted the receiver.
    sending.stop.cancel();
    let sent = tokio::time::timeout(WAIT, sending.task).await.unwrap().unwrap().unwrap();
    assert_eq!(sent, SendOutcome::Completed { receivers: 1 });
    assert!(sender_home.list().unwrap().iter().all(|r| r.state == State::Done));
    assert!(!sender_home.is_running(&id));
}

/// A transfer can only run once on a computer: a second receive of the same
/// thing, from a terminal while the window has it, say, is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transfer_runs_only_once_on_a_computer() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");

    let sending = start_sender(&sender_home, vec![source], Some(1)).await;
    let (outcome, _) = receive_all(&receiver_home, &sending.ticket, &dest, false).await;
    assert!(matches!(outcome.unwrap(), ReceiveOutcome::Completed { .. }));
    let record = receiver_home.list().unwrap().remove(0);

    // As if another program held it: a second receive is refused, and
    // forgetting it is too.
    let held = receiver_home.claim(&record.id).unwrap();
    let mut again = record.clone();
    again.state = State::Active;
    let (tx, _rx) = events::channel();
    let error = recv::resume_receive(&receiver_home, again, None, tx, CancellationToken::new()).await.unwrap_err();
    assert!(format!("{error:#}").contains("already running"), "{error:#}");
    assert!(format!("{:#}", receiver_home.forget(&record.id).unwrap_err()).contains("running"));
    drop(held);
    assert!(receiver_home.forget(&record.id).unwrap());
    let _ = tokio::time::timeout(WAIT, sending.task).await;
}

/// A send in line is off the network until its turn, and says so; one that
/// gives way to another goes off and comes back, and its receiver carries on
/// by itself. Both ends then keep the transfer in their history.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_waits_its_turn_and_can_give_way() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");
    // A fixed port, so the ticket still points at the sender when it comes back.
    let port = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let net = NetOptions { port: Some(port), ..offline() };

    // A send that never gets its turn: in line, with no ticket yet.
    let (_hold, waiting) = tokio::sync::watch::channel(false);
    let (tx, mut rx) = events::channel();
    let stop_queued = CancellationToken::new();
    let queued = tokio::spawn({
        let home = sender_home.clone();
        let stop = stop_queued.clone();
        let opts = SendOptions { paths: vec![source.clone()], ticket: true, receivers: None, net: offline(), turn: Some(waiting) };
        async move { send::send(&home, opts, tx, stop).await }
    });
    loop {
        match tokio::time::timeout(WAIT, rx.recv()).await.expect("the send said it is in line").unwrap() {
            Event::Queued { name, .. } => {
                assert_eq!(name, "photos");
                break;
            }
            Event::Ready { .. } => panic!("a send in line went on the network"),
            _ => {}
        }
    }
    stop_queued.cancel();
    assert_eq!(queued.await.unwrap().unwrap(), SendOutcome::Stopped { receivers: 0 });

    // A send with its turn, which then gives way.
    let (go, turn) = tokio::sync::watch::channel(true);
    let (tx, mut rx) = events::channel();
    let sending = tokio::spawn({
        let home = sender_home.clone();
        let opts = SendOptions { paths: vec![source], ticket: true, receivers: Some(1), net, turn: Some(turn) };
        async move { send::send(&home, opts, tx, CancellationToken::new()).await }
    });
    let ticket = loop {
        if let Event::Ready { ticket: Some(ticket), .. } = tokio::time::timeout(WAIT, rx.recv()).await.unwrap().unwrap() {
            break ticket;
        }
    };
    go.send(false).unwrap();
    loop {
        if let Event::Paused = tokio::time::timeout(WAIT, rx.recv()).await.expect("the send paused").unwrap() {
            break;
        }
    }
    let seen = collect(rx);
    let receiving = tokio::spawn({
        let home = receiver_home.clone();
        let dest = dest.clone();
        async move { receive_all(&home, &ticket, &dest, false).await.0 }
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!receiving.is_finished(), "received from a paused send");

    go.send(true).unwrap();
    let outcome = tokio::time::timeout(WAIT, receiving).await.unwrap().unwrap();
    assert!(matches!(outcome.unwrap(), ReceiveOutcome::Completed { files: 6, .. }));
    assert_eq!(snapshot(&tmp.path().join("source")), snapshot(&dest));
    let sent = tokio::time::timeout(WAIT, sending).await.unwrap().unwrap().unwrap();
    assert_eq!(sent, SendOutcome::Completed { receivers: 1 });
    assert!(seen.lock().unwrap().iter().any(|e| matches!(e, Event::Ready { .. })), "it came back on the network");

    let sent = sender_home.history();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!((sent[0].role, sent[0].name.as_str(), sent[0].files, sent[0].receivers), (Role::Send, "photos", 6, 1));
    let got = receiver_home.history();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!((got[0].role, got[0].files, got[0].dest.as_deref()), (Role::Receive, 6, Some(dest.as_path())));
    assert_eq!(got[0].direct, Some(true));
    assert!(got[0].ended >= got[0].started);
}

/// The history keeps the last three, newest first.
#[test]
fn the_history_keeps_the_last_three() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path()).unwrap();
    for n in 0..5u64 {
        home.remember(carrick_core::running::Finished {
            role: Role::Receive,
            name: format!("t{n}"),
            files: 1,
            bytes: n,
            moved: n,
            started: n,
            ended: n,
            dest: None,
            receivers: 0,
            direct: None,
        });
    }
    let names: Vec<String> = home.history().into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["t4", "t3", "t2"]);
}

/// A folder's hidden desktop.ini arrives hidden, not as a stray file.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hidden_desktop_ini_stays_hidden() {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source").join("videos");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("clip.bin"), b"a video").unwrap();
    let ini = source.join("desktop.ini");
    std::fs::write(&ini, "[.ShellClassInfo]\r\nIconResource=shell32.dll,3\r\n").unwrap();
    let status = std::process::Command::new("attrib").args(["+h", "+s"]).arg(&ini).status().unwrap();
    assert!(status.success());
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");

    let sending = start_sender(&sender_home, vec![source], Some(1)).await;
    let (outcome, _) = receive_all(&receiver_home, &sending.ticket, &dest, false).await;
    assert!(matches!(outcome.unwrap(), ReceiveOutcome::Completed { .. }));
    let arrived = std::fs::metadata(dest.join("videos/desktop.ini")).unwrap().file_attributes();
    assert_eq!(arrived & (HIDDEN | SYSTEM), HIDDEN | SYSTEM, "attributes {arrived:#x}");
    let clip = std::fs::metadata(dest.join("videos/clip.bin")).unwrap().file_attributes();
    assert_eq!(clip & HIDDEN, 0, "an ordinary file came out hidden");
    let _ = tokio::time::timeout(WAIT, sending.task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_key_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");

    let sending = start_sender(&sender_home, vec![source], None).await;
    // Same sender, same addresses, but a key the sender never gave out.
    let mut forged = Ticket::decode(&sending.ticket).unwrap();
    forged.auth_key[0] ^= 0xFF;
    let (outcome, _) = receive_all(&receiver_home, &forged.encode(), &dest, false).await;
    let error = format!("{:#}", outcome.unwrap_err());
    assert!(error.contains("refused"), "unexpected error: {error}");
    assert!(snapshot(&dest).is_empty());

    sending.stop.cancel();
    let sent = tokio::time::timeout(WAIT, sending.task).await.unwrap().unwrap().unwrap();
    assert_eq!(sent, SendOutcome::Stopped { receivers: 0 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_receive_continues_from_where_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source/video.bin");
    fill(&source, 192 * 1024 * 1024, 7);
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");

    let sending = start_sender(&sender_home, vec![source.clone()], Some(1)).await;

    // First run: stop as soon as some of the file has arrived.
    let (tx, mut rx) = events::channel();
    let stop = CancellationToken::new();
    let first = tokio::spawn({
        let home = receiver_home.clone();
        let stop = stop.clone();
        let opts = ReceiveOptions { key: sending.ticket.clone(), dest: dest.clone(), existing: Existing::KeepBoth, answers: None, seed: false, net: offline() };
        async move { recv::receive(&home, opts, tx, stop).await }
    });
    let mut arrived = 0;
    while let Ok(Some(event)) = tokio::time::timeout(WAIT, rx.recv()).await {
        if let Event::Progress { have, .. } = event {
            if have > 8 * 1024 * 1024 {
                arrived = have;
                stop.cancel();
                break;
            }
        }
    }
    assert!(arrived > 0, "no progress was seen");
    let outcome = tokio::time::timeout(WAIT, first).await.unwrap().unwrap().unwrap();
    if outcome != ReceiveOutcome::Stopped {
        // The machine was fast enough to finish first; nothing left to resume.
        assert_eq!(std::fs::read(&source).unwrap(), std::fs::read(dest.join("video.bin")).unwrap());
        return;
    }
    assert!(!dest.join("video.bin").exists());

    // Second run, from the record alone: it must not start from zero.
    let record = receiver_home.list().unwrap().into_iter().find(|r| r.state == State::Active).expect("an active record");
    let (tx, mut rx) = events::channel();
    let second = tokio::spawn({
        let home = receiver_home.clone();
        async move { recv::resume_receive(&home, record, None, tx, CancellationToken::new()).await }
    });
    let mut first_progress = None;
    while let Ok(Some(event)) = tokio::time::timeout(WAIT, rx.recv()).await {
        if let Event::Progress { have, .. } = event {
            first_progress.get_or_insert(have);
        }
    }
    let outcome = tokio::time::timeout(WAIT, second).await.unwrap().unwrap().unwrap();
    assert!(matches!(outcome, ReceiveOutcome::Completed { .. }));
    assert!(first_progress.unwrap() >= 8 * 1024 * 1024, "resumed from {first_progress:?}");
    assert_eq!(std::fs::read(&source).unwrap(), std::fs::read(dest.join("video.bin")).unwrap());

    let sent = tokio::time::timeout(WAIT, sending.task).await.unwrap().unwrap().unwrap();
    assert_eq!(sent, SendOutcome::Completed { receivers: 1 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finished_files_are_in_place_before_the_end_and_after_a_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("source/fotos");
    for i in 0..40 {
        fill(&folder.join(format!("foto-{i:02}.bin")), 4 * 1024 * 1024, 100 + i);
    }
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");
    let sending = start_sender(&sender_home, vec![folder.clone()], Some(1)).await;

    // Stop when about half has arrived.
    let (tx, mut rx) = events::channel();
    let stop = CancellationToken::new();
    let first = tokio::spawn({
        let home = receiver_home.clone();
        let stop = stop.clone();
        let opts = ReceiveOptions { key: sending.ticket.clone(), dest: dest.clone(), existing: Existing::KeepBoth, answers: None, seed: false, net: offline() };
        async move { recv::receive(&home, opts, tx, stop).await }
    });
    while let Ok(Some(event)) = tokio::time::timeout(WAIT, rx.recv()).await {
        if let Event::Progress { have, total } = event {
            if have > total / 2 {
                stop.cancel();
                break;
            }
        }
    }
    let outcome = tokio::time::timeout(WAIT, first).await.unwrap().unwrap().unwrap();
    if outcome == ReceiveOutcome::Stopped {
        // What had arrived whole is already in its place, identical; nothing
        // half-written is there under a real name.
        let placed: Vec<_> = std::fs::read_dir(dest.join("fotos")).map(|d| d.flatten().collect()).unwrap_or_default();
        assert!(!placed.is_empty(), "no finished file was in place after the stop");
        assert!(placed.len() < 40, "everything was in place before the end");
        for entry in &placed {
            let original = folder.join(entry.file_name());
            assert_eq!(std::fs::read(entry.path()).unwrap(), std::fs::read(&original).unwrap(), "{:?}", entry.file_name());
        }
        // And the rest arrives when it continues.
        let record = receiver_home.list().unwrap().into_iter().find(|r| r.state == State::Active).expect("an active record");
        assert_eq!(record.saved.len(), placed.len(), "the record knows what is in place");
        let (tx, _rx) = events::channel();
        let outcome = recv::resume_receive(&receiver_home, record, None, tx, CancellationToken::new()).await.unwrap();
        assert!(matches!(outcome, ReceiveOutcome::Completed { .. }));
    }
    assert_eq!(snapshot(&folder), snapshot(&dest.join("fotos")));
    let sent = tokio::time::timeout(WAIT, sending.task).await.unwrap().unwrap().unwrap();
    assert_eq!(sent, SendOutcome::Completed { receivers: 1 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_already_there_do_not_travel() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let dest = tmp.path().join("dest");

    let sending = start_sender(&sender_home, vec![source.clone()], Some(1)).await;
    let (outcome, _) = receive_all(&receiver_home, &sending.ticket, &dest, false).await;
    outcome.unwrap();
    sending.task.await.unwrap().unwrap();

    // One file changes, one is new; the folder is sent again.
    std::fs::write(source.join("with space and ñ.txt"), "adiós, versión dos").unwrap();
    fill(&source.join("new.bin"), 70_000, 9);
    let sending = start_sender(&sender_home, vec![source.clone()], Some(1)).await;
    let (outcome, seen) = receive_all(&receiver_home, &sending.ticket, &dest, false).await;
    let kept_aside = match outcome.unwrap() {
        ReceiveOutcome::Completed { kept_aside, .. } => kept_aside,
        other => panic!("unexpected outcome {other:?}"),
    };
    sending.task.await.unwrap().unwrap();

    // The four non-empty unchanged files were recognised in place.
    let already: Vec<u64> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| if let Event::AlreadyHave { files, .. } = e { Some(*files) } else { None })
        .collect();
    assert_eq!(already, vec![4]);
    // Without --overwrite the old file stays and the new one sits beside it.
    assert_eq!(kept_aside.len(), 1);
    assert_eq!(std::fs::read_to_string(dest.join("photos/with space and ñ.txt")).unwrap(), "hola");
    assert_eq!(std::fs::read_to_string(dest.join("photos/with space and ñ (2).txt")).unwrap(), "adiós, versión dos");
    assert!(dest.join("photos/new.bin").exists());

    // With --overwrite the destination ends up equal to the source.
    std::fs::remove_file(dest.join("photos/with space and ñ (2).txt")).unwrap();
    let sending = start_sender(&sender_home, vec![source], Some(1)).await;
    let (outcome, _) = receive_all(&receiver_home, &sending.ticket, &dest, true).await;
    outcome.unwrap();
    sending.task.await.unwrap().unwrap();
    assert_eq!(snapshot(&tmp.path().join("source")), snapshot(&dest));
}

/// A folder to send, and a destination that already holds a different
/// `a.bin` and an identical `b.bin`.
fn taken_destination(tmp: &Path) -> (PathBuf, PathBuf) {
    let folder = tmp.join("source/fotos");
    for (i, name) in ["a.bin", "b.bin", "c.bin"].iter().enumerate() {
        fill(&folder.join(name), 3 * 1024 * 1024, 300 + i as u64);
    }
    let dest = tmp.join("dest");
    fill(&dest.join("fotos/a.bin"), 1024, 999);
    std::fs::copy(folder.join("b.bin"), dest.join("fotos/b.bin")).unwrap();
    (folder, dest)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_that_already_exist_are_asked_about_and_can_be_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, dest) = taken_destination(tmp.path());
    let old_a = std::fs::read(dest.join("fotos/a.bin")).unwrap();
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let sending = start_sender(&sender_home, vec![folder.clone()], Some(1)).await;

    let (tx, mut rx) = events::channel();
    let (ask, answers) = recv::answers();
    let receiving = tokio::spawn({
        let home = receiver_home.clone();
        let opts = ReceiveOptions {
            key: sending.ticket.clone(),
            dest: dest.clone(),
            existing: Existing::Ask,
            answers: Some(answers),
            seed: false,
            net: offline(),
        };
        async move { recv::receive(&home, opts, tx, CancellationToken::new()).await }
    });
    let mut asked = None;
    while let Ok(Some(event)) = tokio::time::timeout(WAIT, rx.recv()).await {
        if let Event::AlreadyExist { files, examples, .. } = event {
            asked = Some((files, examples));
            ask.send(Existing::Skip).unwrap();
        }
    }
    let outcome = tokio::time::timeout(WAIT, receiving).await.unwrap().unwrap().unwrap();
    // Only the different one is asked about; the identical one never counts.
    assert_eq!(asked, Some((1, vec!["fotos/a.bin".to_string()])));
    assert!(matches!(outcome, ReceiveOutcome::Completed { files: 2, .. }), "{outcome:?}");
    assert_eq!(std::fs::read(dest.join("fotos/a.bin")).unwrap(), old_a, "a skipped file was touched");
    assert_eq!(std::fs::read(dest.join("fotos/c.bin")).unwrap(), std::fs::read(folder.join("c.bin")).unwrap());
    assert!(!dest.join("fotos/a (2).bin").exists());
    // The answer is kept for this transfer.
    let record = receiver_home.list().unwrap().pop().unwrap();
    assert_eq!(record.existing, Some(Existing::Skip));
    let sent = tokio::time::timeout(WAIT, sending.task).await.unwrap().unwrap().unwrap();
    assert_eq!(sent, SendOutcome::Completed { receivers: 1 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_that_already_exist_can_be_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let (folder, dest) = taken_destination(tmp.path());
    let sender_home = Home::at(tmp.path().join("home-a")).unwrap();
    let receiver_home = Home::at(tmp.path().join("home-b")).unwrap();
    let sending = start_sender(&sender_home, vec![folder.clone()], Some(1)).await;
    let (outcome, _) = receive_all(&receiver_home, &sending.ticket, &dest, true).await;
    assert!(matches!(outcome.unwrap(), ReceiveOutcome::Completed { files: 3, .. }));
    assert_eq!(snapshot(&folder), snapshot(&dest.join("fotos")));
}