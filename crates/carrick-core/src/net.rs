//! Building the iroh endpoint a transfer runs on.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use iroh::{
    address_lookup::MemoryLookup,
    endpoint::{presets, Connection},
    Endpoint, RelayMode, RelayUrl, SecretKey,
};
use iroh_relay::tls::CaTlsConfig;
use n0_future::{task::AbortOnDropHandle, StreamExt};
use rustls_pki_types::{pem::PemObject, CertificateDer};

use crate::{events::Route, proto::ALPN};

/// Which relay servers to use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RelayChoice {
    /// The public relays run by n0: free, rate-limited, no guarantees.
    #[default]
    Default,
    /// No relay at all: only direct connections, e.g. inside one network.
    Disabled,
    /// A relay you run yourself.
    Custom(RelayUrl),
}

impl RelayChoice {
    /// Reads `default`, `disabled`, or the URL of a relay.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "" | "default" => Ok(Self::Default),
            "disabled" | "none" | "off" => Ok(Self::Disabled),
            url => Ok(Self::Custom(
                url.parse().with_context(|| format!("`{url}` is not a relay URL"))?,
            )),
        }
    }

    /// The form stored in a transfer record.
    pub fn to_setting(&self) -> Option<String> {
        match self {
            Self::Default => None,
            Self::Disabled => Some("disabled".to_string()),
            Self::Custom(url) => Some(url.to_string()),
        }
    }

    pub fn from_setting(setting: Option<&str>) -> Result<Self> {
        match setting {
            None => Ok(Self::Default),
            Some(s) => Self::parse(s),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NetOptions {
    pub relay: RelayChoice,
    /// For tests: never use a direct path, carry everything over the relay.
    pub relay_only: bool,
    /// A PEM file with certificate authorities to trust besides the public
    /// ones: for a relay of your own whose certificate comes from a private
    /// authority, as inside a company.
    pub relay_ca: Option<PathBuf>,
    /// A fixed UDP port to listen on, instead of a random one, so it can be
    /// opened (forwarded) on the router once. The router may also open it by
    /// itself (UPnP, NAT-PMP, PCP), which iroh tries on its own.
    pub port: Option<u16>,
    /// Addresses on which this computer can be reached from outside, such as
    /// the router's public IP with the forwarded port. They go into tickets
    /// and are announced to the other side, so it can connect directly even
    /// when its own network would not let a direct path be found.
    pub public_addrs: Vec<SocketAddr>,
}

impl NetOptions {
    /// Options from the environment: `CARRICK_RELAY`, `CARRICK_RELAY_CA`,
    /// `CARRICK_RELAY_ONLY`, `CARRICK_PORT` and `CARRICK_PUBLIC_ADDR`.
    pub fn from_env() -> Result<Self> {
        let relay = match std::env::var("CARRICK_RELAY") {
            Ok(value) => RelayChoice::parse(&value)?,
            Err(_) => RelayChoice::Default,
        };
        let relay_only = std::env::var("CARRICK_RELAY_ONLY").is_ok_and(|v| v == "1");
        let relay_ca = std::env::var_os("CARRICK_RELAY_CA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let port = match std::env::var("CARRICK_PORT") {
            Ok(v) if !v.trim().is_empty() => Some(parse_port(&v)?),
            _ => None,
        };
        let public_addrs = match std::env::var("CARRICK_PUBLIC_ADDR") {
            Ok(v) => parse_public_addrs(&v, port)?,
            Err(_) => Vec::new(),
        };
        Ok(Self { relay, relay_only, relay_ca, port, public_addrs })
    }
}

fn parse_port(s: &str) -> Result<u16> {
    let port: u16 = s.trim().parse().with_context(|| format!("`{}` is not a port number", s.trim()))?;
    anyhow::ensure!(port != 0, "the port cannot be 0");
    Ok(port)
}

/// Reads public addresses separated by commas or spaces. Each is `IP:port`,
/// or just an IP when a fixed port is set, which is then used with it.
pub fn parse_public_addrs(s: &str, port: Option<u16>) -> Result<Vec<SocketAddr>> {
    let mut out = Vec::new();
    for item in s.split([',', ' ']).map(str::trim).filter(|i| !i.is_empty()) {
        let addr = if let Ok(addr) = item.parse::<SocketAddr>() {
            addr
        } else if let Ok(ip) = item.parse::<IpAddr>() {
            let port = port.with_context(|| {
                format!("`{item}` has no port: write it as {item}:PORT, or set a fixed port with --port")
            })?;
            SocketAddr::new(ip, port)
        } else {
            anyhow::bail!("`{item}` is not an address: write it as IP:PORT, e.g. 203.0.113.7:43217");
        };
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    Ok(out)
}

/// Binds an endpoint that both dials and accepts Carrick connections.
///
/// The returned address book is where addresses learned from a ticket or
/// from the sender's list of receivers go, so they can be dialed by id.
pub async fn bind(secret: SecretKey, opts: &NetOptions) -> Result<(Endpoint, MemoryLookup)> {
    let book = MemoryLookup::new();
    if let Some(port) = opts.port.filter(|_| !opts.relay_only) {
        match builder(secret.clone(), opts, &book, Some(port))?.bind().await {
            Ok(endpoint) => return Ok((endpoint, book)),
            // Most likely another transfer on this computer holds the port:
            // this one still works, only without the fixed port.
            Err(e) => tracing::warn!("cannot listen on port {port} ({e:#}); using a random port instead"),
        }
    }
    let endpoint = builder(secret, opts, &book, None)?
        .bind()
        .await
        .context("could not open the network endpoint")?;
    Ok((endpoint, book))
}

fn builder(secret: SecretKey, opts: &NetOptions, book: &MemoryLookup, port: Option<u16>) -> Result<iroh::endpoint::Builder> {
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![ALPN.to_vec()])
        .address_lookup(book.clone());
    builder = match &opts.relay {
        RelayChoice::Default => builder,
        RelayChoice::Disabled => builder.relay_mode(RelayMode::Disabled),
        RelayChoice::Custom(url) => builder.relay_mode(RelayMode::Custom(url.clone().into())),
    };
    if opts.relay_only {
        builder = builder.clear_ip_transports();
    }
    if let Some(path) = &opts.relay_ca {
        let roots = CertificateDer::pem_file_iter(path)
            .map_err(|e| anyhow::anyhow!("cannot read the certificate file {}: {e}", path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("{} is not a PEM certificate file: {e}", path.display()))?;
        anyhow::ensure!(!roots.is_empty(), "{} holds no certificate", path.display());
        // The public authorities stay trusted; these are added to them.
        builder = builder.ca_tls_config(CaTlsConfig::embedded().with_extra_roots(roots));
    }
    if let Some(port) = port {
        builder = builder
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
            .map_err(|e| anyhow::anyhow!("cannot use port {port}: {e}"))?;
    }
    if !opts.relay_only {
        for addr in &opts.public_addrs {
            builder = builder.external_addr(*addr);
        }
    }
    Ok(builder)
}

/// When a fixed port was asked for but another program holds it, what to tell
/// the person: the transfer works, but the port opened on the router does not
/// help this time.
pub fn port_note(endpoint: &Endpoint, opts: &NetOptions) -> Option<String> {
    let wanted = opts.port.filter(|_| !opts.relay_only)?;
    let sockets = endpoint.bound_sockets();
    if sockets.iter().any(|s| s.is_ipv4() && s.port() == wanted) {
        return None;
    }
    let used = sockets.iter().find(|s| s.is_ipv4()).map(|s| s.port().to_string()).unwrap_or_else(|| "another".into());
    Some(format!(
        "Port {wanted} is in use by another program, so this transfer listens on port {used}. \
         It still works, but the port opened on the router does not help; choose another with --port."
    ))
}

/// Waits until the endpoint has a relay to be reached through, up to `limit`.
/// Returns false if it is still offline; direct addresses may work anyway.
pub async fn wait_online(endpoint: &Endpoint, opts: &NetOptions, limit: Duration) -> bool {
    if opts.relay == RelayChoice::Disabled {
        return true;
    }
    tokio::time::timeout(limit, endpoint.online()).await.is_ok()
}

/// Calls `report` whenever data on this connection switches between a
/// direct path and the relay, for as long as the handle lives.
pub(crate) fn watch_route(conn: Connection, report: impl Fn(Route) + Send + 'static) -> AbortOnDropHandle<()> {
    AbortOnDropHandle::new(tokio::spawn(async move {
        let mut paths = conn.paths_stream();
        let mut last = None;
        while let Some(list) = paths.next().await {
            let relayed = (&list).into_iter().find(|p| p.is_selected()).map(|p| p.is_relay());
            if relayed.is_some() && relayed != last {
                report(if relayed == Some(true) { Route::Relay } else { Route::Direct });
                last = relayed;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_addresses_are_read_with_or_without_port() {
        let addrs = parse_public_addrs("203.0.113.7, 198.51.100.2:5000 203.0.113.7:43217", Some(43217)).unwrap();
        assert_eq!(
            addrs,
            vec!["203.0.113.7:43217".parse::<SocketAddr>().unwrap(), "198.51.100.2:5000".parse().unwrap()]
        );
        assert!(parse_public_addrs("203.0.113.7", None).is_err(), "an IP alone needs a fixed port");
        assert!(parse_public_addrs("router.example", Some(1)).is_err());
        assert!(parse_port("0").is_err());
        assert_eq!(parse_port(" 43217 ").unwrap(), 43217);
    }

    #[tokio::test]
    async fn a_fixed_port_is_used_and_a_busy_one_falls_back() {
        // A port that is free right now, then taken by the first endpoint.
        let port = std::net::UdpSocket::bind("0.0.0.0:0").unwrap().local_addr().unwrap().port();
        let opts = NetOptions { relay: RelayChoice::Disabled, port: Some(port), ..Default::default() };
        let (first, _) = bind(crate::access::SenderKeys::random().secret_key(), &opts).await.unwrap();
        assert!(first.bound_sockets().iter().any(|s| s.is_ipv4() && s.port() == port));
        let (second, _) = bind(crate::access::SenderKeys::random().secret_key(), &opts).await.unwrap();
        assert!(second.bound_sockets().iter().all(|s| s.port() != port));
        assert!(port_note(&first, &opts).is_none());
        let note = port_note(&second, &opts).expect("the busy port is told");
        assert!(note.contains(&format!("Port {port} is in use")), "{note}");
        first.close().await;
        second.close().await;
    }

    #[tokio::test]
    async fn a_public_address_is_announced() {
        let public: SocketAddr = "203.0.113.7:43217".parse().unwrap();
        let opts = NetOptions { relay: RelayChoice::Disabled, public_addrs: vec![public], ..Default::default() };
        let (endpoint, _) = bind(crate::access::SenderKeys::random().secret_key(), &opts).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !endpoint.addr().ip_addrs().any(|a| *a == public) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(endpoint.addr().ip_addrs().any(|a| *a == public), "{:?}", endpoint.addr());
        endpoint.close().await;
    }
}
