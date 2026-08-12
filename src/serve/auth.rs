//! Loopback authentication for the local app's HTTP head — re-establishing in
//! userspace what the control socket got from the OS for free.
//!
//! [`crate::control`] has never carried authentication, and correctly so: a Unix
//! socket is gated by filesystem permissions and a Windows named pipe by its
//! DACL, so *being able to open the channel* *is* the credential. Native clients
//! inherit that protection by being local processes.
//!
//! An HTTP port inherits none of it. Two distinct callers appear the moment the
//! same façade is bound to a socket the network stack will route:
//!
//! 1. **any other process on the machine** — loopback has no peer credential we
//!    check, so a different user's process can connect; and
//! 2. **any web page the user visits** — this is the sharp one. A page cannot
//!    read a cross-origin response, but it can *send* the request, and DNS
//!    rebinding (`evil.com` re-resolving to `127.0.0.1`) is specifically designed
//!    to make the browser treat us as same-origin and hand over the reply.
//!
//! So this module reconstructs the socket's implicit guarantee out of three
//! explicit ones. They are defence in depth: each closes a hole the others leave.
//!
//! - **Bind loopback only.** The caller binds `127.0.0.1`, never `0.0.0.0` —
//!   otherwise the LAN gets a vote. Not enforced here; see [`super::run`].
//! - **A per-run bearer token** ([`Guard::check_token`]). Minted at startup,
//!   never persisted, handed to exactly one browser through the opened URL. This
//!   is what stops the *other local process*, which can reach the port but cannot
//!   guess 32 random bytes.
//! - **A strict `Host`/`Origin` allowlist** ([`Guard::check_origin`]). This is
//!   the part that actually defeats rebinding, and it is worth being precise
//!   about *why* the token alone does not: after a successful rebind the browser
//!   believes `evil.com` *is* our origin, so it will attach our cookie to the
//!   attacker's request. The token stops being a secret the attacker lacks. What
//!   the attacker cannot forge is the `Host` header — the browser derives it from
//!   the URL the attacker had to use, so a rebound request arrives stamped
//!   `Host: evil.com` and we refuse it before the token is ever consulted.
//!
//! Both checks are pure functions over header values precisely so the policy is
//! unit-testable without binding a port — the same shape as
//! [`crate::control::check_control_protocol`] and [`crate::sync`]'s version gate.

/// Mint a 32-byte hexadecimal credential for one loopback-server run.
///
/// Kept beside [`Guard`] so every local HTTP surface uses the same credential
/// strength and none persists it by accident.
pub fn mint_token() -> anyhow::Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf)
        .map_err(|error| anyhow::anyhow!("system entropy unavailable: {error}"))?;
    Ok(data_encoding::HEXLOWER.encode(&buf))
}

/// How long a launch credential is good for.
///
/// Long enough that a browser cold-starting on a slow machine still arrives in
/// time, short enough that a URL sitting in shell history is worthless by the
/// time anybody reads it.
pub const LAUNCH_TICKET_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30);

/// A credential minted for exactly one launch.
///
/// The run token authorises everything this head serves, for as long as it
/// serves it. That is the right shape for a head a person opened themselves,
/// and the wrong shape entirely for a URL handed to a browser: a launch URL
/// lands in browser history, in a synchronised profile, and in the shell's
/// recent-documents list. What travels there must be worth nothing shortly
/// after it is used, and must name only the Orbit it was minted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTicket {
    /// The secret itself, carried in the launch URL.
    pub secret: String,
    /// The Orbit this ticket admits to, and only this one.
    pub orbit: String,
    pub expires_at_ms: u64,
}

/// The tickets one head has minted and not yet seen spent.
///
/// Redemption *consumes*: a ticket answers once. Replay from a browser's
/// history therefore fails closed, which is the property that makes putting a
/// credential in a URL defensible at all.
#[derive(Debug, Default)]
pub struct LaunchTickets {
    live: std::sync::Mutex<std::collections::HashMap<String, LaunchTicket>>,
}

impl LaunchTickets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a ticket admitting to `orbit`, expiring `lifetime` from `now_ms`.
    pub fn mint(
        &self,
        orbit: impl Into<String>,
        lifetime: std::time::Duration,
        now_ms: u64,
    ) -> anyhow::Result<LaunchTicket> {
        let ticket = LaunchTicket {
            secret: mint_token()?,
            orbit: orbit.into(),
            expires_at_ms: now_ms
                .saturating_add(u64::try_from(lifetime.as_millis()).unwrap_or(u64::MAX)),
        };
        let mut live = lock_recovering(&self.live);
        // Expired tickets are swept on mint rather than by a timer: the map only
        // grows when somebody launches, so the moment of growth is the only
        // moment sweeping is worth doing.
        live.retain(|_, held| held.expires_at_ms > now_ms);
        live.insert(ticket.secret.clone(), ticket.clone());
        Ok(ticket)
    }

    /// Spend `presented`, returning what it admits to.
    ///
    /// `None` when it is unknown, already spent, or expired — deliberately one
    /// answer for all three, because telling them apart tells a caller which
    /// guess was closer.
    pub fn redeem(&self, presented: &str, now_ms: u64) -> Option<LaunchTicket> {
        let mut live = lock_recovering(&self.live);
        let ticket = live.remove(presented)?;
        (ticket.expires_at_ms > now_ms).then_some(ticket)
    }

    /// How many tickets are minted and unspent. For tests and diagnostics.
    pub fn outstanding(&self) -> usize {
        lock_recovering(&self.live).len()
    }
}

