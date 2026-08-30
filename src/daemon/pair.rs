//! Pairing a new device into a profile: a code-derived endpoint and two
//! round trips.
//!
//! The joiner — a Pi, a NAS, a second laptop — has a seed and a throwaway
//! profile and nobody who knows it. It prints a code. On a device the person
//! already owns, the code is entered; that device is the sponsor. The code is
//! eight symbols of the display rendezvous alphabet, split down the middle:
//! the first four derive the ephemeral endpoint the sponsor dials — a routing
//! hint, 2^20, and public, because the endpoint's key is — and the last four
//! are the password of a balanced PAKE ([`mechanics::pake`], SPAKE2). The
//! endpoint being public means a stranger can stand up the same key and take
//! the sponsor's dial; what the PAKE guarantees is that such a dial yields
//! the stranger **one online guess** at the four secret symbols and nothing
//! to grind offline, and that the sponsor sends nothing else — no card, no
//! nonce — until the other side has proved the same password. Three wrong
//! guesses, wherever they land, burn the code and the joiner mints another.
//!
//! Round trip one, on one stream: `Start` carries the sponsor's share; the
//! joiner spends the code before a byte leaves, answers its share and its
//! confirmation; the sponsor verifies that, then sends its own confirmation
//! with its card and a nonce; the joiner verifies, signs its half of the
//! link, and answers `Offer` with that half and six words. Round trip two,
//! `Complete` → `Done`: once the person confirms the words, the sponsor signs
//! its half, assembles the link — verified as if `seal` had made it — adopts
//! the device, and carries the whole link to the joiner, which verifies it
//! again and becomes a device of the profile. A lost answer costs nothing on
//! either side: the sponsor has already adopted, and the joiner recovers by
//! showing its next code, which the sponsor's card then already roots.
//!
//! What the phrase protects, stated plainly: for a headless joiner the code
//! is the entire approval — whoever proves it is the sponsor. The six words
//! are the **sponsor's** check: they commit the destination profile, both
//! nonces, both device ids and the PAKE's session key, so a stranger who took
//! the dial and guessed wrong shows words the real joiner's journal never
//! does. No seed crosses a machine; only halves do, and nothing half-signed
//! is ever a link.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use comms::policy::Network;
use comms::{Stream, Transport};
use display_protocol::bounds::{MAX_CONFIRMATION_PHRASE_WORDS, MAX_PAIRING_LIFETIME_MS};
use display_protocol::pairing::{
    group_rendezvous_code, normalize_rendezvous_code, CONFIRMATION_WORDS, RENDEZVOUS_CODE_CHARS,
};
use mechanics::actor::device_from_seed;
use mechanics::ids::DeviceId;
use mechanics::kinship::{DeviceLink, Entry, KinshipLog, ProfileId, Signature, Standing};
use mechanics::pake;
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
/// Wrong guesses before the code burns — counted where they land: a wrong
/// confirmation, a dial that never confirms, a `Start` from the wrong key.
const MAX_PAIR_ATTEMPTS: u8 = 3;
/// A code's life — the display rendezvous number, not one of this module's.
const PAIR_CODE_LIFETIME_MS: u64 = RENDEZVOUS_LIFETIME_MS;
/// The routing half of the code.
const HINT_CHARS: usize = RENDEZVOUS_CODE_CHARS / 2;
const ENDPOINT_CONTEXT: &str = "lait/pair/endpoint/v1";
const PHRASE_DOMAIN: &[u8] = b"lait/pair/confirmation-phrase/v2";
const PAIR_PROTOCOL: u16 = 2;
const ROUTE_DEADLINE: Duration = Duration::from_secs(3);
const DIAL_DEADLINE: Duration = Duration::from_secs(10);
const FRAME_DEADLINE: Duration = Duration::from_secs(5);
/// How long `accept_one` parks before it hands the loop back to re-check
/// expiry: short enough that a burnt or expired code is replaced promptly.
const ACCEPT_SLICE: Duration = Duration::from_secs(30);

