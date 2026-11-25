//! Receiving a transfer, and never giving up on it.
//!
//! The receiver keeps everything it has verified in a store next to the
//! destination. Each attempt connects, proves it holds the key, asks only for
//! what is missing and writes what arrives. Any failure short of a refusal is
//! followed by another attempt, for as long as it takes.
//!
//! While it downloads, a receiver also serves what it already holds to the
//! other receivers of the same transfer, and fetches from them what they
//! hold, so the sender does not have to upload everything to everyone.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use iroh::{
    address_lookup::MemoryLookup,
    endpoint::{Connection, RecvStream, SendStream},
    protocol::Router,
    Endpoint, EndpointId, SecretKey,
};
use iroh_blobs::{
    api::{
        blobs::{AddPathOptions, BlobStatus, ExportMode, ExportOptions, ImportMode},
        downloader::{ContentDiscovery, DownloadOptions, DownloadProgressItem, Downloader, SplitStrategy},
        remote::GetProgressItem,
        Store,
    },
    protocol::{ChunkRanges, ChunkRangesExt, GetRequest},
    store::fs::FsStore,
    util::connection_pool,
    BlobFormat, Hash, HashAndFormat,
};
use n0_future::{task::AbortOnDropHandle, StreamExt};
use rand::seq::SliceRandom;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    access::Access,
    events::{emit, Event, EventTx},
    gate::Gate,
    home::{decode_key, now_secs, Existing, Home, Record, Role, State},
    manifest::{local_path, FileEntry, Manifest},
    net::{self, NetOptions, RelayChoice},
    proto::{self, ClientMsg, Hello, Peer, Reply, ServerMsg, Welcome, ALPN},
    running,
    send::{first_child, load_listing},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// With no byte arriving for this long, the connection is considered stuck.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const BACKOFF_STEPS: [u64; 7] = [1, 2, 4, 8, 15, 30, 60];
/// Room left free on the destination disk beyond what the transfer needs.
const SPACE_MARGIN: u64 = 64 * 1024 * 1024;
/// Files smaller than this are always asked of the sender: asking another
/// receiver first costs a round trip that a small file does not repay.
const PEER_MIN_SIZE: u64 = 1024 * 1024;
/// The most a sender's list of pieces may weigh: 32 bytes a file, so about a
/// million files.
const MAX_LIST_BYTES: u64 = 32 * 1_000_001;
/// The most a sender's file list (names, sizes, times) may weigh.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub struct ReceiveOptions {
    /// The four-word code or the ticket.
    pub key: String,
    /// Folder to put the files in.
    pub dest: PathBuf,
    /// What to do with incoming files whose place is taken by a different
    /// file. `Existing::Ask` asks through `answers`; with nobody to answer,
    /// both are kept.
    pub existing: Existing,
    /// Where the answer comes from when the receive asks.
    pub answers: Option<Answers>,
    /// After finishing, keep passing pieces to other receivers until stopped.
    pub seed: bool,
    pub net: NetOptions,
}

/// The answers of the person to the receive's question about files that
/// already exist (`Event::AlreadyExist`). `Existing::Ask` is not an answer.
pub type Answers = tokio::sync::mpsc::UnboundedReceiver<Existing>;
pub type AnswerTx = tokio::sync::mpsc::UnboundedSender<Existing>;