fn lock_recovering<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The loopback origins we answer to, rendered for a given port.
///
/// A browser sends whichever spelling appears in the URL bar, so all three
/// spellings of "this machine" are legitimate; anything else is not us. Note
/// that this is an *allowlist*: the failure mode of a missing entry is a refused
/// request the user can see, whereas the failure mode of a permissive match is
/// silent and remote.
fn loopback_authorities(port: u16) -> [String; 3] {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

/// Why a request was refused. Carries a human reason because these land in the
/// operator's terminal, and "403" alone has taught nobody anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No `Host` header at all — HTTP/1.1 requires one; a client without one is
    /// not a browser we serve.
    MissingHost,
    /// `Host` is not a loopback authority for our port. The rebinding signature.
    ForeignHost,
    /// `Origin` is present and is not us — a cross-origin caller.
    ForeignOrigin,
    /// No credential, or the wrong one.
    BadToken,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::MissingHost => "request has no Host header",
            Refusal::ForeignHost => {
                "Host is not this server's loopback authority (DNS-rebinding guard)"
            }
            Refusal::ForeignOrigin => "Origin is cross-site; this server is same-origin only",
            Refusal::BadToken => "missing or invalid token",
        }
    }
}

/// The per-run loopback credential and origin policy.
pub struct Guard {
    token: String,
    authorities: [String; 3],
}

