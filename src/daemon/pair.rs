//! Pairing a new device into a profile: a code-derived endpoint and two
//! round trips.
//!
//! The joiner — a Pi, a NAS, a second laptop — has a seed and a throwaway
//! profile and nobody who knows it. It prints a code. On a device the person
//! already owns, the code is entered; that device is the sponsor. The code is
//! eight symbols of the display rendezvous alphabet, split down the middle:
//! the first four derive the ephemeral endpoint the sponsor dials (a routing
//! hint, 2^20, fine to leak — anyone can see the endpoint anyway), the last
//! four derive only the proof the sponsor sends, never anything observable.
//! Three wrong proofs burn the code and the joiner mints another.
//!
//! Round trip one, `Start` → `Offer`: the sponsor proves it holds the code and
//! hands over its own card; the joiner spends the code before it answers,
//! signs its half of the link, and answers with that half and six words.
//! Round trip two, `Complete` → `Done`: once the person confirms the words,
//! the sponsor signs its half, assembles the link — verified as if `seal` had
//! made it — adopts the device, and carries the whole link to the joiner,
//! which verifies it again and becomes a device of the profile. A lost
//! answer costs nothing on either side: the sponsor has already adopted, and
//! the joiner recovers by showing its next code, which the sponsor's card
//! then already roots.
//!
//! What the phrase protects, stated plainly: for a headless joiner the code
//! is the entire approval — whoever holds it is the sponsor. The six words
//! protect the **sponsor** against an impersonated joiner: a stranger who
//! stood up the same hint-derived key would produce words the real joiner's
//! journal does not show. Nothing code-derived is ever published, and no
//! seed crosses a machine; only halves do, and nothing half-signed is ever a
//! link.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use comms::policy::Network;
use comms::Transport;
use display_protocol::bounds::{MAX_CONFIRMATION_PHRASE_WORDS, MAX_PAIRING_LIFETIME_MS};
use display_protocol::pairing::{
    group_rendezvous_code, normalize_rendezvous_code, CONFIRMATION_WORDS, RENDEZVOUS_CODE_CHARS,
};
use mechanics::actor::device_from_seed;
use mechanics::ids::DeviceId;
use mechanics::kinship::{DeviceLink, Entry, KinshipLog, ProfileId, Signature, Standing};
use runtime::poison::LockRecovering;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::correspondence::CorrespondenceService;
use super::transport_hub::TransportHubFactory;
use crate::control::{PairOffer, PairingCode};
use crate::display::pairing::{random_rendezvous_code, RENDEZVOUS_LIFETIME_MS};

/// The pairing protocol, on the code-derived endpoint only — never a hub
/// lane, because the endpoint is a different key with one code's life.
pub(crate) const PAIR_ALPN: &[u8] = b"lait/pair/1";
/// One frame's ceiling. An own card of a small profile is a few KiB; a
/// frame that needs more is not a pairing.
pub(crate) const MAX_PAIR_FRAME: usize = 64 * 1024;
/// Wrong proofs before the code burns.
const MAX_PAIR_ATTEMPTS: u8 = 3;
/// A code's life — the display rendezvous number, not one of this module's.
const PAIR_CODE_LIFETIME_MS: u64 = RENDEZVOUS_LIFETIME_MS;
/// The routing half of the code.
const HINT_CHARS: usize = RENDEZVOUS_CODE_CHARS / 2;
const ENDPOINT_CONTEXT: &str = "lait/pair/endpoint/v1";
const PROOF_CONTEXT: &str = "lait/pair/proof/v1";
const PHRASE_DOMAIN: &[u8] = b"lait/pair/confirmation-phrase/v1";
const PAIR_PROTOCOL: u16 = 1;
const ROUTE_DEADLINE: Duration = Duration::from_secs(3);
const DIAL_DEADLINE: Duration = Duration::from_secs(10);
const FRAME_DEADLINE: Duration = Duration::from_secs(5);
/// How long `accept_one` parks before it hands the loop back to re-check
/// expiry: short enough that a burnt or expired code is replaced promptly.
const ACCEPT_SLICE: Duration = Duration::from_secs(30);

/// One frame per dial, sponsor → joiner.
#[derive(Debug, Serialize, Deserialize)]
enum PairFrame {
    Start {
        proof: [u8; 32],
        sponsor: DeviceId,
        nonce: [u8; 16],
        epoch: u64,
        /// `Announcement::encode` of the sponsor's own card. The sponsor does
        /// not know the joiner's id at `Start`, so it projects for itself
        /// under `own: true` — the structural bodies ride regardless of the
        /// `device` slot, and that is what the joiner needs to walk from the
        /// genesis.
        card: Vec<u8>,
        routes: Vec<SocketAddr>,
    },
    Complete {
        pairing: [u8; 16],
        link: DeviceLink,
        routes: Vec<SocketAddr>,
    },
}

/// One frame back, joiner → sponsor.
#[derive(Debug, Serialize, Deserialize)]
enum PairReply {
    Offer {
        pairing: [u8; 16],
        joiner: DeviceId,
        joiner_nonce: [u8; 16],
        name: String,
        joiner_half: Signature,
        routes: Vec<SocketAddr>,
    },
    /// Adopted — or already adopted; the answer is the same.
    Done { device: DeviceId },
    /// One coarse answer for never minted, spent, expired, burnt, wrong
    /// proof, wrong caller, and a link that did not assemble: a caller whose
    /// proof failed learns nothing but that it failed.
    Refused,
}

/// A minted code and the endpoint it derives, alive for the code's life.
struct Outstanding {
    /// Normalised, eight symbols.
    code: String,
    endpoint: Arc<dyn Transport>,
    /// The identity endpoint, for learning the sponsor's routes.
    identity: Arc<dyn Transport>,
    direct: Vec<SocketAddr>,
    attempts: u8,
    expires_at_ms: u64,
    spent: bool,
}

impl Outstanding {
    fn burnt(&self) -> bool {
        self.attempts >= MAX_PAIR_ATTEMPTS
    }

    /// Whether a `Start` naming this code is still answered.
    fn enterable(&self, now_ms: u64) -> bool {
        !self.spent && !self.burnt() && self.expires_at_ms > now_ms
    }
}

/// The joiner's side of a spent code: the offer it made, waiting for the
/// sponsor to come back with the link. A half and a phrase — never a link.
struct PendingPair {
    pairing: [u8; 16],
    sponsor: DeviceId,
    card: addressbook::Announcement,
    nonce: [u8; 16],
    epoch: u64,
    /// The words the journal showed, kept only so a test can hold the two
    /// journals against each other; the ceremony itself never reads them
    /// back — the person does.
    #[cfg(test)]
    phrase: Vec<String>,
    expires_at_ms: u64,
}

