#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "neighbor scores and bounded counters use policy-limited domains"
)]
//! The persistent Neighbor registry (v1) — C2.1.
//!
//! Keyed by `(SpaceId, Key)`: verified Beacon high-water
//! `(epoch, sequence)` persisted across restart, verified route hints with
//! receiver-local lease expiry, advisory reachability, and Orbit-local Contact
//! retry state. Reachability is advisory and never standing. The registry file
//! lives in the Orbit store directory (`neighbors`), atomically replaced on
//! every mutation; a corrupt or version-unknown file is a typed error and is
//! **never** deleted automatically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mechanics::{ids::SpaceId, station::Key};
use serde::{Deserialize, Serialize};

use crate::beacon::VerifiedBeacon;
use crate::lifecycle::{Neighbor, Reachability};

const NEIGHBORS_FILE: &str = "neighbors";
const REGISTRY_VERSION: u8 = 2;

/// The minimum Contact retry backoff (1 second).
pub const RETRY_MIN_MS: u64 = 1_000;
/// The maximum Contact retry backoff (5 minutes).
pub const RETRY_MAX_MS: u64 = 300_000;
/// The registry's hard entry cap. Free key rotation must not grow the durable
/// file without bound; past the cap the least valuable entry is evicted.
pub const MAX_NEIGHBOR_ENTRIES: usize = 256;
/// Minimum interval between persists for freshness-only updates (high-water /
/// lease renewals). Structural changes (new entry, pending flip, retry state)
/// persist immediately; losing a coalesced high-water on crash only means
/// re-accepting an idempotent beacon.
pub const PERSIST_MIN_INTERVAL_MS: u64 = 1_000;

/// Why the registry failed to load. Corrupt state is surfaced, never deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The registry file exists but does not decode canonically.
    CorruptRegistry,
    /// The registry names a version this build does not speak.
    UnsupportedRegistryVersion(u8),
    /// The registry belongs to a different Space.
    ForeignRegistry,
    /// The registry could not be encoded for persistence.
    Encoding,
    /// An I/O failure reading or writing the registry.
    Io(std::io::ErrorKind),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Failure {}

/// A verified route hint with its receiver-local lease expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRoute {
    pub hint: crate::beacon::RouteHint,
    /// Receiver-local wall-clock lease expiry (ms since the unix epoch). An
    /// expired route suppresses dialing until a fresh Beacon renews it.
    pub expires_at_ms: u64,
}

/// One Neighbor's persistent record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeighborRecord {
    pub station: Key,
    /// Verified Beacon high-water: forward-only per Station, fails closed on
    /// u64 overflow.
    pub epoch: u64,
    pub sequence: u64,
    /// The advertised Replica frontier from the freshest verified Beacon.
    pub frontier_root: [u8; 32],
    pub frontier_count: u64,
    pub routes: Vec<StoredRoute>,
    /// Advisory reachability (never standing).
    pub reachability: u8,
    /// Orbit-local Contact retry state.
    pub failures: u32,
    /// Next Contact attempt not before (ms since the unix epoch). Persisted so
    /// a restart restores retry state instead of causing a dial storm.
    pub next_attempt_ms: u64,
    /// Whether a verified Beacon advertised a frontier we have not yet
    /// converged with (queues Contact; duplicates coalesce here).
    pub pending: bool,
    /// When this Neighbor was last heard from (verified beacon or swarm
    /// membership event), receiver-local wall clock. Advisory: drives
    /// presence display and eviction order, never standing.
    pub last_seen_ms: u64,
}

/// The prior (version-1) record shape, kept only to migrate an existing
/// registry file in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorNeighborRecord {
    station: Key,
    epoch: u64,
    sequence: u64,
    frontier_root: [u8; 32],
    frontier_count: u64,
    routes: Vec<StoredRoute>,
    reachability: u8,
    failures: u32,
    next_attempt_ms: u64,
    pending: bool,
}

impl From<PriorNeighborRecord> for NeighborRecord {
    fn from(prior: PriorNeighborRecord) -> Self {
        NeighborRecord {
            station: prior.station,
            epoch: prior.epoch,
            sequence: prior.sequence,
            frontier_root: prior.frontier_root,
            frontier_count: prior.frontier_count,
            routes: prior.routes,
            reachability: prior.reachability,
            failures: prior.failures,
            next_attempt_ms: prior.next_attempt_ms,
            pending: prior.pending,
            last_seen_ms: 0,
        }
    }
}

const REACH_UNKNOWN: u8 = 0;
const REACH_REACHABLE: u8 = 1;
const REACH_UNREACHABLE: u8 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    version: u8,
    space: SpaceId,
    entries: Vec<NeighborRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PriorRegistryFile {
    version: u8,
    space: SpaceId,
    entries: Vec<PriorNeighborRecord>,
}

