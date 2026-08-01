#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "fetch rounds and chunk geometry are bounded by descriptor and transfer policy"
)]
//! Freight's requesting half: choosing providers, and moving only what is
//! missing.
//!
//! **Only missing chunks move.** A fetch starts from what this Station already
//! holds, asks candidates only about the gap, and installs each chunk the moment
//! it verifies. Re-opening content that is already here costs nothing on the
//! wire, which is the property the whole plane exists for.
//!
//! **Nothing is trusted until it verifies.** Bytes arriving from a peer are
//! staged — never resident, never servable, never readable — until they hash to
//! the leaf a proof binds to this content's committed Merkle root. A provider
//! that lies costs one chunk and its own place in the candidate set.
//!
//! **A chunk comes from one provider.** Different chunks may come from
//! different peers, which is what makes a partial holder useful; but assembling
//! one chunk from two peers' ranges would mean a chunk whose bytes no single
//! proof covers, and there would be nobody to blame when it failed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mechanics::{ids::SpaceId, station::Key};
use replica::content::{ChunkProof, ContentDescriptor, ContentRef};

use crate::budget::{deadline, slots};
use crate::content_host::{ContentAction, ContentHost, ContentKeys, ContentPolicy};
use crate::plane::freight::{frame, read_frame};
use crate::plane::{bounds, Accept, FreightFrame, Open, Plane, SPACE_ID_LEN};
use crate::transfer::{TransferHandle, TransferRegistry, TransferState};

/// Why a fetch did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// No committed descriptor. A fetch cannot start from a bare id: without
    /// the geometry there is nothing to verify an answer against.
    UnknownContent,
    /// This Station will not hold that much.
    OverQuota,
    /// Nobody offered the chunks that are missing.
    NoProvider,
    /// Every candidate that offered them failed to deliver.
    Incomplete { missing: usize },
    /// Local storage refused.
    Storage,
    /// This operation is already in flight, or this Station is already moving
    /// as much as it will at once.
    Busy,
}

/// One provider's local reputation. Local policy, never replicated truth.
#[derive(Debug, Clone, Default)]
struct ProviderScore {
    /// Bytes per second, smoothed. Used to break ties, not to rank absolutely —
    /// ranking purely on a throughput estimate makes every fetcher pick the
    /// same peer at the same moment and then punish it for being slow.
    throughput: f64,
    outstanding: u32,
    /// Set when a provider refused or timed out. Decays, because a blip on the
    /// *fetcher's* own uplink should not blacklist everybody.
    probation_until: Option<Instant>,
}

/// How long a refusal or timeout keeps a provider out of the running.
const PROBATION: Duration = Duration::from_secs(30);

/// A connected, admitted provider.
pub struct Provider {
    station: Key,
    connection: Box<dyn comms::Connection>,
}

impl Provider {
    pub fn station(&self) -> &Key {
        &self.station
    }

    /// Ask which of these chunks the provider can serve.
    ///
    /// Bounded by what is asked, and the answer is bounded by what was asked.
    /// An empty answer means "none of these", which is deliberately the same
    /// thing a provider says about content it has never heard of.
    pub async fn have(&self, content: &ContentRef, wanted: &[u32]) -> Result<Vec<u32>, Failure> {
        let request = FreightFrame::Have {
            content_id: content.content_id,
            wanted: wanted.to_vec(),
        };
        let answer = self.ask(&request, deadline::HAVE_RESPONSE).await?;
        match answer {
            (FreightFrame::Available { chunks, .. }, _) => Ok(chunks),
            _ => Err(Failure::NoProvider),
        }
    }