/// The sponsor's side of an entered code: what it needs to assemble once the
/// person confirms. The joiner's half and the phrase — never a link.
struct SponsorPending {
    pairing: [u8; 16],
    pair_id: DeviceId,
    joiner: DeviceId,
    name: String,
    nonce: [u8; 16],
    epoch: u64,
    joiner_half: Signature,
    phrase: Vec<String>,
    expires_at_ms: u64,
}

/// The joiner's whole state, under one lock so spending a code and recording
/// the offer it made are one act.
#[derive(Default)]
struct Joiner {
    outstanding: Option<Outstanding>,
    pending: Option<PendingPair>,
}

/// What entering a code came back with.
#[derive(Debug)]
pub enum SponsorOutcome {
    /// The joiner answered; confirm the words.
    Offer(PairOffer),
    /// The joiner was already a device of this profile and only the answer
    /// had been lost.
    Paired { device: DeviceId },
}

/// What one turn of the joiner's accept loop did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    /// A dial was answered.
    Served,
    /// This device became a device of the profile.
    Adopted,
    /// Nothing arrived within the slice; the caller re-checks the code.
    Idle,
    /// The endpoint is gone.
    Closed,
}

/// The pairing ceremony, both sides. One service because one daemon can be
/// either: the Pi is a joiner today and sponsors the next device later.
pub struct PairService {
    identity: PathBuf,
    /// `None` when the home holds no identity yet; every act refuses then.
    seed: Option<[u8; 32]>,
    correspondence: Arc<CorrespondenceService>,
    hub: Arc<TransportHubFactory>,
    joiner: Mutex<Joiner>,
    sponsoring: Mutex<BTreeMap<String, SponsorPending>>,
    /// Set the moment this device became a device of a profile; the serve
    /// loop relaunches on it, because the display coordinator anchored on the
    /// throwaway profile at boot and re-anchors only by restarting.
    adopted: AtomicBool,
}

impl PairService {
    pub fn open(
        identity: &Path,
        correspondence: Arc<CorrespondenceService>,
        hub: Arc<TransportHubFactory>,
    ) -> Self {
        Self {
            identity: identity.to_path_buf(),
            seed: crate::config::load_identity(identity).ok(),
            correspondence,
            hub,
            joiner: Mutex::new(Joiner::default()),
            sponsoring: Mutex::new(BTreeMap::new()),
            adopted: AtomicBool::new(false),
        }
    }

    fn seed(&self) -> Result<[u8; 32]> {
        self.seed
            .ok_or_else(|| anyhow!("this home holds no identity to pair"))
    }

    fn network(&self) -> Result<Network> {
        crate::config::Settings::load(Some(&self.identity)).network()
    }

    /// Whether this device should be showing a code: founded here, alone in
    /// its set, and not adopted this run. `None` on the watch is not
    /// "unpaired" — it is unknown, and unknown mints nothing.
    #[must_use]
    pub fn unpaired(&self) -> bool {
        if self.adopted.load(Ordering::Acquire) {
            return false;
        }
        let own = self.correspondence.own_devices();
        let alone = own
            .borrow()
            .as_ref()
            .is_some_and(|own| own.devices.len() == 1 && own.devices.contains(&own.me));
        alone
            && matches!(
                self.correspondence.origin(),
                Some(addressbook::reach_store::Origin::Founded)
            )
    }

    /// Whether the code on show needs replacing: none, burnt, expired
    /// unspent, or spent with its offer expired unadopted.
    #[must_use]
    pub fn wants_code(&self, now_ms: u64) -> bool {
        let joiner = self.joiner.lock_recovering();
        match joiner.outstanding.as_ref() {
            None => true,
            Some(outstanding) if outstanding.burnt() => true,
            Some(outstanding) if !outstanding.spent => outstanding.expires_at_ms <= now_ms,
            Some(_) => joiner
                .pending
                .as_ref()
                .is_none_or(|pending| pending.expires_at_ms <= now_ms),
        }
    }

    /// Whether this device became a device of a profile this run.
    #[must_use]
    pub fn adopted(&self) -> bool {
        self.adopted.load(Ordering::Acquire)
    }

    /// Mint a code, raise its endpoint, and say so on the journal. Replaces
    /// whatever code was outstanding — burnt, expired, or spent and never
    /// completed — and shuts its endpoint down first.
    pub async fn mint(&self, network: &Network, now_ms: u64) -> Result<()> {
        let seed = self.seed()?;
        let previous = {
            let mut joiner = self.joiner.lock_recovering();
            joiner.pending = None;
            joiner.outstanding.take()
        };
        if let Some(previous) = previous {
            previous.endpoint.shutdown().await;
        }
        let code = random_rendezvous_code().context("mint a pairing code")?;
        let derived = derive_code(&code)?;
        let endpoint = self
            .hub
            .inner()
            .build(
                &derived.pair_seed,
                network,
                comms::Protocols::framed(&[PAIR_ALPN]),
            )
            .await
            .context("raise the pairing endpoint")?;
        // Direct routes matter only where nothing else resolves the hint:
        // an isolated endpoint without one is a code nobody can dial, said
        // rather than shown as a code.
        let direct = match endpoint.advertised_routes(ROUTE_DEADLINE).await {
            Ok(direct) => direct,
            Err(error) => {
                endpoint.shutdown().await;
                return Err(error.context("the pairing endpoint has no route to print"));
            }
        };
        let identity = match self.hub.identity_transport(&seed, network).await {
            Ok(identity) => identity,
            Err(error) => {
                endpoint.shutdown().await;
                return Err(error.context("the identity endpoint is not up"));
            }
        };
        let grouped = group_rendezvous_code(&derived.code)
            .map_err(|refusal| anyhow!("group the pairing code: {refusal}"))?;
        let enter = entry_spelling(&grouped, &direct);
        {
            let mut joiner = self.joiner.lock_recovering();
            joiner.outstanding = Some(Outstanding {
                code: derived.code,
                endpoint,
                identity,
                direct: direct.clone(),
                attempts: 0,
                expires_at_ms: now_ms.saturating_add(PAIR_CODE_LIFETIME_MS),
                spent: false,
            });
        }
        tracing::info!(
            target: "lait::pair",
            code = %grouped,
            direct = ?direct,
            enter = %enter,
            "pairing code — enter it in Astrolabe → Devices on a device you own"
        );
        Ok(())
    }

    /// Shut the pairing endpoint down. After adoption, and at stop.
    pub async fn close(&self) {
        let outstanding = {
            let mut joiner = self.joiner.lock_recovering();
            joiner.pending = None;
            joiner.outstanding.take()
        };
        if let Some(outstanding) = outstanding {
            outstanding.endpoint.shutdown().await;
        }
    }

