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
use std::time::Instant;

use replica::content::ContentRef;

use crate::admission::AdmittedPeer;
use crate::budget::{deadline, slots, ByteGate, Verdict};
use crate::content_host::{ContentHost, ContentHostError, ContentPolicy};
use crate::lifecycle::CancelToken;
use crate::planes::{bounds, FreightFrame};

/// Length prefix for one framed message on a Freight flow.
///
/// The request needs no delimiter — one request per flow, and finishing says
/// where it ends. A response does: a chunk answer is a header followed by raw
/// ciphertext, and the reader has to know where one stops and the other starts
/// without trusting a length the header itself declares.
const FRAME_PREFIX: usize = 4;

/// Encode one frame with its length prefix, refusing rather than overflowing.
pub fn frame(message: &FreightFrame) -> Vec<u8> {
    let body = message.encode_bounded();
    let mut out = Vec::with_capacity(FRAME_PREFIX + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Read one length-prefixed frame, bounded before anything is allocated.
pub async fn read_frame(
    flow: &mut dyn comms::RecvFlow,
    max: usize,
) -> Result<FreightFrame, FreightError> {
    let header = flow
        .read_exact(FRAME_PREFIX)
        .await
        .map_err(|_| FreightError::Truncated)?;
    let len = u32::from_le_bytes(header.try_into().expect("four bytes")) as usize;
    if len > max.min(bounds::MAX_CONTROL_FRAME_BYTES) {
        // Refused by the *declared* length, before a buffer that size exists.
        return Err(FreightError::TooLarge);
    }
    let body = flow
        .read_exact(len)
        .await
        .map_err(|_| FreightError::Truncated)?;
    FreightFrame::decode_canonical(&body).map_err(|_| FreightError::Malformed)
}

/// Why a Freight exchange did not complete, locally.
///
/// Local diagnostics. None of these ever reach a peer — a peer gets `Refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreightError {
    /// The flow ended before the message did.
    Truncated,
    /// A declared length past what this side will allocate.
    TooLarge,
    /// Structurally invalid or non-canonical.
    Malformed,
    /// The peer stopped talking within a deadline.
    TimedOut,
    /// The local content plane refused or could not answer.
    Content(ContentHostError),
}

impl From<ContentHostError> for FreightError {
    fn from(e: ContentHostError) -> Self {
        FreightError::Content(e)
    }
}

/// Everything the provider half needs.
pub struct FreightService {
    host: std::sync::Arc<ContentHost>,
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
        keys: std::sync::Arc<dyn crate::content_host::ContentKeys>,
        space: mechanics::ids::SpaceId,
        max_content_len: u64,
    ) -> Self {
        Self {
            host,
            workers: std::sync::Arc::new(tokio::sync::Semaphore::new(slots::MAX_SERVE_WORKERS)),
            keys,
            space,
            max_content_len,
        }
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
    async fn serve(
        &self,
        connection: Box<dyn comms::Connection>,
        peer: AdmittedPeer,
        cancel: CancelToken,
    ) {
        let connection = Rc::new(connection);
        let peer = Rc::new(peer);
        let mut requests = tokio::task::JoinSet::new();
        // One gate per connection-owning task: no sharing, no locking on the
        // hot path, and a peer's own conforming traffic pays back its strikes.
        let mut gate = ByteGate::new(Instant::now(), 32, 32, 128 * 1024, 512 * 1024, 64);

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
            if gate.check(Instant::now(), bounds::MAX_CONTROL_FRAME_BYTES) == Verdict::Close {
                connection.close(1, b"");
                break;
            }

            // The permit before the spawn. A peer that opens flows faster than
            // we serve them queues on the semaphore rather than on the task
            // scheduler, and a full one refuses rather than accumulating.
            let Ok(permit) = self.workers.clone().try_acquire_owned() else {
                let mut send = send;
                let _ = send.write_all(&frame(&FreightFrame::Refused)).await;
                let _ = send.finish();
                continue;
            };

            let host = self.host.clone();
            let keys = self.keys.clone();
            let space = self.space.clone();
            let max_content_len = self.max_content_len;
            let standing = peer.clone();
            requests.spawn_local(async move {
                let _permit = permit;
                let authorize = serve_predicate(&standing);
                let policy = ContentPolicy {
                    space: &space,
                    keys,
                    authorize: &authorize,
                    max_content_len,
                };
                serve_request(host.as_ref(), &policy, send, recv).await;
            });
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
                        let header = frame(&FreightFrame::ChunkHeader {
                            content_id: *content_id,
                            chunk_index: *chunk_index,
                            proof: proof.encode(),
                            total_len: total,
                        });
                        let mut answer = header;
                        answer.extend_from_slice(&bytes);
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
        let _ = tokio::time::timeout(deadline::CHUNK_HEADER, write).await;
    }
}
