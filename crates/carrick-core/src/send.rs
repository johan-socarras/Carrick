//! Offering files: fingerprint them, publish the transfer, serve receivers.

use std::{
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{bail, ensure, Context, Result};
use iroh::protocol::Router;
use iroh_blobs::{
    api::{
        blobs::{AddBytesOptions, AddPathOptions, AddProgressItem, ImportMode},
        Store,
    },
    hashseq::HashSeq,
    store::fs::FsStore,
    BlobFormat, Hash, HashAndFormat,
};
use n0_future::{BufferedStreamExt, StreamExt};
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use crate::{
    access::{Code, SenderKeys, Ticket},
    events::{emit, Event, EventTx, Turn},
    gate::Gate,
    home::{decode_key, now_secs, Home, Record, Role, State},
    manifest::{manifest_path, FileEntry, Manifest, MANIFEST_FORMAT},
    net::{self, NetOptions, RelayChoice},
    proto::ALPN,
    running,
};

const ROOT_TAG: &str = "carrick-root";
/// Files fingerprinted at the same time. Kept low so a big send does not
/// take over the machine.
const HASH_PARALLELISM: usize = 2;

#[derive(Debug, Clone)]
pub struct SendOptions {
    /// Files and folders to send.
    pub paths: Vec<PathBuf>,
    /// Give a long ticket instead of a four-word code.
    pub ticket: bool,
    /// Stop once this many receivers have everything. `None`: serve until stopped.
    pub receivers: Option<u32>,
    pub net: NetOptions,
    /// When to be on the network, for a send that waits in line behind
    /// others. `None`: at once, and all the time.
    pub turn: Option<Turn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// The wanted number of receivers finished.
    Completed { receivers: u32 },
    /// Stopped from outside; the transfer stays resumable.
    Stopped { receivers: u32 },
}

struct Planned {
    absolute: PathBuf,
    entry: FileEntry,
}

/// Lists what the given paths contain, without reading file contents.
fn plan(paths: &[PathBuf]) -> Result<(String, Vec<Planned>, Vec<String>)> {
    ensure!(!paths.is_empty(), "nothing to send");
    let mut files = Vec::new();
    let mut empty_dirs = Vec::new();
    let mut top_names = std::collections::HashSet::new();
    for given in paths {
        let absolute = std::path::absolute(given)
            .with_context(|| format!("{} is not a usable path", given.display()))?;
        let meta = std::fs::symlink_metadata(&absolute)
            .with_context(|| format!("{} does not exist", given.display()))?;
        let top = absolute
            .file_name()
            .with_context(|| format!("{} has no name to send it under", given.display()))?
            .to_owned();
        ensure!(
            top_names.insert(top.to_string_lossy().to_lowercase()),
            "two of the things to send are both called {:?}",
            top
        );
        let parent = absolute.parent().unwrap_or(Path::new("")).to_path_buf();
        if meta.is_file() {
            files.push(planned(&absolute, &parent, &meta)?);
            continue;
        }
        ensure!(meta.is_dir(), "{} is neither a file nor a folder", given.display());
        for entry in WalkDir::new(&absolute).follow_links(false) {
            let entry = entry.with_context(|| format!("reading {}", absolute.display()))?;
            let kind = entry.file_type();
            if kind.is_file() {
                let meta = entry.metadata()?;
                files.push(planned(entry.path(), &parent, &meta)?);
            } else if kind.is_dir() {
                let is_empty = std::fs::read_dir(entry.path())
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(false);
                if is_empty {
                    let relative = entry.path().strip_prefix(&parent)?;
                    empty_dirs.push(manifest_path(relative)?);
                }
            }
            // Symbolic links and junctions are not followed and not sent.
        }
    }
    files.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    empty_dirs.sort();
    let name = if paths.len() == 1 {
        files
            .first()
            .map(|f| f.entry.path.split('/').next().unwrap_or_default().to_string())
            .or_else(|| empty_dirs.first().map(|d| d.split('/').next().unwrap_or_default().to_string()))
            .unwrap_or_default()
    } else {
        format!("{} items", paths.len())
    };
    ensure!(
        !files.is_empty() || !empty_dirs.is_empty(),
        "there is nothing to send in what you chose"
    );
    Ok((name, files, empty_dirs))
}

fn planned(path: &Path, parent: &Path, meta: &std::fs::Metadata) -> Result<Planned> {
    let relative = path.strip_prefix(parent)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let mode = None;
    Ok(Planned {
        absolute: path.to_path_buf(),
        entry: FileEntry {
            path: manifest_path(relative)?,
            size: meta.len(),
            mtime,
            mode,
        },
    })
}

/// Fingerprints every file in place and stores the transfer's root.
async fn import(store: &Store, name: String, files: Vec<Planned>, empty_dirs: Vec<String>, events: &EventTx) -> Result<(Hash, Manifest)> {
    let total: u64 = files.iter().map(|f| f.entry.size).sum();
    emit(events, Event::Hashing { done: 0, total });
    let done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    let mut results = n0_future::stream::iter(files.into_iter().enumerate())
        .map(|(index, file)| {
            let store = store.clone();
            let events = events.clone();
            let done = done.clone();
            async move {
                let mut stream = store
                    .add_path_with_opts(AddPathOptions {
                        path: file.absolute.clone(),
                        mode: ImportMode::TryReference,
                        format: BlobFormat::Raw,
                    })
                    .stream()
                    .await;
                let mut last = 0u64;
                loop {
                    let item = stream.next().await.with_context(|| {
                        format!("fingerprinting {} stopped early", file.absolute.display())
                    })?;
                    match item {
                        AddProgressItem::OutboardProgress(offset) => {
                            let delta = offset.saturating_sub(last);
                            last = offset;
                            let now = done.fetch_add(delta, std::sync::atomic::Ordering::Relaxed) + delta;
                            emit(&events, Event::Hashing { done: now, total });
                        }
                        AddProgressItem::Error(cause) => {
                            bail!("could not read {}: {cause}", file.absolute.display());
                        }
                        AddProgressItem::Done(tag) => {
                            let delta = file.entry.size.saturating_sub(last);
                            let now = done.fetch_add(delta, std::sync::atomic::Ordering::Relaxed) + delta;
                            emit(&events, Event::Hashing { done: now, total });
                            return anyhow::Ok((index, file.entry, tag));
                        }
                        _ => {}
                    }
                }
            }
        })
        .buffered_unordered(HASH_PARALLELISM)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    results.sort_by_key(|(index, _, _)| *index);

    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        name,
        files: results.iter().map(|(_, entry, _)| entry.clone()).collect(),
        empty_dirs,
    };
    let manifest_tag = store.add_bytes(manifest.to_bytes()).temp_tag().await?;
    let sequence: HashSeq = std::iter::once(manifest_tag.hash())
        .chain(results.iter().map(|(_, _, tag)| tag.hash()))
        .collect();
    let root_tag = store
        .add_bytes_with_opts(AddBytesOptions {
            data: sequence.into(),
            format: BlobFormat::HashSeq,
        })
        .temp_tag()
        .await?;
    let root = root_tag.hash();
    // A named tag keeps the transfer in the store across restarts.
    store.tags().set(ROOT_TAG, HashAndFormat::hash_seq(root)).await?;
    store.sync_db().await?;
    Ok((root, manifest))
}