    /// Fetch one chunk's bytes, from `offset`, proving the leaf first.
    ///
    /// `resume_leaf` is what makes a resumed transfer safe: it names the leaf
    /// the partial bytes were already validated against, and a provider whose
    /// leaf differs is refused before anything is appended.
    pub async fn chunk(
        &self,
        content: &ContentRef,
        chunk_index: u32,
        offset: u64,
        resume_leaf: Option<[u8; 32]>,
    ) -> Result<(ChunkProof, u32, Vec<u8>), Failure> {
        let request = FreightFrame::GetChunk {
            content_id: content.content_id,
            chunk_index,
            offset,
            max_len: bounds::MAX_CHUNK_FRAME_BYTES as u32,
            resume_leaf,
        };
        let (header, body) = self.ask(&request, deadline::CHUNK_HEADER).await?;
        match header {
            FreightFrame::ChunkHeader {
                chunk_index: answered,
                proof,
                total_len,
                ..
            } if answered == chunk_index => {
                let proof =
                    ChunkProof::decode_canonical(&proof).map_err(|_| Failure::NoProvider)?;
                Ok((proof, total_len, body))
            }
            _ => Err(Failure::NoProvider),
        }
    }

    /// One request, one flow, one answer.
    ///
    /// The body is delimited by the provider finishing the flow, not by any
    /// length the header declares — so a ranged answer needs no second
    /// agreement about how much is coming, and a truncated one is an error
    /// rather than a short read that looks complete.
    async fn ask(
        &self,
        request: &FreightFrame,
        budget: Duration,
    ) -> Result<(FreightFrame, Vec<u8>), Failure> {
        let exchange = async {
            let (mut send, mut recv) = self.connection.open_bi().await.ok()?;
            send.write_all(&frame(request)).await.ok()?;
            send.finish().ok()?;
            let header = read_frame(recv.as_mut(), bounds::MAX_CONTROL_FRAME_BYTES)
                .await
                .ok()?;
            if header == FreightFrame::Refused {
                return Some((header, Vec::new()));
            }
            let body = recv.read_to_end(bounds::MAX_CHUNK_FRAME_BYTES).await.ok()?;
            Some((header, body))
        };
        match tokio::time::timeout(budget, exchange).await {
            Ok(Some((FreightFrame::Refused, _))) => Err(Failure::NoProvider),
            Ok(Some(answer)) => Ok(answer),
            _ => Err(Failure::NoProvider),
        }
    }
}

/// Dial a peer and complete the opening exchange.
///
/// The opening goes on the initiator's first flow and the answer comes back on
/// the responder's, which is the same shape the accepting driver implements —
/// stated in one place on each side rather than negotiated.
///
/// The error says *why*, rather than collapsing every outcome into absence.
/// One of the reasons is actionable and the rest are not, and a caller that
/// could not tell them apart is a caller that reports a generation mismatch as
/// a flaky network.
pub async fn connect_provider(
    transport: &dyn comms::Transport,
    space: &SpaceId,
    local: &Key,
    peer: &Key,
    connection_id: [u8; 16],
) -> Result<Provider, ProviderRefusal> {
    let unreachable = || ProviderRefusal::Unreachable;
    let connection = transport
        .connect_session(peer.as_device(), crate::plane::FREIGHT_ALPN)
        .await
        .map_err(|_| unreachable())?;

    let mut space_bytes = [0u8; SPACE_ID_LEN];
    let raw = space.as_str().as_bytes();
    if raw.len() != SPACE_ID_LEN {
        return Err(unreachable());
    }
    space_bytes.copy_from_slice(raw);
    let mut epoch = [0u8; 16];
    getrandom::fill(&mut epoch).map_err(|_| unreachable())?;

    let open = Open {
        plane: Plane::Freight,
        protocol_version: Plane::Freight.protocol_version(),
        // None. Residency hints are a Live-plane capability answered by a
        // `ResidencyOracle` on a Live session; offering them here asked a
        // Freight provider about something Freight has no way to answer.
        features: 0,
        space: space_bytes,
        initiator_station: local.key_bytes(),
        responder_station: peer.key_bytes(),
        connection_id,
        connection_epoch: epoch,
        authority_frontier: Vec::new(),
        // Freight carries no lanes: the ALPN types the connection, and a lane
        // byte belongs to the live plane.
        requested_lanes: Vec::new(),
    };

    let mut flow = connection.open_uni().await.map_err(|_| unreachable())?;
    flow.write_all(&open.encode())
        .await
        .map_err(|_| unreachable())?;
    flow.finish().map_err(|_| unreachable())?;

    // The answer, bounded and deadlined.
    let answer = tokio::time::timeout(deadline::CHUNK_HEADER, async {
        let mut recv = connection.accept_uni().await.ok()??;
        recv.read_to_end(bounds::MAX_OPENING_BYTES).await.ok()
    })
    .await
    .map_err(|_| unreachable())?
    .ok_or_else(unreachable)?;
    if Accept::decode_canonical(&answer).is_ok() {
        return Ok(Provider {
            station: peer.clone(),
            connection,
        });
    }

    // Not an accept. Most refusals are deliberately coarse and mean the same
    // thing to a fetcher — this provider is not available — but one of them is
    // actionable, and collapsing it into the rest is how a version mismatch
    // presents as an intermittent network problem for a week.
    //
    // Reported and not returned: a fetcher's caller has no version to change.
    // What it needs is for the operator to be able to find out why every peer
    // is suddenly unavailable, which a log line answers and a `None` does not.
    Err(match crate::plane::Refusal::decode_canonical(&answer) {
        Ok(refusal) => ProviderRefusal::Refused(refusal),
        // Neither an accept nor a refusal: a truncated stream, or something that
        // is not this protocol at all. Distinct from a refusal because it is our
        // problem to explain rather than theirs to have sent.
        Err(_) => ProviderRefusal::Unintelligible,
    })
}