    /// The code on show, for `HostContext.pairing`. `None` once it is spent
    /// or burnt: a code that can no longer be entered is not one to print.
    #[must_use]
    pub fn status(&self, now_ms: u64) -> Option<PairingCode> {
        let joiner = self.joiner.lock_recovering();
        let outstanding = joiner.outstanding.as_ref()?;
        if !outstanding.enterable(now_ms) {
            return None;
        }
        Some(PairingCode {
            code: group_rendezvous_code(&outstanding.code).ok()?,
            direct: outstanding.direct.clone(),
            expires_at_ms: outstanding.expires_at_ms,
        })
    }

    /// Answer one dial on the pairing endpoint: a `Start` or a `Complete`.
    /// Parks at most one slice, so the caller can re-check the code's life.
    pub async fn accept_one(&self) -> Result<Accepted> {
        let (endpoint, wait) = {
            let joiner = self.joiner.lock_recovering();
            let Some(outstanding) = joiner.outstanding.as_ref() else {
                return Ok(Accepted::Idle);
            };
            let now = now_ms();
            let deadline = if outstanding.spent {
                joiner
                    .pending
                    .as_ref()
                    .map_or(now, |pending| pending.expires_at_ms)
            } else {
                outstanding.expires_at_ms
            };
            let wait = Duration::from_millis(deadline.saturating_sub(now))
                .clamp(Duration::from_millis(250), ACCEPT_SLICE);
            (outstanding.endpoint.clone(), wait)
        };
        let incoming = match tokio::time::timeout(wait, endpoint.accept()).await {
            Err(_) => return Ok(Accepted::Idle),
            Ok(None) => return Ok(Accepted::Closed),
            Ok(Some(incoming)) => incoming,
        };
        if incoming.alpn != PAIR_ALPN {
            return Ok(Accepted::Served);
        }
        let mut stream = incoming.stream;
        let frame =
            match tokio::time::timeout(FRAME_DEADLINE, stream.recv_bounded(MAX_PAIR_FRAME)).await {
                Ok(Ok(Some(frame))) => frame,
                _ => return Ok(Accepted::Served),
            };
        let Ok(frame) = postcard::from_bytes::<PairFrame>(&frame) else {
            return Ok(Accepted::Served);
        };
        let now = now_ms();
        let reply = match frame {
            PairFrame::Start {
                proof,
                sponsor,
                nonce,
                epoch,
                card,
                routes,
            } => self.start(
                incoming.from,
                Start {
                    proof,
                    sponsor,
                    nonce,
                    epoch,
                    card,
                    routes,
                },
                now,
            ),
            PairFrame::Complete {
                pairing,
                link,
                routes,
            } => self.complete(incoming.from, pairing, link, &routes, now),
        };
        let bytes = postcard::to_stdvec(&reply).context("encode the pairing reply")?;
        let _ = tokio::time::timeout(FRAME_DEADLINE, stream.send(&bytes)).await;
        let _ = stream.finish().await;
        let _ = tokio::time::timeout(FRAME_DEADLINE, stream.wait_closed()).await;
        Ok(if self.adopted() {
            Accepted::Adopted
        } else {
            Accepted::Served
        })
    }

    /// The joiner's answer to `Start`. Spends the code before anything is
    /// answered; a wrong proof counts and burns at three; every refusal is
    /// the same word.
    fn start(&self, from: DeviceId, start: Start, now_ms: u64) -> PairReply {
        let Ok(seed) = self.seed() else {
            return PairReply::Refused;
        };
        let me = device_from_seed(&seed);
        let mut joiner = self.joiner.lock_recovering();
        let Some(outstanding) = joiner.outstanding.as_mut() else {
            return PairReply::Refused;
        };
        if !outstanding.enterable(now_ms) {
            return PairReply::Refused;
        }
        // The proof is bound to the dialer's proven key: a Start naming a
        // sponsor other than the one the transport proved is a wrong try,
        // whatever proof it carries.
        let proof_ok = from == start.sponsor
            && from != me
            && proof_for(&outstanding.code, &from)
                .is_ok_and(|expected| constant_time_eq(&expected, &start.proof));
        if !proof_ok {
            outstanding.attempts = outstanding.attempts.saturating_add(1);
            if outstanding.burnt() {
                tracing::warn!(
                    target: "lait::pair",
                    "somebody tried a wrong code; it is burnt and a fresh one is minted"
                );
            }
            return PairReply::Refused;
        }
        outstanding.spent = true;
        let identity = outstanding.identity.clone();
        let direct = outstanding.direct.clone();

        // The sponsor's card: anchored to its genesis, signed by a device the
        // genesis roots, and legible to an own reader.
        let Ok(card) = addressbook::Announcement::decode(&start.card) else {
            return PairReply::Refused;
        };
        let rooted = KinshipLog::found(card.genesis.clone())
            .is_ok_and(|log| log.profile() == &card.profile)
            && (card.genesis.devices.contains(&from)
                || mechanics::kinship::signer_rooted(
                    &card.genesis,
                    &card.projection.bodies,
                    &from,
                ));
        if !rooted {
            return PairReply::Refused;
        }
        let reader = Standing {
            own: true,
            device: Some(from.clone()),
            ..Standing::default()
        };
        if card.projection.verify(&reader).is_err() {
            return PairReply::Refused;
        }
        identity.learn(from.clone(), &start.routes);

        // Already a device of this profile — the sponsor adopted and the
        // `Complete` was lost, or this is a replay of a finished ceremony.
        // The card roots me: adopt from it, and answer as if nothing had
        // been lost.
        if self.holds_with(&from) {
            return PairReply::Done { device: me };
        }
        let carried = card.projection.bodies.iter().find_map(|entry| match entry {
            Entry::Link(link) if link.names(&me) && link.names(&from) => Some(link.clone()),
            _ => None,
        });
        if let Some(link) = carried {
            joiner.pending = None;
            return match self
                .correspondence
                .become_device_of(card, from, link, now_ms / 1000)
            {
                Ok(()) => {
                    self.adopted.store(true, Ordering::Release);
                    PairReply::Done { device: me }
                }
                Err(error) => {
                    tracing::warn!(%error, "the sponsor's card roots this device, but adoption failed");
                    PairReply::Refused
                }
            };
        }

        let joiner_half = DeviceLink::half(&seed, &from, start.nonce, start.epoch);
        let (Ok(pairing), Ok(joiner_nonce)) = (random_bytes(), random_bytes()) else {
            return PairReply::Refused;
        };
        let phrase = confirmation_phrase(&card.profile, &start.nonce, &joiner_nonce, &me, &from);
        tracing::info!(
            target: "lait::pair",
            sponsor = %from,
            phrase = %phrase.join(" "),
            "a device you own is pairing this one — confirm these six words there"
        );
        joiner.pending = Some(PendingPair {
            pairing,
            sponsor: from,
            card,
            nonce: start.nonce,
            epoch: start.epoch,
            #[cfg(test)]
            phrase,
            expires_at_ms: now_ms.saturating_add(u64::from(MAX_PAIRING_LIFETIME_MS)),
        });
        PairReply::Offer {
            pairing,
            joiner: me,
            joiner_nonce,
            name: device_name(),
            joiner_half,
            routes: direct,
        }
    }

