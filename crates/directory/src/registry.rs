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
//! # The registrar keeps a chronicle, and that is the one key it holds
//!
//! A mirror that holds no key cannot author — but it can serve two readers
//! two different worlds, silently. So the registrar keeps a
//! [`mechanics::chronicle`]: every accepted publication is appended to a
//! committed log **before** its route goes live, and the surface signs a head
//! over that log. That key signs **which publications were recorded, in which
//! order** — never their contents, which remain self-signed by their subjects
//! and verified against their own geneses exactly as before. The asymmetry
//! survives the key: a compromise can still only deny or equivocate, and
//! equivocation against anyone who pins is now non-repudiable instead of
//! silent, because two irreconcilable heads *from the pinned signer* are the
//! proof of it. The key that could impersonate a *person* still does not
//! exist here.
//!
//! What each answer carries, precisely, because the strength differs by
//! surface:
//!
//! - A **publish receipt** ([`Chronicled`] from [`Registrar::publish`]) carries
//!   the head, the entry index, and the inclusion path for that entry, so the
//!   publisher proves *its own* publication was recorded — and reconciles that
//!   receipt against the canonical chronicle so a head minted over a private
//!   side branch cannot stand in for it.
//! - The **chronicle surface** ([`Registrar::answer`]) serves the current head
//!   and, from a pinned size, the consistency path — the equivocation ratchet
//!   any pinning follower runs.
//! - A **label resolution** ([`Registrar::resolve`]) carries the current head
//!   but **not** yet an inclusion path binding the resolved route to it: a
//!   resolver would need the publication's bytes to recompute the leaf, and
//!   the shipped read path (the `reach` router) holds no pin to check one
//!   against — route-level substitution is caught downstream at pairing, where
//!   the confirmation phrase commits the destination *profile*. Binding a
//!   resolved route to the chronicle is future work for a pinning resolver,
//!   and deliberately not claimed here until one exists.
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
    "chronicle",
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

/// Domain separator for a chronicle entry. The entry commits to the whole
/// accepted publication — announcement and signature included — so inclusion
/// proves *the evidence was recorded*, not merely that something was.
const CHRONICLE_ENTRY_DOMAIN: &[u8] = b"lait-registry/chronicle-entry/v1";

/// The canonical bytes one accepted publication contributes to the chronicle.
/// Deterministic and serde-free, so any holder of the publication recomputes
/// the same leaf.
#[must_use]
pub fn chronicle_entry(publish: &RoutePublish) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 + publish.announcement.len());
    framed(&mut out, CHRONICLE_ENTRY_DOMAIN);
    framed(&mut out, publish.label.as_str().as_bytes());
    framed(&mut out, &publish.announcement);
    framed(&mut out, publish.endpoint.as_bytes());
    framed(&mut out, &publish.epoch.to_be_bytes());
    framed(&mut out, publish.device.as_str().as_bytes());
    framed(&mut out, &publish.signature);
    out
}

/// A registry answer, wearing the chronicle's memory beside it.
///
/// The `Resolved` fields are flattened, so a reader that knows nothing of
/// chronicles decodes the answer it always did and ignores the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chronicled {
    #[serde(flatten)]
    pub resolved: Resolved,
    /// The signed chronicle head at answer time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<mechanics::chronicle::Head>,
    /// This publication's entry index — publish receipts only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<u64>,
    /// Inclusion path for `entry` under `head` — publish receipts only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inclusion: Vec<[u8; 32]>,
}

/// The chronicle surface's answer: the current signed head, and — when a
/// reader named the size it pinned — the path proving this head extends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChronicleAnswer {
    pub head: mechanics::chronicle::Head,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consistency: Vec<[u8; 32]>,
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

    /// Every chronicle leaf hash, in append order. Read once at open, and
    /// again after a raced append.
    fn chronicle_leaves(&mut self) -> anyhow::Result<Vec<[u8; 32]>>;

    /// Append a leaf at `index`. `Ok(false)` when the index is already taken
    /// — another holder appended first; the caller reloads and takes the next
    /// slot. Refusing a taken index is the linearization point: two holders
    /// can never write different leaves at one index, so roots cannot fork.
    fn append_chronicle(&mut self, index: u64, leaf: [u8; 32]) -> anyhow::Result<bool>;
}