pub fn answers() -> (AnswerTx, Answers) {
    tokio::sync::mpsc::unbounded_channel()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveOutcome {
    Completed {
        files: u64,
        bytes: u64,
        dest: PathBuf,
        /// Incoming files saved under another name because a different file
        /// was already there: (path in the transfer, name used).
        kept_aside: Vec<(String, PathBuf)>,
    },
    /// Stopped from outside; `carrick resume` continues it.
    Stopped,
}

/// Why an attempt ended without finishing.
enum Stop {
    /// Trying again cannot help.
    Fatal(anyhow::Error),
    /// Worth another attempt.
    Retry(anyhow::Error),
}

fn retry(e: impl Into<anyhow::Error>) -> Stop {
    Stop::Retry(e.into())
}

fn fatal(e: impl Into<anyhow::Error>) -> Stop {
    Stop::Fatal(e.into())
}

/// Starts receiving, or continues if this transfer was already under way.
pub async fn receive(home: &Home, opts: ReceiveOptions, events: EventTx, stop: CancellationToken) -> Result<ReceiveOutcome> {
    let key = opts.key.clone();
    let access = tokio::task::spawn_blocking(move || Access::parse(&key)).await??;
    let id = format!("r{}", &access.sender.to_string()[..15]);
    let dest = std::path::absolute(&opts.dest)
        .with_context(|| format!("{} is not a usable folder", opts.dest.display()))?;

    let claim;
    let record = match home.load(&id)? {
        Some(existing) if existing.state == State::Active => {
            // Running elsewhere on this computer: say so, whatever the folder.
            claim = home.claim(&id)?;
            let previous = existing.dest.clone().unwrap_or_default();
            if previous != dest {
                bail!(
                    "this transfer is already being received into {}; run `carrick resume` to continue it, or `carrick cancel {}` to start over",
                    previous.display(),
                    existing.id
                );
            }
            existing
        }
        other => {
            if other.is_some() {
                home.remove(&id)?;
            }
            claim = home.claim(&id)?;
            use rand::RngExt;
            let secret: [u8; 32] = rand::rng().random();
            let mut record = Record {
                id: id.clone(),
                role: Role::Receive,
                state: State::Active,
                created: now_secs(),
                updated: 0,
                code: None,
                key: Some(opts.key.trim().to_string()),
                endpoint_secret: hex::encode(secret),
                auth_key: hex::encode(access.auth_key),
                root: access.root.map(hex::encode),
                name: String::new(),
                total_bytes: 0,
                files: 0,
                sources: Vec::new(),
                dest: Some(dest.clone()),
                store: dest.join(format!(".carrick-{id}")),
                relay: opts.net.relay.to_setting(),
                receivers_wanted: None,
                finished_receivers: Vec::new(),
                overwrite: opts.existing == Existing::Replace,
                existing: Some(opts.existing),
                saved: Vec::new(),
            };
            home.save(&mut record)?;
            record
        }
    };
    let (events, watching) = running::watch(home, &id, events);
    let outcome = run(home, record, access, opts.net, opts.seed, opts.answers, events, stop).await;
    if matches!(outcome, Ok(ReceiveOutcome::Completed { .. })) {
        watching.remember(home, &id);
    }
    drop(claim);
    outcome
}

/// Continues a receive from its record, e.g. after a restart.
pub async fn resume_receive(home: &Home, record: Record, answers: Option<Answers>, events: EventTx, stop: CancellationToken) -> Result<ReceiveOutcome> {
    anyhow::ensure!(record.role == Role::Receive, "transfer {} is not a receive", record.id);
    let _claim = home.claim(&record.id)?;
    let (events, watching) = running::watch(home, &record.id, events);
    let id = record.id.clone();
    let key = record.key.clone().context("the transfer record has no code or ticket")?;
    let access = tokio::task::spawn_blocking(move || Access::parse(&key)).await??;
    let net = NetOptions {
        relay: RelayChoice::from_setting(record.relay.as_deref())?,
        ..NetOptions::from_env()?
    };
    let outcome = run(home, record, access, net, false, answers, events, stop).await;
    if matches!(outcome, Ok(ReceiveOutcome::Completed { .. })) {
        watching.remember(home, &id);
    }
    outcome
}

/// The other receivers of this transfer that can be asked for pieces.
type Peers = Arc<Mutex<BTreeSet<EndpointId>>>;

struct Session<'a> {
    home: &'a Home,
    record: Record,
    access: Access,
    net: NetOptions,
    endpoint: Endpoint,
    book: MemoryLookup,
    store: Store,
    dest: PathBuf,
    events: EventTx,
    /// Set whenever verified bytes arrive; read by the retry loop.
    progressed: Arc<AtomicBool>,
    /// Destination files already known to match the transfer, by index.
    at_dest: HashSet<usize>,
    /// Files this transfer has put in their place, by index (also kept in
    /// the record, so a restart trusts them without reading them again).
    saved: HashSet<usize>,
    /// When the record was last written because of `saved`.
    saved_written: std::time::Instant,
    scanned: bool,
    kept_aside: Vec<(String, PathBuf)>,
    /// Where the person's answer comes from, when there is someone to ask.
    answers: Option<Answers>,
    /// Files left alone because a different file holds their place and the
    /// answer was to skip them, by index; they are not fetched either.
    skip: HashSet<usize>,
    peers: Peers,
    /// Peers already mentioned to the person.
    helpers: HashSet<EndpointId>,
    /// Serves our store to other receivers once the root is known.
    router: Option<Router>,
    downloader: Option<Downloader>,
}