    /// The joiner's answer to `Complete`: a fully verified link naming both
    /// sides, at the nonce and epoch the offer was made for, from the sponsor
    /// it was made to. Then adoption, which is the one write.
    fn complete(
        &self,
        from: DeviceId,
        pairing: [u8; 16],
        link: DeviceLink,
        routes: &[SocketAddr],
        now_ms: u64,
    ) -> PairReply {
        let Ok(seed) = self.seed() else {
            return PairReply::Refused;
        };
        let me = device_from_seed(&seed);
        let mut joiner = self.joiner.lock_recovering();
        let Some(pending) = joiner.pending.as_ref() else {
            // Idempotent: a repeated `Complete` after the answer was lost.
            return if self.holds_with(&from) {
                PairReply::Done { device: me }
            } else {
                PairReply::Refused
            };
        };
        let matches = pending.pairing == pairing
            && pending.sponsor == from
            && pending.expires_at_ms > now_ms
            && link.verify().is_ok()
            && link.names(&me)
            && link.names(&from)
            && link.nonce == pending.nonce
            && link.epoch == pending.epoch;
        if !matches {
            return PairReply::Refused;
        }
        let Some(pending) = joiner.pending.take() else {
            return PairReply::Refused;
        };
        if let Some(outstanding) = joiner.outstanding.as_ref() {
            outstanding.identity.learn(from.clone(), routes);
        }
        match self
            .correspondence
            .become_device_of(pending.card, from, link, now_ms / 1000)
        {
            Ok(()) => {
                self.adopted.store(true, Ordering::Release);
                PairReply::Done { device: me }
            }
            Err(error) => {
                tracing::warn!(%error, "the link verified, but this device could not be adopted");
                PairReply::Refused
            }
        }
    }

    /// The words the joiner's journal showed for its outstanding offer.
    #[cfg(test)]
    fn pending_phrase(&self) -> Option<Vec<String>> {
        self.joiner
            .lock_recovering()
            .pending
            .as_ref()
            .map(|pending| pending.phrase.clone())
    }

    /// The terms the outstanding offer was made under: pairing, nonce, epoch.
    #[cfg(test)]
    fn pending_terms(&self) -> Option<([u8; 16], [u8; 16], u64)> {
        self.joiner
            .lock_recovering()
            .pending
            .as_ref()
            .map(|pending| (pending.pairing, pending.nonce, pending.epoch))
    }

    /// Whether the published device set already holds `other` beside me.
    fn holds_with(&self, other: &DeviceId) -> bool {
        self.correspondence
            .own_devices()
            .borrow()
            .as_ref()
            .is_some_and(|own| own.devices.contains(other) && own.devices.len() > 1)
    }

    // ---- the sponsor's side ----

    /// Enter a code a new device printed: dial the endpoint it derives,
    /// prove the code, hand over this profile's card, and hold the offer
    /// that comes back for the person to confirm.
    pub async fn enter(&self, entered: &str, now_ms: u64) -> Result<SponsorOutcome> {
        let seed = self.seed()?;
        let me = device_from_seed(&seed);
        let (code, addrs) = parse_entry(entered)?;
        let derived = derive_code(&code)?;
        let proof = proof_for(&derived.code, &me)?;
        let network = self.network()?;
        let identity = self
            .hub
            .identity_transport(&seed, &network)
            .await
            .context("the identity endpoint is not up")?;
        if !addrs.is_empty() {
            identity.learn(derived.pair_id.clone(), &addrs);
        }
        let own = self
            .correspondence
            .own_card()
            .map_err(|error| anyhow!(error))?;
        let card = own
            .card
            .encode()
            .map_err(|error| anyhow!("encode my own card: {error}"))?;
        let routes = identity
            .advertised_routes(ROUTE_DEADLINE)
            .await
            .unwrap_or_default();
        let nonce = random_bytes()?;
        let epoch = own.epoch.saturating_add(1);
        let frame = PairFrame::Start {
            proof,
            sponsor: me.clone(),
            nonce,
            epoch,
            card,
            routes,
        };
        match exchange(identity.as_ref(), &derived.pair_id, &frame).await? {
            PairReply::Refused => bail!("the device refused that code"),
            PairReply::Done { device } => Ok(SponsorOutcome::Paired { device }),
            PairReply::Offer {
                pairing,
                joiner,
                joiner_nonce,
                name,
                joiner_half,
                routes,
            } => {
                if joiner == me {
                    bail!("that code is this device's own");
                }
                identity.learn(joiner.clone(), &routes);
                let phrase =
                    confirmation_phrase(&own.card.profile, &nonce, &joiner_nonce, &me, &joiner);
                let id = data_encoding::HEXLOWER.encode(&pairing);
                let expires_at_ms = now_ms.saturating_add(u64::from(MAX_PAIRING_LIFETIME_MS));
                let offer = PairOffer {
                    pairing: id.clone(),
                    device: joiner.as_str().to_owned(),
                    name: name.clone(),
                    phrase: phrase.clone(),
                    expires_at_ms,
                };
                self.sponsoring.lock_recovering().insert(
                    id,
                    SponsorPending {
                        pairing,
                        pair_id: derived.pair_id,
                        joiner,
                        name,
                        nonce,
                        epoch,
                        joiner_half,
                        phrase,
                        expires_at_ms,
                    },
                );
                Ok(SponsorOutcome::Offer(offer))
            }
        }
    }