impl Guard {
    pub fn new(token: String, port: u16) -> Self {
        Self {
            token,
            authorities: loopback_authorities(port),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Enforce the rebinding guard: the request must be addressed to *us*, by a
    /// loopback name, and must not be initiated by another site.
    ///
    /// `Origin` absent is allowed on purpose. Browsers omit it on same-origin
    /// GETs (including the `EventSource` handshake in some engines), and a
    /// non-browser client like `curl` never sends one — but neither can a
    /// non-browser client be *tricked* into carrying our cookie, which is the
    /// only attack this pair exists to stop. When `Origin` *is* present it is
    /// authoritative and must be us: a cross-origin `fetch` always sends it, so
    /// its presence-and-mismatch is a positive signal, not an absence.
    ///
    /// `Host`, by contrast, is mandatory. It is the single field a rebinding
    /// attacker cannot launder, because the browser fills it in from the URL
    /// they were forced to navigate to.
    pub fn check_origin(&self, host: Option<&str>, origin: Option<&str>) -> Result<(), Refusal> {
        let Some(host) = host else {
            return Err(Refusal::MissingHost);
        };
        if !self.authorities.iter().any(|a| a == host) {
            return Err(Refusal::ForeignHost);
        }
        if let Some(origin) = origin {
            let ok = self
                .authorities
                .iter()
                .any(|a| origin == format!("http://{a}"));
            if !ok {
                return Err(Refusal::ForeignOrigin);
            }
        }
        Ok(())
    }

    /// The stricter Origin check a WebSocket upgrade needs.
    ///
    /// [`Self::check_origin`] admits an absent Origin, and it is right to: a
    /// `curl` or an MCP client sends none, and refusing them would make the
    /// surface browser-only. An upgrade is the case where that reasoning
    /// inverts. A WebSocket handshake is **exempt from CORS** — the browser
    /// sends it cross-origin without a preflight and hands over our cookie —
    /// so Origin is the whole defence, and an absent one is not a non-browser
    /// client but an attacker who simply did not send it.
    ///
    /// Required, and required to be one of ours.
    pub fn check_upgrade_origin(
        &self,
        host: Option<&str>,
        origin: Option<&str>,
    ) -> Result<(), Refusal> {
        self.check_origin(host, origin)?;
        let Some(origin) = origin else {
            return Err(Refusal::ForeignOrigin);
        };
        if self
            .authorities
            .iter()
            .any(|a| origin == format!("http://{a}"))
        {
            Ok(())
        } else {
            Err(Refusal::ForeignOrigin)
        }
    }

    /// Check a presented credential against this run's token.
    ///
    /// Compared in constant time. The window is admittedly narrow — an attacker
    /// who can time this can usually also read our stdout — but a token check
    /// that leaks its prefix through early return is the kind of thing that is
    /// free to do right and embarrassing to explain later.
    pub fn check_token(&self, presented: Option<&str>) -> Result<(), Refusal> {
        match presented {
            Some(t) if ct_eq(t.as_bytes(), self.token.as_bytes()) => Ok(()),
            _ => Err(Refusal::BadToken),
        }
    }
}

/// Constant-time byte equality. Length is not secret (the token is fixed-width),
/// so an early length return is fine; the *content* comparison is not allowed to
/// short-circuit.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extract our cookie's value from a `Cookie` header.
///
/// Hand-rolled rather than pulling a cookie crate: we need exactly one name out
/// of a `; `-separated list, and the parse is four lines. Tolerates the whitespace
/// browsers actually emit and ignores every other cookie on the jar.
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == name).then(|| v.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORT: u16 = 7717;

    fn guard() -> Guard {
        Guard::new("s3cret-token".into(), PORT)
    }

    #[test]
    fn accepts_every_loopback_spelling_the_url_bar_can_produce() {
        let g = guard();
        for host in ["127.0.0.1:7717", "localhost:7717", "[::1]:7717"] {
            assert!(
                g.check_origin(Some(host), None).is_ok(),
                "{host} should be accepted"
            );
        }
    }

    #[test]
    fn rebound_host_is_refused_before_any_credential_is_considered() {
        // The DNS-rebinding signature: evil.com has re-resolved to 127.0.0.1, so
        // the packet genuinely arrives on our loopback socket and the browser
        // will happily attach our cookie — but it stamps the attacker's name in
        // Host, which is the one field they cannot launder.
        let g = guard();
        assert_eq!(
            g.check_origin(Some("evil.com"), None),
            Err(Refusal::ForeignHost)
        );
        // Right name, wrong port: a different local service, not us.
        assert_eq!(
            g.check_origin(Some("127.0.0.1:9999"), None),
            Err(Refusal::ForeignHost)
        );
        // A bare loopback host with no port is not our authority either.
        assert_eq!(
            g.check_origin(Some("127.0.0.1"), None),
            Err(Refusal::ForeignHost)
        );
    }

    #[test]
    fn missing_host_is_refused() {
        assert_eq!(guard().check_origin(None, None), Err(Refusal::MissingHost));
    }

    #[test]
    fn cross_origin_caller_is_refused_even_when_it_addresses_us_correctly() {
        // A page on evil.com fetching http://127.0.0.1:7717 directly: Host is
        // legitimately ours, so only Origin distinguishes it.
        let g = guard();
        assert_eq!(
            g.check_origin(Some("127.0.0.1:7717"), Some("http://evil.com")),
            Err(Refusal::ForeignOrigin)
        );
        // https on a loopback authority is still not our origin (we are http).
        assert_eq!(
            g.check_origin(Some("127.0.0.1:7717"), Some("https://127.0.0.1:7717")),
            Err(Refusal::ForeignOrigin)
        );
        // A same-origin Origin is fine.
        assert!(g
            .check_origin(Some("127.0.0.1:7717"), Some("http://127.0.0.1:7717"))
            .is_ok());
    }

    #[test]
    fn absent_origin_is_allowed_so_same_origin_gets_and_curl_still_work() {
        assert!(guard().check_origin(Some("localhost:7717"), None).is_ok());
    }

    #[test]
    fn token_must_match_exactly() {
        let g = guard();
        assert!(g.check_token(Some("s3cret-token")).is_ok());
        assert_eq!(g.check_token(Some("s3cret-toke")), Err(Refusal::BadToken));
        assert_eq!(g.check_token(Some("s3cret-tokeX")), Err(Refusal::BadToken));
        assert_eq!(g.check_token(Some("")), Err(Refusal::BadToken));
        assert_eq!(g.check_token(None), Err(Refusal::BadToken));
    }

    #[test]
    fn cookie_parses_out_of_a_realistic_jar() {
        assert_eq!(
            cookie_value("other=1; lait_token=abc123; third=x", "lait_token"),
            Some("abc123")
        );
        assert_eq!(
            cookie_value("lait_token=abc123", "lait_token"),
            Some("abc123")
        );
        assert_eq!(cookie_value("other=1", "lait_token"), None);
        assert_eq!(cookie_value("", "lait_token"), None);
        // A cookie whose *value* contains our name must not be mistaken for it.
        assert_eq!(cookie_value("x=lait_token=no", "lait_token"), None);
    }

    #[test]
    fn ct_eq_agrees_with_plain_equality() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }
}

#[cfg(test)]
mod upgrade_origin {
    use super::*;

    const PORT: u16 = 7717;

    fn guard() -> Guard {
        Guard::new("s3cret-token".into(), PORT)
    }

