//! The initiator, portable: one dial, pull whole, validate, incorporate.
//!
//! A faithful port of the runtime's `contact_driver::initiate` for peers that
//! have no driver — a browser tab above all. Differences are exactly the ones
//! the environment forces and none of substance: deadlines ride n0-future's
//! web-time clock instead of tokio's; validation and incorporation run inline
//! (a worker has no blocking pool, and a pulling tab has nothing else to
//! freeze); and `declare` is always false — a fresh peer holds nothing worth
//! declaring, and declaring nothing makes the responder omit nothing, so root
//! completeness is always satisfiable from what arrives. The declared-
//! holdings path and the StaleDeclaration repair stay with the runtime's
//! driver, which is the only caller with a catalog to declare.
//!
//! The wire discipline is identical frame for frame: Open → Accept/Refusal →
//! signed Offer → Proof (bound to the transport peer BEFORE any staging is
//! allocated) → the twelve-frame grammar through [`InitiatorReceiver`] → ack,
//! finish, drop — the dialer's close. Authority is durably incorporated
//! inside validation, before the manifest and Bodies are checked.

use mechanics::ids::SpaceId;
use mechanics::station::Key;
use n0_future::time::{timeout, Duration, Instant};
use replica::transaction::{CommitContext, SeedSigner};
use replica::Replica;

use crate::admission::{Accept, Open, Plane, Refusal};
use crate::{
    Authority, ContactId, Failure, InitiatorReceiver, Offer, Outcome, Progress, Proof,
    CONTACT_ALPN, CONTACT_PROTOCOL, MAX_FRAME,
};

/// The two deadlines every step runs under: the whole Contact, and forward
/// progress within it.
#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    pub whole: Duration,
    pub progress: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            whole: Duration::from_secs(120),
            progress: Duration::from_secs(20),
        }
    }
}

/// A step under both deadlines.
async fn step<F: std::future::Future>(
    whole_deadline: Instant,
    progress: Duration,
    fut: F,
) -> Result<F::Output, ()> {
    let now = Instant::now();
    if now >= whole_deadline {
        return Err(());
    }
    let budget = progress.min(whole_deadline - now);
    timeout(budget, fut).await.map_err(|_| ())
}

/// The transcript a [`pull_receive`] moved, staged for incorporation but not
/// yet committed. Separating it from the commit is what lets a live re-pull
/// install into an already-composed Station's Replica: the async receive runs
/// with no Replica lock, then the caller commits the `staged` material inside
/// the Station's own writer (`StationCore::with_replica_convergence`), exactly
/// as the native Contact driver does.
pub struct ReceivedContact {
    pub staged: replica::convergence::StagedContactMaterial,
    pub bytes_moved: u64,
}