    /// Confirm or reject an offer. Rejecting drops it and sends nothing.
    /// Confirming signs this side's half, assembles the link, adopts the
    /// device here, and carries the link to it; a lost `Done` costs nothing,
    /// because the adoption already happened and the joiner recovers from
    /// the next code it shows.
    pub async fn confirm(
        &self,
        pairing: &str,
        accept: bool,
        now_ms: u64,
    ) -> Result<Option<DeviceId>> {
        let pending = self
            .sponsoring
            .lock_recovering()
            .remove(pairing)
            .ok_or_else(|| anyhow!("no offer by that id is waiting"))?;
        if pending.expires_at_ms <= now_ms {
            bail!("that offer has expired; enter the device's code again");
        }
        if !accept {
            return Ok(None);
        }
        let seed = self.seed()?;
        let me = device_from_seed(&seed);
        let sponsor_half = DeviceLink::half(&seed, &pending.joiner, pending.nonce, pending.epoch);
        let link = DeviceLink::assemble(
            (me, sponsor_half),
            (pending.joiner.clone(), pending.joiner_half),
            pending.nonce,
            pending.epoch,
        )
        .map_err(|refusal| anyhow!("the halves do not make a link: {refusal}"))?;
        self.correspondence
            .adopt_device(link.clone(), now_ms / 1000)
            .map_err(|error| anyhow!(error))?;

        let network = self.network()?;
        let identity = self
            .hub
            .identity_transport(&seed, &network)
            .await
            .context("the identity endpoint is not up")?;
        let routes = identity
            .advertised_routes(ROUTE_DEADLINE)
            .await
            .unwrap_or_default();
        let frame = PairFrame::Complete {
            pairing: pending.pairing,
            link,
            routes,
        };
        match exchange(identity.as_ref(), &pending.pair_id, &frame).await {
            Ok(PairReply::Done { .. }) => {}
            Ok(other) => tracing::warn!(
                device = %pending.joiner,
                ?other,
                "the device was adopted here but did not take the link; its next code completes it"
            ),
            Err(error) => tracing::warn!(
                device = %pending.joiner,
                %error,
                "the device was adopted here but could not be reached; its next code completes it"
            ),
        }
        Ok(Some(pending.joiner))
    }

    /// Offers awaiting confirmation, for `HostContext.pair_offers`.
    #[must_use]
    pub fn offers(&self, now_ms: u64) -> Vec<PairOffer> {
        let mut sponsoring = self.sponsoring.lock_recovering();
        sponsoring.retain(|_, pending| pending.expires_at_ms > now_ms);
        sponsoring
            .iter()
            .map(|(id, pending)| PairOffer {
                pairing: id.clone(),
                device: pending.joiner.as_str().to_owned(),
                name: pending.name.clone(),
                phrase: pending.phrase.clone(),
                expires_at_ms: pending.expires_at_ms,
            })
            .collect()
    }
}

/// The fields of a `Start`, named so `start` reads them rather than a tuple.
struct Start {
    proof: [u8; 32],
    sponsor: DeviceId,
    nonce: [u8; 16],
    epoch: u64,
    card: Vec<u8>,
    routes: Vec<SocketAddr>,
}

/// One dial: send the frame, take the one reply.
async fn exchange(
    transport: &dyn Transport,
    peer: &DeviceId,
    frame: &PairFrame,
) -> Result<PairReply> {
    let mut stream =
        tokio::time::timeout(DIAL_DEADLINE, transport.connect(peer.clone(), PAIR_ALPN))
            .await
            .context("the pairing endpoint did not answer the dial")?
            .context("dial the pairing endpoint")?;
    let bytes = postcard::to_stdvec(frame).context("encode the pairing frame")?;
    if bytes.len() > MAX_PAIR_FRAME {
        bail!("the pairing frame is larger than the protocol allows");
    }
    tokio::time::timeout(FRAME_DEADLINE, stream.send(&bytes))
        .await
        .context("the pairing endpoint stopped taking the frame")??;
    let reply = tokio::time::timeout(FRAME_DEADLINE, stream.recv_bounded(MAX_PAIR_FRAME))
        .await
        .context("the pairing endpoint did not answer")??
        .ok_or_else(|| anyhow!("the pairing endpoint closed without answering"))?;
    postcard::from_bytes(&reply).context("decode the pairing reply")
}

/// A code, split: the hint derives the endpoint, the rest is the secret.
struct Derived {
    code: String,
    pair_seed: [u8; 32],
    pair_id: DeviceId,
}

fn derive_code(entered: &str) -> Result<Derived> {
    let code = normalize_rendezvous_code(entered)
        .map_err(|refusal| anyhow!("that is not a pairing code: {refusal}"))?;
    let hint = code
        .get(..HINT_CHARS)
        .ok_or_else(|| anyhow!("that is not a pairing code"))?;
    let pair_seed = blake3::derive_key(ENDPOINT_CONTEXT, hint.as_bytes());
    let pair_id = device_from_seed(&pair_seed);
    Ok(Derived {
        code,
        pair_seed,
        pair_id,
    })
}

/// The proof the sponsor sends: the secret half bound to the sponsor's own
/// key, so a proof overheard on one dial names nobody else.
fn proof_for(code: &str, sponsor: &DeviceId) -> Result<[u8; 32]> {
    let secret = code
        .get(HINT_CHARS..)
        .ok_or_else(|| anyhow!("that is not a pairing code"))?;
    let key = sponsor
        .key_bytes()
        .ok_or_else(|| anyhow!("the sponsor id carries no key"))?;
    let mut material = Vec::with_capacity(secret.len().saturating_add(key.len()));
    material.extend_from_slice(secret.as_bytes());
    material.extend_from_slice(&key);
    Ok(blake3::derive_key(PROOF_CONTEXT, &material))
}