/// Verify and apply one route publication against a store.
///
/// Verification consults no authority and needs no key: the announcement's
/// profile id is re-derived from the genesis it carries, the presenting
/// device must be one that genesis roots, and the label must already be bound
/// to that profile by the curated act. Everything checkable is checked; what
/// is not checkable is not stored. The chronicle's key lives one layer up, in
/// [`Registrar`], and signs only what this function accepted.
pub fn publish_route<S: RegistryStore>(
    store: &mut S,
    publish: &RoutePublish,
) -> Result<Resolved, Refusal> {
    let resolved = verify_route(store, publish)?;
    if !store
        .record_route(&resolved)
        .map_err(|error| Refusal::Unavailable(error.to_string()))?
    {
        return Err(Refusal::NotAvailable);
    }
    Ok(resolved)
}

/// Verify a publication and derive the route it authorizes, **without**
/// recording it. Split out so the registrar can chronicle a publication
/// before making its route live, keeping the log a superset of every live
/// route rather than a lagging shadow of it.
pub fn verify_route<S: RegistryStore>(
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
    Ok(Resolved {
        label: publish.label.clone(),
        profile: announcement.profile.as_str().to_string(),
        endpoint: publish.endpoint.clone(),
        epoch: publish.epoch,
    })
}

/// How many times an append retries after losing an index race before the
/// answer is "unavailable". Each loss means another holder appended; losing
/// this many in a row means the store is churning faster than one request
/// deserves to wait.
const MAX_APPEND_RACES: usize = 8;

/// The registrar: the store, the chronicle over it, and the one key — which
/// signs the chronicle's heads and nothing else.
pub struct Registrar<S> {
    store: S,
    chronicle: mechanics::chronicle::Chronicle,
    seed: [u8; 32],
}

impl<S: RegistryStore> Registrar<S> {
    /// Open over a store, restoring the chronicle from its persisted leaves.
    pub fn open(mut store: S, seed: [u8; 32]) -> anyhow::Result<Self> {
        let leaves = store.chronicle_leaves()?;
        let chronicle = mechanics::chronicle::Chronicle::from_leaves(leaves)
            .map_err(|refusal| anyhow::anyhow!("chronicle restore: {refusal}"))?;
        Ok(Self {
            store,
            chronicle,
            seed,
        })
    }

    /// The store, for the operator acts that bypass the request surface.
    pub fn store(&mut self) -> &mut S {
        &mut self.store
    }

    fn reload(&mut self) -> Result<(), Refusal> {
        let leaves = self
            .store
            .chronicle_leaves()
            .map_err(|error| Refusal::Unavailable(error.to_string()))?;
        self.chronicle = mechanics::chronicle::Chronicle::from_leaves(leaves)
            .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?;
        Ok(())
    }