/// Receive one Space's material over `lait/contact/2` and stage it — the async
/// half of a pull, touching no Replica. `holdings_root` is the caller's
/// current published root (the one-shot reads it from its own Replica; a live
/// re-pull reads it from the composed core), sent in the signed Offer so the
/// responder can diff against what this node already holds.
#[allow(clippy::too_many_lines, reason = "one wire exchange, ported verbatim")]
pub async fn pull_receive(
    transport: &dyn comms::Transport,
    responder: &Key,
    space: &SpaceId,
    station_seed: &[u8; 32],
    authority: &Authority,
    holdings_root: [u8; 32],
    deadlines: Deadlines,
) -> Result<ReceivedContact, Failure> {
    let space_bytes = <[u8; 29]>::try_from(space.as_str().as_bytes())
        .map_err(|_| Failure::Protocol("space id is not 29 rendered bytes".into()))?;
    let initiator_station = mechanics::actor::device_from_seed(station_seed)
        .key_bytes()
        .ok_or(Failure::Signing)?;
    let deadline = Instant::now() + deadlines.whole;
    let progress = deadlines.progress;

    let peer = responder.as_device();
    let mut stream = step(deadline, progress, transport.connect(peer, CONTACT_ALPN))
        .await
        .map_err(|_| Failure::Unreachable("connect: no route answered within the deadline".into()))?
        .map_err(|e| Failure::Unreachable(format!("connect: {e:#}")))?;

    let contact = ContactId::mint();
    let connection_id = contact.as_bytes();
    let mut connection_epoch = [0u8; 16];
    getrandom::fill(&mut connection_epoch).map_err(|_| Failure::Entropy)?;
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).map_err(|_| Failure::Entropy)?;
    let opening = Open {
        plane: Plane::Contact,
        protocol_version: CONTACT_PROTOCOL,
        features: 0,
        space: space_bytes,
        initiator_station,
        responder_station: responder.key_bytes(),
        connection_id,
        connection_epoch,
        authority_frontier: (authority.frontier)().as_bytes().to_vec(),
        requested_lanes: Vec::new(),
    };
    step(deadline, progress, stream.send(&opening.encode()))
        .await
        .map_err(|_| Failure::Unreachable("open: no progress within the deadline".into()))?
        .map_err(|e| Failure::Transport(format!("open: {e:#}")))?;
    let admission_bytes = step(deadline, progress, stream.recv())
        .await
        .map_err(|_| Failure::Unreachable("accept: no reply within the deadline".into()))?
        .map_err(|e| Failure::Transport(format!("accept: {e:#}")))?
        .ok_or_else(|| Failure::Unreachable("accept: the peer closed the stream".into()))?;
    // An undecodable Accept is usually a Refusal — the peer answered, it said
    // no, and which refusal is the one thing a turned-away caller can act on.
    let accept =
        Accept::decode_canonical(&admission_bytes).map_err(|_| match Refusal::decode_canonical(
            &admission_bytes,
        ) {
            Ok(refusal) => Failure::Admission(format!("the peer refused: {refusal:?}")),
            Err(_) => Failure::Admission("the admission reply did not decode".into()),
        })?;
    if accept.connection_id != connection_id
        || accept.connection_epoch != connection_epoch
        || accept.capability.plane != Plane::Contact
        || accept.capability.protocol_version != CONTACT_PROTOCOL
    {
        return Err(Failure::Admission(
            "the accept did not echo this connection".into(),
        ));
    }

    // declare == false, always: holdings root only, empty declaration. The
    // root is the caller's, so this half never touches a Replica.
    let hello = Offer::sign(
        opening.hash(),
        CONTACT_PROTOCOL,
        space_bytes,
        responder.key_bytes(),
        nonce,
        contact,
        holdings_root,
        0,
        [0u8; 32],
        station_seed,
    )
    .ok_or(Failure::Signing)?;
    step(deadline, progress, stream.send(&hello.encode()))
        .await
        .map_err(|_| Failure::Unreachable("hello: no progress within the deadline".into()))?
        .map_err(|e| Failure::Transport(format!("hello: {e:#}")))?;

    let ack_bytes = step(deadline, progress, stream.recv())
        .await
        .map_err(|_| Failure::Unreachable("hello-ack: no reply within the deadline".into()))?
        .map_err(|e| Failure::Transport(format!("hello-ack: {e:#}")))?
        .ok_or_else(|| Failure::Unreachable("hello-ack: the peer closed the stream".into()))?;
    let ack = Proof::decode(&ack_bytes)
        .map_err(|_| Failure::Protocol("the hello ack did not decode".into()))?;
    // Bind the negotiated transport peer to the signed Station identity
    // BEFORE any staging is allocated.
    ack.verify(&hello, responder).map_err(|_| {
        Failure::Protocol("the hello ack did not bind the hello and the Station identity".into())
    })?;

    let mut receiver = InitiatorReceiver::new(contact);
    let mut bytes_moved = (hello.encode().len() + ack_bytes.len()) as u64;
    loop {
        let frame = step(deadline, progress, stream.recv())
            .await
            .map_err(|_| Failure::Deadline("transfer: no frame within the deadline".into()))?
            .map_err(|e| Failure::Transport(format!("transfer: {e:#}")))?
            .ok_or_else(|| {
                Failure::Protocol("transfer: the peer closed before the material ended".into())
            })?;
        if frame.len() > MAX_FRAME {
            return Err(Failure::Protocol(format!(
                "a {}-byte frame exceeds the {MAX_FRAME}-byte maximum",
                frame.len()
            )));
        }
        bytes_moved += frame.len() as u64;
        match receiver.on_frame(&frame) {
            Ok(Progress::Continue) => {}
            Ok(Progress::SendAck(ack_frame)) => {
                let raw = ack_frame.encode(&contact);
                bytes_moved += raw.len() as u64;
                step(deadline, progress, stream.send(&raw))
                    .await
                    .map_err(|_| {
                        Failure::Deadline("transfer-ack: no progress within the deadline".into())
                    })?
                    .map_err(|e| Failure::Transport(format!("transfer-ack: {e:#}")))?;
                let _ = step(deadline, progress, stream.finish()).await;
                break;
            }
            Ok(Progress::PeerAborted(code)) | Err(code) => {
                return Err(Failure::PeerAborted(code));
            }
        }
    }
    let received = receiver.into_received().ok_or_else(|| {
        Failure::Protocol("the transfer ended before the material was complete".into())
    })?;
    drop(stream); // dialer close: we have the transcript

    // Stage the transcript. TransferAck already went out — it acknowledged the
    // transcript, not convergence — so the commit is the caller's to run
    // through its own Replica writer.
    let staged = replica::convergence::StagedContactMaterial {
        authority_records: received.authority_records,
        manifest_root_bytes: received.manifest_root_bytes,
        manifest_nodes: received.manifest_nodes.into_values().collect(),
        bodies: received
            .bodies
            .into_iter()
            .map(|((tx, key), bytes)| (tx, key, bytes))
            .collect(),
    };
    Ok(ReceivedContact {
        staged,
        bytes_moved,
    })
}