/// `CODE` or `CODE@host:port[,host:port]`.
fn parse_entry(entered: &str) -> Result<(String, Vec<SocketAddr>)> {
    let entered = entered.trim();
    let (code, addrs) = match entered.split_once('@') {
        Some((code, addrs)) => (code, addrs),
        None => (entered, ""),
    };
    let addrs = addrs
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<SocketAddr>()
                .with_context(|| format!("`{part}` is not a host:port address"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((code.to_owned(), addrs))
}

/// What a person types: the grouped code, with the direct routes beside it
/// when there are any to carry.
fn entry_spelling(grouped: &str, direct: &[SocketAddr]) -> String {
    if direct.is_empty() {
        return grouped.to_owned();
    }
    let routes = direct
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("{grouped}@{routes}")
}

/// The six words both sides show. Commits the destination profile, both
/// nonces and both device ids — the whole link that will be signed — so a
/// stranger who stood up the hint-derived key produces words the real
/// joiner's journal never shows.
pub(crate) fn confirmation_phrase(
    profile: &ProfileId,
    nonce: &[u8; 16],
    joiner_nonce: &[u8; 16],
    a: &DeviceId,
    b: &DeviceId,
) -> Vec<String> {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut preimage = Vec::new();
    framed(&mut preimage, PHRASE_DOMAIN);
    framed(&mut preimage, &PAIR_PROTOCOL.to_be_bytes());
    framed(&mut preimage, profile.as_str().as_bytes());
    framed(&mut preimage, nonce);
    framed(&mut preimage, joiner_nonce);
    framed(&mut preimage, lo.as_str().as_bytes());
    framed(&mut preimage, hi.as_str().as_bytes());
    Sha256::digest(&preimage)
        .iter()
        .take(MAX_CONFIRMATION_PHRASE_WORDS)
        .filter_map(|byte| CONFIRMATION_WORDS.get(usize::from(byte & 0x1f)))
        .map(|word| (*word).to_owned())
        .collect()
}

/// `u32 BE length ‖ bytes` — the display transcript's shape.
fn framed(out: &mut Vec<u8>, field: &[u8]) {
    let length = u32::try_from(field.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(field);
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).context("obtain pairing randomness")?;
    Ok(bytes)
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .unwrap_or(0)
}

/// What this device calls itself in an offer: its hostname, which is the
/// name a person gave the box. Never an identifier — the id rides beside it.
fn device_name() -> String {
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        // SAFETY: `gethostname` writes at most `buffer.len()` bytes into a
        // buffer that lives for the call; the length is the buffer's own.
        let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if rc == 0 {
            let end = buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(buffer.len());
            if let Some(name) = buffer.get(..end) {
                let name = String::from_utf8_lossy(name).trim().to_owned();
                if !name.is_empty() {
                    return name;
                }
            }
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "a device".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use comms::mem::MemNet;
    use comms::TransportFactory;

    /// The in-memory network, one peer per key. What the hub's own tests
    /// use; a pairing runs its code-derived endpoint through the same
    /// factory, so it is a peer here like any other.
    struct MemFactory(MemNet);

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _protocols: comms::Protocols<'_>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(self.0.peer(device_from_seed(identity_seed))))
        }
    }

    /// One daemon's worth of pairing: a founded home, a restored plane, a hub
    /// over the shared network, and the service over all three.
    struct Side {
        home: PathBuf,
        seed: [u8; 32],
        correspondence: Arc<CorrespondenceService>,
        pair: Arc<PairService>,
    }

    impl Side {
        fn stand(tag: &str, net: &MemNet) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let home =
                std::env::temp_dir().join(format!("lait-pair-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&home).expect("home");
            // What boot does before the router stands: mint the identity and
            // found its profile. The service restores and never founds.
            let seed = crate::config::load_or_create_identity(&home).expect("identity");
            crate::config::identity_profile(&home).expect("profile");
            let correspondence = Arc::new(CorrespondenceService::open(&home));
            correspondence
                .restore(super::super::correspondence::now_secs())
                .expect("restore");
            let hub = Arc::new(TransportHubFactory::new(
                Arc::new(MemFactory(net.clone())),
                correspondence.own_devices(),
            ));
            let pair = Arc::new(PairService::open(&home, correspondence.clone(), hub));
            Self {
                home,
                seed,
                correspondence,
                pair,
            }
        }

        fn me(&self) -> DeviceId {
            device_from_seed(&self.seed)
        }

        fn devices(&self) -> Vec<DeviceId> {
            self.correspondence
                .own_devices()
                .borrow()
                .as_ref()
                .expect("restored")
                .devices
                .clone()
        }

        fn profile(&self) -> ProfileId {
            self.correspondence
                .own_devices()
                .borrow()
                .as_ref()
                .expect("restored")
                .profile
                .clone()
        }

        /// The store as the next boot would read it: the carried genesis
        /// stands the plane up and it resolves to this set.
        fn devices_on_disk(&self) -> Vec<DeviceId> {
            let state = addressbook::ReachStore::at(&self.home)
                .load()
                .expect("load")
                .expect("kept");
            let plane = correspondence::plane::ReachPlane::restore(self.seed, state, 1)
                .expect("the kept store stands up again");
            let mut devices = plane.my_devices();
            devices.sort();
            devices
        }

        async fn mint(&self) {
            let network = self.pair.network().expect("network");
            self.pair
                .mint(&network, now_ms())
                .await
                .expect("a code is minted");
        }

        /// Answer dials until the task is dropped.
        fn serve(&self) -> tokio::task::JoinHandle<()> {
            let pair = self.pair.clone();
            tokio::spawn(async move {
                loop {
                    let _ = pair.accept_one().await;
                    tokio::task::yield_now().await;
                }
            })
        }
    }

    impl Drop for Side {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn code_of(side: &Side) -> String {
        side.pair.status(now_ms()).expect("a code is on show").code
    }

    /// The same code with its last symbol changed: the hint — and so the
    /// endpoint — is the same, the secret is not.
    fn wrong_secret(code: &str) -> String {
        let mut symbols: Vec<char> = code.chars().collect();
        let last = symbols.last_mut().expect("eight symbols");
        *last = if *last == '7' { '3' } else { '7' };
        symbols.into_iter().collect()
    }

    /// Dial the joiner's endpoint as `from`, with whatever frame, the way a
    /// stranger or a misbehaving sponsor would.
    async fn raw_dial(
        net: &MemNet,
        from_seed: &[u8; 32],
        code: &str,
        frame: &PairFrame,
    ) -> PairReply {
        let stranger = net.peer(device_from_seed(from_seed));
        let derived = derive_code(code).expect("derive");
        exchange(&stranger, &derived.pair_id, frame)
            .await
            .expect("the endpoint answers")
    }

    fn sorted(mut devices: Vec<DeviceId>) -> Vec<DeviceId> {
        devices.sort();
        devices
    }

    /// The whole ceremony over the in-memory network: two daemons, one code,
    /// two round trips — and at the end both hold the same verified link
    /// under the same profile, both watches say so, both stores reload to
    /// it, and the joiner's journal words are the ones the sponsor showed.
    #[tokio::test]
    async fn two_services_pair_into_one_profile_over_a_code() {
        let net = MemNet::new();
        let sponsor = Side::stand("sponsor", &net);
        let joiner = Side::stand("joiner", &net);
        assert!(joiner.pair.unpaired());
        assert_ne!(sponsor.profile(), joiner.profile());
        assert!(joiner.pair.status(now_ms()).is_none(), "nothing minted yet");

        joiner.mint().await;
        let code = code_of(&joiner);
        assert_eq!(code.len(), 9, "grouped XXXX-XXXX: {code}");
        let serving = joiner.serve();

        let offer = match sponsor.pair.enter(&code, now_ms()).await.expect("enter") {
            SponsorOutcome::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        };
        assert_eq!(offer.device, joiner.me().as_str());
        assert_eq!(offer.phrase.len(), MAX_CONFIRMATION_PHRASE_WORDS);
        assert_eq!(
            joiner
                .pair
                .pending_phrase()
                .expect("the joiner holds the offer"),
            offer.phrase,
            "both sides derive the same six words"
        );
        assert!(
            joiner.pair.status(now_ms()).is_none(),
            "a spent code is not on show"
        );
        assert_eq!(sponsor.pair.offers(now_ms()).len(), 1);
        // Nothing is written by an offer: both sets are still one device.
        assert_eq!(sponsor.devices(), vec![sponsor.me()]);
        assert_eq!(joiner.devices(), vec![joiner.me()]);

        let adopted = sponsor
            .pair
            .confirm(&offer.pairing, true, now_ms())
            .await
            .expect("confirm");
        assert_eq!(adopted, Some(joiner.me()));
        assert!(sponsor.pair.offers(now_ms()).is_empty());

        let both = sorted(vec![sponsor.me(), joiner.me()]);
        assert_eq!(sponsor.devices(), both, "the sponsor's watch republished");
        assert_eq!(joiner.devices(), both, "the joiner's watch republished");
        assert_eq!(joiner.profile(), sponsor.profile());
        assert_eq!(sponsor.devices_on_disk(), both);
        assert_eq!(joiner.devices_on_disk(), both);
        assert!(matches!(
            joiner.correspondence.origin(),
            Some(addressbook::reach_store::Origin::Adopted { from, .. }) if from == sponsor.me()
        ));
        assert!(joiner.pair.adopted(), "the relaunch is asked for");
        assert!(!joiner.pair.unpaired());
        serving.abort();
    }

    /// The code's whole security posture: the first `Start` that proves it
    /// spends it and a replay is refused as never minted; a wrong proof
    /// counts and three burn it; the proof is bound to the dialer's key, so
    /// a stranger carrying the right proof for somebody else — or naming
    /// somebody else — is refused without spending anything.
    #[tokio::test]
    async fn a_code_is_spent_by_the_first_start_and_a_wrong_proof_burns_it() {
        let net = MemNet::new();
        let sponsor = Side::stand("sponsor", &net);
        let joiner = Side::stand("joiner", &net);
        joiner.mint().await;
        let code = code_of(&joiner);
        let serving = joiner.serve();

        // A stranger who somehow holds the sponsor's proof, naming the
        // sponsor: the transport proved a different key.
        let card = sponsor.correspondence.own_card().expect("card");
        let stranger_seed = [93u8; 32];
        let sponsor_proof = proof_for(
            &normalize_rendezvous_code(&code).expect("code"),
            &sponsor.me(),
        )
        .expect("proof");
        let impersonation = PairFrame::Start {
            proof: sponsor_proof,
            sponsor: sponsor.me(),
            nonce: [1u8; 16],
            epoch: 2,
            card: card.card.encode().expect("encode"),
            routes: Vec::new(),
        };
        assert!(matches!(
            raw_dial(&net, &stranger_seed, &code, &impersonation).await,
            PairReply::Refused
        ));
        // The same stranger naming itself, with the proof for the sponsor's
        // key: bound to the wrong key.
        let misbound = PairFrame::Start {
            proof: sponsor_proof,
            sponsor: device_from_seed(&stranger_seed),
            nonce: [1u8; 16],
            epoch: 2,
            card: card.card.encode().expect("encode"),
            routes: Vec::new(),
        };
        assert!(matches!(
            raw_dial(&net, &stranger_seed, &code, &misbound).await,
            PairReply::Refused
        ));
        assert!(
            joiner.pair.status(now_ms()).is_some(),
            "two wrong tries: the code is still on show"
        );

        // The sponsor with the right proof spends it.
        let offer = match sponsor.pair.enter(&code, now_ms()).await.expect("enter") {
            SponsorOutcome::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        };
        // A replay is refused exactly as a code never minted.
        let replayed = sponsor.pair.enter(&code, now_ms()).await;
        assert!(replayed.is_err(), "a spent code is refused: {replayed:?}");
        assert!(
            joiner.pair.pending_terms().is_some(),
            "the first offer stands"
        );
        drop(offer);
        serving.abort();

        // A fresh code: three wrong secrets burn it, and a fresh one is
        // wanted at once.
        let joiner = Side::stand("joiner-burn", &net);
        joiner.mint().await;
        let code = code_of(&joiner);
        let serving = joiner.serve();
        let wrong = wrong_secret(&code);
        for attempt in 1..=MAX_PAIR_ATTEMPTS {
            let refused = sponsor.pair.enter(&wrong, now_ms()).await;
            assert!(refused.is_err(), "wrong secret, try {attempt}");
        }
        assert!(
            joiner.pair.status(now_ms()).is_none(),
            "three wrong proofs burn the code"
        );
        assert!(joiner.pair.wants_code(now_ms()));
        // Even the right secret is refused now: burnt is burnt.
        assert!(sponsor.pair.enter(&code, now_ms()).await.is_err());
        joiner.mint().await;
        let fresh = code_of(&joiner);
        assert_ne!(fresh, code, "a fresh code is minted, not the burnt one");
        assert!(
            sponsor.pair.enter(&code, now_ms()).await.is_err(),
            "the burnt code's endpoint is gone"
        );
        assert!(matches!(
            sponsor
                .pair
                .enter(&fresh, now_ms())
                .await
                .expect("the fresh code works"),
            SponsorOutcome::Offer(_)
        ));
        assert_eq!(joiner.devices(), vec![joiner.me()], "nothing adopted");
        serving.abort();
    }

    /// Rejecting at confirm is the person saying the words did not match.
    /// Nothing is signed, nothing is sent, nothing is written on either
    /// side — and the offer is gone, so it cannot be confirmed later.
    #[tokio::test]
    async fn an_offer_rejected_at_confirm_writes_nothing() {
        let net = MemNet::new();
        let sponsor = Side::stand("sponsor", &net);
        let joiner = Side::stand("joiner", &net);
        joiner.mint().await;
        let code = code_of(&joiner);
        let serving = joiner.serve();
        let offer = match sponsor.pair.enter(&code, now_ms()).await.expect("enter") {
            SponsorOutcome::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        };
        let before = std::fs::read(joiner.home.join("kinship.bin")).expect("the joiner's store");

        let rejected = sponsor
            .pair
            .confirm(&offer.pairing, false, now_ms())
            .await
            .expect("reject");
        assert_eq!(rejected, None);
        assert!(sponsor.pair.offers(now_ms()).is_empty());
        assert!(
            sponsor
                .pair
                .confirm(&offer.pairing, true, now_ms())
                .await
                .is_err(),
            "a rejected offer cannot be confirmed afterwards"
        );
        assert_eq!(sponsor.devices(), vec![sponsor.me()]);
        assert_eq!(sponsor.devices_on_disk(), vec![sponsor.me()]);
        assert_eq!(joiner.devices(), vec![joiner.me()]);
        assert_eq!(
            std::fs::read(joiner.home.join("kinship.bin")).expect("store"),
            before,
            "the joiner's store is byte-for-byte what it was"
        );
        assert!(!joiner.pair.adopted());
        serving.abort();
    }

    /// The second dial is the sponsor's or nobody's. A `Complete` carrying a
    /// perfectly valid link — both signatures, the right terms — from a key
    /// other than the one the offer was made to is refused, and the joiner
    /// stays unadopted; the same link from the sponsor completes it.
    #[tokio::test]
    async fn a_complete_from_anyone_but_the_sponsor_is_refused() {
        let net = MemNet::new();
        let sponsor = Side::stand("sponsor", &net);
        let joiner = Side::stand("joiner", &net);
        joiner.mint().await;
        let code = code_of(&joiner);
        let serving = joiner.serve();
        let offer = match sponsor.pair.enter(&code, now_ms()).await.expect("enter") {
            SponsorOutcome::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        };
        let (pairing, nonce, epoch) = joiner.pair.pending_terms().expect("pending");
        // The test holds both seeds, so it can make the link the sponsor
        // would; a stranger on the wire cannot, and does not need to for
        // this to be the right refusal.
        let link = DeviceLink::seal(&sponsor.seed, &joiner.seed, nonce, epoch).expect("seal");
        let complete = PairFrame::Complete {
            pairing,
            link,
            routes: Vec::new(),
        };
        assert!(matches!(
            raw_dial(&net, &[94u8; 32], &code, &complete).await,
            PairReply::Refused
        ));
        assert_eq!(joiner.devices(), vec![joiner.me()], "not adopted");
        assert!(!joiner.pair.adopted());
        assert!(
            joiner.pair.pending_terms().is_some(),
            "the offer still stands for the sponsor"
        );

        let adopted = sponsor
            .pair
            .confirm(&offer.pairing, true, now_ms())
            .await
            .expect("confirm");
        assert_eq!(adopted, Some(joiner.me()));
        assert_eq!(joiner.devices(), sorted(vec![sponsor.me(), joiner.me()]));
        serving.abort();
    }

    /// The sponsor adopts before it dials `Complete`, so losing that dial
    /// costs nothing: the joiner shows its next code, the sponsor enters it,
    /// and the card the sponsor sends already roots the joiner — `Start`
    /// adopts from it and answers `Done`.
    #[tokio::test]
    async fn a_lost_complete_is_recovered_by_entering_the_next_code() {
        let net = MemNet::new();
        let sponsor = Side::stand("sponsor", &net);
        let joiner = Side::stand("joiner", &net);
        joiner.mint().await;
        let code = code_of(&joiner);
        let serving = joiner.serve();
        let offer = match sponsor.pair.enter(&code, now_ms()).await.expect("enter") {
            SponsorOutcome::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        };
        let pair_id = derive_code(&code).expect("derive").pair_id;
        net.partition(&sponsor.me(), &pair_id);
        let adopted = sponsor
            .pair
            .confirm(&offer.pairing, true, now_ms())
            .await
            .expect("the sponsor adopts whether or not the joiner hears");
        assert_eq!(adopted, Some(joiner.me()));
        assert_eq!(sponsor.devices(), sorted(vec![sponsor.me(), joiner.me()]));
        assert_eq!(
            joiner.devices(),
            vec![joiner.me()],
            "the joiner never heard"
        );
        net.heal();
        serving.abort();

        joiner.mint().await;
        let next = code_of(&joiner);
        assert_ne!(next, code);
        let serving = joiner.serve();
        match sponsor.pair.enter(&next, now_ms()).await.expect("enter") {
            SponsorOutcome::Paired { device } => assert_eq!(device, joiner.me()),
            other => panic!("expected the joiner to be paired from the card, got {other:?}"),
        }
        let both = sorted(vec![sponsor.me(), joiner.me()]);
        assert_eq!(joiner.devices(), both);
        assert_eq!(joiner.devices_on_disk(), both);
        assert_eq!(joiner.profile(), sponsor.profile());
        assert!(joiner.pair.adopted());
        serving.abort();
    }

    /// The six words commit the destination profile, both nonces and both
    /// device ids, and nothing else: change any one and the words change;
    /// swap the two devices and they do not.
    #[test]
    fn the_phrase_commits_the_profile_both_nonces_and_both_devices() {
        let a = device_from_seed(&[1u8; 32]);
        let b = device_from_seed(&[2u8; 32]);
        let c = device_from_seed(&[3u8; 32]);
        let profile = ProfileId::from_genesis(b"one");
        let other = ProfileId::from_genesis(b"two");
        let words = confirmation_phrase(&profile, &[4u8; 16], &[5u8; 16], &a, &b);
        assert_eq!(words.len(), MAX_CONFIRMATION_PHRASE_WORDS);
        assert!(words
            .iter()
            .all(|word| CONFIRMATION_WORDS.contains(&word.as_str())));
        assert_eq!(
            words,
            confirmation_phrase(&profile, &[4u8; 16], &[5u8; 16], &b, &a),
            "device order is not a fact"
        );
        assert_ne!(
            words,
            confirmation_phrase(&other, &[4u8; 16], &[5u8; 16], &a, &b)
        );
        assert_ne!(
            words,
            confirmation_phrase(&profile, &[6u8; 16], &[5u8; 16], &a, &b)
        );
        assert_ne!(
            words,
            confirmation_phrase(&profile, &[4u8; 16], &[6u8; 16], &a, &b)
        );
        assert_ne!(
            words,
            confirmation_phrase(&profile, &[4u8; 16], &[5u8; 16], &a, &c)
        );
    }

    /// The routing half derives the endpoint and only the secret half the
    /// proof — so two codes sharing a hint share an endpoint and nothing
    /// else, and a proof is bound to the sponsor that sends it.
    #[test]
    fn the_hint_derives_the_endpoint_and_the_secret_only_the_proof() {
        let sponsor = device_from_seed(&[7u8; 32]);
        let other = device_from_seed(&[8u8; 32]);
        let one = derive_code("ABCD-2345").expect("code");
        let two = derive_code("abcd 6789").expect("code");
        assert_eq!(one.pair_id, two.pair_id, "same hint, same endpoint");
        assert_ne!(
            proof_for(&one.code, &sponsor).expect("proof"),
            proof_for(&two.code, &sponsor).expect("proof"),
            "different secret, different proof"
        );
        assert_ne!(
            proof_for(&one.code, &sponsor).expect("proof"),
            proof_for(&one.code, &other).expect("proof"),
            "the proof names its sponsor"
        );
        let (code, addrs) = parse_entry(" ABCD-2345@127.0.0.1:7000,[::1]:7001 ").expect("entry");
        assert_eq!(code, "ABCD-2345");
        assert_eq!(addrs.len(), 2);
        assert!(parse_entry("ABCD-2345@nowhere").is_err());
        assert_eq!(entry_spelling("ABCD-2345", &[]), "ABCD-2345");
        assert_eq!(
            entry_spelling("ABCD-2345", &addrs),
            "ABCD-2345@127.0.0.1:7000,[::1]:7001"
        );
    }
}