/// The hash of a transfer's file list: the first entry of its root.
pub(crate) async fn first_child(store: &Store, root: Hash) -> Result<Hash> {
    let bytes = store.get_bytes(root).await.context("the transfer's root is missing from the store")?;
    let sequence = HashSeq::try_from(bytes).map_err(|e| anyhow::anyhow!("damaged transfer root: {e}"))?;
    let first = sequence.iter().next().context("the transfer is empty")?;
    Ok(first)
}

/// Reads the manifest of a transfer already in the store.
pub(crate) async fn load_listing(store: &Store, root: Hash) -> Result<(Vec<Hash>, Manifest)> {
    let bytes = store.get_bytes(root).await.context("the transfer's root is missing from the store")?;
    let sequence = HashSeq::try_from(bytes).map_err(|e| anyhow::anyhow!("damaged transfer root: {e}"))?;
    let hashes: Vec<Hash> = sequence.iter().collect();
    let manifest_hash = *hashes.first().context("the transfer is empty")?;
    let manifest_bytes = store.get_bytes(manifest_hash).await.context("the file list is missing from the store")?;
    let manifest = Manifest::from_bytes(&manifest_bytes)?;
    ensure!(
        hashes.len() == manifest.files.len() + 1,
        "the file list and the transfer do not match"
    );
    Ok((hashes, manifest))
}

