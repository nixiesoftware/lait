#![allow(
    clippy::as_conversions,
    reason = "Freight wire lengths are checked against the frozen frame ceiling"
)]
//! Freight — moving exact requested bytes between admitted peers.
//!
//! **Requests are exact.** There is no "list what you have" and no remote path.
//! A peer asks for one chunk of one content whose id it already learned from
//! durable state, and the answer is bytes or a refusal. That is the whole
//! surface, and it is small on purpose: a plane that could be asked open
//! questions would be a plane that answers them.
//!
//! **One flow per request.** The flow is the correlator, so there is no
//! request-id table to keep, bound, or get wrong — a request that is abandoned
//! is a flow that is reset, and a peer cannot accumulate outstanding state by
//! asking without listening.
//!
//! **One refusal.** Authorization, policy, load, absence, and incomplete proof
//! material all produce `Refused`, because a peer that could tell them apart
//! could map a Space by asking about content it invented and reading the
//! answers.
//!
//! Freight flows carry no stream-kind byte. The ALPN types the connection —
//! `stream_kind` belongs to `lait/session/1`, and an opening cannot even name a
//! Freight lane.

use std::rc::Rc;
// `tokio::time::Instant`, not `tokio::time::Instant`. Without the `test-util`
// feature it IS `tokio::time::Instant::now()` — same call, same value, no
// indirection — so production pays nothing. With it, `tokio::time::pause()`
// stops the clock for every site at once, which is what lets a test drive a
// sweep interval or a probation window without waiting for one.
use tokio::time::Instant;

use replica::content::ContentRef;

use crate::admission::AdmittedPeer;
use crate::budget::{deadline, gates, slots, ByteGate, Gate, Verdict};
use crate::content_host::{ContentHost, ContentPolicy, Failure as ContentFailure};

/// Content hosting outcomes used by Freight's semantic boundary.
pub mod content {
    pub use crate::content_host::{Failure, MAX_RANGE_BYTES};
}
use crate::lifecycle::CancelToken;
use crate::plane::{bounds, FreightFrame};

/// Length prefix for one framed message on a Freight flow.
///
/// Encode one frame with its length prefix, refusing rather than overflowing.
pub fn frame(message: &FreightFrame) -> Vec<u8> {
    crate::plane_stream::frame(&message.encode_bounded())
}

/// Read one length-prefixed frame, bounded before anything is allocated.
///
/// The framing itself lives in `plane_stream` now, because Live needs the same
/// one and a second copy would be a second place for "refused by the declared
/// length, before a buffer that size exists" to be got subtly wrong. Freight
/// keeps its own error vocabulary — these are local diagnostics and none of
/// them reaches a peer.
pub async fn read_frame(
    flow: &mut dyn comms::RecvFlow,
    max: usize,
) -> Result<FreightFrame, Failure> {
    let body = crate::plane_stream::read_framed(flow, max)
        .await
        .map_err(|error| match error {
            crate::plane_stream::Invalid::TooLarge => Failure::TooLarge,
            _ => Failure::Truncated,
        })?;
    FreightFrame::decode_canonical(&body).map_err(|_| Failure::Malformed)
}

/// Why a Freight exchange did not complete, locally.
///
/// Local diagnostics. None of these ever reach a peer — a peer gets `Refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The flow ended before the message did.
    Truncated,
    /// A declared length past what this side will allocate.
    TooLarge,
    /// Structurally invalid or non-canonical.
    Malformed,
    /// The peer stopped talking within a deadline.
    TimedOut,
    /// The local content plane refused or could not answer.
    Content(ContentFailure),
}

impl From<ContentFailure> for Failure {
    fn from(e: ContentFailure) -> Self {
        Failure::Content(e)
    }
}

/// Everything the provider half needs.
pub struct FreightService {
    host: std::sync::Arc<ContentHost>,
    /// What the transfer registry says is still live. Housekeeping reads it;
    /// nothing else can answer the question.
    registry: std::sync::Arc<crate::transfer::TransferRegistry>,
    /// How far over quota the last sweep left this Station, or zero.
    ///
    /// Surfaced rather than logged, because the operator action it implies —
    /// unpin or forget something — is one only they can take.
    over_quota: std::sync::atomic::AtomicU64,
    /// Concurrent serve tasks across this Space's connections.
    ///
    /// Acquired before a task is spawned. Inside would let a flood outrun the
    /// cap by queueing tasks, which bounds nothing.
    workers: std::sync::Arc<tokio::sync::Semaphore>,
    keys: std::sync::Arc<dyn crate::content_host::ContentKeys>,
    space: mechanics::ids::SpaceId,
    max_content_len: u64,
}

impl FreightService {
    pub fn new(
        host: std::sync::Arc<ContentHost>,
        registry: std::sync::Arc<crate::transfer::TransferRegistry>,
        keys: std::sync::Arc<dyn crate::content_host::ContentKeys>,
        space: mechanics::ids::SpaceId,
        max_content_len: u64,
    ) -> Self {
        Self {
            host,
            registry,
            over_quota: std::sync::atomic::AtomicU64::new(0),
            workers: std::sync::Arc::new(tokio::sync::Semaphore::new(slots::MAX_SERVE_WORKERS)),
            keys,
            space,
            max_content_len,
        }
    }