#[allow(clippy::too_many_arguments)]
async fn run(home: &Home, record: Record, access: Access, net_opts: NetOptions, seed: bool, answers: Option<Answers>, events: EventTx, stop: CancellationToken) -> Result<ReceiveOutcome> {
    let dest = record.dest.clone().context("the transfer record has no destination")?;
    tokio::fs::create_dir_all(&dest)
        .await
        .with_context(|| format!("cannot create {}", dest.display()))?;
    tokio::fs::create_dir_all(&record.store).await?;
    hide(&record.store);
    let fs_store = FsStore::load(&record.store).await.map_err(|e| anyhow!("open store: {e}"))?;
    let secret = SecretKey::from_bytes(&decode_key(&record.endpoint_secret)?);
    let (endpoint, book) = match net::bind(secret, &net_opts).await {
        Ok(e) => e,
        Err(e) => {
            fs_store.shutdown().await.ok();
            return Err(e);
        }
    };
    if let Some(note) = net::port_note(&endpoint, &net_opts) {
        emit(&events, Event::Note(note));
    }
    // A ticket carries the sender's addresses; a code relies on lookup.
    book.add_endpoint_info(access.endpoint_addr());

    let record_saved: HashSet<usize> = record.saved.iter().map(|i| *i as usize).collect();
    let mut session = Session {
        home,
        record,
        access,
        net: net_opts,
        endpoint: endpoint.clone(),
        book,
        store: (*fs_store).clone(),
        dest,
        events: events.clone(),
        progressed: Arc::new(AtomicBool::new(false)),
        at_dest: HashSet::new(),
        saved: record_saved,
        saved_written: std::time::Instant::now(),
        scanned: false,
        kept_aside: Vec::new(),
        answers,
        skip: HashSet::new(),
        peers: Arc::new(Mutex::new(BTreeSet::new())),
        helpers: HashSet::new(),
        router: None,
        downloader: None,
    };

    let mut attempt = 0u32;
    let mut step = 0usize;
    let mut idle_attempts = 0u32;
    let result = loop {
        attempt += 1;
        emit(&events, Event::Connecting { attempt });
        let res = tokio::select! {
            res = session.attempt() => res,
            _ = stop.cancelled() => break Ok(ReceiveOutcome::Stopped),
        };
        match res {
            Ok(outcome) => break Ok(outcome),
            Err(Stop::Fatal(e)) => break Err(e),
            Err(Stop::Retry(e)) => {
                if session.progressed.swap(false, Ordering::Relaxed) {
                    step = 0;
                    idle_attempts = 0;
                } else {
                    idle_attempts += 1;
                }
                let seconds = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
                step += 1;
                let mut reason = format!("{e:#}");
                if idle_attempts == 5 {
                    reason.push_str(" (still nothing after several tries: check that the sender is running and the code is right)");
                }
                emit(&events, Event::Waiting { reason, seconds });
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
                    _ = stop.cancelled() => break Ok(ReceiveOutcome::Stopped),
                }
            }
        }
    };

    let completed = matches!(result, Ok(ReceiveOutcome::Completed { .. }));
    if completed {
        session.record.state = State::Done;
        home.save(&mut session.record)?;
    } else {
        // Stopped or failed: note which files are already in place.
        let _ = home.save(&mut session.record);
    }
    if completed {
        if seed && session.router.is_some() {
            emit(&events, Event::Note("Everything is here. Still passing pieces to other receivers; Ctrl+C stops.".into()));
            stop.cancelled().await;
        }
    }
    drop(session.downloader.take());
    if let Some(router) = session.router.take() {
        let _ = tokio::time::timeout(Duration::from_secs(5), router.shutdown()).await;
    }
    endpoint.close().await;
    fs_store.shutdown().await.ok();
    if completed {
        // Everything was moved into place; the store is no longer needed.
        let _ = tokio::fs::remove_dir_all(&session.record.store).await;
    }
    result
}

/// Opens the control stream of a connection and proves we hold the key.
async fn handshake(endpoint: &Endpoint, conn: &Connection, auth_key: &[u8; 32], serves: bool) -> Result<(SendStream, RecvStream, Welcome), Stop> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| retry(anyhow!("connection lost: {e}")))?;
    let hello = Hello {
        version: proto::VERSION,
        token: proto::auth_token(auth_key, &endpoint.id(), &conn.remote_id()),
        serves,
    };
    proto::write_frame(&mut send, &hello).await.map_err(retry)?;
    let reply: Reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, proto::read_frame(&mut recv))
        .await
        .map_err(|_| retry(anyhow!("the other side did not answer")))?
        .map_err(retry)?;
    match reply {
        Reply::Welcome(welcome) => Ok((send, recv, welcome)),
        Reply::Reject(reason) => Err(fatal(anyhow!("the connection was refused: {reason}"))),
    }
}

/// Who to ask for a given blob: other receivers first, the sender last.
#[derive(Debug, Clone)]
struct Sources {
    sender: EndpointId,
    peers: Peers,
    sizes: Arc<HashMap<Hash, u64>>,
}

impl ContentDiscovery for Sources {
    fn find_providers(&self, content: HashAndFormat) -> n0_future::stream::Boxed<EndpointId> {
        let mut order = Vec::new();
        if self.sizes.get(&content.hash).copied().unwrap_or(0) >= PEER_MIN_SIZE {
            let mut peers: Vec<EndpointId> = self.peers.lock().expect("lock").iter().copied().collect();
            peers.shuffle(&mut rand::rng());
            order.extend(peers);
        }
        order.push(self.sender);
        Box::pin(n0_future::stream::iter(order))
    }
}

