//! **Hosting a relay** — the server half of [`policy::Network::Local`].
//!
//! [`policy`] already knows how to *use* a relay lait supplies: `LAIT_NETWORK=local`
//! plus `LAIT_RELAY=<url>` and every peer rendezvouses through it with no discovery
//! service at all. What has never existed is the other end. Until now "a relay lait
//! supplies" meant an in-process test fixture or somebody else's deployment, which
//! makes a local-first tool depend on infrastructure its users do not own.
//!
//! This is that other end, and it belongs here for the same reason the client half
//! does: this crate is the only one that names a concrete network, and hosting a
//! relay is network mechanism. A consumer configures a [`RelayHome`] and gets a
//! [`RunningRelay`] — no vendor type crosses the boundary, so replacing the
//! contractor stays a change in this crate and nowhere else.
//!
//! **The deployment shapes, and which to pick.**
//!
//! - [`RelayCertificate::None`] serves plain HTTP. Correct when something else
//!   terminates TLS — a reverse proxy, a Cloudflare tunnel, a service mesh — and
//!   correct on a LAN where there is no public name to certify. `policy` accepts
//!   an `http://` relay URL, so this is a complete deployment, not a degraded one.
//! - [`RelayCertificate::Automatic`] obtains a Let's Encrypt certificate for a
//!   domain the operator controls. This is the one-command public relay: point a
//!   DNS name at the box, name it here, and peers can use `https://`.
//!
//! There is deliberately no "self-signed" shape. `policy` requires a CA-valid
//! certificate under `Local` and gates the verification skip to test builds — a
//! self-signed option here would exist only to be refused there.
//!
//! [`policy`]: crate::policy
//! [`policy::Network::Local`]: crate::policy::Network::Local

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

/// How a relay proves it is the host peers were told to expect.
#[derive(Debug, Clone)]
pub enum RelayCertificate {
    /// Serve plain HTTP and let something in front handle TLS.
    ///
    /// Not an insecure mode in the sense that matters: relay traffic is already
    /// end-to-end encrypted between the two peers, and the relay is a forwarder
    /// that cannot read it. What TLS protects here is the *metadata* — who is
    /// talking to whom — and behind a proxy or on a trusted LAN that is a
    /// deliberate choice rather than an oversight.
    None,
    /// Obtain and renew a Let's Encrypt certificate over ACME.
    ///
    /// Requires the domains to already resolve to this host and port 80 to be
    /// reachable, because that is how the challenge is answered.
    Automatic {
        /// Where the HTTPS service binds. Conventionally port 443.
        https: SocketAddr,
        /// The names to certify. The first is what peers are told to use.
        domains: Vec<String>,
        /// Contact addresses for the ACME account (`mailto:` URIs).
        contact: Vec<String>,
        /// Where to persist the account key and certificate.
        ///
        /// **Set this.** Without it every restart re-runs the ACME order, and
        /// Let's Encrypt's rate limits turn a restart loop into a multi-hour
        /// outage. `None` is honoured rather than defaulted because a cache path
        /// is a filesystem decision this module has no business making.
        cache: Option<PathBuf>,
        /// Use the staging directory, whose certificates no browser trusts and
        /// whose rate limits are generous. The right setting for the first run.
        staging: bool,
    },
}

impl RelayCertificate {
    const fn serves_tls(&self) -> bool {
        matches!(self, Self::Automatic { .. })
    }
}

/// Where a relay lives and what it answers on.
#[derive(Debug, Clone)]
pub struct RelayHome {
    /// Where the HTTP service binds. Conventionally port 80 — and it must stay
    /// reachable even under [`RelayCertificate::Automatic`], because the ACME
    /// challenge is answered there.
    pub http: SocketAddr,
    /// How the relay identifies itself.
    pub certificate: RelayCertificate,
    /// Bind address for the QUIC address-discovery service, which is how a peer
    /// learns the address the outside world sees it as — the observation
    /// holepunching starts from.
    ///
    /// **Requires TLS**, so it is refused alongside [`RelayCertificate::None`]
    /// rather than silently ignored: a relay that quietly failed to offer address
    /// discovery would look like a relay whose peers just never holepunch.
    pub quic: Option<SocketAddr>,
    /// Where to serve Prometheus metrics, if anywhere.
    pub metrics: Option<SocketAddr>,
    /// The host peers should be told to use.
    ///
    /// Needed because a bind address is not a URL: a relay bound to `0.0.0.0`
    /// answers everywhere and is reachable at none of them by that name. When
    /// absent it is derived — the first certified domain under `Automatic`, the
    /// bind address under `None` — and the derivation is only right when the bind
    /// address is one a peer can actually dial.
    pub advertise: Option<String>,
}

impl RelayHome {
    /// A relay on the conventional ports with no TLS of its own.
    pub fn plain(http: SocketAddr) -> Self {
        Self {
            http,
            certificate: RelayCertificate::None,
            quic: None,
            metrics: None,
            advertise: None,
        }
    }

    /// What an operator puts in `LAIT_RELAY`, and what peers dial.
    fn relay_url(&self) -> String {
        if let Some(advertise) = &self.advertise {
            return normalise_url(advertise, self.certificate.serves_tls());
        }
        match &self.certificate {
            RelayCertificate::Automatic { domains, .. } => domains.first().map_or_else(
                || format!("http://{}", self.http),
                |d| format!("https://{d}"),
            ),
            RelayCertificate::None => format!("http://{}", self.http),
        }
    }
}

