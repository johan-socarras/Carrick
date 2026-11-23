//! The accepting side of a Carrick connection.
//!
//! Every connection must first prove it holds the transfer key. Only then is
//! it handed to iroh-blobs, which serves verified ranges of the transfer.
//! The original sender and every receiver run the same gate, each over its
//! own store, which is what lets receivers pass pieces to one another.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
    EndpointId,
};
use iroh_blobs::{
    api::Store,
    provider::events::{EventMask, EventSender, ObserveMode},
};
use tokio::sync::watch;

use crate::{
    events::{emit, Event, EventTx},
    net,
    proto::{self, ClientMsg, Hello, Peer, Reply, ServerMsg, Welcome},
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// What a gate knows about one connected receiver.
#[derive(Debug, Clone, Default)]
pub struct ReceiverState {
    pub have: u64,
    pub total: u64,
    pub done: bool,
    /// Where other receivers can reach it, if it serves.
    pub reachable: Option<Peer>,
    /// Number of live connections from this receiver.
    connections: u32,
}

#[derive(Debug)]
struct Inner {
    me: EndpointId,
    store: Store,
    auth_key: [u8; 32],
    root: [u8; 32],
    origin: bool,
    blob_events: EventSender,
    receivers: Mutex<BTreeMap<EndpointId, ReceiverState>>,
    /// Bumped whenever the set of reachable receivers changes.
    peers_version: watch::Sender<u64>,
    /// Bumped whenever a receiver reports it is done.
    done_version: watch::Sender<u64>,
    events: EventTx,
}

/// Protocol handler for [`proto::ALPN`].
#[derive(Debug, Clone)]
pub struct Gate {
    inner: Arc<Inner>,
}

impl Gate {
    pub fn new(
        me: EndpointId,
        store: Store,
        auth_key: [u8; 32],
        root: [u8; 32],
        origin: bool,
        events: EventTx,
    ) -> Self {
        // Reads are allowed without asking; observe lets a peer learn which
        // ranges we hold. Push stays disabled: nobody writes into our store.
        let mask = EventMask {
            observe: ObserveMode::None,
            ..EventMask::DEFAULT
        };
        let (blob_events, _unused) = EventSender::channel(1, mask);
        Self {
            inner: Arc::new(Inner {
                me,
                store,
                auth_key,
                root,
                origin,
                blob_events,
                receivers: Mutex::new(BTreeMap::new()),
                peers_version: watch::channel(0).0,
                done_version: watch::channel(0).0,
                events,
            }),
        }
    }

    /// Receivers that reported the whole transfer, as endpoint ids.
    pub fn finished(&self) -> Vec<EndpointId> {
        let receivers = self.inner.receivers.lock().expect("lock");
        receivers
            .iter()
            .filter(|(_, state)| state.done)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Resolves each time a receiver reports it is done.
    pub fn watch_done(&self) -> watch::Receiver<u64> {
        self.inner.done_version.subscribe()
    }

    /// Number of receivers connected right now.
    pub fn connected(&self) -> usize {
        let receivers = self.inner.receivers.lock().expect("lock");
        receivers.values().filter(|s| s.connections > 0).count()
    }

    fn peers_for(&self, asking: &EndpointId) -> Vec<Peer> {
        let receivers = self.inner.receivers.lock().expect("lock");
        receivers
            .iter()
            .filter(|(id, state)| *id != asking && state.connections > 0)
            .filter_map(|(_, state)| state.reachable.clone())
            .collect()
    }

    async fn serve(&self, conn: Connection) -> anyhow::Result<()> {
        let inner = &self.inner;
        let remote = conn.remote_id();
        let (mut send, mut recv) =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, conn.accept_bi()).await??;
        let hello: Hello =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, proto::read_frame(&mut recv)).await??;

        let refusal = if hello.version != proto::VERSION {
            Some("the other side runs an incompatible Carrick version")
        } else if !proto::token_matches(&inner.auth_key, &remote, &inner.me, &hello.token) {
            Some("wrong code or ticket")
        } else {
            None
        };
        if let Some(reason) = refusal {
            let _ = proto::write_frame(&mut send, &Reply::Reject(reason.to_string())).await;
            let _ = send.finish();
            // Give the refusal a moment to arrive before closing.
            let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
            conn.close(1u32.into(), b"refused");
            anyhow::bail!("refused {}: {reason}", remote.fmt_short());
        }

        let welcome = Welcome {
            root: inner.root,
            origin: inner.origin,
            peers: self.peers_for(&remote),
        };
        proto::write_frame(&mut send, &Reply::Welcome(welcome)).await?;

        let first_connection = {
            let mut receivers = inner.receivers.lock().expect("lock");
            let state = receivers.entry(remote).or_default();
            state.connections += 1;
            state.connections == 1
        };
        // The first connection of a receiver is its control connection;
        // the sender reports how it reaches that receiver.
        let _route = if inner.origin && first_connection {
            let receiver = remote.fmt_short().to_string();
            emit(&inner.events, Event::ReceiverJoined { receiver: receiver.clone() });
            let events = inner.events.clone();
            Some(net::watch_route(conn.clone(), move |route| {
                emit(&events, Event::ReceiverRoute { receiver: receiver.clone(), route })
            }))
        } else {
            None
        };

        // From here on the connection may ask for blobs.
        let blobs = tokio::spawn(iroh_blobs::provider::handle_connection(
            conn.clone(),
            inner.store.clone(),
            inner.blob_events.clone(),
        ));

        // Reading a frame cannot be cancelled half way without losing bytes,
        // so the control stream has its own task and the loop below only
        // sees whole messages.
        let (msg_tx, mut msg_rx) = tokio::sync::mpsc::channel::<ClientMsg>(16);
        let reader = tokio::spawn(async move {
            while let Ok(msg) = proto::read_frame::<ClientMsg>(&mut recv).await {
                if msg_tx.send(msg).await.is_err() {
                    break;
                }
            }
        });

        let mut peers_changed = inner.peers_version.subscribe();
        peers_changed.mark_unchanged();
        // A connection opened only to fetch blobs closes its control stream
        // right after the handshake; it keeps being served all the same.
        let mut control_open = true;
        loop {
            tokio::select! {
                msg = msg_rx.recv(), if control_open => {
                    match msg {
                        Some(ClientMsg::Progress { have, total, done }) => {
                            self.on_progress(remote, have, total, done);
                        }
                        Some(ClientMsg::Reachable(peer)) => {
                            // A receiver can only announce itself.
                            if peer.id == *remote.as_bytes() {
                                let mut receivers = inner.receivers.lock().expect("lock");
                                receivers.entry(remote).or_default().reachable = Some(peer);
                                drop(receivers);
                                inner.peers_version.send_modify(|v| *v += 1);
                            }
                        }
                        None => control_open = false,
                    }
                }
                changed = peers_changed.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if control_open {
                        let peers = self.peers_for(&remote);
                        // A client that stopped listening is not an error.
                        let _ = proto::write_frame(&mut send, &ServerMsg::Peers(peers)).await;
                    }
                }
                _ = conn.closed() => break,
            }
        }
        reader.abort();

        blobs.abort();
        let last_connection = {
            let mut receivers = inner.receivers.lock().expect("lock");
            let state = receivers.entry(remote).or_default();
            state.connections = state.connections.saturating_sub(1);
            state.connections == 0
        };
        if last_connection {
            inner.peers_version.send_modify(|v| *v += 1);
            if inner.origin {
                emit(&inner.events, Event::ReceiverLeft { receiver: remote.fmt_short().to_string() });
            }
        }
        Ok(())
    }

    fn on_progress(&self, remote: EndpointId, have: u64, total: u64, done: bool) {
        let inner = &self.inner;
        let newly_done = {
            let mut receivers = inner.receivers.lock().expect("lock");
            let state = receivers.entry(remote).or_default();
            state.have = have;
            state.total = total;
            let newly = done && !state.done;
            state.done |= done;
            newly
        };
        if !inner.origin {
            return;
        }
        let receiver = remote.fmt_short().to_string();
        emit(&inner.events, Event::ReceiverProgress { receiver: receiver.clone(), have, total });
        if newly_done {
            emit(&inner.events, Event::ReceiverDone { receiver });
            inner.done_version.send_modify(|v| *v += 1);
        }
    }
}

impl ProtocolHandler for Gate {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if let Err(e) = self.serve(connection).await {
            tracing::debug!("connection ended: {e:#}");
        }
        Ok(())
    }
}
