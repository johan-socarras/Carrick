//! The key to a transfer: a four-word code, or a long ticket.
//!
//! A code is four words from the EFF large word list (about 52 bits). Run
//! through Argon2id it yields a seed, and from the seed come the sender's
//! network identity and the key receivers use to prove they hold the code.
//! Nothing here is a new primitive: Argon2id, BLAKE3 and ed25519 come from
//! published crates; only the way they fit together is ours.

use std::{net::SocketAddr, sync::OnceLock};

use anyhow::{bail, ensure, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use data_encoding::BASE32_NOPAD;
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey};
use rand::RngExt;
use serde::{Deserialize, Serialize};

/// The EFF Large Wordlist for Passphrases (https://www.eff.org/dice), by the
/// Electronic Frontier Foundation, under CC BY 3.0 US, minus its four
/// hyphenated words. Its credit goes in the window's About and in
/// THIRD-PARTY-LICENSES.txt.
const WORDS_RAW: &str = include_str!("words.txt");

/// Number of words in a code.
pub const CODE_WORDS: usize = 4;

const TICKET_PREFIX: &str = "ck1";
const ARGON_SALT: &[u8] = b"carrick/v1/code-to-seed";
const CTX_ENDPOINT: &str = "carrick v1 2026-11 sender endpoint secret key";
const CTX_AUTH: &str = "carrick v1 2026-11 transfer authorization key";

fn words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| WORDS_RAW.lines().filter(|l| !l.is_empty()).collect())
}

/// A four-word transfer code, normalized to `word-word-word-word`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code(String);

impl Code {
    /// Draws a new code from the operating system's randomness.
    pub fn generate() -> Self {
        let list = words();
        let mut rng = rand::rng();
        let picked: Vec<&str> = (0..CODE_WORDS)
            .map(|_| list[rng.random_range(0..list.len())])
            .collect();
        Code(picked.join("-"))
    }

