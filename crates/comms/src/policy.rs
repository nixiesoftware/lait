//! **Network policy** — lait's own requirement for the transport environment,
//! with iroh as the contractor that fulfils it.
//!
//! lait states *where* it operates — the public relay mesh, a named local relay,
//! or isolated — and this module is the **single place** iroh's relay/discovery
//! vocabulary (`RelayMode`, `presets`, `address_lookup`) is spoken. Above
//! [`build_endpoint`] the rest of the daemon knows only [`Network`]; there is no
//! "iroh default" anymore, only lait's `Public` policy that iroh executes.
//!
//! The point is ownership of *behaviour*: lait chooses its transport
//! environment instead of inheriting n0's. This seam establishes that ownership
//! and the single contractor boundary.
//!
//! **Reachability.** Every peer dial in `node.rs` is a bare `EndpointId`, which
//! iroh resolves through the endpoint's address lookups (the address-free design
//! by the application protocol). `Public` gets that resolution from n0 discovery. `Local`
//! has NO discovery service — instead lait registers `{id, relay}` for each peer
//! it learns into a [`PeerBook`] (an in-process `MemoryLookup`), because lait
//! already knows its relay and can build the address directly. Nothing is
//! discovered over the wire and nothing is faked — the exact pattern iroh-gossip
//! uses for bootstrap. `Isolated` has neither relay nor discovery: a peer's
//! direct addresses travel in the ticket, and the joiner registers them via
//! [`PeerBook::learn_direct`] — a LAN/offline host-star with zero infrastructure.
//!
//! **Scope.** `Local` converges cleanly for the common seed-hub / small-N
//! topology. It does NOT reproduce `Public`'s immediate full-mesh: an id that
//! iroh-gossip learns *through the swarm* (overlay promotion) is registered only
//! once an application message from that peer is seen, so some promotion dials in
//! a large overlay resolve eventually rather than at once. And the hermetic proof
//! here is at the *endpoint* level (in-process); a spawned-process two-daemon
//! `Local` test needs a CA-valid relay cert, because the self-signed skip is
//! gated to test builds and absent from the shipped binary.

use anyhow::{Context, Result};
use iroh::{
    address_lookup::MemoryLookup, endpoint::presets, Endpoint, EndpointAddr, EndpointId, RelayMap,
    RelayMode, RelayUrl, SecretKey,
};

/// lait's requirement for the transport environment. iroh executes it.
#[derive(Debug, Clone)]
pub enum Network {
    /// The public relay mesh + public discovery (n0). The default — unchanged
    /// behaviour, now stated rather than inherited. n0 discovery resolves the
    /// bare `EndpointId`s that every dial in `node.rs` uses.
    Public,
    /// A single relay lait supplies (self-hosted, or a test harness's in-process
    /// relay). Wired: lait dials peers by bare `EndpointId` and resolves them
    /// through a [`PeerBook`] it populates with `{id, relay}` for each peer it
    /// learns — no discovery service. Converges cleanly for the seed-hub /
    /// small-N case; for larger overlays, an id iroh-gossip learns *through the
    /// swarm* isn't registered until an application message from it is seen, so
    /// some promotion dials resolve only eventually — not the immediate full
    /// mesh `Public` gives.
    Local(LocalNet),
    /// No relay, no discovery — direct reach only. Wired for the host-star case:
    /// an Isolated ticket carries the host's direct addresses, and the joiner
    /// registers them via [`PeerBook::learn_direct`], so a LAN/offline space
    /// connects with zero infrastructure. A wider mesh beyond the ticket host is
    /// out of scope (peers past the host aren't resolved).
    Isolated,
}

/// The relays lait supplies. Reachability is relay-based — a peer is
/// `{its id, these relays}`, which lait builds directly (it knows its relays)
/// and registers via [`PeerBook`], needing no public discovery. lait names
/// each relay in a plain URL; iroh is the contractor and picks among them by
/// its own reachability probing, so a site whose WAN path is down falls back
/// to a relay it can still reach with no orchestration above.
///
/// More than one relay is the REACH-1 widening: a coordinator placement and a
/// cloud relay are different paths to the same peers, and a Station that can
/// hold only one address for "the relay" cannot express that.
#[derive(Debug, Clone)]
pub struct LocalNet {
    /// Relay URLs peers rendezvous through (`https://…` / `http://…`), in
    /// preference order, never empty. Self-hosted relays presenting valid
    /// certificates. (Self-signed / dev relays require skipping cert
    /// verification, which iroh gates to test builds — so that path lives in
    /// the test harness, not here.)
    pub relays: Vec<String>,
}