impl Session<'_> {
    async fn attempt(&mut self) -> Result<ReceiveOutcome, Stop> {
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(self.access.endpoint_addr(), ALPN))
            .await
            .map_err(|_| retry(anyhow!("could not reach the sender")))?
            .map_err(|e| {
                let text = e.to_string();
                if text.contains("No addressing information") {
                    retry(anyhow!("no sender found for this code yet"))
                } else {
                    retry(anyhow!("could not reach the sender: {text}"))
                }
            })?;

        let (mut send, recv, welcome) = handshake(&self.endpoint, &conn, &self.access.auth_key, true).await?;
        let root = Hash::from_bytes(welcome.root);
        if let Some(expected) = self.access.root {
            if expected != welcome.root {
                return Err(fatal(anyhow!("the sender offers different content than this ticket names")));
            }
        }
        let root_hex = root.to_hex().to_string();
        match &self.record.root {
            Some(known) if *known != root_hex => {
                return Err(fatal(anyhow!(
                    "the sender now offers different content under this code; run `carrick cancel {}` and receive again",
                    self.record.id
                )));
            }
            Some(_) => {}
            None => {
                self.record.root = Some(root_hex);
                self.home.save(&mut self.record).map_err(fatal)?;
            }
        }

        let _route = {
            let events = self.events.clone();
            net::watch_route(conn.clone(), move |route| emit(&events, Event::Connected { route }))
        };

        // Other receivers may ask us for pieces from now on, and the sender
        // is told where to find us.
        if self.router.is_none() {
            let gate = Gate::new(self.endpoint.id(), self.store.clone(), self.access.auth_key, welcome.root, false, self.events.clone());
            self.router = Some(Router::builder(self.endpoint.clone()).accept(ALPN, gate).spawn());
        }
        set_peers(&self.peers, &self.book, &self.endpoint.id(), &welcome.peers);
        net::wait_online(&self.endpoint, &self.net, Duration::from_secs(3)).await;
        let me = Peer::from_addr(&self.endpoint.addr());
        proto::write_frame(&mut send, &ClientMsg::Reachable(me)).await.map_err(retry)?;
        let _peer_updates = AbortOnDropHandle::new(tokio::spawn(read_peer_updates(
            recv,
            self.peers.clone(),
            self.book.clone(),
            self.endpoint.id(),
        )));

        // The list of hashes and the manifest come first. A sender is not
        // trusted with the receiver's memory: their sizes are learned from
        // their verified last chunk, and checked, before either is fetched.
        self.fetch(&conn, GetRequest::builder().root(ChunkRanges::last_chunk()).build(root)).await?;
        self.check_size(root, MAX_LIST_BYTES, "list of files").await?;
        self.fetch(&conn, GetRequest::builder().root(ChunkRanges::all()).child(0, ChunkRanges::last_chunk()).build(root))
            .await?;
        let manifest_hash = first_child(&self.store, root).await.map_err(fatal)?;
        self.check_size(manifest_hash, MAX_MANIFEST_BYTES, "file list").await?;
        let head = GetRequest::builder()
            .root(ChunkRanges::all())
            .child(0, ChunkRanges::all())
            .build(root);
        self.fetch(&conn, head).await?;
        let (hashes, manifest) = load_listing(&self.store, root).await.map_err(fatal)?;
        let total = manifest.total_bytes();
        if self.record.name != manifest.name || self.record.total_bytes != total {
            self.record.name = manifest.name.clone();
            self.record.total_bytes = total;
            self.record.files = manifest.files.len() as u64;
            self.home.save(&mut self.record).map_err(fatal)?;
        }
        emit(&self.events, Event::Listing { name: manifest.name.clone(), files: manifest.files.len() as u64, total });

        if !self.scanned {
            self.adopt_existing(&hashes, &manifest).await.map_err(fatal)?;
            self.scanned = true;
        }
        self.decide_existing(&manifest).await.map_err(fatal)?;
        // Files skipped do not count: the transfer is done without them.
        let skipped: u64 = self.skip.iter().map(|i| manifest.files[*i].size).sum();
        let total = total.saturating_sub(skipped);

        // What is still missing, and whether it fits.
        let have = self.local_file_bytes(&hashes).await.map_err(fatal)?.min(total);
        let missing = total.saturating_sub(have);
        if missing > 0 {
            if let Ok(free) = fs4::available_space(&self.dest) {
                if free < missing.saturating_add(SPACE_MARGIN) {
                    return Err(fatal(anyhow!(
                        "not enough room in {}: {} more are needed and {} are free",
                        self.dest.display(),
                        human(missing),
                        human(free)
                    )));
                }
            }
        }

        // Tell the sender how far along we are, on the control stream.
        let (progress_tx, progress_rx) = watch::channel((have, total, false));
        let reporter = AbortOnDropHandle::new(tokio::spawn(report_progress(send, progress_rx)));
        emit(&self.events, Event::Progress { have, total });

        self.download(root, &hashes, &manifest, have, total, &progress_tx).await?;
        emit(&self.events, Event::Progress { have: total, total });

        self.export(&hashes, &manifest).await.map_err(fatal)?;

        // Let the sender know before leaving.
        let _ = progress_tx.send((total, total, true));
        let _ = tokio::time::timeout(Duration::from_secs(5), reporter).await;
        conn.close(0u32.into(), b"done");

        Ok(ReceiveOutcome::Completed {
            files: (manifest.files.len() - self.skip.len()) as u64,
            bytes: total,
            dest: self.dest.clone(),
            kept_aside: std::mem::take(&mut self.kept_aside),
        })
    }

    /// Refuses a blob from the sender that is larger than `max`, judged by its
    /// verified size, so it is never loaded into memory.
    async fn check_size(&self, hash: Hash, max: u64, what: &str) -> Result<(), Stop> {
        let bitfield = self.store.observe(hash).await.map_err(|e| fatal(anyhow!("store: {e}")))?;
        if !bitfield.is_validated() {
            return Err(retry(anyhow!("the size of the {what} did not arrive")));
        }
        if bitfield.size() > max {
            return Err(fatal(anyhow!(
                "the sender's {what} is {}, more than the {} Carrick accepts; refusing it",
                human(bitfield.size()),
                human(max)
            )));
        }
        Ok(())
    }

    /// Bytes of the transfer's files that are already in the store.
    async fn local_file_bytes(&self, hashes: &[Hash]) -> Result<u64> {
        // Counted per listing, like the total: the same content listed twice
        // counts twice on both sides.
        let mut sum = 0u64;
        for (index, hash) in hashes.iter().skip(1).enumerate() {
            if self.skip.contains(&index) {
                continue;
            }
            let bitfield = self.store.observe(*hash).await.map_err(|e| anyhow!("store: {e}"))?;
            sum += bitfield.total_bytes();
        }
        Ok(sum)
    }

    /// Runs one request against the sender to the end, watching for stalls.
    async fn fetch(&self, conn: &Connection, request: GetRequest) -> Result<(), Stop> {
        let local = self.store.remote().local_for_request(request).await.map_err(|e| fatal(anyhow!("store: {e}")))?;
        if local.is_complete() {
            return Ok(());
        }
        let mut stream = self.store.remote().execute_get(conn.clone(), local.missing()).stream();
        loop {
            let item = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
                Ok(item) => item,
                Err(_) => {
                    conn.close(2u32.into(), b"stalled");
                    return Err(retry(anyhow!("nothing arrived for {} seconds", STALL_TIMEOUT.as_secs())));
                }
            };
            match item {
                Some(GetProgressItem::Progress(_)) => self.progressed.store(true, Ordering::Relaxed),
                Some(GetProgressItem::Done(_)) => return Ok(()),
                Some(GetProgressItem::Error(e)) => return Err(retry(anyhow!("transfer interrupted: {e}"))),
                None => return Err(retry(anyhow!("transfer interrupted"))),
            }
        }
    }

    fn downloader(&mut self, root: [u8; 32]) -> Downloader {
        if let Some(downloader) = &self.downloader {
            return downloader.clone();
        }
        let auth_key = self.access.auth_key;
        let mut options = connection_pool::Options::default();
        options.connect_timeout = CONNECT_TIMEOUT;
        options.idle_timeout = Duration::from_secs(30);
        // Every pooled connection, to the sender or to another receiver, is
        // let in the same way: by proving we hold the transfer key.
        let options = options.with_on_connected(move |endpoint: Endpoint, conn: Connection| async move {
            match handshake(&endpoint, &conn, &auth_key, false).await {
                Ok((_send, _recv, welcome)) if welcome.root == root => Ok(()),
                Ok(_) => Err(std::io::Error::other("the other side holds a different transfer")),
                Err(Stop::Fatal(e)) | Err(Stop::Retry(e)) => Err(std::io::Error::other(format!("{e:#}"))),
            }
        });
        let downloader = Downloader::builder(&self.store, &self.endpoint)
            .alpn(ALPN)
            .pool_options(options)
            .build();
        self.downloader = Some(downloader.clone());
        downloader
    }

    /// Fetches every missing file, from other receivers where they have it
    /// and from the sender otherwise.
    async fn download(&mut self, root: Hash, hashes: &[Hash], manifest: &Manifest, have0: u64, total: u64, progress: &watch::Sender<(u64, u64, bool)>) -> Result<(), Stop> {
        let mut wanted = Vec::new();
        // Complete in the store but maybe not yet in the destination, e.g.
        // after a restart: they go to their place before anything else.
        let mut ready = Vec::new();
        let mut local0: HashMap<Hash, u64> = HashMap::new();
        let mut sizes: HashMap<Hash, u64> = HashMap::new();
        for (index, file) in manifest.files.iter().enumerate() {
            let hash = hashes[index + 1];
            if self.skip.contains(&index) || file.size == 0 || sizes.insert(hash, file.size).is_some() {
                continue;
            }
            let status = self.store.blobs().status(hash).await.map_err(|e| fatal(anyhow!("store: {e}")))?;
            match status {
                BlobStatus::Complete { .. } => ready.push(hash),
                BlobStatus::NotFound => wanted.push(hash),
                BlobStatus::Partial { .. } => {
                    let bitfield = self.store.observe(hash).await.map_err(|e| fatal(anyhow!("store: {e}")))?;
                    local0.insert(hash, bitfield.total_bytes());
                    wanted.push(hash);
                }
            }
        }
        for hash in ready {
            self.save_ready(hash, hashes, manifest).await;
        }
        if wanted.is_empty() {
            return Ok(());
        }
        // Each receiver takes the files in its own order, so that receivers
        // soon hold different files and have something to give each other.
        wanted.shuffle(&mut rand::rng());

        let sender = self.access.sender;
        let sizes = Arc::new(sizes);
        let sources = Sources { sender, peers: self.peers.clone(), sizes: sizes.clone() };
        let downloader = self.downloader(*root.as_bytes());
        let mut stream = downloader
            .download_with_opts(DownloadOptions::new(wanted, sources, SplitStrategy::Split))
            .stream()
            .await
            .map_err(|e| retry(anyhow!("could not start the download: {e}")))?;

        // The stream reports, for the files it has started, how many bytes
        // each holds in all. What they held before starting is in `have0`
        // already, so it is taken out as each file starts.
        let mut started: HashSet<Hash> = HashSet::new();
        let mut started_local = 0u64;
        // Which source each file is being asked of, to credit other receivers.
        let mut asking: HashMap<Hash, EndpointId> = HashMap::new();
        let (mut peer_files, mut peer_bytes) = (0u64, 0u64);
        loop {
            let item = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
                Ok(item) => item,
                Err(_) => return Err(retry(anyhow!("nothing arrived for {} seconds", STALL_TIMEOUT.as_secs()))),
            };
            match item {
                Some(DownloadProgressItem::TryProvider { id, request }) => {
                    if started.insert(request.hash) {
                        started_local += local0.get(&request.hash).copied().unwrap_or(0);
                    }
                    asking.insert(request.hash, id);
                    if id != sender && self.helpers.insert(id) {
                        emit(&self.events, Event::Note(format!("Also getting pieces from another receiver ({})", id.fmt_short())));
                    }
                }
                Some(DownloadProgressItem::Progress(sum)) => {
                    self.progressed.store(true, Ordering::Relaxed);
                    let have = have0.saturating_sub(started_local).saturating_add(sum).min(total);
                    let _ = progress.send((have, total, false));
                    emit(&self.events, Event::Progress { have, total });
                }
                Some(DownloadProgressItem::ProviderFailed { id, .. }) => {
                    // Another receiver not having a piece is routine; the
                    // downloader moves on to the next source by itself.
                    if id == sender {
                        return Err(retry(anyhow!("lost contact with the sender")));
                    }
                }
                Some(DownloadProgressItem::PartComplete { request }) => {
                    if asking.get(&request.hash).is_some_and(|id| *id != sender) {
                        peer_files += 1;
                        peer_bytes += sizes.get(&request.hash).copied().unwrap_or(0);
                    }
                    // The file is whole and checked: to its place now, so it
                    // can be opened, and stays if the transfer is stopped.
                    self.save_ready(request.hash, hashes, manifest).await;
                }
                Some(DownloadProgressItem::DownloadError) => return Err(retry(anyhow!("a piece could not be fetched"))),
                Some(DownloadProgressItem::Error(e)) => return Err(retry(anyhow!("transfer interrupted: {e}"))),
                None => break,
            }
        }
        // Everything that was to come is here (skipped files were not asked for).
        for hash in sizes.keys() {
            match self.store.blobs().status(*hash).await {
                Ok(BlobStatus::Complete { .. }) => {}
                _ => return Err(retry(anyhow!("the transfer stopped before everything arrived"))),
            }
        }
        if peer_files > 0 {
            emit(
                &self.events,
                Event::Note(format!(
                    "{peer_files} file(s), {}, were completed by other receivers instead of the sender",
                    human(peer_bytes)
                )),
            );
        }
        Ok(())
    }

    /// Looks for files whose place is taken by a different file and settles
    /// what to do with them: as the record says, or by asking once. The
    /// answer is kept in the record, so a restart does not ask again.
    async fn decide_existing(&mut self, manifest: &Manifest) -> Result<()> {
        let mut taken = Vec::new();
        for (index, file) in manifest.files.iter().enumerate() {
            if self.at_dest.contains(&index) || self.saved.contains(&index) {
                continue;
            }
            if std::fs::symlink_metadata(local_path(&self.dest, &file.path)?).is_ok() {
                taken.push(index);
            }
        }
        let mut choice = self.record.existing();
        if taken.is_empty() {
            self.skip.clear();
            return Ok(());
        }
        if choice == Existing::Ask {
            let bytes = taken.iter().map(|i| manifest.files[*i].size).sum();
            let examples = taken.iter().take(3).map(|i| manifest.files[*i].path.clone()).collect();
            choice = match self.answers.as_mut() {
                Some(answers) => {
                    emit(&self.events, Event::AlreadyExist { files: taken.len() as u64, bytes, examples });
                    loop {
                        match answers.recv().await {
                            Some(Existing::Ask) => continue,
                            Some(answer) => break answer,
                            // Nobody left to answer: keep both, which loses nothing.
                            None => break Existing::KeepBoth,
                        }
                    }
                }
                None => Existing::KeepBoth,
            };
            self.record.existing = Some(choice);
            self.record.overwrite = choice == Existing::Replace;
            self.home.save(&mut self.record)?;
        }
        self.skip = if choice == Existing::Skip { taken.into_iter().collect() } else { HashSet::new() };
        Ok(())
    }

    /// Finds destination files that already match, so they do not travel.
    async fn adopt_existing(&mut self, hashes: &[Hash], manifest: &Manifest) -> Result<()> {
        // Files this transfer already put in place are trusted while they
        // look untouched, so a restart does not read them all again.
        let untouched = |index: usize| {
            let Some(file) = manifest.files.get(index) else { return false };
            let Ok(target) = local_path(&self.dest, &file.path) else { return false };
            std::fs::metadata(&target).is_ok_and(|m| {
                let mtime = filetime::FileTime::from_last_modification_time(&m).unix_seconds();
                m.is_file() && m.len() == file.size && file.mtime.is_none_or(|t| t == mtime)
            })
        };
        let changed: Vec<usize> = self.saved.iter().copied().filter(|i| !untouched(*i)).collect();
        for index in changed {
            self.saved.remove(&index);
            self.record.saved.retain(|i| *i as usize != index);
        }

        let mut candidates = Vec::new();
        for (index, file) in manifest.files.iter().enumerate() {
            if self.saved.contains(&index) {
                continue;
            }
            let target = local_path(&self.dest, &file.path)?;
            let Ok(meta) = std::fs::metadata(&target) else { continue };
            if meta.is_file() && meta.len() == file.size && file.size > 0 {
                candidates.push((index, target, file.size));
            }
        }
        if candidates.is_empty() {
            return Ok(());
        }
        let total: u64 = candidates.iter().map(|c| c.2).sum();
        let mut done = 0u64;
        let (mut files, mut bytes) = (0u64, 0u64);
        emit(&self.events, Event::Checking { done, total });
        for (index, target, size) in candidates {
            let wanted = hashes[index + 1];
            let status = self.store.blobs().status(wanted).await.map_err(|e| anyhow!("store: {e}"))?;
            let same = if matches!(status, BlobStatus::NotFound) {
                // Fingerprint the file where it is; if it is the wanted one,
                // the store now holds it without a byte having been copied.
                let tag = self
                    .store
                    .add_path_with_opts(AddPathOptions {
                        path: target.clone(),
                        mode: ImportMode::TryReference,
                        format: BlobFormat::Raw,
                    })
                    .temp_tag()
                    .await
                    .map_err(|e| anyhow!("reading {}: {e}", target.display()))?;
                tag.hash() == wanted
            } else {
                // The store already knows this content, e.g. from a twin of
                // this file; only the file itself is left to check.
                file_hash(&target).await? == wanted
            };
            if same {
                self.at_dest.insert(index);
                files += 1;
                bytes += size;
            }
            done += size;
            emit(&self.events, Event::Checking { done, total });
        }
        if files > 0 {
            emit(&self.events, Event::AlreadyHave { files, bytes });
        }
        Ok(())
    }

    /// Puts in their place the files whose content is `hash`, if it is
    /// complete. A failure here is not the end: the last pass tries again.
    async fn save_ready(&mut self, hash: Hash, hashes: &[Hash], manifest: &Manifest) {
        match self.store.blobs().status(hash).await {
            Ok(BlobStatus::Complete { .. }) => {}
            _ => return,
        }
        for (index, file) in manifest.files.iter().enumerate() {
            if hashes[index + 1] != hash || self.at_dest.contains(&index) || self.saved.contains(&index) || self.skip.contains(&index) {
                continue;
            }
            if let Err(e) = self.save_file(index, hash, file).await {
                tracing::warn!("could not save {} yet: {e:#}", file.path);
            }
        }
    }

    /// Moves every finished file not yet in place to the destination, and
    /// makes the empty folders.
    async fn export(&mut self, hashes: &[Hash], manifest: &Manifest) -> Result<()> {
        let total = manifest.files.len() as u64;
        for (index, file) in manifest.files.iter().enumerate() {
            emit(&self.events, Event::Saving { done: index as u64, total });
            if self.at_dest.contains(&index) || self.saved.contains(&index) || self.skip.contains(&index) {
                continue;
            }
            self.save_file(index, hashes[index + 1], file).await?;
        }
        for dir in &manifest.empty_dirs {
            let path = local_path(&self.dest, dir)?;
            std::fs::create_dir_all(&path).with_context(|| format!("cannot create {}", path.display()))?;
        }
        // Windows keeps these hidden; the transfer does not carry attributes,
        // so they are hidden again here, also when they were already in place.
        for (index, file) in manifest.files.iter().enumerate() {
            if is_hidden_by_windows(&file.path) && !self.skip.contains(&index) {
                hide_as_windows_does(&local_path(&self.dest, &file.path)?);
            }
        }
        emit(&self.events, Event::Saving { done: total, total });
        self.home.save(&mut self.record)?;
        Ok(())
    }

    /// Moves one finished file to its place in the destination. The store
    /// lives inside the destination folder, so this is a rename, not a copy.
    async fn save_file(&mut self, index: usize, hash: Hash, file: &FileEntry) -> Result<()> {
        let mut target = local_path(&self.dest, &file.path)?;
        if let Ok(meta) = std::fs::symlink_metadata(&target) {
            if meta.is_file() && meta.len() == file.size && file_hash(&target).await? == hash {
                // Already in place, e.g. moved there before a restart.
                self.mark_saved(index);
                return Ok(());
            }
            if self.record.existing() == Existing::Replace && meta.is_file() {
                std::fs::remove_file(&target).with_context(|| format!("cannot replace {}", target.display()))?;
            } else {
                let aside = free_name(&target);
                self.kept_aside.push((file.path.clone(), aside.clone()));
                target = aside;
            }
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        if file.size == 0 {
            std::fs::write(&target, b"").with_context(|| format!("cannot create {}", target.display()))?;
        } else {
            self.store
                .export_with_opts(ExportOptions { hash, target: target.clone(), mode: ExportMode::TryReference })
                .finish()
                .await
                .map_err(|e| anyhow!("cannot save {}: {e}", target.display()))?;
        }
        if let Some(mtime) = file.mtime {
            let _ = filetime::set_file_mtime(&target, filetime::FileTime::from_unix_time(mtime, 0));
        }
        #[cfg(unix)]
        if let Some(mode) = file.mode {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
        }
        self.mark_saved(index);
        Ok(())
    }

    /// Notes that a file is in place, and writes the record now and then so
    /// a restart knows it too.
    fn mark_saved(&mut self, index: usize) {
        if self.saved.insert(index) {
            self.record.saved.push(index as u32);
            if self.saved_written.elapsed() >= Duration::from_secs(2) {
                let _ = self.home.save(&mut self.record);
                self.saved_written = std::time::Instant::now();
            }
        }
    }}