    #[test]
    fn an_upgrade_without_an_origin_is_refused() {
        // The one case where absent-is-fine inverts. A WebSocket handshake is
        // exempt from CORS: the browser sends it cross-origin with no preflight
        // and attaches our cookie, so Origin is the whole defence and an absent
        // one is not a `curl` — it is an attacker who did not send it.
        assert!(guard().check_origin(Some("127.0.0.1:7717"), None).is_ok());
        assert_eq!(
            guard().check_upgrade_origin(Some("127.0.0.1:7717"), None),
            Err(Refusal::ForeignOrigin),
        );
    }

    #[test]
    fn a_foreign_origin_is_refused_however_the_host_reads() {
        for host in ["127.0.0.1:7717", "localhost:7717", "[::1]:7717"] {
            assert_eq!(
                guard().check_upgrade_origin(Some(host), Some("http://evil.example.com")),
                Err(Refusal::ForeignOrigin),
                "{host}"
            );
        }
    }

    #[test]
    fn each_loopback_spelling_upgrades() {
        for host in ["127.0.0.1:7717", "localhost:7717", "[::1]:7717"] {
            assert!(
                guard()
                    .check_upgrade_origin(Some(host), Some(&format!("http://{host}")))
                    .is_ok(),
                "{host}"
            );
        }
    }

    #[test]
    fn the_host_check_still_runs_first() {
        // An upgrade to a rebound Host is refused as a rebind, not as an origin
        // problem — the ordering the shared gate already establishes, kept.
        assert_eq!(
            guard().check_upgrade_origin(Some("evil.example.com"), Some("http://evil.example.com")),
            Err(Refusal::ForeignHost),
        );
        assert_eq!(
            guard().check_upgrade_origin(None, Some("http://127.0.0.1:7717")),
            Err(Refusal::MissingHost),
        );
    }

    #[test]
    fn https_is_not_one_of_ours() {
        // The authorities are compared scheme-and-all. A page served over TLS
        // from a loopback name it does not own is still not us.
        assert_eq!(
            guard().check_upgrade_origin(Some("127.0.0.1:7717"), Some("https://127.0.0.1:7717")),
            Err(Refusal::ForeignOrigin),
        );
    }

    /// Replay is what putting a credential in a URL invites, so redemption
    /// consumes. A launch URL in browser history must be worth nothing.
    #[test]
    fn a_launch_ticket_answers_exactly_once() {
        let tickets = LaunchTickets::new();
        let ticket = tickets
            .mint("orb_one", LAUNCH_TICKET_LIFETIME, 1_000)
            .expect("mint");

        let redeemed = tickets.redeem(&ticket.secret, 1_100).expect("first use");
        assert_eq!(redeemed.orbit, "orb_one");
        assert!(
            tickets.redeem(&ticket.secret, 1_100).is_none(),
            "a spent ticket was accepted a second time"
        );
    }

    #[test]
    fn an_expired_ticket_is_refused_and_an_unknown_one_is_indistinguishable() {
        let tickets = LaunchTickets::new();
        let ticket = tickets
            .mint("orb_one", std::time::Duration::from_secs(30), 1_000)
            .expect("mint");
        assert!(
            tickets.redeem(&ticket.secret, 1_000 + 30_001).is_none(),
            "an expired ticket was accepted"
        );
        assert!(tickets.redeem("not-a-ticket", 1_000).is_none());
    }

    /// A ticket admits to the Orbit it was minted for. Scope is the whole
    /// difference between this and the run-wide token.
    #[test]
    fn a_ticket_names_one_orbit() {
        let tickets = LaunchTickets::new();
        let one = tickets
            .mint("orb_one", LAUNCH_TICKET_LIFETIME, 0)
            .expect("one");
        let two = tickets
            .mint("orb_two", LAUNCH_TICKET_LIFETIME, 0)
            .expect("two");
        assert_ne!(one.secret, two.secret);
        assert_eq!(
            tickets.redeem(&one.secret, 1).expect("redeem one").orbit,
            "orb_one"
        );
        assert_eq!(
            tickets.redeem(&two.secret, 1).expect("redeem two").orbit,
            "orb_two"
        );
    }

    /// Unspent tickets would otherwise accumulate for the life of the head.
    #[test]
    fn minting_sweeps_what_has_expired() {
        let tickets = LaunchTickets::new();
        for _ in 0..4 {
            tickets
                .mint("orb_one", std::time::Duration::from_secs(30), 0)
                .expect("mint");
        }
        assert_eq!(tickets.outstanding(), 4);
        tickets
            .mint("orb_one", std::time::Duration::from_secs(30), 60_000)
            .expect("mint later");
        assert_eq!(
            tickets.outstanding(),
            1,
            "expired tickets were kept alive by a later mint"
        );
    }
}