/// Give a bare host the scheme its certificate implies, and leave an explicit
/// one alone — an operator who wrote `http://` behind a TLS-terminating proxy
/// means it.
fn normalise_url(advertise: &str, tls: bool) -> String {
    if advertise.starts_with("http://") || advertise.starts_with("https://") {
        return advertise.to_string();
    }
    if tls {
        format!("https://{advertise}")
    } else {
        format!("http://{advertise}")
    }
}

/// A relay that is up and serving.
///
/// Dropping this does **not** stop the relay — the server owns its own tasks.
/// Call [`shutdown`](RunningRelay::shutdown) to stop it and find out whether it
/// stopped cleanly.
pub struct RunningRelay {
    server: iroh_relay::server::Server,
    relay_url: String,
}

impl std::fmt::Debug for RunningRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningRelay")
            .field("relay_url", &self.relay_url)
            .finish_non_exhaustive()
    }
}

impl RunningRelay {
    /// What to put in `LAIT_RELAY` on every peer that should use this relay.
    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }

    /// The address the HTTP service actually bound, which is where a `:0` bind
    /// resolves to a real port.
    pub fn http_addr(&self) -> Option<SocketAddr> {
        self.server.http_addr()
    }

    /// The address the HTTPS service actually bound, if it is serving TLS.
    pub fn https_addr(&self) -> Option<SocketAddr> {
        self.server.https_addr()
    }

    /// The address the QUIC address-discovery service actually bound.
    pub fn quic_addr(&self) -> Option<SocketAddr> {
        self.server.quic_addr()
    }

    /// Stop serving and wait for the tasks to finish.
    pub async fn shutdown(self) -> Result<()> {
        self.server.shutdown().await.context("shut down relay")
    }
}

/// Start a relay and return once it is bound and serving.
pub async fn host(home: RelayHome) -> Result<RunningRelay> {
    if home.quic.is_some() && !home.certificate.serves_tls() {
        anyhow::bail!(
            "QUIC address discovery needs TLS: give the relay a certificate, or drop the QUIC address"
        );
    }

    let relay_url = home.relay_url();
    let mut relay = iroh_relay::server::RelayConfig::new(home.http);

    relay.tls = match &home.certificate {
        RelayCertificate::None => None,
        RelayCertificate::Automatic {
            https,
            domains,
            contact,
            cache,
            staging,
        } => {
            if domains.is_empty() {
                anyhow::bail!("an automatic certificate needs at least one domain to certify");
            }
            let mut acme = iroh_relay::server::AcmeConfig::letsencrypt(!*staging)
                .domains(domains.clone())
                .contact(contact.clone());
            if let Some(cache) = cache {
                acme = acme.cache_path(cache.clone());
            }
            Some(iroh_relay::server::TlsConfig::new(
                *https,
                iroh_relay::server::CertConfig::LetsEncrypt {
                    acme_config: acme,
                    server_config_builder: rustls::ServerConfig::builder().with_no_client_auth(),
                },
            ))
        }
    };

    // Both configs are `#[non_exhaustive]`, so they are built and then assigned
    // rather than named field by field: a field added upstream must not become a
    // compile error here, which is exactly what non-exhaustive is asking for.
    let mut config = iroh_relay::server::ServerConfig::default();
    config.relay = Some(relay);
    // `QuicConfig::new` leaves the TLS config unset, which means "inherit the
    // relay's" — the only setting that can be right here, since the guard above
    // already refused the case where there is none to inherit.
    config.quic = home.quic.map(iroh_relay::server::QuicConfig::new);
    config.metrics_addr = home.metrics;

    let server = iroh_relay::server::Server::spawn(config)
        .await
        .context("start relay")?;

    Ok(RunningRelay { server, relay_url })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn a_plain_relay_advertises_http() {
        let home = RelayHome::plain(addr(8080));
        assert_eq!(home.relay_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn an_explicit_advertise_wins_over_the_bind_address() {
        // The case a `0.0.0.0` bind exists for: what it binds and what peers dial
        // are different strings, and only the operator knows the second.
        let mut home = RelayHome::plain(SocketAddr::from(([0, 0, 0, 0], 80)));
        home.advertise = Some("relay.example.com".into());
        assert_eq!(home.relay_url(), "http://relay.example.com");
    }

    #[test]
    fn an_operator_written_scheme_is_left_alone() {
        // Plain HTTP behind a proxy that terminates TLS: the relay serves http,
        // peers dial https, and the operator is the only one who knows that.
        let mut home = RelayHome::plain(addr(80));
        home.advertise = Some("https://relay.example.com".into());
        assert_eq!(home.relay_url(), "https://relay.example.com");
    }

    #[test]
    fn an_automatic_certificate_advertises_its_first_domain() {
        let home = RelayHome {
            http: addr(80),
            certificate: RelayCertificate::Automatic {
                https: addr(443),
                domains: vec!["relay.example.com".into(), "alt.example.com".into()],
                contact: vec!["mailto:ops@example.com".into()],
                cache: None,
                staging: false,
            },
            quic: None,
            metrics: None,
            advertise: None,
        };
        assert_eq!(home.relay_url(), "https://relay.example.com");
    }

    #[tokio::test]
    async fn quic_without_tls_is_refused_rather_than_ignored() {
        let mut home = RelayHome::plain(addr(0));
        home.quic = Some(addr(0));
        let refused = host(home).await;
        assert!(refused.is_err(), "QUIC with no TLS must not start");
    }
}