/// Why a provider did not become one.
///
/// A type rather than a log line, because the distinction is testable and a log
/// line is not. Most refusals are deliberately coarse and mean the same thing to
/// a fetcher — this provider is not available — but `UnsupportedVersion` is
/// actionable, and collapsing it into the rest is how a generation mismatch
/// presents as an intermittent network fault for a week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRefusal {
    /// The dial, the opening, or the answer did not complete in time.
    Unreachable,
    /// The peer said no, in its own words.
    Refused(crate::plane::Refusal),
    /// The peer answered with something that is neither an accept nor a
    /// refusal.
    Unintelligible,
}

impl ProviderRefusal {
    /// Whether an operator could do something about this.
    ///
    /// The only one that is worth telling somebody about: every other refusal
    /// is a peer exercising a policy it is entitled to, and a fetcher that
    /// reported those would be reporting normal operation.
    pub fn is_actionable(&self) -> bool {
        matches!(
            self,
            Self::Refused(crate::plane::Refusal::UnsupportedVersion { .. })
        )
    }
}

/// What a fetch needs that is not about one particular content.
pub struct Fetcher {
    pub host: Arc<ContentHost>,
    pub registry: Arc<TransferRegistry>,
    pub space: SpaceId,
    pub keys: Arc<dyn ContentKeys>,
    /// Resident bytes this Station will hold. A fetch that would cross it is
    /// refused before anything is staged, because failing after moving a
    /// gigabyte is a worse answer than refusing before moving any of it.
    pub cache_quota_bytes: u64,
    pub max_content_len: u64,
}