    fn head(&self) -> Result<mechanics::chronicle::Head, Refusal> {
        self.chronicle
            .head(&self.seed)
            .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))
    }

    /// Verify, chronicle, then record one publication — in that order, so the
    /// chronicle is a superset of every live route rather than a lagging
    /// shadow. The receipt carries the signed head and the inclusion path for
    /// the entry just appended, and (when the pin's size is offered) a
    /// consistency path so the publisher's ratchet runs on this same act.
    pub fn publish(&mut self, publish: &RoutePublish) -> Result<Chronicled, Refusal> {
        let resolved = verify_route(&mut self.store, publish)?;
        // Refuse a non-advancing epoch before touching the chronicle, so a
        // replayed-but-valid publication cannot pad the log toward its cap.
        // A publication that clears this and later loses the record race to a
        // higher epoch is still honestly chronicled — it was accepted; only
        // liveness is decided by the record's atomic guard.
        if let Some(held) = self
            .store
            .route(&publish.label)
            .map_err(|error| Refusal::Unavailable(error.to_string()))?
        {
            if publish.epoch <= held.epoch {
                return Err(Refusal::NotAvailable);
            }
        }
        let leaf = mechanics::chronicle::Chronicle::leaf_of(&chronicle_entry(publish));
        let mut races = 0;
        let entry = loop {
            let index = self.chronicle.size();
            if index >= mechanics::chronicle::MAX_CHRONICLE_ENTRIES {
                return Err(Refusal::Unavailable("the chronicle is full".into()));
            }
            match self.store.append_chronicle(index, leaf) {
                Ok(true) => {
                    self.chronicle
                        .append_leaf(leaf)
                        .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?;
                    break index;
                }
                Ok(false) => {
                    races += 1;
                    if races > MAX_APPEND_RACES {
                        return Err(Refusal::Unavailable("chronicle append raced out".into()));
                    }
                    self.reload()?;
                }
                Err(error) => return Err(Refusal::Unavailable(error.to_string())),
            }
        };
        // Now make the route live. A lost race here (a higher epoch landed in
        // the window) leaves this publication chronicled but not live, which
        // is honest — the log records what was accepted, not only what won.
        if !self
            .store
            .record_route(&resolved)
            .map_err(|error| Refusal::Unavailable(error.to_string()))?
        {
            return Err(Refusal::NotAvailable);
        }
        let head = self.head()?;
        let inclusion = self
            .chronicle
            .inclusion(entry)
            .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?;
        Ok(Chronicled {
            resolved,
            head: Some(head),
            entry: Some(entry),
            inclusion,
        })
    }

    /// Resolve a label. The answer wears the current signed head; a reader's
    /// ratchet runs on that even when the route itself is what it wanted.
    pub fn resolve(&mut self, label: &Label) -> Result<Option<Chronicled>, Refusal> {
        let Some(resolved) = self
            .store
            .route(label)
            .map_err(|error| Refusal::Unavailable(error.to_string()))?
        else {
            return Ok(None);
        };
        let head = self.head()?;
        Ok(Some(Chronicled {
            resolved,
            head: Some(head),
            entry: None,
            inclusion: Vec::new(),
        }))
    }

    /// The chronicle surface: always the current signed head, and — when the
    /// reader named a pin size this log still covers — the consistency path
    /// from it.
    ///
    /// A `first` *past* the current head is not an error and must not 404: a
    /// chronicle now shorter than a reader's pin is a **rollback**, the
    /// strongest signal of a rewritten log, and the reader's own [`advance`]
    /// is what must see it. So the head goes back regardless (with an empty
    /// path), and `offered.size < pinned.size` is judged where the pin lives,
    /// not folded into "not found" here.
    pub fn answer(&self, first: Option<u64>) -> Result<ChronicleAnswer, Refusal> {
        let head = self.head()?;
        let consistency = match first {
            Some(first) if first <= self.chronicle.size() && first > 0 => self
                .chronicle
                .consistency(first)
                .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?,
            _ => Vec::new(),
        };
        Ok(ChronicleAnswer { head, consistency })
    }
}