/// Sponsor → joiner.
#[derive(Debug, Serialize, Deserialize)]
enum PairFrame {
    /// Round trip one, first frame: who is dialing, and its PAKE share.
    Start { sponsor: DeviceId, share: [u8; 32] },
    /// Round trip one, third frame, on the same stream: the sponsor's PAKE
    /// confirmation and — now that the joiner has proved the password — its
    /// card and the terms of the link.
    Confirm {
        confirmation: [u8; 32],
        nonce: [u8; 16],
        epoch: u64,
        /// `Announcement::encode` of the sponsor's own card. The sponsor does
        /// not know the joiner's id at this point, so it projects for itself
        /// under `own: true` — the structural bodies ride regardless of the
        /// `device` slot, and that is what the joiner needs to walk from the
        /// genesis.
        card: Vec<u8>,
        routes: Vec<SocketAddr>,
    },
    /// Round trip two: the assembled link.
    Complete {
        pairing: [u8; 16],
        link: DeviceLink,
        routes: Vec<SocketAddr>,
    },
    /// Round trip two, the other way: the person rejected the words. The
    /// joiner drops the offer and shows a fresh code at once.
    Abort { pairing: [u8; 16] },
}

/// Joiner → sponsor.
#[derive(Debug, Serialize, Deserialize)]
enum PairReply {
    /// Round trip one, second frame: the joiner's share and confirmation.
    Share {
        share: [u8; 32],
        confirmation: [u8; 32],
    },
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
    /// password, wrong caller, and a link that did not assemble: a caller
    /// whose guess failed learns nothing but that it failed.
    Refused,
}

/// A minted code and the endpoint it derives, alive for the code's life.
struct Outstanding {
    /// Normalised, eight symbols.
    code: String,
    pair_id: DeviceId,
    endpoint: Arc<dyn Transport>,
    /// The identity endpoint, for learning the sponsor's routes.
    identity: Arc<dyn Transport>,
    direct: Vec<SocketAddr>,
    attempts: u8,
    expires_at_ms: u64,
    /// Taken by a `Start`: from then until the dial either confirms or
    /// ends, and for good once an offer has been made on it.
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

    /// One wrong guess. The code is free to be tried again unless that was
    /// the last try.
    fn missed(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
        self.spent = false;
        if self.burnt() {
            tracing::warn!(
                target: "lait::pair",
                "somebody tried a wrong code; it is burnt and a fresh one is minted"
            );
        }
    }