/// The one-shot pull: receive over the wire, then validate and incorporate
/// into a Replica this caller owns. The receive half is [`pull_receive`], so a
/// live re-pull can reuse it and run the validate/incorporate inside a live
/// Station's own writer — exactly as the native Contact driver does, which is
/// why the commit is inlined here rather than factored out (its error type is
/// the Replica's, unexported, and inlining keeps both callers identical to the
/// driver).
pub async fn pull_whole(
    transport: &dyn comms::Transport,
    responder: &Key,
    space: &SpaceId,
    station_seed: &[u8; 32],
    authority: &Authority,
    replica: &mut Replica,
    deadlines: Deadlines,
) -> Result<Outcome, Failure> {
    let holdings_root = replica.published_root().map(|r| r.0).unwrap_or([0u8; 32]);
    let received = pull_receive(
        transport,
        responder,
        space,
        station_seed,
        authority,
        holdings_root,
        deadlines,
    )
    .await?;
    let frontier = (authority.frontier)();
    let signer = SeedSigner(station_seed);
    let commit_ctx = CommitContext {
        space,
        signer: &signer,
        authority_frontier: frontier,
    };
    let mut incorporator = authority
        .incorporator
        .lock()
        .map_err(|_| Failure::Convergence("the incorporator lock is poisoned".into()))?;
    let bundle = replica
        .validate_contact(
            &received.staged,
            authority.source.as_ref(),
            &mut *incorporator,
        )
        .map_err(|failure| Failure::Convergence(format!("{failure:?}")))?;
    let convergence = replica
        .incorporate_bundle(&commit_ctx, bundle, authority.source.as_ref())
        .map_err(|failure| Failure::Convergence(format!("{failure:?}")))?;

    Ok(Outcome {
        bytes_moved: received.bytes_moved,
        convergence,
    })
}
