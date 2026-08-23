//! The label registry: public names for identities, and where they answer.
//!
//! A sibling of the address directory, and deliberately its opposite on the
//! two axes that define it. An address is *issued, never chosen*, and its
//! existence is a secret the refusal shape protects; a label is **chosen** —
//! `acme` — and its existence is **public**, because the label's whole job is
//! to be printed on a card, typed into a television, and resolved by a router
//! that has never met anybody. Those are different services with different
//! obligations, which is why this is a separate surface with its own rules
//! rather than a mode of the directory.
//!
//! What it shares with the directory is the position that matters: **evidence,
//! never authority.** A publication carries the identity's own announcement,
//! verified against its self-certifying genesis; the route is signed by a
//! device that genesis roots. The registry can withhold and it can deny — it
//! cannot author. A compromise is a denial, which is detectable, not an
//! impersonation.
//!
//! Allocation is curated for the first wave: a label→profile binding is an
//! operator act on the store, not a route. Open registration arrives with the
//! rendezvous design or not at all, and nothing here forecloses either.
//!
//! Resolution is public and unauthenticated, and the label grammar is one DNS
//! label because a label *is* one: `acme` resolves wherever
//! `acme.<deployment root>` routes. Which also means the label space is
//! walkable — a deliberate, recorded trade: existence leaks, content never
//! does, and the ceremony above this is what admits a receiver, not the name.

use mechanics::kinship::{KinshipLog, ProfileId};
use serde::{Deserialize, Serialize};

use crate::wire::{framed, verify};
use crate::Refusal;

/// Domain separator for the route signature.
const ROUTE_DOMAIN: &[u8] = b"lait-registry/route/v1";

/// Labels no customer may hold: infrastructure names this deployment already
/// answers on, and names whose only use is impersonating it.
pub const RESERVED: &[&str] = &[
    "admin",
    "api",
    "astrolabe",
    "directory",
    "dist",
    "foundation",
    "mail",
    "post",
    "registry",
    "relay",
    "router",
    "smtp",
    "www",
];

/// One DNS label: what a person reads off a card and types with a remote.
///
/// The same grammar the receivers' site provisioning validates, because they
/// are the same value at two ends of one wire.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Label(String);

impl Label {
    pub fn parse(value: impl Into<String>) -> Result<Self, Refusal> {
        let value = value.into();
        let valid = (1..=32).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-');
        if !valid {
            return Err(Refusal::Malformed);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn reserved(&self) -> bool {
        RESERVED.contains(&self.0.as_str())
    }
}

impl TryFrom<String> for Label {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value).map_err(|_| "invalid label".to_string())
    }
}

impl From<Label> for String {
    fn from(label: Label) -> Self {
        label.0
    }
}

/// What resolving a label answers: who, and where they answer right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub label: Label,
    pub profile: String,
    /// The identity's display-plane endpoint: a device id, the overlay
    /// address a router dials. `PeerId = DeviceId` is the comms identity rule,
    /// and the registry speaks the same vocabulary rather than a hex re-spelling.
    pub endpoint: String,
    pub epoch: u64,
}

/// A signed route publication: this identity answers at this endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePublish {
    pub label: Label,
    /// The identity's announcement, exactly as encoded by its publisher.
    #[serde(with = "crate::wire::hex_bytes")]
    pub announcement: Vec<u8>,
    /// The display-plane endpoint id, 32 bytes hex.
    pub endpoint: String,
    pub epoch: u64,
    /// The device presenting this — it must be one the genesis roots.
    pub device: mechanics::ids::DeviceId,
    #[serde(with = "crate::wire::hex_signature")]
    pub signature: [u8; 64],
}

impl RoutePublish {
    pub(crate) fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        framed(&mut out, ROUTE_DOMAIN);
        framed(&mut out, self.label.as_str().as_bytes());
        framed(&mut out, self.endpoint.as_bytes());
        framed(&mut out, &self.epoch.to_be_bytes());
        framed(&mut out, self.device.as_str().as_bytes());
        out
    }

    /// Sign a route publication as `seed`'s device.
    #[must_use]
    pub fn sign(
        label: Label,
        announcement: Vec<u8>,
        endpoint: String,
        epoch: u64,
        seed: &[u8; 32],
    ) -> Self {
        let device = mechanics::actor::device_from_seed(seed);
        let mut publish = Self {
            label,
            announcement,
            endpoint,
            epoch,
            device,
            signature: [0; 64],
        };
        publish.signature = mechanics::actor::sign_detached(seed, &publish.preimage());
        publish
    }
}