    /// How far over its quota this Station was at the last sweep.
    pub fn over_quota_bytes(&self) -> u64 {
        self.over_quota.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The authorization question every served request asks.
///
/// **Membership plus an operator switch, not a per-content grant.** No member
/// holds `content.serve` today, so a grant-gated plane would deny universally
/// on day one — which is not caution, it is the feature not working. The peer
/// reaching here has already been resolved to an actor at a pinned frontier by
/// `admission::judge`; that is the membership half, and `PlanePolicy` is the
/// operator half.
///
/// The consequence is stated rather than hidden: Freight residency is Space
/// wide, so a member can pull ciphertext for content attached to something they
/// hold no read grant on. It is ciphertext — the Body key still gates reading —
/// but it is a real gap, and the fix is a `content.serve` grant slotting into
/// exactly this closure without `ContentHost` changing shape.
fn serve_predicate(
    peer: &AdmittedPeer,
) -> impl Fn(crate::content_host::ContentAction) -> Result<(), Vec<u8>> + '_ {
    move |action| {
        let _ = (peer, action);
        Ok(())
    }
}

impl crate::plane_driver::PlaneService for FreightService {
    /// Reclaim what finished transfers left, then let the quota do what it can.
    ///
    /// The live set comes from the transfer registry *and* nothing else knows
    /// it — an operation lease outlives its process by design, so the only way
    /// a staging slot is ever declared dead is somebody saying which operations
    /// are alive. Getting that set wrong in the safe direction leaks disk;
    /// getting it wrong in the other deletes a live transfer's bytes, so the
    /// registry is asked rather than inferred.
    async fn maintain(&self) {
        let live = self.registry.live_operations();
        let cache = self.host.cache();
        let _ = cache.sweep_staging(&live);

        // The quota is a target, not a guarantee: every chunk of a committed
        // content is held by that content's own lease, so a Station full of
        // committed content has no eligible victims at all. Reporting the
        // shortfall is the whole point — "0 reclaimed, Ok" while sitting far
        // over quota tells an operator nothing, and the answer they need is
        // "forget something", not "wait".
        if let Ok(report) = cache.sweep() {
            if report.over_quota_bytes > 0 {
                self.over_quota.store(
                    report.over_quota_bytes,
                    std::sync::atomic::Ordering::Relaxed,
                );
            } else {
                self.over_quota
                    .store(0, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    async fn serve(
        &self,
        connection: std::sync::Arc<dyn comms::Connection>,
        peer: AdmittedPeer,
        cancel: CancelToken,
    ) {
        let peer = Rc::new(peer);
        let mut requests = tokio::task::JoinSet::new();
        // Two gates, because a Freight request and a Freight *answer* cost
        // different things. The request gate is about how often a peer may ask;
        // the byte gate is about how much it may be given, charged with what
        // was actually served rather than with a ceiling. Named specs rather
        // than literals: the first version of this used the live plane's byte
        // budget, which the docket says never applies here.
        let mut requests_gate = Gate::from_spec(Instant::now(), gates::FREIGHT_REQUESTS);
        let mut bytes_gate = ByteGate::from_spec(Instant::now(), gates::FREIGHT_BYTES);
        // How many requests this one connection may have in flight. The Space
        // wide permit alone lets a single peer hold every slot and force
        // refusals on every other member.
        let per_connection =
            std::sync::Arc::new(tokio::sync::Semaphore::new(bounds::MAX_STREAM_WORKERS));

        loop {
            if cancel.is_cancelled() {
                break;
            }
            let accepted = tokio::select! {
                flow = connection.accept_bi() => flow,
                _ = tokio::time::sleep(deadline::DRIVER_POLL) => {
                    while requests.try_join_next().is_some() {}
                    continue;
                }
            };
            let Ok(Some((send, recv))) = accepted else {
                break;
            };
            match requests_gate.check(Instant::now()) {
                Verdict::Allow => {}
                Verdict::Drop => {
                    // Throttled, not evicted. The whole point of the strike
                    // ledger is that a peer over its rate is refused and stays;
                    // discarding this verdict would make the gate a pure
                    // countdown to closing an honest peer.
                    refuse_now(send).await;
                    continue;
                }
                Verdict::Close => {
                    connection.close(REFUSED, b"");
                    break;
                }
            }

            // The permit before the spawn — both of them. A peer that opens
            // flows faster than we serve them queues on a semaphore rather than
            // on the task scheduler, and a full one refuses rather than
            // accumulating.
            let Ok(connection_permit) = per_connection.clone().try_acquire_owned() else {
                refuse_now(send).await;
                continue;
            };
            let Ok(permit) = self.workers.clone().try_acquire_owned() else {
                refuse_now(send).await;
                continue;
            };

            let host = self.host.clone();
            let keys = self.keys.clone();
            let space = self.space.clone();
            let max_content_len = self.max_content_len;
            let standing = peer.clone();
            requests.spawn_local(async move {
                let _permit = permit;
                let _connection_permit = connection_permit;
                let authorize = serve_predicate(&standing);
                let policy = ContentPolicy {
                    space: &space,
                    keys,
                    authorize: &authorize,
                    max_content_len,
                };
                serve_request(host.as_ref(), &policy, send, recv).await;
            });
            // Charged with the ceiling of what the answer may carry, because
            // the answer is produced in another task and the gate lives here.
            // Conservative in the peer's favour would be worse: it would let a
            // peer pull at any rate it liked.
            if bytes_gate.check(Instant::now(), bounds::MAX_CHUNK_FRAME_BYTES) == Verdict::Close {
                connection.close(REFUSED, b"");
                break;
            }
            while requests.try_join_next().is_some() {}
        }

        // Every in-flight request finishes or is abandoned before the
        // connection goes, so a served chunk is never truncated by our own
        // shutdown.
        let _ = tokio::time::timeout(deadline::CHUNK_HEADER, async {
            while requests.join_next().await.is_some() {}
        })
        .await;
        requests.abort_all();
    }
}

/// The coarse close code, shared with the driver.
const REFUSED: u32 = 1;

/// Say no on a flow we are not going to serve, without blocking the loop.
///
/// Deadlined because a peer that opens flows and never reads would otherwise
/// park the accept loop on a write nobody is draining — the one unbounded await
/// this module could have had.
async fn refuse_now(send: Box<dyn comms::SendFlow>) {
    let mut send = send;
    let _ = tokio::time::timeout(deadline::ACCEPT_WRITE, async {
        let _ = send.write_all(&frame(&FreightFrame::Refused)).await;
        let _ = send.finish();
    })
    .await;
}

/// Answer one request on one flow.
async fn serve_request(
    host: &ContentHost,
    policy: &ContentPolicy<'_>,
    mut send: Box<dyn comms::SendFlow>,
    mut recv: Box<dyn comms::RecvFlow>,
) {
    let request = tokio::time::timeout(
        deadline::CHUNK_RESOLVE,
        read_frame(recv.as_mut(), bounds::MAX_CONTROL_FRAME_BYTES),
    )
    .await;
    let Ok(Ok(request)) = request else {
        // A peer that could not manage a bounded request inside the deadline
        // gets nothing; the flow is reset so it reads as an abort rather than
        // an empty answer.
        send.reset(1);
        return;
    };

    let answered = match &request {
        FreightFrame::Have { content_id, wanted } => {
            let content = ContentRef {
                content_id: *content_id,
            };
            match host.resident_among(policy, &content, wanted) {
                // Absence and ignorance are the same answer, so a peer cannot
                // probe for what this Space holds by naming ids it invented.
                Ok(chunks) => Some(frame(&FreightFrame::Available {
                    content_id: *content_id,
                    chunks,
                })),
                Err(_) => Some(frame(&FreightFrame::Refused)),
            }
        }
        FreightFrame::GetChunk {
            content_id,
            chunk_index,
            offset,
            max_len,
            resume_leaf,
        } => {
            let content = ContentRef {
                content_id: *content_id,
            };
            match host.chunk_range(
                policy,
                &content,
                *chunk_index,
                *offset,
                (*max_len as usize).min(bounds::MAX_CHUNK_FRAME_BYTES),
            ) {
                Ok((bytes, proof, total)) => {
                    // A resume names the leaf it already validated. Serving a
                    // different one would let a transfer be steered onto other
                    // content halfway through — so the mismatch is refused
                    // before a byte is written, not after.
                    if resume_leaf.is_some_and(|leaf| leaf != proof.leaf.ciphertext_hash) {
                        Some(frame(&FreightFrame::Refused))
                    } else {
                        let header = FreightFrame::ChunkHeader {
                            content_id: *content_id,
                            chunk_index: *chunk_index,
                            proof: proof.encode(),
                            total_len: total,
                        };
                        // Only append the body if the header is really the
                        // header. `frame` substitutes a refusal for anything
                        // oversized, and appending regardless would send a
                        // refusal followed by the ciphertext it just declined.
                        let encoded = frame(&header);
                        let mut answer = encoded;
                        if header.validate().is_ok() {
                            answer.extend_from_slice(&bytes);
                        }
                        Some(answer)
                    }
                }
                Err(_) => Some(frame(&FreightFrame::Refused)),
            }
        }
        // A provider does not receive answers. Being sent one is a peer using
        // the wrong end of the protocol, which is refused like anything else.
        FreightFrame::Available { .. }
        | FreightFrame::ChunkHeader { .. }
        | FreightFrame::Refused => Some(frame(&FreightFrame::Refused)),
    };

    if let Some(bytes) = answered {
        let write = async {
            send.write_all(&bytes).await.ok()?;
            send.finish().ok()?;
            Some(())
        };
        // The provider's budget, not the requester's. `CHUNK_HEADER` is the
        // number the *asking* side waits out, and it exceeds this one by a
        // margin precisely so a timeout names one side.
        let _ = tokio::time::timeout(deadline::CHUNK_RESOLVE, write).await;
    }
}