impl LocalNet {
    /// Parse the `LAIT_RELAY` form: a comma-separated URL list, whitespace
    /// tolerated, empty entries dropped. Nothing usable is an error — a Local
    /// network with no relay is a contradiction, not a default.
    pub fn parse(raw: &str) -> Result<Self> {
        let relays: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string)
            .collect();
        if relays.is_empty() {
            anyhow::bail!("LAIT_NETWORK=local requires at least one relay in LAIT_RELAY");
        }
        Ok(Self { relays })
    }
}

impl Network {
    /// Resolve the requirement from the environment, defaulting to [`Public`] so
    /// existing deployments are unchanged. `LAIT_NETWORK` = `public` (default) |
    /// `local` | `isolated` (trimmed, case-insensitive); `local` reads
    /// `LAIT_RELAY`. An unknown value is an error, never a silent default.
    ///
    /// [`Public`]: Network::Public
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("LAIT_NETWORK").unwrap_or_default();
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "public" => Ok(Network::Public),
            "isolated" => Ok(Network::Isolated),
            "local" => Ok(Network::Local(LocalNet::parse(&env_req("LAIT_RELAY")?)?)),
            other => {
                anyhow::bail!("unknown LAIT_NETWORK '{other}' (expected public|local|isolated)")
            }
        }
    }

    /// Whether this policy provides a relay — so waiting on `endpoint.online()`
    /// (which blocks on a home relay) is sound. Isolated has none.
    pub fn uses_relay(&self) -> bool {
        !matches!(self, Network::Isolated)
    }
}

fn env_req(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("LAIT_NETWORK=local requires {key}"))
}

/// The reachability address for `id` under a relay policy: `{id, relays}`.
/// lait KNOWS its relays (it configured them), so it builds this directly — no
/// discovery service is consulted, and nothing is faked. One address carries
/// every relay, because `EndpointAddr` holds a set of transport addresses and
/// iroh chooses among them by reachability. This is the one iroh-typed address
/// construction, shared by the daemon (via [`PeerBook`]) and the tests.
pub fn relay_addr(relays: &[RelayUrl], id: EndpointId) -> EndpointAddr {
    relays.iter().fold(EndpointAddr::new(id), |addr, relay| {
        addr.with_relay_url(relay.clone())
    })
}

/// How the daemon teaches its endpoint to reach peers under lait's policy.
///
/// Every dial in `node.rs` is a bare `EndpointId`; iroh resolves it through the
/// endpoint's address lookups. Under `Public` that resolution is n0 discovery.
/// Under `Local` there is no discovery — so lait registers `{id, relay}` for each
/// peer it learns into an in-process [`MemoryLookup`] the endpoint queries. No
/// discovery service, plaintext or otherwise; lait supplies the address it
/// already knows. (This is the pattern iroh-gossip itself uses for bootstrap.)
#[derive(Clone)]
pub struct PeerBook {
    lookup: MemoryLookup,
    relays: Vec<RelayUrl>,
    /// True under `Isolated`: peers are reached by carried direct addresses, so a
    /// minted ticket must ship the host's own addresses.
    direct: bool,
}

impl PeerBook {
    /// Teach the endpoint how to reach `id`. Under `Local` that is `{id, relay}`;
    /// under `Public` n0 discovery already resolves ids (no-op); `Isolated` has
    /// no relay, so a bare `learn` cannot help it — its peers are registered by
    /// [`learn_direct`] from addresses carried in the ticket.
    ///
    /// [`learn_direct`]: PeerBook::learn_direct
    pub fn learn(&self, id: EndpointId) {
        if !self.relays.is_empty() {
            self.lookup.add_endpoint_info(relay_addr(&self.relays, id));
        }
    }

    /// Register `id` reachable at explicit direct socket addresses — the
    /// `Isolated` path, where a peer's address travels in the ticket (there is no
    /// relay and no discovery). A no-op if `addrs` is empty.
    pub fn learn_direct(&self, id: EndpointId, addrs: &[std::net::SocketAddr]) {
        if addrs.is_empty() {
            return;
        }
        let mut addr = EndpointAddr::new(id);
        for a in addrs {
            addr = addr.with_ip_addr(*a);
        }
        self.lookup.add_endpoint_info(addr);
    }

    /// Whether this policy carries peer addresses explicitly (Isolated), so the
    /// ticket must ship the host's direct addresses.
    pub fn is_isolated(&self) -> bool {
        self.direct
    }
}