/// The registry's persistence surface. `&mut` for the same reason the
/// directory's store is: the network-backed implementation holds a credential
/// it refreshes.
pub trait RegistryStore {
    /// The profile bound to `label`, if an operator has bound one.
    fn binding(&mut self, label: &Label) -> anyhow::Result<Option<ProfileId>>;

    /// Bind `label` to `profile` — the curated allocation act. `Ok(false)`
    /// when the label is already bound (to anyone): a binding never moves
    /// through this path, transfer is a deliberate operator act elsewhere.
    fn bind(&mut self, label: &Label, profile: &ProfileId) -> anyhow::Result<bool>;

    /// The current route record for `label`.
    fn route(&mut self, label: &Label) -> anyhow::Result<Option<Resolved>>;

    /// Record a verified route. `Ok(false)` when `epoch` does not advance the
    /// held one — a replay is refused without an error a prober can read.
    fn record_route(&mut self, resolved: &Resolved) -> anyhow::Result<bool>;
}

/// Verify and apply one route publication against a store.
///
/// The registry holds no key and consults no authority: the announcement's
/// profile id is re-derived from the genesis it carries, the presenting
/// device must be one that genesis roots, and the label must already be bound
/// to that profile by the curated act. Everything checkable is checked; what
/// is not checkable is not stored.
pub fn publish_route<S: RegistryStore>(
    store: &mut S,
    publish: &RoutePublish,
) -> Result<Resolved, Refusal> {
    if publish.label.reserved() {
        return Err(Refusal::NotAvailable);
    }
    // The endpoint must be a canonically spelled device id: the router dials
    // it as a peer, and a re-spelling would resolve in a map while missing
    // every string-keyed store. Same rule as the directory's `canonical_key`.
    match mechanics::ids::DeviceId::parse(&publish.endpoint) {
        Some(parsed) if parsed.as_str() == publish.endpoint => {}
        _ => return Err(Refusal::Malformed),
    }
    // The announcement is the identity's own evidence: decode, and re-derive
    // the profile from the genesis rather than trusting the field.
    let announcement =
        addressbook::Announcement::decode(&publish.announcement).map_err(|_| Refusal::Malformed)?;
    let log = KinshipLog::found(announcement.genesis.clone()).map_err(|_| Refusal::NotAuthentic)?;
    if log.profile() != &announcement.profile {
        return Err(Refusal::NotAuthentic);
    }
    // The presenting device must be rooted by that genesis, and the route
    // signature must be its own.
    if !announcement
        .genesis
        .devices
        .iter()
        .any(|device| device == &publish.device)
    {
        return Err(Refusal::NotAuthentic);
    }
    verify(&publish.device, &publish.preimage(), &publish.signature)?;
    // The label must be bound to this profile by the curated act. An unbound
    // label and a differently-bound label answer identically: this route is
    // not a claim path.
    let bound = store
        .binding(&publish.label)
        .map_err(|error| Refusal::Unavailable(error.to_string()))?;
    if bound.as_ref() != Some(&announcement.profile) {
        return Err(Refusal::NotAvailable);
    }
    let resolved = Resolved {
        label: publish.label.clone(),
        profile: announcement.profile.as_str().to_string(),
        endpoint: publish.endpoint.clone(),
        epoch: publish.epoch,
    };
    if !store
        .record_route(&resolved)
        .map_err(|error| Refusal::Unavailable(error.to_string()))?
    {
        return Err(Refusal::NotAvailable);
    }
    Ok(resolved)
}

/// The registry's HTTP surface, mounted beside the directory's router.
///
/// Resolution is a public `GET` because a label's existence is public by
/// design; publication carries its own evidence and needs no session. The
/// refusal shape borrows the directory's coarse wire form so a store failure
/// never explains itself to a prober.
pub fn router<S: RegistryStore + Send + 'static>(
    store: std::sync::Arc<std::sync::Mutex<S>>,
) -> axum::Router {
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::Json;

    type Held<S> = std::sync::Arc<std::sync::Mutex<S>>;

    async fn resolve<S: RegistryStore + Send + 'static>(
        State(store): State<Held<S>>,
        Path(label): Path<String>,
    ) -> Result<Json<Resolved>, StatusCode> {
        let label = Label::parse(label).map_err(|_| StatusCode::NOT_FOUND)?;
        let mut store = store.lock().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        match store.route(&label) {
            Ok(Some(resolved)) => Ok(Json(resolved)),
            Ok(None) => Err(StatusCode::NOT_FOUND),
            Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    async fn publish<S: RegistryStore + Send + 'static>(
        State(store): State<Held<S>>,
        Json(publish): Json<RoutePublish>,
    ) -> Result<Json<Resolved>, (StatusCode, Json<crate::http::Refused>)> {
        let mut store = store.lock().map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(crate::http::Refused::Unavailable),
            )
        })?;
        publish_route(&mut *store, &publish)
            .map(Json)
            .map_err(|refusal| {
                (
                    StatusCode::FORBIDDEN,
                    Json(crate::http::Refused::from(&refusal)),
                )
            })
    }

    axum::Router::new()
        .route("/registry/{label}", get(resolve::<S>))
        .route("/registry/route", post(publish::<S>))
        .with_state(store)
}