    /// The four symbols the PAKE is over.
    fn password(&self) -> &str {
        self.code.get(HINT_CHARS..).unwrap_or("")
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
            correspondence,
            hub,
            joiner: Mutex::new(Joiner::default()),
            sponsoring: Mutex::new(BTreeMap::new()),
            adopted: AtomicBool::new(false),
        }
    }

    /// The identity's seed, read when an act needs it and never cached: a
    /// seed held from construction would be a second copy that could not
    /// notice the home changing under it.
    fn seed(&self) -> Result<[u8; 32]> {
        crate::config::load_identity(&self.identity).context("this home holds no identity to pair")
    }

    pub(crate) fn network(&self) -> Result<Network> {
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
    /// unspent, or spent with its offer gone or expired unadopted.
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
                pair_id: derived.pair_id,
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
    ///
    /// The blind window, stated: a spent code whose offer is waiting on the
    /// sponsor shows `None` for up to `MAX_PAIRING_LIFETIME_MS`. A rejection
    /// reaches here as `Abort` and ends it at once; a `Complete` that was
    /// lost does not, and the next code appears when the offer expires —
    /// after which entering it completes the adoption from the sponsor's
    /// card alone.
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

    /// Answer one dial on the pairing endpoint: round trip one (`Start`,
    /// then `Confirm` on the same stream), a `Complete`, or an `Abort`.
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
        let from = incoming.from;
        let mut stream = incoming.stream;
        let Some(frame) = recv_frame::<PairFrame>(stream.as_mut()).await else {
            return Ok(Accepted::Served);
        };
        let reply = match frame {
            PairFrame::Start { sponsor, share } => {
                self.round_trip_one(from, sponsor, share, stream.as_mut())
                    .await
            }
            PairFrame::Complete {
                pairing,
                link,
                routes,
            } => Some(self.complete(from, pairing, link, &routes, now_ms())),
            PairFrame::Abort { pairing } => Some(self.aborted(&from, pairing)),
            PairFrame::Confirm { .. } => Some(PairReply::Refused),
        };
        if let Some(reply) = reply {
            let bytes = postcard::to_stdvec(&reply).context("encode the pairing reply")?;
            let _ = tokio::time::timeout(FRAME_DEADLINE, stream.send(&bytes)).await;
        }
        let _ = stream.finish().await;
        let _ = tokio::time::timeout(FRAME_DEADLINE, stream.wait_closed()).await;
        Ok(if self.adopted() {
            Accepted::Adopted
        } else {
            Accepted::Served
        })
    }

    /// The joiner's side of round trip one. The code is spent when the
    /// `Start` is taken; the sponsor's confirmation is what keeps it spent.
    /// A dial that confirms wrongly, or never confirms, is one guess.
    async fn round_trip_one(
        &self,
        from: DeviceId,
        sponsor: DeviceId,
        share: [u8; 32],
        stream: &mut dyn Stream,
    ) -> Option<PairReply> {
        let session = match self.begin(&from, &sponsor, &share) {
            Ok((session, reply)) => {
                let bytes = postcard::to_stdvec(&reply).ok()?;
                if tokio::time::timeout(FRAME_DEADLINE, stream.send(&bytes))
                    .await
                    .is_err()
                {
                    self.missed();
                    return None;
                }
                session
            }
            Err(reply) => return Some(reply),
        };
        let Some(PairFrame::Confirm {
            confirmation,
            nonce,
            epoch,
            card,
            routes,
        }) = recv_frame::<PairFrame>(stream).await
        else {
            // The dial ended without a confirmation: a wrong guess that
            // learned it was wrong from our confirmation, or a line that
            // dropped. Either way the sponsor did not prove the code.
            self.missed();
            return None;
        };
        Some(self.confirmed(
            from,
            &session,
            Confirm {
                confirmation,
                nonce,
                epoch,
                card,
                routes,
            },
            now_ms(),
        ))
    }

    /// Take a `Start`: spend the code, run the joiner's side of the PAKE,
    /// and answer the share and confirmation. Refuses — as one word — a code
    /// not on show and a dialer who is not the sponsor it names.
    fn begin(
        &self,
        from: &DeviceId,
        sponsor: &DeviceId,
        share: &[u8; 32],
    ) -> Result<(pake::Session, PairReply), PairReply> {
        let me = device_from_seed(&self.seed().map_err(|_| PairReply::Refused)?);
        let mut joiner = self.joiner.lock_recovering();
        let outstanding = joiner.outstanding.as_mut().ok_or(PairReply::Refused)?;
        if !outstanding.enterable(now_ms()) {
            return Err(PairReply::Refused);
        }
        // The password is bound to the dialer's proven key through the PAKE
        // identities: a Start naming a sponsor other than the key the
        // transport proved is a wrong try, whatever share it carries.
        if from != sponsor || from == &me {
            outstanding.missed();
            return Err(PairReply::Refused);
        }
        outstanding.spent = true;
        let (exchange, mine) = pake::Exchange::start(
            pake::Role::B,
            sponsor.as_str().as_bytes(),
            outstanding.pair_id.as_str().as_bytes(),
            outstanding.password().as_bytes(),
        )
        .map_err(|_| PairReply::Refused)?;
        let session = exchange.finish(share).map_err(|_| {
            outstanding.missed();
            PairReply::Refused
        })?;
        let confirmation = session.confirmation();
        Ok((
            session,
            PairReply::Share {
                share: mine,
                confirmation,
            },
        ))
    }

    /// One wrong guess against the outstanding code.
    fn missed(&self) {
        if let Some(outstanding) = self.joiner.lock_recovering().outstanding.as_mut() {
            outstanding.missed();
        }
    }

    /// The sponsor confirmed: verify it, then the card, then make the
    /// offer. Every refusal after the confirmation verified leaves the code
    /// spent, because the sponsor proved it — the loop mints another.
    fn confirmed(
        &self,
        from: DeviceId,
        session: &pake::Session,
        confirm: Confirm,
        now_ms: u64,
    ) -> PairReply {
        let Ok(seed) = self.seed() else {
            return PairReply::Refused;
        };
        let me = device_from_seed(&seed);
        let mut joiner = self.joiner.lock_recovering();
        let Some(outstanding) = joiner.outstanding.as_mut() else {
            return PairReply::Refused;
        };
        if session.confirm(&confirm.confirmation).is_err() {
            outstanding.missed();
            return PairReply::Refused;
        }
        let identity = outstanding.identity.clone();
        let direct = outstanding.direct.clone();

        // The sponsor's card: anchored to its genesis, signed by a device the
        // genesis roots, and legible to an own reader.
        let Ok(card) = addressbook::Announcement::decode(&confirm.card) else {
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
        identity.learn(from.clone(), &confirm.routes);

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

        let joiner_half = DeviceLink::half(&seed, &from, confirm.nonce, confirm.epoch);
        let (Ok(pairing), Ok(joiner_nonce)) = (random_bytes(), random_bytes()) else {
            return PairReply::Refused;
        };
        let phrase = confirmation_phrase(
            &card.profile,
            session.key(),
            &confirm.nonce,
            &joiner_nonce,
            &me,
            &from,
        );
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
            nonce: confirm.nonce,
            epoch: confirm.epoch,
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

    /// The sponsor rejected the words: drop the offer, so the loop mints a
    /// fresh code at once rather than waiting the offer out. Only the
    /// sponsor the offer was made to can end it.
    fn aborted(&self, from: &DeviceId, pairing: [u8; 16]) -> PairReply {
        let mut joiner = self.joiner.lock_recovering();
        let ours = joiner
            .pending
            .as_ref()
            .is_some_and(|pending| pending.pairing == pairing && &pending.sponsor == from);
        if ours {
            joiner.pending = None;
            tracing::info!(target: "lait::pair", "the words were rejected; showing a fresh code");
        }
        PairReply::Refused
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

    /// Wrong guesses counted against the code on show.
    #[cfg(test)]
    fn attempts(&self) -> u8 {
        self.joiner
            .lock_recovering()
            .outstanding
            .as_ref()
            .map_or(0, |outstanding| outstanding.attempts)
    }

    /// Whether a pairing endpoint is up.
    #[cfg(test)]
    pub(crate) fn endpoint_open(&self) -> bool {
        self.joiner.lock_recovering().outstanding.is_some()
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

    /// Enter a code a new device printed: dial the endpoint it derives, run
    /// the PAKE over its secret half, and only once the other side has
    /// proved the same code hand over this profile's card and hold the offer
    /// that comes back for the person to confirm.
    pub async fn enter(&self, entered: &str, now_ms: u64) -> Result<SponsorOutcome> {
        let seed = self.seed()?;
        let me = device_from_seed(&seed);
        let (code, addrs) = parse_entry(entered)?;
        let derived = derive_code(&code)?;
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

        let (exchange, share) = pake::Exchange::start(
            pake::Role::A,
            me.as_str().as_bytes(),
            derived.pair_id.as_str().as_bytes(),
            derived.password.as_bytes(),
        )
        .map_err(|refusal| anyhow!("begin the pairing exchange: {refusal}"))?;
        let mut stream = dial(identity.as_ref(), &derived.pair_id).await?;
        send_frame(
            stream.as_mut(),
            &PairFrame::Start {
                sponsor: me.clone(),
                share,
            },
        )
        .await?;
        let (their_share, their_confirmation) = match recv_reply(stream.as_mut()).await? {
            PairReply::Share {
                share,
                confirmation,
            } => (share, confirmation),
            PairReply::Refused => bail!("the device refused that code"),
            other => bail!("the device answered out of turn: {other:?}"),
        };
        let session = exchange
            .finish(&their_share)
            .map_err(|refusal| anyhow!("the device's share is not usable: {refusal}"))?;
        // Nothing further leaves until the other side has proved the code:
        // dropping the stream here is what tells it the guess was wrong.
        if session.confirm(&their_confirmation).is_err() {
            drop(stream);
            bail!("the device did not accept that code");
        }
        let routes = identity
            .advertised_routes(ROUTE_DEADLINE)
            .await
            .unwrap_or_default();
        let nonce = random_bytes()?;
        let epoch = own.epoch.saturating_add(1);
        send_frame(
            stream.as_mut(),
            &PairFrame::Confirm {
                confirmation: session.confirmation(),
                nonce,
                epoch,
                card,
                routes,
            },
        )
        .await?;
        match recv_reply(stream.as_mut()).await? {
            PairReply::Refused => bail!("the device refused that code"),
            PairReply::Done { device } => Ok(SponsorOutcome::Paired { device }),
            PairReply::Share { .. } => bail!("the device answered out of turn"),
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
                let phrase = confirmation_phrase(
                    &own.card.profile,
                    session.key(),
                    &nonce,
                    &joiner_nonce,
                    &me,
                    &joiner,
                );
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

    /// Confirm or reject an offer. Rejecting drops it and tells the joiner,
    /// so it shows a fresh code at once. Confirming signs this side's half,
    /// assembles the link, adopts the device here, and carries the link to
    /// it; once the adoption is kept nothing after it can fail the call —
    /// the joiner recovers from the next code it shows.
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
            // Best effort: the offer is gone here whether or not the joiner
            // hears; what the dial buys is a fresh code there now rather
            // than when the offer expires.
            if let Err(error) = self
                .carry(
                    &pending.pair_id,
                    PairFrame::Abort {
                        pairing: pending.pairing,
                    },
                )
                .await
            {
                tracing::debug!(%error, "the rejected device was not told; its offer expires");
            }
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

        match self
            .carry(
                &pending.pair_id,
                PairFrame::Complete {
                    pairing: pending.pairing,
                    link,
                    routes: Vec::new(),
                },
            )
            .await
        {
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

    /// Round trip two: one frame to the joiner's endpoint, one answer back.
    /// The routes the frame carries are the identity endpoint's.
    async fn carry(&self, pair_id: &DeviceId, frame: PairFrame) -> Result<PairReply> {
        let seed = self.seed()?;
        let network = self.network()?;
        let identity = self
            .hub
            .identity_transport(&seed, &network)
            .await
            .context("the identity endpoint is not up")?;
        let frame = match frame {
            PairFrame::Complete { pairing, link, .. } => PairFrame::Complete {
                pairing,
                link,
                routes: identity
                    .advertised_routes(ROUTE_DEADLINE)
                    .await
                    .unwrap_or_default(),
            },
            other => other,
        };
        let mut stream = dial(identity.as_ref(), pair_id).await?;
        send_frame(stream.as_mut(), &frame).await?;
        recv_reply(stream.as_mut()).await
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

/// The fields of a `Confirm`, named so `confirmed` reads them rather than a
/// tuple.
struct Confirm {
    confirmation: [u8; 32],
    nonce: [u8; 16],
    epoch: u64,
    card: Vec<u8>,
    routes: Vec<SocketAddr>,
}

async fn dial(transport: &dyn Transport, peer: &DeviceId) -> Result<Box<dyn Stream>> {
    tokio::time::timeout(DIAL_DEADLINE, transport.connect(peer.clone(), PAIR_ALPN))
        .await
        .context("the pairing endpoint did not answer the dial")?
        .context("dial the pairing endpoint")
}

async fn send_frame(stream: &mut dyn Stream, frame: &PairFrame) -> Result<()> {
    let bytes = postcard::to_stdvec(frame).context("encode the pairing frame")?;
    if bytes.len() > MAX_PAIR_FRAME {
        bail!("the pairing frame is larger than the protocol allows");
    }
    tokio::time::timeout(FRAME_DEADLINE, stream.send(&bytes))
        .await
        .context("the pairing endpoint stopped taking the frame")?
}

async fn recv_reply(stream: &mut dyn Stream) -> Result<PairReply> {
    let reply = tokio::time::timeout(FRAME_DEADLINE, stream.recv_bounded(MAX_PAIR_FRAME))
        .await
        .context("the pairing endpoint did not answer")??
        .ok_or_else(|| anyhow!("the pairing endpoint closed without answering"))?;
    postcard::from_bytes(&reply).context("decode the pairing reply")
}

/// One bounded frame, decoded; `None` for a stream that ended, timed out,
/// or carried something else.
async fn recv_frame<T: serde::de::DeserializeOwned>(stream: &mut dyn Stream) -> Option<T> {
    match tokio::time::timeout(FRAME_DEADLINE, stream.recv_bounded(MAX_PAIR_FRAME)).await {
        Ok(Ok(Some(frame))) => postcard::from_bytes(&frame).ok(),
        _ => None,
    }
}

/// A code, split: the hint derives the endpoint, the rest is the password.
struct Derived {
    code: String,
    password: String,
    pair_seed: [u8; 32],
    pair_id: DeviceId,
}

fn derive_code(entered: &str) -> Result<Derived> {
    let code = normalize_rendezvous_code(entered)
        .map_err(|refusal| anyhow!("that is not a pairing code: {refusal}"))?;
    let (Some(hint), Some(password)) = (code.get(..HINT_CHARS), code.get(HINT_CHARS..)) else {
        bail!("that is not a pairing code");
    };
    let pair_seed = blake3::derive_key(ENDPOINT_CONTEXT, hint.as_bytes());
    let pair_id = device_from_seed(&pair_seed);
    Ok(Derived {
        password: password.to_owned(),
        code,
        pair_seed,
        pair_id,
    })
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

/// The six words both sides show. Commits the destination profile, the
/// PAKE's session key, both nonces and both device ids — the whole link that
/// will be signed, and the session it was agreed in — so a stranger who took
/// the dial and guessed wrong shows words the real joiner's journal never
/// does.
pub(crate) fn confirmation_phrase(
    profile: &ProfileId,
    session_key: &[u8; 16],
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
    framed(&mut preimage, session_key);
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
/// Read from the environment and `/etc/hostname`; a box that names itself
/// nowhere is "a device", which is a name and not a claim.
fn device_name() -> String {
    let named = |name: String| {
        let name = name.trim().to_owned();
        (!name.is_empty()).then_some(name)
    };
    std::env::var("HOSTNAME")
        .ok()
        .and_then(named)
        .or_else(|| std::env::var("COMPUTERNAME").ok().and_then(named))
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .and_then(named)
        })
        .unwrap_or_else(|| "a device".to_owned())
}

#[cfg(test)]
pub(crate) mod tests {
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
    pub(crate) struct Side {
        home: PathBuf,
        seed: [u8; 32],
        correspondence: Arc<CorrespondenceService>,
        pub(crate) pair: Arc<PairService>,
    }

    impl Side {
        pub(crate) fn stand(tag: &str, net: &MemNet) -> Self {
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

        pub(crate) fn me(&self) -> DeviceId {
            device_from_seed(&self.seed)
        }

        pub(crate) fn devices(&self) -> Vec<DeviceId> {
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

    pub(crate) fn code_of(side: &Side) -> String {
        side.pair.status(now_ms()).expect("a code is on show").code
    }

    /// The same code with its last symbol changed: the hint — and so the
    /// endpoint — is the same, the secret is not.
    pub(crate) fn wrong_secret(code: &str) -> String {
        let mut symbols: Vec<char> = code.chars().collect();
        let last = symbols.last_mut().expect("eight symbols");
        *last = if *last == '7' { '3' } else { '7' };
        symbols.into_iter().collect()
    }

    /// Dial the joiner's endpoint as `from` with one frame — a `Complete` or
    /// an `Abort` — the way a stranger or a misbehaving sponsor would.
    async fn raw_dial(
        net: &MemNet,
        from_seed: &[u8; 32],
        code: &str,
        frame: &PairFrame,
    ) -> PairReply {
        let stranger = net.peer(device_from_seed(from_seed));
        let derived = derive_code(code).expect("derive");
        let mut stream = dial(&stranger, &derived.pair_id).await.expect("dial");
        send_frame(stream.as_mut(), frame).await.expect("send");
        recv_reply(stream.as_mut())
            .await
            .expect("the endpoint answers")
    }

    /// What a `Start` came back with.
    enum Started {
        Refused,
        /// A share and a confirmation, and whether that confirmation
        /// verified under the password the dialer used.
        Share {
            verified: bool,
        },
    }

    /// Dial round trip one as `from_seed`, naming `sponsor`, over `password`,
    /// and stop after the joiner's answer — the way a stranger with a guess
    /// would, having learned whether the guess was right and nothing else.
    async fn raw_start(
        net: &MemNet,
        from_seed: &[u8; 32],
        code: &str,
        sponsor: &DeviceId,
        password: &str,
    ) -> Started {
        let stranger = net.peer(device_from_seed(from_seed));
        let derived = derive_code(code).expect("derive");
        let (exchange, share) = pake::Exchange::start(
            pake::Role::A,
            sponsor.as_str().as_bytes(),
            derived.pair_id.as_str().as_bytes(),
            password.as_bytes(),
        )
        .expect("start");
        let mut stream = dial(&stranger, &derived.pair_id).await.expect("dial");
        send_frame(
            stream.as_mut(),
            &PairFrame::Start {
                sponsor: sponsor.clone(),
                share,
            },
        )
        .await
        .expect("send");
        match recv_reply(stream.as_mut())
            .await
            .expect("the endpoint answers")
        {
            PairReply::Refused => Started::Refused,
            PairReply::Share {
                share,
                confirmation,
            } => {
                let session = exchange.finish(&share).expect("finish");
                Started::Share {
                    verified: session.confirm(&confirmation).is_ok(),
                }
            }
            other => panic!("out of turn: {other:?}"),
        }
    }

    /// Poll until `check` holds, or fail: the joiner counts a guess when the
    /// dial ends, which is a moment after the sponsor gave up on it.
    async fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("never: {what}");
    }

    pub(crate) fn sorted(mut devices: Vec<DeviceId>) -> Vec<DeviceId> {
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
    /// spends it and a replay is refused as never minted; a wrong guess
    /// counts and three burn it; the PAKE identities bind the password to
    /// the dialer's key, so a stranger naming somebody else is refused
    /// before a share is answered.
    #[tokio::test]
    async fn a_code_is_spent_by_the_first_start_and_a_wrong_guess_burns_it() {
        let net = MemNet::new();
        let sponsor = Side::stand("sponsor", &net);
        let joiner = Side::stand("joiner", &net);
        joiner.mint().await;
        let code = code_of(&joiner);
        let serving = joiner.serve();

        // A stranger dialing as itself but naming the sponsor: the transport
        // proved a different key, and the Start is refused before any share
        // is answered — a guess spent on nothing.
        let stranger_seed = [93u8; 32];
        assert!(matches!(
            raw_start(&net, &stranger_seed, &code, &sponsor.me(), "0000").await,
            Started::Refused
        ));
        // The same stranger naming itself, with a wrong password: it gets a
        // share and a confirmation it cannot verify, and learns exactly that
        // its guess was wrong.
        assert!(matches!(
            raw_start(
                &net,
                &stranger_seed,
                &code,
                &device_from_seed(&stranger_seed),
                "0000"
            )
            .await,
            Started::Share { verified: false }
        ));
        wait_until("two guesses are counted", || joiner.pair.attempts() == 2).await;
        assert!(
            joiner.pair.status(now_ms()).is_some(),
            "two wrong tries: the code is still on show"
        );

        // The sponsor with the right password spends it.
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
        wait_until("the third guess is counted", || {
            joiner.pair.status(now_ms()).is_none()
        })
        .await;
        assert!(
            joiner.pair.wants_code(now_ms()),
            "three wrong guesses burn the code"
        );
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

    /// The six words commit the destination profile, the session key, both
    /// nonces and both device ids, and nothing else: change any one and the
    /// words change; swap the two devices and they do not.
    #[test]
    fn the_phrase_commits_the_profile_both_nonces_and_both_devices() {
        let a = device_from_seed(&[1u8; 32]);
        let b = device_from_seed(&[2u8; 32]);
        let c = device_from_seed(&[3u8; 32]);
        let profile = ProfileId::from_genesis(b"one");
        let other = ProfileId::from_genesis(b"two");
        let key = [9u8; 16];
        let words = confirmation_phrase(&profile, &key, &[4u8; 16], &[5u8; 16], &a, &b);
        assert_eq!(words.len(), MAX_CONFIRMATION_PHRASE_WORDS);
        assert!(words
            .iter()
            .all(|word| CONFIRMATION_WORDS.contains(&word.as_str())));
        assert_eq!(
            words,
            confirmation_phrase(&profile, &key, &[4u8; 16], &[5u8; 16], &b, &a),
            "device order is not a fact"
        );
        let other_key = [8u8; 16];
        assert_ne!(
            words,
            confirmation_phrase(&other, &key, &[4u8; 16], &[5u8; 16], &a, &b)
        );
        assert_ne!(
            words,
            confirmation_phrase(&profile, &other_key, &[4u8; 16], &[5u8; 16], &a, &b)
        );
        assert_ne!(
            words,
            confirmation_phrase(&profile, &key, &[6u8; 16], &[5u8; 16], &a, &b)
        );
        assert_ne!(
            words,
            confirmation_phrase(&profile, &key, &[4u8; 16], &[6u8; 16], &a, &b)
        );
        assert_ne!(
            words,
            confirmation_phrase(&profile, &key, &[4u8; 16], &[5u8; 16], &a, &c)
        );
    }

    /// The routing half derives the endpoint and only the secret half the
    /// PAKE password — so two codes sharing a hint share an endpoint and
    /// nothing else, and the hint never enters the password.
    #[test]
    fn the_hint_derives_the_endpoint_and_the_secret_only_the_password() {
        let one = derive_code("ABCD-2345").expect("code");
        let two = derive_code("abcd 6789").expect("code");
        assert_eq!(one.pair_id, two.pair_id, "same hint, same endpoint");
        assert_eq!(one.password, "2345");
        assert_eq!(two.password, "6789");
        assert_ne!(
            derive_code("ABCE-2345").expect("code").pair_id,
            one.pair_id,
            "a different hint is a different endpoint"
        );
        assert_ne!(
            pake::password_scalar(one.password.as_bytes()),
            pake::password_scalar(two.password.as_bytes()),
            "the password is the secret half alone"
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

    /// The threat the PAKE closes. A stranger inverts the hint, stands up the
    /// same endpoint key and takes the sponsor's dial. What it records is the
    /// sponsor's share and nothing after it: the sponsor sends no
    /// confirmation, no card and no nonce, because the stranger's own
    /// confirmation — made with a guess — does not verify. Offline, every
    /// candidate password yields a session with nothing recorded to check it
    /// against, the true one included: one online guess, nothing to grind.
    /// And the real joiner, which never saw the dial, is untouched.
    #[tokio::test]
    async fn an_impostor_endpoint_learns_one_guess_and_nothing_to_grind() {
        let net = MemNet::new();
        let sponsor = Side::stand("sponsor", &net);
        let joiner = Side::stand("joiner", &net);
        joiner.mint().await;
        let code = code_of(&joiner);
        let derived = derive_code(&code).expect("derive");
        let pair_id = derived.pair_id.clone();

        // The impostor stands up the hint-derived key; on this network that
        // takes the sponsor's next dial.
        let impostor: Arc<dyn Transport> = Arc::new(net.peer(pair_id.clone()));
        let recorder = {
            let pair_id = pair_id.clone();
            tokio::spawn(async move {
                let incoming = impostor.accept().await.expect("the sponsor's dial");
                let mut stream = incoming.stream;
                let Some(PairFrame::Start { sponsor, share }) =
                    recv_frame::<PairFrame>(stream.as_mut()).await
                else {
                    panic!("the first frame is a Start");
                };
                // Answer with a guess, as the protocol allows exactly once.
                let (exchange, mine) = pake::Exchange::start(
                    pake::Role::B,
                    sponsor.as_str().as_bytes(),
                    pair_id.as_str().as_bytes(),
                    b"0000",
                )
                .expect("start");
                let session = exchange.finish(&share).expect("finish");
                let reply = postcard::to_stdvec(&PairReply::Share {
                    share: mine,
                    confirmation: session.confirmation(),
                })
                .expect("encode");
                stream.send(&reply).await.expect("send");
                // Everything the sponsor sends after that is the transcript
                // the impostor gets to keep.
                let further = recv_frame::<PairFrame>(stream.as_mut()).await;
                (sponsor, share, further)
            })
        };
        let entered = sponsor.pair.enter(&code, now_ms()).await;
        assert!(entered.is_err(), "the sponsor did not accept the impostor");
        let (sponsor_id, share, further) = recorder.await.expect("recorded");
        assert!(
            further.is_none(),
            "the sponsor sent nothing after the failed confirmation: {further:?}"
        );
        assert!(sponsor.pair.offers(now_ms()).is_empty());

        // Offline, against the transcript: the sponsor's share alone. Every
        // candidate — a sample, and the true password among it — makes a
        // session; none has a confirmation on record to verify against.
        let alphabet = display_protocol::pairing::RENDEZVOUS_CODE_ALPHABET;
        let mut candidates: Vec<String> = (0u32..512)
            .map(|n| {
                (0..HINT_CHARS)
                    .map(|i| {
                        let index = usize::try_from((n >> (5 * i)) & 0x1f).expect("small");
                        char::from(alphabet[index])
                    })
                    .collect()
            })
            .collect();
        candidates.push(derived.password.clone());
        let recorded_confirmation = [0u8; 32];
        for candidate in &candidates {
            let (exchange, _) = pake::Exchange::start(
                pake::Role::B,
                sponsor_id.as_str().as_bytes(),
                pair_id.as_str().as_bytes(),
                candidate.as_bytes(),
            )
            .expect("start");
            let session = exchange.finish(&share).expect("finish");
            assert!(
                session.confirm(&recorded_confirmation).is_err(),
                "candidate {candidate} confirmed against a transcript that holds no confirmation"
            );
        }
        // The real joiner never saw the dial.
        assert_eq!(joiner.pair.attempts(), 0);
        assert!(joiner.pair.status(now_ms()).is_some());
        assert_eq!(joiner.devices(), vec![joiner.me()]);
    }

    /// Rejecting at confirm tells the joiner: its offer is dropped at once,
    /// so it wants a fresh code now rather than when the offer would have
    /// expired.
    #[tokio::test]
    async fn a_rejection_reaches_the_joiner_and_frees_its_code() {
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
        assert!(!joiner.pair.wants_code(now_ms()), "an offer is waiting");
        sponsor
            .pair
            .confirm(&offer.pairing, false, now_ms())
            .await
            .expect("reject");
        wait_until("the joiner drops the offer", || {
            joiner.pair.pending_terms().is_none()
        })
        .await;
        assert!(
            joiner.pair.wants_code(now_ms()),
            "a fresh code is wanted at once"
        );
        assert_eq!(joiner.devices(), vec![joiner.me()]);
        serving.abort();
    }
}