/// The registry's HTTP surface, mounted beside the directory's router.
///
/// Resolution is a public `GET` because a label's existence is public by
/// design; publication carries its own evidence and needs no session. The
/// refusal shape borrows the directory's coarse wire form so a store failure
/// never explains itself to a prober.
pub fn router<S: RegistryStore + Send + 'static>(
    registrar: std::sync::Arc<std::sync::Mutex<Registrar<S>>>,
) -> axum::Router {
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::Json;

    type Held<S> = std::sync::Arc<std::sync::Mutex<Registrar<S>>>;

    async fn resolve<S: RegistryStore + Send + 'static>(
        State(registrar): State<Held<S>>,
        Path(label): Path<String>,
    ) -> Result<Json<Chronicled>, StatusCode> {
        let label = Label::parse(label).map_err(|_| StatusCode::NOT_FOUND)?;
        let mut registrar = registrar
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        match registrar.resolve(&label) {
            Ok(Some(answer)) => Ok(Json(answer)),
            Ok(None) => Err(StatusCode::NOT_FOUND),
            Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    async fn publish<S: RegistryStore + Send + 'static>(
        State(registrar): State<Held<S>>,
        Json(publish): Json<RoutePublish>,
    ) -> Result<Json<Chronicled>, (StatusCode, Json<crate::http::Refused>)> {
        let mut registrar = registrar.lock().map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(crate::http::Refused::Unavailable),
            )
        })?;
        registrar.publish(&publish).map(Json).map_err(|refusal| {
            (
                StatusCode::FORBIDDEN,
                Json(crate::http::Refused::from(&refusal)),
            )
        })
    }

    async fn chronicle_head<S: RegistryStore + Send + 'static>(
        State(registrar): State<Held<S>>,
    ) -> Result<Json<ChronicleAnswer>, StatusCode> {
        let registrar = registrar
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        match registrar.answer(None) {
            Ok(answer) => Ok(Json(answer)),
            Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    async fn chronicle_consistency<S: RegistryStore + Send + 'static>(
        State(registrar): State<Held<S>>,
        Path(first): Path<u64>,
    ) -> Result<Json<ChronicleAnswer>, StatusCode> {
        let registrar = registrar
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        match registrar.answer(Some(first)) {
            Ok(answer) => Ok(Json(answer)),
            Err(Refusal::NotAvailable) => Err(StatusCode::NOT_FOUND),
            Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    axum::Router::new()
        .route("/registry/chronicle", get(chronicle_head::<S>))
        .route(
            "/registry/chronicle/{first}",
            get(chronicle_consistency::<S>),
        )
        .route("/registry/{label}", get(resolve::<S>))
        .route("/registry/route", post(publish::<S>))
        .with_state(registrar)
}

/// Publish a route over the registry's HTTP surface, as a client.
///
/// Blocking, like every network call in this tree — callers on an async
/// runtime hop through `spawn_blocking`. The coarse error carries no more
/// than an operator log needs.
pub fn publish_over_http(base: &str, publish: &RoutePublish) -> anyhow::Result<Chronicled> {
    let response = ureq::post(&format!("{}/registry/route", base.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(10))
        .send_json(serde_json::to_value(publish)?)
        .map_err(|error| anyhow::anyhow!("registry refused the route: {error}"))?;
    Ok(response.into_json()?)
}

/// Ask the registrar's chronicle surface for its current head — and, when
/// `pinned` names the size a reader holds, the path proving extension.
pub fn chronicle_over_http(base: &str, pinned: Option<u64>) -> anyhow::Result<ChronicleAnswer> {
    let base = base.trim_end_matches('/');
    let url = match pinned {
        None => format!("{base}/registry/chronicle"),
        Some(first) => format!("{base}/registry/chronicle/{first}"),
    };
    let response = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|error| anyhow::anyhow!("chronicle could not be asked: {error}"))?;
    Ok(response.into_json()?)
}

/// The chronicle seed, from `REGISTRY_CHRONICLE_SEED` (64 hex chars) — or a
/// minted one, flagged ephemeral so the operator log can say the identity
/// will not survive a restart. An ephemeral registrar still chronicles
/// correctly — the leaf sequence in the store is the continuity, and a
/// reader's ratchet runs on roots, not signers — but its head signer changes
/// on restart, which anchoring above this will eventually care about.
pub fn chronicle_seed_from_env() -> anyhow::Result<([u8; 32], bool)> {
    match std::env::var("REGISTRY_CHRONICLE_SEED") {
        Ok(raw) => {
            let decoded = data_encoding::HEXLOWER_PERMISSIVE
                .decode(raw.trim().as_bytes())
                .map_err(|_| anyhow::anyhow!("REGISTRY_CHRONICLE_SEED is not hex"))?;
            let seed = <[u8; 32]>::try_from(decoded.as_slice())
                .map_err(|_| anyhow::anyhow!("REGISTRY_CHRONICLE_SEED is not 32 bytes"))?;
            Ok((seed, false))
        }
        Err(_) => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|error| anyhow::anyhow!("entropy: {error}"))?;
            Ok((seed, true))
        }
    }
}

/// In-memory store, enforcing the same rules a backing store must.
#[derive(Debug, Default)]
pub struct MemRegistry {
    bindings: std::collections::BTreeMap<String, ProfileId>,
    routes: std::collections::BTreeMap<String, Resolved>,
    chronicle: Vec<[u8; 32]>,
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

    fn chronicle_leaves(&mut self) -> anyhow::Result<Vec<[u8; 32]>> {
        Ok(self.chronicle.clone())
    }