/// Publish a route over the registry's HTTP surface, as a client.
///
/// Blocking, like every network call in this tree — callers on an async
/// runtime hop through `spawn_blocking`. The coarse error carries no more
/// than an operator log needs.
pub fn publish_over_http(base: &str, publish: &RoutePublish) -> anyhow::Result<Resolved> {
    let response = ureq::post(&format!("{}/registry/route", base.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(10))
        .send_json(serde_json::to_value(publish)?)
        .map_err(|error| anyhow::anyhow!("registry refused the route: {error}"))?;
    Ok(response.into_json()?)
}

/// In-memory store, enforcing the same rules a backing store must.
#[derive(Debug, Default)]
pub struct MemRegistry {
    bindings: std::collections::BTreeMap<String, ProfileId>,
    routes: std::collections::BTreeMap<String, Resolved>,
}

impl RegistryStore for MemRegistry {
    fn binding(&mut self, label: &Label) -> anyhow::Result<Option<ProfileId>> {
        Ok(self.bindings.get(label.as_str()).cloned())
    }

    fn bind(&mut self, label: &Label, profile: &ProfileId) -> anyhow::Result<bool> {
        if self.bindings.contains_key(label.as_str()) {
            return Ok(false);
        }
        self.bindings
            .insert(label.as_str().to_string(), profile.clone());
        Ok(true)
    }

    fn route(&mut self, label: &Label) -> anyhow::Result<Option<Resolved>> {
        Ok(self.routes.get(label.as_str()).cloned())
    }

    fn record_route(&mut self, resolved: &Resolved) -> anyhow::Result<bool> {
        if let Some(held) = self.routes.get(resolved.label.as_str()) {
            if resolved.epoch <= held.epoch {
                return Ok(false);
            }
        }
        self.routes
            .insert(resolved.label.as_str().to_string(), resolved.clone());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::kinship::DeviceLink;

    const SEED_A: [u8; 32] = [21u8; 32];
    const SEED_B: [u8; 32] = [22u8; 32];
    const STRANGER: [u8; 32] = [66u8; 32];

    fn identity_from(a: [u8; 32], b: [u8; 32]) -> (addressbook::Announcement, ProfileId) {
        let genesis = DeviceLink::seal(&a, &b, [7u8; 16], 1).expect("seal genesis");
        let log = KinshipLog::found(genesis.clone()).expect("found");
        let profile = log.profile().clone();
        let projection = log
            .project(&a, 1, &mechanics::kinship::Standing::default())
            .expect("project");
        (
            addressbook::Announcement::new(profile.clone(), genesis, projection),
            profile,
        )
    }

    fn identity() -> (addressbook::Announcement, ProfileId) {
        identity_from(SEED_A, SEED_B)
    }

    fn endpoint() -> String {
        mechanics::actor::device_from_seed(&[77u8; 32])
            .as_str()
            .to_string()
    }

    #[test]
    fn a_bound_label_resolves_and_a_replay_does_not_roll_it_back() {
        let (announcement, profile) = identity();
        let mut store = MemRegistry::default();
        let label = Label::parse("acme").unwrap();
        assert!(store.bind(&label, &profile).unwrap(), "curated bind");

        let publish = RoutePublish::sign(
            label.clone(),
            announcement.encode().expect("encode"),
            endpoint(),
            2,
            &SEED_A,
        );
        let resolved = publish_route(&mut store, &publish).expect("route records");
        assert_eq!(resolved.endpoint, endpoint());

        // An older epoch is refused without distinguishing itself.
        let stale = RoutePublish::sign(
            label.clone(),
            announcement.encode().expect("encode"),
            mechanics::actor::device_from_seed(&[78u8; 32])
                .as_str()
                .to_string(),
            1,
            &SEED_A,
        );
        assert_eq!(
            publish_route(&mut store, &stale),
            Err(Refusal::NotAvailable)
        );
        assert_eq!(
            store.route(&label).unwrap().unwrap().endpoint,
            endpoint(),
            "the held route did not move backwards"
        );
    }

    #[test]
    fn an_unbound_label_is_not_a_claim_path() {
        let (announcement, _) = identity();
        let mut store = MemRegistry::default();
        let publish = RoutePublish::sign(
            Label::parse("acme").unwrap(),
            announcement.encode().expect("encode"),
            endpoint(),
            1,
            &SEED_A,
        );
        assert_eq!(
            publish_route(&mut store, &publish),
            Err(Refusal::NotAvailable),
            "publishing a route never allocates a label"
        );
    }

    #[test]
    fn a_stranger_cannot_route_a_label_it_does_not_root() {
        let (announcement, profile) = identity();
        let mut store = MemRegistry::default();
        let label = Label::parse("acme").unwrap();
        store.bind(&label, &profile).unwrap();

        // Signed by a device the genesis does not root.
        let forged = RoutePublish::sign(
            label.clone(),
            announcement.encode().expect("encode"),
            endpoint(),
            3,
            &STRANGER,
        );
        assert_eq!(
            publish_route(&mut store, &forged),
            Err(Refusal::NotAuthentic)
        );

        // A rooted device signing, but the label bound to somebody else.
        let (other_announcement, other_profile) = identity_from([31u8; 32], [32u8; 32]);
        assert_ne!(profile, other_profile);
        let wrong_holder = RoutePublish::sign(
            label,
            other_announcement.encode().unwrap(),
            endpoint(),
            3,
            &[31u8; 32],
        );
        assert_eq!(
            publish_route(&mut store, &wrong_holder),
            Err(Refusal::NotAvailable),
            "a valid identity cannot route a label bound to another"
        );
    }

    /// The wire round trip: sign, publish over HTTP, resolve over HTTP — the
    /// same path the daemon and the reach router take, against the mounted
    /// axum surface on a real listener.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_route_publishes_and_resolves_over_the_wire() {
        let (announcement, profile) = identity();
        let store = std::sync::Arc::new(std::sync::Mutex::new(MemRegistry::default()));
        {
            let mut held = store.lock().unwrap();
            held.bind(&Label::parse("acme").unwrap(), &profile).unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, router(store)).await.ok();
        });

        let publish = RoutePublish::sign(
            Label::parse("acme").unwrap(),
            announcement.encode().expect("encode"),
            endpoint(),
            5,
            &SEED_A,
        );
        let published = tokio::task::spawn_blocking({
            let base = base.clone();
            move || publish_over_http(&base, &publish)
        })
        .await
        .expect("join")
        .expect("publish");
        assert_eq!(published.endpoint, endpoint());

        let resolved: Resolved = tokio::task::spawn_blocking(move || {
            ureq::get(&format!("{base}/registry/acme"))
                .call()
                .expect("resolve")
                .into_json()
                .expect("decode")
        })
        .await
        .expect("join");
        assert_eq!(resolved.profile, profile.as_str());
        assert_eq!(resolved.epoch, 5);
    }

    #[test]
    fn labels_are_one_dns_label_and_infrastructure_is_reserved() {
        for good in ["acme", "acme-2", "a", "x".repeat(32).as_str()] {
            assert!(Label::parse(good.to_string()).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-acme",
            "acme-",
            "Acme",
            "acme.lobby",
            "x".repeat(33).as_str(),
        ] {
            assert!(Label::parse(bad.to_string()).is_err(), "{bad}");
        }
        assert!(Label::parse("astrolabe").unwrap().reserved());
        assert!(Label::parse("post").unwrap().reserved());
        assert!(!Label::parse("acme").unwrap().reserved());
    }

    #[test]
    fn a_binding_never_moves_through_bind() {
        let (_, profile) = identity();
        let mut store = MemRegistry::default();
        let label = Label::parse("acme").unwrap();
        assert!(store.bind(&label, &profile).unwrap());
        let other = ProfileId::from_genesis(b"other");
        assert!(
            !store.bind(&label, &other).unwrap(),
            "rebinding is an operator act elsewhere, never this path"
        );
        assert_eq!(store.binding(&label).unwrap(), Some(profile));
    }
}