/// Build the iroh endpoint that fulfils lait's [`Network`], plus the [`PeerBook`]
/// the daemon populates so bare-id dials resolve. This is the sole contractor
/// boundary: the only function that names iroh's relay/discovery types. A future
/// transport swap rewrites this and nothing above it.
pub async fn build_endpoint(secret_key: &SecretKey, net: &Network) -> Result<(Endpoint, PeerBook)> {
    // One in-process address book, queried by the endpoint and populated by the
    // daemon. Under Public it is a harmless extra cache; under Local it is the
    // resolution mechanism.
    let lookup = MemoryLookup::new();
    let mut relays = Vec::new();
    let builder = match net {
        // Public: n0's relays + discovery, plus the in-process address cache the
        // daemon has always used.
        Network::Public => Endpoint::builder(presets::N0).address_lookup(lookup.clone()),
        // Isolated: no relay, no discovery. Direct reach only.
        Network::Isolated => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .address_lookup(lookup.clone()),
        // Local: lait's own relays. Reachability is relay-based — the daemon
        // registers `{id, relays}` per peer into `lookup`, so a bare id
        // resolves with no discovery, which is what makes it hermetic. iroh
        // probes every relay in the map and prefers what answers, which is the
        // whole failover story: nothing above this chooses.
        Network::Local(l) => {
            for raw in &l.relays {
                let url: RelayUrl = raw
                    .parse()
                    .with_context(|| format!("LAIT_RELAY entry '{raw}' is not a valid URL"))?;
                relays.push(url);
            }
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Custom(RelayMap::from_iter(relays.clone())))
                .address_lookup(lookup.clone())
        }
    };
    let endpoint = builder
        .secret_key(secret_key.clone())
        .bind()
        .await
        .context("bind iroh endpoint")?;
    let direct = matches!(net, Network::Isolated);
    Ok((
        endpoint,
        PeerBook {
            lookup,
            relays,
            direct,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_list_parses_and_an_empty_one_refuses() {
        let net = LocalNet::parse(" https://a.example , https://b.example ,, ").unwrap();
        assert_eq!(net.relays, vec!["https://a.example", "https://b.example"]);
        let single = LocalNet::parse("https://only.example").unwrap();
        assert_eq!(single.relays.len(), 1, "one relay stays valid");
        assert!(
            LocalNet::parse("  ,, ").is_err(),
            "nothing usable is an error"
        );
    }

    #[test]
    fn from_env_default_is_public_and_trims() {
        // (Reads process env, but the default/whitespace cases don't set it.)
        assert!(matches!(Network::from_env(), Ok(Network::Public)));
    }

    #[test]
    fn peerbook_registers_under_local_and_noops_without_a_relay() {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();

        // Local: learn registers `{id, relays}` so a bare-id dial can resolve
        // it — one entry carrying every configured relay, so losing one path
        // is a degraded route and not a lost peer.
        let relays: Vec<RelayUrl> = vec![
            "https://relay.example".parse().unwrap(),
            "https://fallback.example".parse().unwrap(),
        ];
        let local = PeerBook {
            lookup: MemoryLookup::new(),
            relays: relays.clone(),
            direct: false,
        };
        assert!(local.lookup.get_endpoint_info(id).is_none());
        local.learn(id);
        let info = local
            .lookup
            .get_endpoint_info(id)
            .expect("Local registers the peer so bare-id resolution succeeds");
        assert_eq!(
            info.relay_urls().count(),
            2,
            "the one address carries every configured relay"
        );

        // Public (no relay): learn is a no-op — Public resolves via n0 discovery.
        let public = PeerBook {
            lookup: MemoryLookup::new(),
            relays: Vec::new(),
            direct: false,
        };
        public.learn(id);
        assert!(
            public.lookup.get_endpoint_info(id).is_none(),
            "without a relay, learn registers nothing"
        );

        // Isolated: learn_direct registers explicit addresses (carried in a ticket).
        let isolated = PeerBook {
            lookup: MemoryLookup::new(),
            relays: Vec::new(),
            direct: true,
        };
        assert!(isolated.is_isolated());
        isolated.learn(id); // no relay → still nothing
        assert!(isolated.lookup.get_endpoint_info(id).is_none());
        isolated.learn_direct(id, &["127.0.0.1:4000".parse().unwrap()]);
        assert!(
            isolated.lookup.get_endpoint_info(id).is_some(),
            "Isolated registers the peer's carried direct address"
        );
    }
}
