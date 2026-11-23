//! The Carrick wire protocol: one authenticated control stream, then blobs.
//!
//! A connection on [`ALPN`] starts with the dialing side opening a stream and
//! sending [`Hello`] with a token that proves it holds the transfer key. Only
//! after [`Reply::Welcome`] does the accepting side serve blob requests on the
//! same connection. The control stream stays open for progress and peer lists.

use std::net::SocketAddr;

use anyhow::{bail, ensure, Context, Result};
use iroh::{
    endpoint::{RecvStream, SendStream},
    EndpointAddr, EndpointId,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// ALPN of the Carrick protocol.
pub const ALPN: &[u8] = b"carrick/1";

/// Protocol version carried in [`Hello`].
pub const VERSION: u32 = 1;

const MAX_FRAME: usize = 4 * 1024 * 1024;
const TOKEN_CONTEXT: &[u8] = b"carrick/v1/hello";

/// First message of a connection, from the side that dialed.
#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// [`auth_token`] for this pair of endpoints.
    pub token: [u8; 32],
    /// True if this endpoint also accepts connections from other receivers.
    pub serves: bool,
}

/// Answer to [`Hello`].
#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Welcome(Welcome),
    /// The connection is refused; the text says why.
    Reject(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    /// Root hash of the transfer (a hash sequence: manifest, then files).
    pub root: [u8; 32],
    /// True when the answering side is the original sender.
    pub origin: bool,
    /// Other receivers of this transfer that accept connections.
    pub peers: Vec<Peer>,
}

/// Another receiver of the same transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub id: [u8; 32],
    pub relay: Option<String>,
    pub addrs: Vec<SocketAddr>,
}

impl Peer {
    pub fn from_addr(addr: &EndpointAddr) -> Self {
        Self {
            id: *addr.id.as_bytes(),
            relay: addr.relay_urls().next().map(|u| u.to_string()),
            addrs: addr.ip_addrs().copied().collect(),
        }
    }

    pub fn endpoint_addr(&self) -> Option<EndpointAddr> {
        let id = EndpointId::from_bytes(&self.id).ok()?;
        let mut addr = EndpointAddr::new(id);
        if let Some(relay) = self.relay.as_ref().and_then(|r| r.parse().ok()) {
            addr = addr.with_relay_url(relay);
        }
        for ip in &self.addrs {
            addr = addr.with_ip_addr(*ip);
        }
        Some(addr)
    }
}

/// Messages from the dialing side after the handshake.
#[derive(Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    /// How much of the transfer this receiver holds, in payload bytes.
    Progress { have: u64, total: u64, done: bool },
    /// Where this receiver can be reached by other receivers.
    Reachable(Peer),
}

/// Messages from the accepting side after the handshake.
#[derive(Debug, Serialize, Deserialize)]
pub enum ServerMsg {
    /// The current list of other receivers.
    Peers(Vec<Peer>),
}

/// The proof a dialing endpoint presents: a keyed BLAKE3 MAC over both
/// endpoint ids. QUIC already authenticates each id, so the token cannot be
/// replayed from another endpoint.
pub fn auth_token(auth_key: &[u8; 32], client: &EndpointId, server: &EndpointId) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(auth_key);
    hasher.update(TOKEN_CONTEXT);
    hasher.update(client.as_bytes());
    hasher.update(server.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Constant-time check of a presented token.
pub fn token_matches(
    auth_key: &[u8; 32],
    client: &EndpointId,
    server: &EndpointId,
    presented: &[u8; 32],
) -> bool {
    // blake3::Hash compares in constant time.
    blake3::Hash::from(auth_token(auth_key, client, server)) == blake3::Hash::from(*presented)
}

/// Writes one length-prefixed message.
pub async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let body = postcard::to_stdvec(msg).context("encode message")?;
    ensure!(body.len() <= MAX_FRAME, "message too large");
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    send.write_all(&frame).await.context("write message")?;
    Ok(())
}

/// Reads one length-prefixed message.
pub async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.context("read message length")?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("peer sent an oversized message ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.context("read message")?;
    postcard::from_bytes(&body).context("decode message")
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn token_is_bound_to_key_and_to_both_endpoints() {
        let a = SecretKey::from_bytes(&[1u8; 32]).public();
        let b = SecretKey::from_bytes(&[2u8; 32]).public();
        let c = SecretKey::from_bytes(&[3u8; 32]).public();
        let key = [9u8; 32];
        let token = auth_token(&key, &a, &b);
        assert!(token_matches(&key, &a, &b, &token));
        // Another key, another client, another server, or swapped roles: no.
        assert!(!token_matches(&[8u8; 32], &a, &b, &token));
        assert!(!token_matches(&key, &c, &b, &token));
        assert!(!token_matches(&key, &a, &c, &token));
        assert!(!token_matches(&key, &b, &a, &token));
    }
}