/// Replaces the set of known receivers and records their addresses.
fn set_peers(peers: &Peers, book: &MemoryLookup, me: &EndpointId, list: &[Peer]) {
    let mut set = BTreeSet::new();
    for peer in list {
        let Some(addr) = peer.endpoint_addr() else { continue };
        if addr.id == *me {
            continue;
        }
        set.insert(addr.id);
        book.add_endpoint_info(addr);
    }
    *peers.lock().expect("lock") = set;
}

/// Follows the sender's updates of who else is receiving.
async fn read_peer_updates(mut recv: RecvStream, peers: Peers, book: MemoryLookup, me: EndpointId) {
    while let Ok(ServerMsg::Peers(list)) = proto::read_frame::<ServerMsg>(&mut recv).await {
        set_peers(&peers, &book, &me, &list);
    }
}

/// Sends progress to the sender whenever it changes, at most once a second.
async fn report_progress(mut send: SendStream, mut rx: watch::Receiver<(u64, u64, bool)>) {
    loop {
        let (have, total, done) = *rx.borrow_and_update();
        if proto::write_frame(&mut send, &ClientMsg::Progress { have, total, done }).await.is_err() {
            return;
        }
        if done {
            let _ = send.finish();
            let _ = tokio::time::timeout(Duration::from_secs(3), send.stopped()).await;
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// BLAKE3 of a file, read in a blocking thread.
async fn file_hash(path: &Path) -> Result<Hash> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<Hash> {
        let mut hasher = blake3::Hasher::new();
        let mut file = std::fs::File::open(&path).with_context(|| format!("cannot read {}", path.display()))?;
        std::io::copy(&mut file, &mut hasher).with_context(|| format!("cannot read {}", path.display()))?;
        Ok(Hash::from_bytes(*hasher.finalize().as_bytes()))
    })
    .await?
}

/// First name next to `path` that is not taken: `name (2).ext`, `name (3).ext`…
fn free_name(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    let mut n = 2u32;
    loop {
        let name = match &ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = path.with_file_name(name);
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Marks the working folder hidden on Windows; elsewhere the dot does it.
/// Files Windows keeps hidden in folders: a folder's settings (its icon,
/// its view) and its thumbnails. Arriving visible, they clutter the folder.
fn is_hidden_by_windows(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name == "desktop.ini" || name == "thumbs.db"
}

/// Marks a file hidden and system, as Windows marks those files.
fn hide_as_windows_does(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, INVALID_FILE_ATTRIBUTES,
        };
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives both calls.
        unsafe {
            let now = GetFileAttributesW(wide.as_ptr());
            if now != INVALID_FILE_ATTRIBUTES {
                SetFileAttributesW(wide.as_ptr(), now | FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

fn hide(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("attrib").arg("+h").arg(path).output();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_name_skips_taken_names() {
        let dir = std::env::temp_dir().join(format!("carrick-free-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("report.pdf");
        std::fs::write(&original, b"x").unwrap();
        assert_eq!(free_name(&original), dir.join("report (2).pdf"));
        std::fs::write(dir.join("report (2).pdf"), b"x").unwrap();
        assert_eq!(free_name(&original), dir.join("report (3).pdf"));
        assert_eq!(free_name(&dir.join("noext")), dir.join("noext (2)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(100 * 1024 * 1024 * 1024), "100.0 GiB");
    }
}