/// Checks that what was fingerprinted is still what is on disk.
fn check_unchanged(record: &Record, manifest: &Manifest) -> Result<()> {
    let mut by_path = std::collections::HashMap::new();
    for source in &record.sources {
        let parent = source.parent().unwrap_or(Path::new("")).to_path_buf();
        if let Some(top) = source.file_name().and_then(|n| n.to_str()) {
            by_path.insert(top.to_string(), parent);
        }
    }
    for file in &manifest.files {
        let top = file.path.split('/').next().unwrap_or_default();
        let Some(parent) = by_path.get(top) else {
            bail!("the record of this transfer no longer matches its file list");
        };
        let mut path = parent.clone();
        for part in file.path.split('/') {
            path.push(part);
        }
        let meta = std::fs::metadata(&path)
            .with_context(|| format!("{} is gone; send it again to make a new transfer", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        if meta.len() != file.size || mtime != file.mtime {
            bail!(
                "{} changed since this transfer was prepared; send it again to make a new transfer",
                path.display()
            );
        }
    }
    Ok(())
}

fn new_id() -> String {
    use rand::RngExt;
    let bytes: [u8; 5] = rand::rng().random();
    hex::encode(bytes)
}

/// Sends files: prepares a new transfer and serves it.
pub async fn send(home: &Home, opts: SendOptions, events: EventTx, stop: CancellationToken) -> Result<SendOutcome> {
    let (name, files, empty_dirs) = plan(&opts.paths)?;

    let (code, keys) = if opts.ticket {
        (None, SenderKeys::random())
    } else {
        let code = Code::generate();
        let for_keys = code.clone();
        let keys = tokio::task::spawn_blocking(move || SenderKeys::from_code(&for_keys)).await??;
        (Some(code), keys)
    };

    let id = new_id();
    let claim = home.claim(&id)?;
    let (events, watching) = running::watch(home, &id, events);
    let store_dir = home.transfer_dir(&id).join("store");
    let mut record = Record {
        id: id.clone(),
        role: Role::Send,
        state: State::Active,
        created: now_secs(),
        updated: 0,
        code: code.as_ref().map(|c| c.to_string()),
        key: None,
        endpoint_secret: hex::encode(keys.endpoint_secret),
        auth_key: hex::encode(keys.auth_key),
        root: None,
        name: name.clone(),
        total_bytes: files.iter().map(|f| f.entry.size).sum(),
        files: files.len() as u64,
        sources: opts
            .paths
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .collect(),
        dest: None,
        store: store_dir.clone(),
        relay: opts.net.relay.to_setting(),
        receivers_wanted: opts.receivers,
        finished_receivers: Vec::new(),
        overwrite: false,
        existing: None,
        saved: Vec::new(),
    };
    home.save(&mut record)?;

    tokio::fs::create_dir_all(&store_dir).await?;
    let store = FsStore::load(&store_dir).await.map_err(|e| anyhow::anyhow!("open store: {e}"))?;
    let prepared = tokio::select! {
        res = import(&store, name, files, empty_dirs, &events) => res,
        _ = stop.cancelled() => {
            store.shutdown().await.ok();
            drop(claim);
            home.remove(&id)?;
            return Ok(SendOutcome::Stopped { receivers: 0 });
        }
    };
    let (root, manifest) = match prepared {
        Ok(v) => v,
        Err(e) => {
            store.shutdown().await.ok();
            drop(claim);
            home.remove(&id)?;
            return Err(e);
        }
    };
    record.root = Some(root.to_hex().to_string());
    home.save(&mut record)?;

    let outcome = serve(home, record, store, root, manifest, opts.net, opts.turn, events, stop).await;
    if matches!(outcome, Ok(SendOutcome::Completed { .. })) {
        watching.remember(home, &id);
    }
    drop(claim);
    outcome
}

/// Serves again a transfer prepared earlier, e.g. after a restart.
pub async fn resume_send(home: &Home, record: Record, turn: Option<Turn>, events: EventTx, stop: CancellationToken) -> Result<SendOutcome> {
    ensure!(record.role == Role::Send, "transfer {} is not a send", record.id);
    let _claim = home.claim(&record.id)?;
    let (events, watching) = running::watch(home, &record.id, events);
    let id = record.id.clone();
    let root_hex = record
        .root
        .clone()
        .with_context(|| format!("transfer {} was interrupted while preparing; send the files again", record.id))?;
    let root: Hash = root_hex.parse().map_err(|_| anyhow::anyhow!("damaged root in transfer record"))?;
    let store = FsStore::load(&record.store).await.map_err(|e| anyhow::anyhow!("open store: {e}"))?;
    let listing = load_listing(&store, root).await;
    let (_, manifest) = match listing.and_then(|l| check_unchanged(&record, &l.1).map(|_| l)) {
        Ok(v) => v,
        Err(e) => {
            store.shutdown().await.ok();
            return Err(e);
        }
    };
    let net = NetOptions {
        relay: RelayChoice::from_setting(record.relay.as_deref())?,
        ..NetOptions::from_env()?
    };
    let outcome = serve(home, record, store, root, manifest, net, turn, events, stop).await;
    if matches!(outcome, Ok(SendOutcome::Completed { .. })) {
        watching.remember(home, &id);
    }
    outcome
}

fn has_turn(turn: &Option<Turn>) -> bool {
    turn.as_ref().is_none_or(|t| *t.borrow())
}

/// Waits until it is this send's turn. False if it was stopped first.
async fn wait_for_turn(turn: &mut Option<Turn>, stop: &CancellationToken) -> bool {
    let Some(t) = turn else { return true };
    tokio::select! {
        // Whoever kept the line is gone: nothing holds this send back.
        _ = t.wait_for(|go| *go) => true,
        _ = stop.cancelled() => false,
    }
}

/// Resolves when this send must give way to another.
async fn turn_taken(turn: &mut Option<Turn>) {
    match turn {
        Some(t) => {
            if t.wait_for(|go| !*go).await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// Adds the receivers that finished to the record, saving it if any is new.
fn note_finished(home: &Home, record: &mut Record, gate: &Gate) -> Result<()> {
    let mut changed = false;
    for id in gate.finished() {
        let id = id.to_string();
        if !record.finished_receivers.contains(&id) {
            record.finished_receivers.push(id);
            changed = true;
        }
    }
    if changed {
        home.save(record)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn serve(
    home: &Home,
    mut record: Record,
    store: FsStore,
    root: Hash,
    manifest: Manifest,
    net_opts: NetOptions,
    mut turn: Option<Turn>,
    events: EventTx,
    stop: CancellationToken,
) -> Result<SendOutcome> {
    let keys = SenderKeys {
        endpoint_secret: decode_key(&record.endpoint_secret)?,
        auth_key: decode_key(&record.auth_key)?,
    };
    let mut been_on = false;
    // Each round puts the transfer on the network until it ends, or until
    // another send is let go first; then it waits and comes back.
    let outcome = loop {
        if !has_turn(&turn) {
            emit(
                &events,
                if been_on {
                    Event::Paused
                } else {
                    Event::Queued {
                        id: record.id.clone(),
                        code: record.code.clone(),
                        name: manifest.name.clone(),
                        files: manifest.files.len() as u64,
                        total: manifest.total_bytes(),
                    }
                },
            );
            if !wait_for_turn(&mut turn, &stop).await {
                break SendOutcome::Stopped { receivers: record.finished_receivers.len() as u32 };
            }
        }
        been_on = true;
        let (endpoint, _book) = match net::bind(keys.secret_key(), &net_opts).await {
            Ok(v) => v,
            Err(e) => {
                store.shutdown().await.ok();
                return Err(e);
            }
        };
        if let Some(note) = net::port_note(&endpoint, &net_opts) {
            emit(&events, Event::Note(note));
        }
        let online = net::wait_online(&endpoint, &net_opts, Duration::from_secs(20)).await;
        if !online {
            emit(&events, Event::Note("no relay reachable yet; receivers on this network can still connect".into()));
        }
        let gate = Gate::new(endpoint.id(), (*store).clone(), keys.auth_key, *root.as_bytes(), true, events.clone());
        let router = Router::builder(endpoint.clone()).accept(ALPN, gate.clone()).spawn();

        let ticket = if record.code.is_none() {
            // A ticket is only useful with an address in it; give the endpoint a
            // moment to learn its own.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while endpoint.addr().is_empty() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let addr = endpoint.addr();
            let relay = addr.relay_urls().next().map(|u| u.to_string());
            let mut addrs: Vec<std::net::SocketAddr> = addr.ip_addrs().copied().collect();
            // The public address given by hand goes first: it is the one that
            // works from outside when the router forwards the port.
            for public in net_opts.public_addrs.iter().rev() {
                addrs.retain(|a| a != public);
                addrs.insert(0, *public);
            }
            let ticket = Ticket {
                sender: *endpoint.id().as_bytes(),
                auth_key: keys.auth_key,
                root: *root.as_bytes(),
                relay,
                addrs,
            };
            Some(ticket.encode())
        } else {
            None
        };
        emit(
            &events,
            Event::Ready {
                id: record.id.clone(),
                code: record.code.clone(),
                ticket,
                name: manifest.name.clone(),
                files: manifest.files.len() as u64,
                total: manifest.total_bytes(),
            },
        );

        let mut done_watch = gate.watch_done();
        let round = loop {
            // Remember who finished, so a restart does not count them twice.
            note_finished(home, &mut record, &gate)?;
            let count = record.finished_receivers.len() as u32;
            if let Some(wanted) = record.receivers_wanted {
                if count >= wanted {
                    break Some(SendOutcome::Completed { receivers: count });
                }
            }
            tokio::select! {
                _ = stop.cancelled() => break Some(SendOutcome::Stopped { receivers: count }),
                _ = turn_taken(&mut turn) => break None,
                res = done_watch.changed() => {
                    if res.is_err() {
                        break Some(SendOutcome::Stopped { receivers: count });
                    }
                }
            }
        };
        // Look once more at who finished: one may have just now.
        note_finished(home, &mut record, &gate)?;
        if matches!(round, Some(SendOutcome::Completed { .. })) {
            // Let the last receiver's final messages drain before closing.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), router.shutdown()).await;
        // Let go of the port too, so the next round can take the same one.
        let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.close()).await;
        match round {
            Some(outcome) => break outcome,
            None => continue,
        }
    };
    // Offered to anyone until stopped, and someone got everything: stopping
    // ends it. Left unfinished, it would be offered again for nobody.
    let outcome = match outcome {
        SendOutcome::Stopped { .. } => {
            let receivers = record.finished_receivers.len() as u32;
            if receivers > 0 && record.receivers_wanted.is_none() {
                SendOutcome::Completed { receivers }
            } else {
                SendOutcome::Stopped { receivers }
            }
        }
        other => other,
    };

    if matches!(outcome, SendOutcome::Completed { .. }) {
        record.state = State::Done;
        home.save(&mut record)?;
    }
    store.shutdown().await.ok();
    if record.state == State::Done {
        // The fingerprints are of no use once the transfer is over.
        let _ = tokio::fs::remove_dir_all(&record.store).await;
    }
    Ok(outcome)
}