/// The persistent registry. Structural mutations persist atomically before
/// returning; freshness-only updates coalesce under
/// [`PERSIST_MIN_INTERVAL_MS`] (drain with [`NeighborRegistry::flush`]).
#[derive(Debug)]
pub struct NeighborRegistry {
    path: PathBuf,
    space: SpaceId,
    entries: BTreeMap<Key, NeighborRecord>,
    /// Unpersisted freshness-only changes.
    dirty: bool,
    last_persist_ms: u64,
}

impl NeighborRegistry {
    /// Load (or initialize empty) the registry for a Space from the Orbit
    /// store directory. Corrupt or foreign state is a typed error, untouched.
    pub fn load(store_dir: &Path, space: &SpaceId) -> Result<Self, Failure> {
        let path = store_dir.join(NEIGHBORS_FILE);
        let entries = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(Failure::Io(e.kind())),
            Ok(bytes) => Self::decode_entries(&bytes, space)?,
        };
        Ok(Self {
            path,
            space: space.clone(),
            entries,
            dirty: false,
            last_persist_ms: 0,
        })
    }

    /// Decode a registry file, migrating a v1 file to the v2 record shape in
    /// memory (the next persist rewrites it as v2 — never deleted, upgraded).
    fn decode_entries(
        bytes: &[u8],
        space: &SpaceId,
    ) -> Result<BTreeMap<Key, NeighborRecord>, Failure> {
        // Postcard tolerates trailing bytes, so each shape must round-trip
        // exactly before it is believed — a v1 file can never half-decode as
        // v2 (or vice versa) and produce garbled records.
        if let Ok(file) = postcard::from_bytes::<RegistryFile>(bytes) {
            if file.version == REGISTRY_VERSION
                && postcard::to_stdvec(&file)
                    .map(|b| b == bytes)
                    .unwrap_or(false)
            {
                if &file.space != space {
                    return Err(Failure::ForeignRegistry);
                }
                return Ok(file
                    .entries
                    .into_iter()
                    .map(|e| (e.station.clone(), e))
                    .collect());
            }
        }
        if let Ok(file) = postcard::from_bytes::<PriorRegistryFile>(bytes) {
            if postcard::to_stdvec(&file)
                .map(|b| b == bytes)
                .unwrap_or(false)
            {
                if file.version == 1 {
                    if &file.space != space {
                        return Err(Failure::ForeignRegistry);
                    }
                    return Ok(file
                        .entries
                        .into_iter()
                        .map(NeighborRecord::from)
                        .map(|e| (e.station.clone(), e))
                        .collect());
                }
                // A structurally sound file naming a version we do not speak.
                return Err(Failure::UnsupportedRegistryVersion(file.version));
            }
        }
        Err(Failure::CorruptRegistry)
    }

    fn persist_now(&mut self, now_ms: u64) -> Result<(), Failure> {
        let file = RegistryFile {
            version: REGISTRY_VERSION,
            space: self.space.clone(),
            entries: self.entries.values().cloned().collect(),
        };
        let bytes = postcard::to_stdvec(&file).map_err(|_| Failure::Encoding)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| Failure::Io(e.kind()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| Failure::Io(e.kind()))?;
        self.dirty = false;
        self.last_persist_ms = now_ms;
        Ok(())
    }

    /// Record a freshness-only change: persist if the coalescing window has
    /// passed, otherwise leave it dirty for [`NeighborRegistry::flush`].
    fn persist_soft(&mut self, now_ms: u64) -> Result<(), Failure> {
        self.dirty = true;
        if now_ms.saturating_sub(self.last_persist_ms) >= PERSIST_MIN_INTERVAL_MS {
            self.persist_now(now_ms)?;
        }
        Ok(())
    }

    /// Drain coalesced freshness updates to disk once the window has passed.
    /// Cheap when clean; the driver calls it on its tick.
    pub fn flush(&mut self, now_ms: u64) -> Result<(), Failure> {
        if self.dirty && now_ms.saturating_sub(self.last_persist_ms) >= PERSIST_MIN_INTERVAL_MS {
            self.persist_now(now_ms)?;
        }
        Ok(())
    }

    /// Drain any coalesced updates immediately (dormancy path).
    pub fn flush_now(&mut self, now_ms: u64) -> Result<(), Failure> {
        if self.dirty {
            self.persist_now(now_ms)?;
        }
        Ok(())
    }

    /// Evict the least valuable entries until a new one fits under
    /// [`MAX_NEIGHBOR_ENTRIES`]: never-pending before pending, unreachable
    /// before reachable, stalest first.
    fn evict_for_insert(&mut self) {
        while self.entries.len() >= MAX_NEIGHBOR_ENTRIES {
            let victim = self
                .entries
                .values()
                .min_by_key(|e| (e.pending, e.reachability == REACH_REACHABLE, e.last_seen_ms))
                .map(|e| e.station.clone());
            let Some(victim) = victim else { break };
            self.entries.remove(&victim);
        }
    }

    /// Offer a **verified** Beacon (only [`VerifiedBeacon`] is accepted — a
    /// forged structure cannot reach this state). Forward-only per Station:
    /// stale or replayed coordinates are ignored; a fresh one updates the
    /// high-water, renews route leases, and — when the advertised frontier is
    /// not the local one — marks the Neighbor pending Contact (duplicates
    /// coalesce; a malicious repeat of the same coordinate cannot re-queue).
    pub fn observe_beacon(
        &mut self,
        beacon: &VerifiedBeacon,
        local_frontier: (&[u8; 32], u64),
        now_ms: u64,
        route_lease_ms: u64,
    ) -> Result<bool, Failure> {
        if beacon.space() != &self.space {
            return Ok(false);
        }
        let station = beacon.station().clone();
        let (epoch, sequence) = beacon.coordinate();
        let is_new = !self.entries.contains_key(&station);
        if is_new {
            self.evict_for_insert();
        }
        let entry = self
            .entries
            .entry(station.clone())
            .or_insert_with(|| NeighborRecord {
                station,
                epoch: 0,
                sequence: 0,
                frontier_root: [0u8; 32],
                frontier_count: 0,
                routes: Vec::new(),
                reachability: REACH_UNKNOWN,
                failures: 0,
                next_attempt_ms: 0,
                pending: false,
                last_seen_ms: 0,
            });
        // Forward-only: an old or equal coordinate is a replay, ignored. A
        // brand-new entry (0,0) accepts any coordinate.
        let fresh = (epoch, sequence) > (entry.epoch, entry.sequence)
            || (entry.epoch == 0 && entry.sequence == 0 && entry.frontier_count == 0);
        if !fresh {
            return Ok(false);
        }
        entry.epoch = epoch;
        entry.sequence = sequence;
        entry.last_seen_ms = now_ms;
        let (root, count) = beacon.frontier();
        entry.frontier_root = root;
        entry.frontier_count = count;
        // Signed quiescence: a dormancy announcement is unambiguous planned
        // silence — mark unreachable, cancel queued work (the station greets
        // on return), and queue nothing new.
        if beacon.dormant() {
            let was_pending = entry.pending;
            entry.pending = false;
            entry.reachability = REACH_UNREACHABLE;
            if is_new || was_pending {
                self.persist_now(now_ms)?;
            } else {
                self.persist_soft(now_ms)?;
            }
            return Ok(false);
        }
        // A verified beacon always renews the bare-id dial lease (scheme 0):
        // hearing from a Station proves it is alive and overlay-reachable, and
        // an address-free beacon (relay/discovery transports) must not wipe
        // eligibility. Explicit hints ride alongside.
        let expires_at_ms = now_ms.saturating_add(route_lease_ms);
        let mut new_routes: Vec<StoredRoute> = vec![StoredRoute {
            hint: crate::beacon::RouteHint {
                scheme: 0,
                bytes: Vec::new(),
            },
            expires_at_ms,
        }];
        new_routes.extend(beacon.routes().iter().map(|r| StoredRoute {
            hint: r.clone(),
            expires_at_ms,
        }));
        let routes_changed = entry.routes.len() != new_routes.len()
            || entry
                .routes
                .iter()
                .zip(new_routes.iter())
                .any(|(a, b)| a.hint != b.hint);
        entry.routes = new_routes;
        // A fresh verified beacon is live evidence: advisory-reachable (never
        // standing). A failed dial or swarm NeighborDown flips it back.
        entry.reachability = REACH_REACHABLE;
        // Queue Contact only when the advertised frontier is news. Equality is
        // the full pair — root alone is not path-independence-safe.
        let newsworthy =
            (entry.frontier_root, entry.frontier_count) != (*local_frontier.0, local_frontier.1);
        let pending_flipped = newsworthy && !entry.pending;
        if newsworthy {
            entry.pending = true;
        }
        if is_new || pending_flipped || routes_changed {
            self.persist_now(now_ms)?;
        } else {
            self.persist_soft(now_ms)?;
        }
        Ok(newsworthy)
    }

    /// Feed a swarm membership event (advisory reachability, never standing,
    /// never routes — the eclipse fence still gates learning). Only known
    /// Neighbors are touched: a bare overlay event can never create an entry.
    pub fn note_swarm(&mut self, station: &Key, up: bool, now_ms: u64) -> Result<bool, Failure> {
        let Some(entry) = self.entries.get_mut(station) else {
            return Ok(false);
        };
        entry.reachability = if up {
            REACH_REACHABLE
        } else {
            REACH_UNREACHABLE
        };
        if up {
            entry.last_seen_ms = now_ms;
        }
        self.persist_soft(now_ms)?;
        Ok(true)
    }

    /// The eager-convergence belt (W0-S3): a fresh local durable commit marks
    /// every known Neighbor pending-Contact, so loopback-scale convergence never
    /// waits for the beacon floor. Eligibility still gates on backoff and an
    /// unexpired route lease.
    ///
    /// It is a **pull**, despite the "push" this was once called. Contact is
    /// initiator-driven and only the dialer incorporates, so a local commit
    /// makes this Station dial *out and read*; it does not deliver the commit
    /// anywhere. What tells a neighbour we have news is the Beacon, and — once
    /// it dials us — the holdings declaration it reads in [`note_reciprocable`].
    pub fn mark_all_pending(&mut self, now_ms: u64) -> Result<usize, Failure> {
        let mut flipped = 0;
        for entry in self.entries.values_mut() {
            if !entry.pending {
                entry.pending = true;
                flipped += 1;
            }
        }
        if flipped > 0 {
            self.persist_now(now_ms)?;
        }
        Ok(flipped)
    }

    /// The Stations worth bootstrapping the gossip swarm from: entries holding
    /// an unexpired route lease, with those routes (W0-S1(c)).
    pub fn bootstrap_candidates(&self, now_ms: u64) -> Vec<(Key, Vec<crate::beacon::RouteHint>)> {
        self.entries
            .values()
            .filter_map(|e| {
                let routes: Vec<crate::beacon::RouteHint> = e
                    .routes
                    .iter()
                    .filter(|r| r.expires_at_ms > now_ms)
                    .map(|r| r.hint.clone())
                    .collect();
                (!routes.is_empty()).then(|| (e.station.clone(), routes))
            })
            .collect()
    }

    /// Note a Station we just accepted an inbound Contact from, so the scheduler
    /// dials it **back** to complete the bidirectional exchange (the responder
    /// side only served material; a reciprocal pull is what redeems a joiner's
    /// admission and converges the responder). Marks the entry pending with a
    /// direct, leased route toward the peer — dialing resolves by Key, so
    /// the route only needs to be present and unexpired to pass eligibility.
    /// Never overwrites a fresher Beacon-derived frontier/coordinate.
    ///
    /// `peer_holds_news` is the responder's read of the initiator's signed
    /// holdings declaration: true when it named an interpreted head this
    /// Replica does not hold. See the gate below for why that predicate, and
    /// not root inequality, is the one that is safe to arm on.
    pub fn note_reciprocable(
        &mut self,
        station: &Key,
        peer_holds_news: bool,
        now_ms: u64,
        route_lease_ms: u64,
    ) -> Result<(), Failure> {
        // Two reasons to dial back at a peer that just dialed us.
        //
        // * **First contact** — a peer we have never pulled from (unknown
        //   reachability). This is what redeems a joiner's admission.
        // * **`peer_holds_news`** — its declaration named an interpreted head
        //   we lack. A peer dials because it just committed, so that
        //   declaration is the most reliable evidence of divergence this
        //   Station will ever get: signed, digest-bound, and delivered on a
        //   stream that already exists. Without this arm it is discarded and
        //   the lossy Beacon — whose floor decays to five minutes — becomes the
        //   only way a passive node ever hears about a neighbour's work.
        //
        // First contact alone used to be the whole gate, to stop two Stations
        // ping-ponging once converged. It bought that at the price of going
        // permanently deaf: `reachability` leaves `REACH_UNKNOWN` on the first
        // Beacon (`observe_beacon`) or swarm `NeighborUp` (`note_swarm`), both
        // of which normally land before any Contact, so in a live daemon the
        // gate shut before it ever opened.
        //
        // `peer_holds_news` restores the arm without restoring the ping-pong,
        // because it is **positive evidence the peer is ahead**, not a
        // symmetric "we differ". Once we pull, its heads are a subset of ours
        // and it stops arming — the predicate has a fixpoint and converged
        // peers fall quiet on their own. Root inequality has no fixpoint and
        // must not be used here: opaque Bodies append heads where interpreted
        // Bodies replace them, so a peer holding material it cannot yet decrypt
        // would differ forever and dial forever.
        match self.entries.get(station) {
            Some(existing) if existing.reachability == REACH_UNKNOWN => {}
            Some(_) if peer_holds_news => {}
            Some(_) => return Ok(()),
            None => self.evict_for_insert(),
        }
        let entry = self
            .entries
            .entry(station.clone())
            .or_insert_with(|| NeighborRecord {
                station: station.clone(),
                epoch: 0,
                sequence: 0,
                frontier_root: [0u8; 32],
                frontier_count: 0,
                routes: Vec::new(),
                reachability: REACH_UNKNOWN,
                failures: 0,
                next_attempt_ms: 0,
                pending: false,
                last_seen_ms: 0,
            });
        let pending_flipped = !entry.pending;
        entry.pending = true;
        entry.last_seen_ms = now_ms;
        // A direct route toward the inbound peer (scheme 0, no address bytes):
        // the transport dials by Key, so this only keeps eligibility open.
        let expires_at_ms = now_ms.saturating_add(route_lease_ms);
        let direct = crate::beacon::RouteHint {
            scheme: 0,
            bytes: Vec::new(),
        };
        if let Some(r) = entry.routes.iter_mut().find(|r| r.hint == direct) {
            r.expires_at_ms = r.expires_at_ms.max(expires_at_ms);
        } else {
            entry.routes.push(StoredRoute {
                hint: direct,
                expires_at_ms,
            });
        }
        // This now runs on every inbound Contact from a peer that is ahead,
        // not once per peer per lifetime, so a re-arm that changes nothing
        // durable must not cost a write+rename of the whole registry.
        if pending_flipped {
            self.persist_now(now_ms)
        } else {
            self.persist_soft(now_ms)
        }
    }

    /// The Neighbors eligible for a Contact attempt now: pending, past their
    /// backoff, and holding an unexpired route lease (route expiry suppresses
    /// dialing). Fair order: sorted by `next_attempt_ms` then Key.
    pub fn eligible(&self, now_ms: u64) -> Vec<Key> {
        let mut due: Vec<(&NeighborRecord, &Key)> = self
            .entries
            .iter()
            .filter(|(_, e)| {
                e.pending
                    && e.next_attempt_ms <= now_ms
                    && e.routes.iter().any(|r| r.expires_at_ms > now_ms)
            })
            .map(|(k, e)| (e, k))
            .collect();
        due.sort_by(|a, b| {
            a.0.next_attempt_ms
                .cmp(&b.0.next_attempt_ms)
                .then_with(|| a.1.cmp(b.1))
        });
        due.into_iter().map(|(_, k)| k.clone()).collect()
    }

    /// Neighbors dialable now: a live route and no backoff.
    ///
    /// Unlike [`NeighborRegistry::eligible`] this does not require `pending`.
    /// That flag means "advertised a frontier we have not taken" and
    /// [`NeighborRegistry::record_success`] clears it — which is exactly when a
    /// peer's content becomes nameable here, so a fetch keyed on `eligible`
    /// could only ever ask a peer it still owed a convergence.
    pub fn reachable(&self, now_ms: u64) -> Vec<Key> {
        let mut due: Vec<&Key> = self
            .entries
            .iter()
            .filter(|(_, e)| {
                e.next_attempt_ms <= now_ms && e.routes.iter().any(|r| r.expires_at_ms > now_ms)
            })
            .map(|(k, _)| k)
            .collect();
        due.sort();
        due.into_iter().cloned().collect()
    }

    /// Record a successful Contact: backoff resets, the pending mark clears,
    /// reachability turns advisory-reachable.
    pub fn record_success(&mut self, station: &Key, now_ms: u64) -> Result<(), Failure> {
        if let Some(e) = self.entries.get_mut(station) {
            e.failures = 0;
            e.pending = false;
            e.reachability = REACH_REACHABLE;
            e.next_attempt_ms = now_ms;
            e.last_seen_ms = now_ms;
            self.persist_now(now_ms)?;
        }
        Ok(())
    }

    /// Record a failed Contact attempt: exponential backoff from 1 s to 5 min
    /// with deterministic per-Station jitter; the Neighbor stays pending.
    pub fn record_failure(&mut self, station: &Key, now_ms: u64) -> Result<(), Failure> {
        if let Some(e) = self.entries.get_mut(station) {
            e.failures = e.failures.saturating_add(1);
            e.reachability = REACH_UNREACHABLE;
            let base = RETRY_MIN_MS.saturating_mul(1u64 << e.failures.min(16));
            let capped = base.min(RETRY_MAX_MS);
            // Deterministic jitter (±12.5%) from the station key + failures so
            // tests are reproducible and herds still spread.
            let seed = blake3::hash(
                &[
                    e.station.key_bytes().as_slice(),
                    &e.failures.to_le_bytes()[..],
                ]
                .concat(),
            );
            let prefix = seed
                .as_bytes()
                .get(..8)
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                .unwrap_or([0u8; 8]);
            let jitter = u64::from_le_bytes(prefix) % (capped / 8).max(1);
            e.next_attempt_ms = now_ms.saturating_add(capped.saturating_sub(jitter));
            self.persist_now(now_ms)?;
        }
        Ok(())
    }

    /// Whether a Neighbor is currently marked pending.
    pub fn is_pending(&self, station: &Key) -> bool {
        self.entries.get(station).is_some_and(|e| e.pending)
    }

    /// The persisted high-water for a Station (`None` if unknown).
    pub fn high_water(&self, station: &Key) -> Option<(u64, u64)> {
        self.entries.get(station).map(|e| (e.epoch, e.sequence))
    }

    /// A consistent advisory snapshot for [`crate::lifecycle::Station::neighbors`].
    pub fn snapshot(&self) -> Vec<Neighbor> {
        self.entries
            .values()
            .map(|e| Neighbor {
                station: e.station.clone(),
                reachability: match e.reachability {
                    REACH_REACHABLE => Reachability::Reachable,
                    REACH_UNREACHABLE => Reachability::Unreachable,
                    _ => Reachability::Unknown,
                },
                last_seen_ms: e.last_seen_ms,
                pending: e.pending,
                next_attempt_ms: e.next_attempt_ms,
                route_lease_expires_ms: e.routes.iter().map(|r| r.expires_at_ms).max().unwrap_or(0),
                failures: e.failures,
            })
            .collect()
    }

    /// The freshest route hints for a Station (unexpired only).
    pub fn routes(&self, station: &Key, now_ms: u64) -> Vec<crate::beacon::RouteHint> {
        self.entries
            .get(station)
            .map(|e| {
                e.routes
                    .iter()
                    .filter(|r| r.expires_at_ms > now_ms)
                    .map(|r| r.hint.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beacon::SignedBeacon;
    use mechanics::station::Epoch;

    const SEED: [u8; 32] = [77u8; 32];

    fn space() -> SpaceId {
        SpaceId::from_digest([51u8; 16])
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lait-registry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn beacon(epoch: u64, sequence: u64, root: [u8; 32]) -> VerifiedBeacon {
        beacon_counted(epoch, sequence, root, 1, 0)
    }

    fn beacon_counted(
        epoch: u64,
        sequence: u64,
        root: [u8; 32],
        count: u64,
        flags: u8,
    ) -> VerifiedBeacon {
        SignedBeacon::emit(
            crate::beacon::BEACON_PROTOCOL,
            &space(),
            Epoch::from_u64(epoch),
            sequence,
            root,
            count,
            flags,
            vec![crate::beacon::RouteHint {
                scheme: 1,
                bytes: b"127.0.0.1:9000".to_vec(),
            }],
            &SEED,
        )
        .unwrap()
        .verify()
        .unwrap()
    }

    #[test]
    fn high_water_is_forward_only_and_persists_across_restart() {
        let dir = temp_dir("hw");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let local = [0u8; 32];
        assert!(reg
            .observe_beacon(&beacon(2, 5, [9u8; 32]), (&local, 0), 1_000, 60_000)
            .unwrap());
        // A replay or stale coordinate is ignored.
        assert!(!reg
            .observe_beacon(&beacon(2, 5, [9u8; 32]), (&local, 0), 1_100, 60_000)
            .unwrap());
        assert!(!reg
            .observe_beacon(&beacon(1, 9, [9u8; 32]), (&local, 0), 1_200, 60_000)
            .unwrap());
        // Restart: the high-water survives, so the stale coordinate is STILL
        // ignored.
        drop(reg);
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let station = mechanics::actor::device_from_seed(&SEED);
        let station = Key::from_device(&station).unwrap();
        assert_eq!(reg.high_water(&station), Some((2, 5)));
        assert!(!reg
            .observe_beacon(&beacon(2, 4, [9u8; 32]), (&local, 0), 2_000, 60_000)
            .unwrap());
        assert!(reg
            .observe_beacon(&beacon(2, 6, [10u8; 32]), (&local, 0), 2_100, 60_000)
            .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_known_frontier_does_not_queue_and_duplicates_coalesce() {
        let dir = temp_dir("coalesce");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        // The advertised frontier equals ours: no Contact queued.
        let ours = [9u8; 32];
        reg.observe_beacon(&beacon(1, 1, ours), (&ours, 1), 1_000, 60_000)
            .unwrap();
        assert!(reg.eligible(1_000).is_empty());
        // A new frontier queues once; repeated beacons coalesce into the same
        // pending mark rather than stacking work.
        reg.observe_beacon(&beacon(1, 2, [8u8; 32]), (&ours, 1), 1_100, 60_000)
            .unwrap();
        reg.observe_beacon(&beacon(1, 3, [8u8; 32]), (&ours, 1), 1_200, 60_000)
            .unwrap();
        assert_eq!(reg.eligible(1_300).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backoff_grows_persists_and_route_expiry_suppresses_dialing() {
        let dir = temp_dir("backoff");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let local = [0u8; 32];
        reg.observe_beacon(&beacon(1, 1, [7u8; 32]), (&local, 0), 1_000, 10_000)
            .unwrap();
        let station = reg.eligible(1_001)[0].clone();
        // Failures push the next attempt out (bounded by 5 minutes).
        reg.record_failure(&station, 1_001).unwrap();
        let first = reg.entries[&station].next_attempt_ms;
        assert!(first > 1_001 && first <= 1_001 + RETRY_MAX_MS);
        assert!(reg.eligible(1_002).is_empty(), "backoff suppresses");
        reg.record_failure(&station, first).unwrap();
        let second = reg.entries[&station].next_attempt_ms;
        assert!(second > first);
        // Restart restores the retry state — no dial storm.
        drop(reg);
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        assert!(reg.eligible(second - 1).is_empty());
        // Route lease expiry suppresses dialing even when the backoff is due.
        assert!(
            reg.eligible(1_000 + 10_000 + RETRY_MAX_MS).is_empty(),
            "expired route lease suppresses dialing"
        );
        // A fresh beacon renews the lease and the Neighbor dials again.
        reg.observe_beacon(&beacon(1, 2, [7u8; 32]), (&local, 0), second + 1, 60_000)
            .unwrap();
        assert_eq!(reg.eligible(second + 2).len(), 1);
        // Success resets backoff and clears pending.
        reg.record_success(&station, second + 3).unwrap();
        assert!(reg.eligible(second + 4).is_empty());
        assert_eq!(reg.entries[&station].failures, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frontier_equality_is_the_full_pair_not_root_alone() {
        // Path-independence guard (BEACON-6): the same root at a different
        // transaction count IS news — root-only equality would silently skip
        // the Contact that reconciles the divergent history.
        let dir = temp_dir("pair");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let ours = [9u8; 32];
        assert!(reg
            .observe_beacon(&beacon_counted(1, 1, ours, 4, 0), (&ours, 3), 1_000, 60_000)
            .unwrap());
        assert_eq!(reg.eligible(1_001).len(), 1);
        // The exact pair is quiet.
        let mut reg2 = NeighborRegistry::load(&temp_dir("pair2"), &space()).unwrap();
        assert!(!reg2
            .observe_beacon(&beacon_counted(1, 1, ours, 3, 0), (&ours, 3), 1_000, 60_000)
            .unwrap());
        assert!(reg2.eligible(1_001).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dormancy_beacon_clears_pending_and_marks_unreachable() {
        let dir = temp_dir("dormant");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let local = [0u8; 32];
        reg.observe_beacon(&beacon(1, 1, [7u8; 32]), (&local, 0), 1_000, 60_000)
            .unwrap();
        let station = reg.eligible(1_001)[0].clone();
        assert!(reg.is_pending(&station));
        // Signed quiescence: planned silence cancels queued work.
        let dormant = beacon_counted(1, 2, [7u8; 32], 1, crate::beacon::BEACON_FLAG_DORMANT);
        assert!(!reg
            .observe_beacon(&dormant, (&local, 0), 2_000, 60_000)
            .unwrap());
        assert!(!reg.is_pending(&station));
        assert_eq!(
            reg.snapshot()[0].reachability,
            crate::lifecycle::Reachability::Unreachable
        );
        // A fresh live beacon revives it.
        assert!(reg
            .observe_beacon(&beacon(1, 3, [8u8; 32]), (&local, 0), 3_000, 60_000)
            .unwrap());
        assert!(reg.is_pending(&station));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swarm_events_touch_only_known_neighbors() {
        let dir = temp_dir("swarm");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let stranger = Key::from_key_bytes([3u8; 32]);
        // A bare overlay event can never create an entry (eclipse fence).
        assert!(!reg.note_swarm(&stranger, true, 1_000).unwrap());
        assert!(reg.snapshot().is_empty());
        // A known Neighbor's reachability updates advisorily.
        let local = [0u8; 32];
        reg.observe_beacon(&beacon(1, 1, [7u8; 32]), (&local, 0), 1_000, 60_000)
            .unwrap();
        let station = reg.snapshot()[0].station.clone();
        assert!(reg.note_swarm(&station, false, 2_000).unwrap());
        assert_eq!(
            reg.snapshot()[0].reachability,
            crate::lifecycle::Reachability::Unreachable
        );
        assert!(reg.note_swarm(&station, true, 3_000).unwrap());
        assert_eq!(
            reg.snapshot()[0].reachability,
            crate::lifecycle::Reachability::Reachable
        );
        assert_eq!(reg.snapshot()[0].last_seen_ms, 3_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_all_pending_queues_every_neighbor_once() {
        let dir = temp_dir("belt");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let local = [0u8; 32];
        // Neighbor already converged with us (same pair): not pending.
        let ours = [9u8; 32];
        reg.observe_beacon(&beacon_counted(1, 1, ours, 1, 0), (&ours, 1), 1_000, 60_000)
            .unwrap();
        assert!(reg.eligible(1_001).is_empty());
        // A local commit marks it pending (the eager-push belt).
        assert_eq!(reg.mark_all_pending(1_100).unwrap(), 1);
        assert_eq!(reg.eligible(1_101).len(), 1);
        // Idempotent while already queued.
        assert_eq!(reg.mark_all_pending(1_200).unwrap(), 0);
        let _ = local;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The station whose Beacon `beacon()` emits, so a registry entry created
    /// by `observe_beacon` and one addressed by `note_reciprocable` are the
    /// same neighbour.
    fn beacon_station() -> Key {
        beacon(1, 1, [0u8; 32]).station().clone()
    }

    #[test]
    fn an_established_peer_that_declares_news_re_arms_a_dial_back() {
        let dir = temp_dir("reciprocable-news");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let local = [0u8; 32];
        let station = beacon_station();

        // Meet the peer over gossip, converge, and settle: reachability leaves
        // REACH_UNKNOWN and nothing is queued. This is the steady state a live
        // daemon spends all its time in, and the state the old first-contact
        // gate made permanently deaf.
        reg.observe_beacon(&beacon(1, 1, local), (&local, 0), 1_000, 600_000)
            .unwrap();
        reg.record_success(&station, 1_000).unwrap();
        assert!(!reg.snapshot()[0].pending, "converged peers queue nothing");

        // It dials us carrying no head we lack: still nothing to do.
        reg.note_reciprocable(&station, false, 2_000, 600_000)
            .unwrap();
        assert!(
            !reg.snapshot()[0].pending,
            "a peer with nothing we lack must not arm a dial — this is the \
             ping-pong the first-contact gate existed to prevent"
        );

        // It dials us declaring an interpreted head we do not hold. That is
        // positive evidence it is ahead, and the only reliable news a responder
        // ever gets, so it must arm.
        reg.note_reciprocable(&station, true, 3_000, 600_000)
            .unwrap();
        let entry = &reg.snapshot()[0];
        assert!(entry.pending, "a peer that is ahead of us must arm a pull");
        assert_eq!(
            reg.eligible(3_000),
            vec![station.clone()],
            "arming must also leave the entry dialable, not merely pending"
        );

        // And the arm has a fixpoint: once we pull, the peer stops being ahead
        // and the next inbound Contact goes quiet again.
        reg.record_success(&station, 4_000).unwrap();
        reg.note_reciprocable(&station, false, 5_000, 600_000)
            .unwrap();
        assert!(
            !reg.snapshot()[0].pending,
            "convergence must silence the arm, or it is a dial loop"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_contact_still_arms_without_news() {
        let dir = temp_dir("reciprocable-first");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let station = Key::from_key_bytes([9u8; 32]);
        // A joiner pre-admission publishes nothing, so it declares nothing —
        // the reciprocal pull is what redeems its admission and it must still
        // be armed on a bare first contact.
        reg.note_reciprocable(&station, false, 1_000, 600_000)
            .unwrap();
        assert!(reg.snapshot()[0].pending);
        assert_eq!(reg.eligible(1_000), vec![station]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_registry_is_capped_and_evicts_the_least_valuable() {
        let dir = temp_dir("cap");
        let mut reg = NeighborRegistry::load(&dir, &space()).unwrap();
        let local = [0u8; 32];
        // Fill to the cap with distinct stations via reciprocal notes.
        for i in 0..MAX_NEIGHBOR_ENTRIES {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&(i as u64).to_le_bytes());
            key[31] = 1;
            let station = Key::from_key_bytes(key);
            reg.note_reciprocable(&station, false, 1_000 + i as u64, 600_000)
                .unwrap();
        }
        assert_eq!(reg.snapshot().len(), MAX_NEIGHBOR_ENTRIES);
        // One more (a fresh verified beacon) evicts rather than growing.
        reg.observe_beacon(&beacon(1, 1, [7u8; 32]), (&local, 0), 5_000, 60_000)
            .unwrap();
        assert_eq!(reg.snapshot().len(), MAX_NEIGHBOR_ENTRIES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_v1_registry_file_migrates_in_place() {
        let dir = temp_dir("migrate");
        let station = Key::from_key_bytes([5u8; 32]);
        let v1 = PriorRegistryFile {
            version: 1,
            space: space(),
            entries: vec![PriorNeighborRecord {
                station: station.clone(),
                epoch: 3,
                sequence: 9,
                frontier_root: [1u8; 32],
                frontier_count: 7,
                routes: vec![],
                reachability: 1,
                failures: 2,
                next_attempt_ms: 42,
                pending: true,
            }],
        };
        std::fs::write(dir.join(NEIGHBORS_FILE), postcard::to_stdvec(&v1).unwrap()).unwrap();
        let reg = NeighborRegistry::load(&dir, &space()).unwrap();
        assert_eq!(reg.high_water(&station), Some((3, 9)));
        assert!(reg.is_pending(&station));
        assert_eq!(reg.snapshot()[0].last_seen_ms, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_and_foreign_registries_are_typed_errors_never_deleted() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join(NEIGHBORS_FILE), b"garbage").unwrap();
        assert_eq!(
            NeighborRegistry::load(&dir, &space()).unwrap_err(),
            Failure::CorruptRegistry
        );
        assert!(dir.join(NEIGHBORS_FILE).exists(), "never deleted");

        // An unsupported version is ITS error, not corruption.
        let file = RegistryFile {
            version: 9,
            space: space(),
            entries: vec![],
        };
        std::fs::write(
            dir.join(NEIGHBORS_FILE),
            postcard::to_stdvec(&file).unwrap(),
        )
        .unwrap();
        assert_eq!(
            NeighborRegistry::load(&dir, &space()).unwrap_err(),
            Failure::UnsupportedRegistryVersion(9)
        );

        // A registry for another Space is refused.
        let file = RegistryFile {
            version: REGISTRY_VERSION,
            space: SpaceId::from_digest([99u8; 16]),
            entries: vec![],
        };
        std::fs::write(
            dir.join(NEIGHBORS_FILE),
            postcard::to_stdvec(&file).unwrap(),
        )
        .unwrap();
        assert_eq!(
            NeighborRegistry::load(&dir, &space()).unwrap_err(),
            Failure::ForeignRegistry
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