impl Fetcher {
    fn policy<'a>(
        &'a self,
        authorize: &'a dyn Fn(ContentAction) -> Result<(), Vec<u8>>,
    ) -> ContentPolicy<'a> {
        ContentPolicy {
            space: &self.space,
            keys: self.keys.clone(),
            authorize,
            max_content_len: self.max_content_len,
        }
    }

    /// Fetch everything missing, from whoever will serve it.
    pub async fn fetch(
        &self,
        content: &ContentRef,
        operation: [u8; 16],
        providers: &[Provider],
    ) -> Result<(), Failure> {
        let allow = |_: ContentAction| Ok(());
        let policy = self.policy(&allow);
        let descriptor = self
            .host
            .descriptor_of(&policy, content)
            .map_err(|_| Failure::UnknownContent)?;

        let missing = self.missing_chunks(&policy, content, &descriptor);
        if missing.is_empty() {
            // Already here. The second open of a content costs no wire at all,
            // which is the point.
            return Ok(());
        }
        self.admit_by_quota(&descriptor)?;
        if providers.is_empty() {
            return Err(Failure::NoProvider);
        }

        let handle = TransferHandle::new(
            self.registry.clone(),
            self.host.cache_handle(),
            operation,
            *content,
            Instant::now(),
        )
        .map_err(|_| Failure::Busy)?;
        handle.advance(TransferState::Connecting, Instant::now());

        // Who can serve what. Asked once per provider about the whole gap,
        // rather than per chunk: a provider's answer is cheap and asking again
        // for every chunk would turn one question into thousands.
        let mut offers: BTreeMap<Key, BTreeSet<u32>> = BTreeMap::new();
        let mut scores: BTreeMap<Key, ProviderScore> = BTreeMap::new();
        for provider in providers {
            match provider.have(content, &missing).await {
                Ok(chunks) if !chunks.is_empty() => {
                    offers.insert(provider.station.clone(), chunks.into_iter().collect());
                    scores.insert(provider.station.clone(), ProviderScore::default());
                }
                Ok(_) => {}
                Err(_) => {
                    scores
                        .entry(provider.station.clone())
                        .or_default()
                        .probation_until = Some(Instant::now() + PROBATION);
                }
            }
        }
        if offers.is_empty() {
            handle.finish(TransferState::Failed, Instant::now());
            return Err(Failure::NoProvider);
        }

        let mut outstanding = missing.clone();
        let mut moved: u64 = 0;
        let total = descriptor.plaintext_len;

        // Scarcest first. A chunk only one peer holds is the one that decides
        // whether this fetch can finish at all, so it is fetched while that
        // peer is still here — the same reason a swarm asks for the rarest
        // piece before a common one.
        outstanding
            .sort_by_key(|index| offers.values().filter(|held| held.contains(index)).count());

        for index in outstanding.clone() {
            let candidates = self.rank(&offers, &scores, index);
            if candidates.is_empty() {
                continue;
            }
            let mut installed = false;
            for station in candidates {
                let Some(provider) = providers.iter().find(|p| p.station == station) else {
                    continue;
                };
                match self
                    .fetch_one(&policy, content, &descriptor, provider, index, operation)
                    .await
                {
                    Ok(bytes) => {
                        moved += bytes;
                        handle.advance(
                            TransferState::Transferring {
                                bytes: moved,
                                total: Some(total),
                            },
                            Instant::now(),
                        );
                        installed = true;
                        break;
                    }
                    Err(lied) => {
                        // A proof failure is attributable to exactly one peer,
                        // and it costs a whole chunk — so that peer is out for
                        // this content immediately rather than after a budget.
                        // A refusal or a timeout only earns decaying probation.
                        let score = scores.entry(station.clone()).or_default();
                        if lied {
                            offers.remove(&station);
                        } else {
                            score.probation_until = Some(Instant::now() + PROBATION);
                        }
                    }
                }
            }
            if !installed {
                continue;
            }
        }

        handle.advance(TransferState::Verifying, Instant::now());
        let still_missing = self.missing_chunks(&policy, content, &descriptor);
        if still_missing.is_empty() {
            handle.succeed(Instant::now());
            Ok(())
        } else {
            handle.finish(TransferState::Failed, Instant::now());
            Err(Failure::Incomplete {
                missing: still_missing.len(),
            })
        }
    }

    /// Fetch and install one chunk. `Err(true)` means the provider lied.
    ///
    /// Loops because a provider may answer short — deliberately, under its own
    /// budget, or because the path is slow. Each round resumes from what is
    /// already staged and names the leaf the previous round validated, so a
    /// provider that changes its mind about which content this is gets refused
    /// before a byte is appended.
    async fn fetch_one(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        descriptor: &ContentDescriptor,
        provider: &Provider,
        index: u32,
        operation: [u8; 16],
    ) -> Result<u64, bool> {
        let cache = self.host.cache();
        let part = index;

        // Bytes staged by a previous *process* carry no live leaf binding —
        // the binding lives in this loop, not on disk — so they are discarded
        // rather than resumed. Chunk-granular resume survives a restart because
        // installed chunks are durable; sub-chunk resume deliberately does not.
        if cache.staged_len(&operation, part) > 0 {
            let _ = cache.discard_staged_part(&operation, part);
        }

        let mut known_leaf: Option<[u8; 32]> = None;
        let mut last_proof: Option<ChunkProof> = None;
        let mut moved = 0u64;
        for _ in 0..MAX_CHUNK_ROUNDS {
            let offset = cache.staged_len(&operation, part);
            let (proof, total_len, body) = provider
                .chunk(content, index, offset, known_leaf)
                .await
                .map_err(|_| false)?;

            // The leaf is judged before the bytes are kept: a wrong leaf means
            // the bytes about to be written are the wrong bytes, and refusing
            // now costs a refusal rather than a quarter megabyte.
            if descriptor.verify_leaf(&proof).is_err() || proof.leaf.chunk_index != index {
                return Err(true);
            }
            if known_leaf.is_some_and(|leaf| leaf != proof.leaf.ciphertext_hash) {
                // Mid-chunk, the provider started describing different bytes.
                return Err(true);
            }
            known_leaf = Some(proof.leaf.ciphertext_hash);
            last_proof = Some(proof);

            if body.is_empty() {
                // No progress. Not a lie, but not useful either.
                return Err(false);
            }
            cache
                .append_staged(&operation, part, offset, &body)
                .map_err(|_| false)?;
            moved += body.len() as u64;
            if cache.staged_len(&operation, part) >= total_len as u64 {
                break;
            }
        }

        let Some(proof) = last_proof else {
            return Err(false);
        };
        // Installing is where the whole chunk is re-hashed and checked against
        // the committed root. Until it returns, these bytes are staged: not
        // resident, not servable, not readable.
        self.host
            .install_staged_chunk(policy, content, operation, part, &proof)
            .map_err(|_| true)?;
        Ok(moved)
    }

    fn missing_chunks(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        descriptor: &ContentDescriptor,
    ) -> Vec<u32> {
        let all: Vec<u32> = (0..descriptor.chunk_count).collect();
        let held: BTreeSet<u32> = self
            .host
            .resident_among(policy, content, &all)
            .unwrap_or_default()
            .into_iter()
            .collect();
        all.into_iter().filter(|i| !held.contains(i)).collect()
    }

    /// Refuse before staging rather than after moving bytes.
    fn admit_by_quota(&self, descriptor: &ContentDescriptor) -> Result<(), Failure> {
        let cache = self.host.cache();
        let projected = cache
            .resident_bytes()
            .saturating_add(cache.staged_bytes())
            .saturating_add(descriptor.plaintext_len);
        if projected > self.cache_quota_bytes {
            return Err(Failure::OverQuota);
        }
        Ok(())
    }

    /// Candidates for one chunk, best first.
    ///
    /// Better-of-two rather than a strict ranking: picking the single fastest
    /// peer makes every fetcher choose the same one at the same moment and then
    /// blame it for being slow. Sampling breaks that herd.
    fn rank(
        &self,
        offers: &BTreeMap<Key, BTreeSet<u32>>,
        scores: &BTreeMap<Key, ProviderScore>,
        index: u32,
    ) -> Vec<Key> {
        let now = Instant::now();
        let mut able: Vec<&Key> = offers
            .iter()
            .filter(|(_, held)| held.contains(&index))
            .map(|(station, _)| station)
            .filter(|station| {
                scores
                    .get(*station)
                    .and_then(|s| s.probation_until)
                    .is_none_or(|until| until <= now)
            })
            .collect();
        able.sort_by(|a, b| {
            let sa = scores.get(*a).cloned().unwrap_or_default();
            let sb = scores.get(*b).cloned().unwrap_or_default();
            sa.outstanding
                .cmp(&sb.outstanding)
                .then(sb.throughput.total_cmp(&sa.throughput))
        });
        able.into_iter()
            .take(slots::MAX_INFLIGHT_CHUNKS_PER_PROVIDER)
            .cloned()
            .collect()
    }
}

/// How many short answers one chunk may take before the provider is giving up
/// without saying so.
///
/// A chunk is at most one frame, so an honest provider finishes in one round
/// and a slow one in a handful. Bounded because "keep asking until it is done"
/// is a loop a peer controls.
const MAX_CHUNK_ROUNDS: usize = 16;