    fn append_chronicle(&mut self, index: u64, leaf: [u8; 32]) -> anyhow::Result<bool> {
        if index != u64::try_from(self.chronicle.len())? {
            return Ok(false);
        }
        self.chronicle.push(leaf);
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
    /// axum surface on a real listener. And the chronicle chain over it,
    /// asserted as a chain rather than as parts: the receipt proves the
    /// publication was recorded, a reader pins the head, a second publication
    /// extends it through the consistency surface.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_route_publishes_and_resolves_over_the_wire() {
        let (announcement, profile) = identity();
        let registrar = Registrar::open(MemRegistry::default(), [51u8; 32]).expect("open");
        let registrar = std::sync::Arc::new(std::sync::Mutex::new(registrar));
        {
            let mut held = registrar.lock().unwrap();
            held.store()
                .bind(&Label::parse("acme").unwrap(), &profile)
                .unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, router(registrar)).await.ok();
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
            let publish = publish.clone();
            move || publish_over_http(&base, &publish)
        })
        .await
        .expect("join")
        .expect("publish");
        assert_eq!(published.resolved.endpoint, endpoint());

        // The receipt proves this very publication was recorded.
        let head = published.head.expect("a chronicled receipt");
        head.verify().expect("the head verifies");
        let leaf = mechanics::chronicle::Chronicle::leaf_of(&chronicle_entry(&publish));
        mechanics::chronicle::verify_inclusion(
            &leaf,
            published.entry.expect("an entry index"),
            head.size,
            &head.root,
            &published.inclusion,
        )
        .expect("the inclusion path verifies");

        // A reader pins what it was served.
        let pin = mechanics::chronicle::PinnedHead::from(&head);

        // The plain resolve still decodes for a reader that knows nothing of
        // chronicles, and carries the head for one that does.
        let resolved: Resolved = tokio::task::spawn_blocking({
            let base = base.clone();
            move || {
                ureq::get(&format!("{base}/registry/acme"))
                    .call()
                    .expect("resolve")
                    .into_json()
                    .expect("decode")
            }
        })
        .await
        .expect("join");
        assert_eq!(resolved.profile, profile.as_str());
        assert_eq!(resolved.epoch, 5);

        // A second publication moves the chronicle; the consistency surface
        // proves the new head extends the pinned one, and the ratchet takes it.
        let second = RoutePublish::sign(
            Label::parse("acme").unwrap(),
            announcement.encode().expect("encode"),
            endpoint(),
            6,
            &SEED_A,
        );
        tokio::task::spawn_blocking({
            let base = base.clone();
            move || publish_over_http(&base, &second).expect("second publish")
        })
        .await
        .expect("join");

        let answer = tokio::task::spawn_blocking({
            let base = base.clone();
            let pinned = pin.size;
            move || chronicle_over_http(&base, Some(pinned)).expect("chronicle")
        })
        .await
        .expect("join");
        assert_eq!(
            mechanics::chronicle::advance(Some(&pin), &answer.head, &answer.consistency),
            Ok(mechanics::chronicle::Advance::Extended),
            "the served head provably extends the pinned one"
        );

        // A reader ahead of the log is not 404'd — that would fold a rollback
        // into "not found". It gets the current (smaller) head, and its own
        // ratchet reads the shortfall as Rollback.
        let ahead: ChronicleAnswer = tokio::task::spawn_blocking({
            let base = base.clone();
            move || {
                ureq::get(&format!("{base}/registry/chronicle/99"))
                    .call()
                    .expect("ahead resolves")
                    .into_json()
                    .expect("decode")
            }
        })
        .await
        .expect("join");
        assert!(ahead.head.size < 99);
        let far_ahead = mechanics::chronicle::PinnedHead {
            size: 99,
            root: [0u8; 32],
            by: ahead.head.by.clone(),
        };
        assert_eq!(
            mechanics::chronicle::advance(Some(&far_ahead), &ahead.head, &ahead.consistency),
            Err(mechanics::chronicle::Refusal::Rollback),
            "a chronicle shorter than the pin is a rollback, judged at the reader"
        );
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