    /// Parses what a person typed: any case, dashes or spaces between words.
    pub fn parse(input: &str) -> Result<Self> {
        let lowered = input.trim().to_lowercase();
        let parts: Vec<&str> = lowered
            .split(|c: char| c == '-' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        ensure!(
            parts.len() == CODE_WORDS,
            "a code has {CODE_WORDS} words, like `apple-river-stone-cloud`; this one has {}",
            parts.len()
        );
        let list = words();
        for part in &parts {
            if list.binary_search(part).is_err() {
                bail!("`{part}` is not a word a code can contain; check for a typo");
            }
        }
        Ok(Code(parts.join("-")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the sender holds for one transfer.
#[derive(Clone)]
pub struct SenderKeys {
    /// Secret key of the sender's endpoint for this transfer.
    pub endpoint_secret: [u8; 32],
    /// Shared key receivers prove knowledge of.
    pub auth_key: [u8; 32],
}

impl SenderKeys {
    /// Keys for a code: everything derives from the code.
    pub fn from_code(code: &Code) -> Result<Self> {
        // 64 MiB, 3 passes, 1 lane: a guess costs real memory and time.
        let params = Params::new(64 * 1024, 3, 1, Some(32))
            .map_err(|e| anyhow::anyhow!("argon2 parameters: {e}"))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut seed = [0u8; 32];
        argon
            .hash_password_into(code.as_str().as_bytes(), ARGON_SALT, &mut seed)
            .map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
        Ok(Self {
            endpoint_secret: blake3::derive_key(CTX_ENDPOINT, &seed),
            auth_key: blake3::derive_key(CTX_AUTH, &seed),
        })
    }

    /// Fresh random keys, for a ticket-only transfer.
    pub fn random() -> Self {
        let mut rng = rand::rng();
        Self {
            endpoint_secret: rng.random(),
            auth_key: rng.random(),
        }
    }

    pub fn secret_key(&self) -> SecretKey {
        SecretKey::from_bytes(&self.endpoint_secret)
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.secret_key().public()
    }
}

/// What a receiver needs to reach and be let into a transfer.
#[derive(Clone, Debug)]
pub struct Access {
    /// The sender's endpoint.
    pub sender: EndpointId,
    /// Shared key to present.
    pub auth_key: [u8; 32],
    /// Root hash of the transfer, when the key carries it (tickets do).
    pub root: Option<[u8; 32]>,
    /// Known addresses of the sender, when the key carries them.
    pub relay: Option<RelayUrl>,
    pub addrs: Vec<SocketAddr>,
}

impl Access {
    /// Reads a code or a ticket, whichever the person pasted.
    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        if looks_like_ticket(trimmed) {
            Ticket::decode(trimmed).map(Into::into)
        } else {
            let code = Code::parse(trimmed)?;
            Self::from_code(&code)
        }
    }

    pub fn from_code(code: &Code) -> Result<Self> {
        let keys = SenderKeys::from_code(code)?;
        Ok(Self {
            sender: keys.endpoint_id(),
            auth_key: keys.auth_key,
            root: None,
            relay: None,
            addrs: Vec::new(),
        })
    }

    /// Where to dial the sender.
    pub fn endpoint_addr(&self) -> EndpointAddr {
        let mut addr = EndpointAddr::new(self.sender);
        if let Some(relay) = &self.relay {
            addr = addr.with_relay_url(relay.clone());
        }
        for ip in &self.addrs {
            addr = addr.with_ip_addr(*ip);
        }
        addr
    }
}

fn looks_like_ticket(s: &str) -> bool {
    s.len() > 60 && s.to_lowercase().starts_with(TICKET_PREFIX) && !s.contains(['-', ' '])
}

/// The long form of a transfer key. Unlike a code it does not let its holder
/// impersonate the sender, pins the content by hash, and carries addresses, so
/// it works without a rendezvous server.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ticket {
    pub sender: [u8; 32],
    pub auth_key: [u8; 32],
    pub root: [u8; 32],
    pub relay: Option<String>,
    pub addrs: Vec<SocketAddr>,
}

impl Ticket {
    pub fn encode(&self) -> String {
        let bytes = postcard::to_stdvec(self).expect("ticket serializes");
        format!("{TICKET_PREFIX}{}", BASE32_NOPAD.encode(&bytes).to_lowercase())
    }

    pub fn decode(s: &str) -> Result<Self> {
        let lowered = s.trim().to_lowercase();
        let body = lowered
            .strip_prefix(TICKET_PREFIX)
            .context("a ticket starts with `ck1`")?;
        let bytes = BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .context("the ticket is damaged or incomplete")?;
        let ticket: Ticket =
            postcard::from_bytes(&bytes).context("the ticket is damaged or incomplete")?;
        // Reject a public key that is not a valid point early.
        EndpointId::from_bytes(&ticket.sender).context("the ticket names an invalid sender")?;
        Ok(ticket)
    }
}

impl From<Ticket> for Access {
    fn from(t: Ticket) -> Self {
        Access {
            sender: EndpointId::from_bytes(&t.sender).expect("checked in decode"),
            auth_key: t.auth_key,
            root: Some(t.root),
            relay: t.relay.and_then(|r| r.parse().ok()),
            addrs: t.addrs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_list_is_sorted_and_complete() {
        let list = words();
        assert_eq!(list.len(), 7772);
        assert!(list.windows(2).all(|w| w[0] < w[1]));
        assert!(list.iter().all(|w| w.chars().all(|c| c.is_ascii_lowercase())));
    }

    #[test]
    fn code_round_trips_through_what_people_type() {
        let code = Code::generate();
        assert_eq!(code.as_str().split('-').count(), CODE_WORDS);
        let shouted = code.as_str().to_uppercase().replace('-', "  ");
        assert_eq!(Code::parse(&format!("  {shouted} ")).unwrap(), code);
    }

    #[test]
    fn code_rejects_typos_and_wrong_length() {
        assert!(Code::parse("apple-river-stone").is_err());
        assert!(Code::parse("apple-river-stone-qqqqzz").is_err());
        assert!(Code::parse("").is_err());
    }

    #[test]
    fn codes_are_not_repeated() {
        let a = Code::generate();
        let b = Code::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn same_code_gives_same_keys_and_different_codes_differ() {
        let code = Code::parse("abacus-zoom-acorn-yoga").unwrap();
        let a = SenderKeys::from_code(&code).unwrap();
        let b = SenderKeys::from_code(&code).unwrap();
        assert_eq!(a.endpoint_secret, b.endpoint_secret);
        assert_eq!(a.auth_key, b.auth_key);
        assert_ne!(a.endpoint_secret, a.auth_key);
        let other = SenderKeys::from_code(&Code::parse("abacus-zoom-acorn-zone").unwrap()).unwrap();
        assert_ne!(a.endpoint_secret, other.endpoint_secret);
        assert_ne!(a.auth_key, other.auth_key);
        // The receiver reaches the same identity from the code alone.
        assert_eq!(Access::from_code(&code).unwrap().sender, a.endpoint_id());
    }

    #[test]
    fn ticket_round_trips() {
        let keys = SenderKeys::random();
        let ticket = Ticket {
            sender: *keys.endpoint_id().as_bytes(),
            auth_key: keys.auth_key,
            root: [7u8; 32],
            relay: Some("https://relay.example.org./".to_string()),
            addrs: vec!["192.168.1.20:4433".parse().unwrap()],
        };
        let text = ticket.encode();
        assert!(looks_like_ticket(&text));
        assert_eq!(Ticket::decode(&text).unwrap(), ticket);
        let access = Access::parse(&text).unwrap();
        assert_eq!(access.sender, keys.endpoint_id());
        assert_eq!(access.root, Some([7u8; 32]));
        assert_eq!(access.addrs.len(), 1);
        assert!(Ticket::decode(&text[..text.len() - 9]).is_err());
    }
}
