//! Turning "these chunks are missing" into peers that will serve them.
//!
//! [`ContentCursor`](crate::content_cursor::ContentCursor) asks a
//! [`ChunkSupply`](crate::content_cursor::ChunkSupply) for chunks it does not
//! hold. [`Fetcher::fetch_chunks`](crate::fetch::Fetcher::fetch_chunks) fetches
//! exactly those chunks from a set of [`Provider`](crate::fetch::Provider)s.
//! Both existed and nothing joined them, because a `Provider` is a *live,
//! admitted connection* rather than a descriptor — somebody has to decide who
//! to dial, dial them, and own the result.
//!
//! ## Why this is a thread and not a function
//!
//! `fetch_chunks` is not `Send`: its `ContentPolicy` holds a non-`Sync`
//! `&dyn Fn`, so the future cannot be `tokio::spawn`ed onto a multi-thread
//! runtime. The Contact and plane drivers already answer this the same way — a
//! current-thread runtime under a `LocalSet`, on a thread of its own — and this
//! follows them rather than inventing a second answer.
//!
//! It also has to be a thread because [`ChunkSupply::request`] must not block.
//! A cursor steps; it does not wait. So a request hands work across a channel
//! and answers with what is true *now*, which is usually
//! [`Gap::Fetching`](crate::content_cursor::Gap::Fetching).
//!
//! ## The distinction this exists to keep
//!
//! `fetch_chunks` answers `Failure::NoProvider` for two different facts: the
//! provider set was empty, and the provider set was asked and nobody offered.
//! On the cursor's side those are [`Gap::Unasked`] and [`Gap::Unoffered`], and
//! the `Gap` vocabulary exists precisely so they do not collapse — "could not
//! ask" is about this Station's reach, "nobody has it" is about the content.
//!
//! Only the thing that performs the dialling can tell them apart, which is this
//! module. It counts the providers it managed to connect before it calls
//! `fetch_chunks`, so a `NoProvider` from a non-empty set means the peers
//! answered and did not have it.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mechanics::station::Key;
use replica::content::ContentRef;

use crate::content_cursor::{ChunkSupply, Gap};
use crate::content_host::{Acquisition, ContentAction};
use crate::fetch::{connect_provider, Failure, Fetcher, Provider};
use crate::lifecycle::CancelToken;

/// How many peers one demand-paged read dials at once.
///
/// Bounded because a wide Space would otherwise be dialled in full for a
/// window of a film, and because the value of another provider falls off fast:
/// the first gives the bytes, the second and third cover a peer leaving
/// mid-window. Beyond that it is sessions held open for nothing.
const MAX_PROVIDERS: usize = 3;

/// May this Station acquire these bytes?
///
/// Supplied by the composition root, never invented here — the same rule
/// `HostResidency` follows, and for the same reason: a `ContentPolicy` names
/// the Space, the epoch key source and the operator ceiling, and a component
/// that built its own would be deciding for itself what it is allowed to want.
pub trait AcquireAuthority: Send + 'static {
    fn may(&self, action: ContentAction<'_>) -> Result<(), Vec<u8>>;
}

/// A member of this Space may acquire content in it.
///
/// The acquiring counterpart of Freight's `serve_predicate`, and it says the
/// same thing that one does, in the same place: **this does not scope**.
/// Admission is the membership half — a non-member never reaches a plane — and
/// `PlanePolicy` is the operator half. What is missing on both sides is a
/// per-content grant, and nobody holds one, so gating on it would refuse
/// everything.
///
/// Stated rather than hidden, exactly as the serving side states it:
/// acquisition is Space-wide, so a member can pull ciphertext for content
/// attached to something they hold no read grant on. It is ciphertext — the
/// Body key still gates reading — but it is a real gap, and the fix slots into
/// this one predicate without `Fetcher` changing shape.
pub struct MemberMayAcquire;

impl AcquireAuthority for MemberMayAcquire {
    fn may(&self, action: ContentAction<'_>) -> Result<(), Vec<u8>> {
        let _ = action;
        Ok(())
    }
}

/// Who might hold this content.
///
/// The same shape `plane::live::DialContext` already takes, and for the same
/// job — choosing who to dial — rather than a second abstraction over the same
/// registry.
///
/// Deliberately *not* content-addressed. There is no index from a content id to
/// the Stations holding it, and inventing one would be a replicated structure
/// with its own staleness. This answers "who is reachable at all" and the
/// availability round inside `fetch_chunks` asks them what they actually have.
/// A wrong guess costs one `have` round trip, which is bounded and already
/// charged like a chunk by the serving gate.
pub type Candidates = Arc<dyn Fn() -> Vec<Key> + Send + Sync>;

/// What is true about one content-and-operation right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Progress {
    /// Asked for, and the driver has it.
    Running,
    /// Ended, and the next ask should start again. Carries what to say once.
    Ended(Ended),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ended {
    /// Nothing could be dialled. This Station's reach, not the content.
    Unasked,
    /// Peers answered and none held it.
    Unoffered,
    /// The fetch was refused — quota, authority, or a Station that is busy.
    Refused,
    /// The chunks are here.
    Landed,
}

impl Ended {
    fn gap(self) -> Gap {
        match self {
            Self::Unasked => Gap::Unasked,
            Self::Unoffered => Gap::Unoffered,
            Self::Refused => Gap::Refused,
            // The chunks arrived, and the step that asked is not the step that
            // reads — one step yields at most one chunk. The cursor asks again
            // and finds them resident.
            Self::Landed => Gap::Fetching,
        }
    }
}

type Job = ([u8; 32], [u8; 16]);

enum Command {
    Want {
        content: ContentRef,
        operation: [u8; 16],
        chunks: Vec<u32>,
    },
    Abandon {
        job: Job,
    },
}

/// A [`ChunkSupply`] that dials peers and fetches what a cursor is missing.
pub struct PeerSupply {
    commands: Sender<Command>,
    progress: Arc<Mutex<BTreeMap<Job, Progress>>>,
}

/// Everything the driver thread owns.
pub struct SupplyContext {
    pub fetcher: Fetcher,
    pub transport: Arc<dyn comms::Transport>,
    pub local: Key,
    pub authority: Box<dyn AcquireAuthority>,
    pub candidates: Candidates,
    pub cancel: CancelToken,
}

impl PeerSupply {
    /// The supply, and the driver that has to be run for it to do anything.
    ///
    /// Split rather than spawned here so the thread belongs to whoever owns the
    /// Station: `spawn_tracked` is what makes `drain_tasks` join it, and a
    /// thread this module started for itself would outlive the Station that
    /// wanted it. One per Station, never joined, is a leak that only shows up
    /// as a slow machine.
    ///
    /// A supply whose driver has stopped answers [`Gap::Unasked`], which is
    /// true: there is nothing left that could ask.
    pub fn mount(context: SupplyContext) -> (Self, impl FnOnce(CancelToken) + Send + 'static) {
        let (commands, inbox) = std::sync::mpsc::channel();
        let progress: Arc<Mutex<BTreeMap<Job, Progress>>> = Arc::default();
        let shared = progress.clone();
        let driver = move |cancel: CancelToken| drive(context, inbox, shared, cancel);
        (Self { commands, progress }, driver)
    }

    fn note(&self, job: Job, progress: Progress) {
        if let Ok(mut held) = self.progress.lock() {
            held.insert(job, progress);
        }
    }
}

impl ChunkSupply for PeerSupply {
    fn request(&self, content: &ContentRef, operation: [u8; 16], chunks: &[u32]) -> Gap {
        let job = (*content.as_bytes(), operation);
        let standing = self
            .progress
            .lock()
            .ok()
            .and_then(|held| held.get(&job).copied());
        match standing {
            // Already working. Saying so is the whole answer — asking twice
            // would dial twice for the same window.
            Some(Progress::Running) => return Gap::Fetching,
            // The last attempt finished. Report it once and clear it, so the
            // next ask is a fresh attempt rather than a replayed old answer.
            Some(Progress::Ended(ended)) => {
                if let Ok(mut held) = self.progress.lock() {
                    held.remove(&job);
                }
                if ended != Ended::Landed {
                    return ended.gap();
                }
            }
            None => {}
        }
        let sent = self.commands.send(Command::Want {
            content: *content,
            operation,
            chunks: chunks.to_vec(),
        });
        if sent.is_err() {
            // The driver is gone. Nothing can be asked — which is not the same
            // as nobody having it, and this is exactly the pair that must not
            // collapse.
            return Gap::Unasked;
        }
        self.note(job, Progress::Running);
        Gap::Fetching
    }

    fn abandon(&self, content: &ContentRef, operation: [u8; 16]) {
        let job = (*content.as_bytes(), operation);
        if let Ok(mut held) = self.progress.lock() {
            held.remove(&job);
        }
        let _ = self.commands.send(Command::Abandon { job });
    }
}

/// The driver thread: a current-thread runtime under a `LocalSet`.
///
/// Same shape as the Contact and plane drivers, because the reason is the same
/// — the futures it runs are not `Send`.
fn drive(
    context: SupplyContext,
    inbox: Receiver<Command>,
    progress: Arc<Mutex<BTreeMap<Job, Progress>>>,
    cancel: CancelToken,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let mut abandoned: std::collections::BTreeSet<Job> = std::collections::BTreeSet::new();
        loop {
            if cancel.is_cancelled() {
                return;
            }
            // Timed rather than blocking: this thread is joined at shutdown, so
            // it has to wake on its own to notice cancellation. A bare `recv`
            // would sit here until somebody sent a command, and the join would
            // wait for a fetch nobody is going to ask for.
            let command = match inbox.recv_timeout(WAKE) {
                Ok(command) => command,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                // Every supply has been dropped. Nothing can ask again.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            };
            match command {
                Command::Abandon { job } => {
                    abandoned.insert(job);
                }
                Command::Want {
                    content,
                    operation,
                    chunks,
                } => {
                    let job = (*content.as_bytes(), operation);
                    if abandoned.remove(&job) {
                        continue;
                    }
                    let ended = serve(&context, &content, operation, &chunks).await;
                    if let Ok(mut held) = progress.lock() {
                        held.insert(job, Progress::Ended(ended));
                    }
                }
            }
        }
    });
}

/// How often an idle driver looks up to see whether it should stop.
const WAKE: Duration = Duration::from_millis(200);

/// Dial, fetch, and say which kind of nothing happened if nothing did.
async fn serve(
    context: &SupplyContext,
    content: &ContentRef,
    operation: [u8; 16],
    chunks: &[u32],
) -> Ended {
    let providers = dial(context, operation).await;
    if providers.is_empty() {
        // Nobody was reachable. This is about this Station, not the content —
        // `fetch_chunks` would fold it into `NoProvider` and lose that.
        return Ended::Unasked;
    }

    let authority = context.authority.as_ref();
    let allow = move |action: ContentAction<'_>| authority.may(action);
    let outcome = context
        .fetcher
        .fetch_chunks(
            content,
            chunks,
            &providers,
            operation,
            // A demand-paged read takes no lease: what is behind the playhead
            // has to be reclaimable, or a film longer than the cache can never
            // finish.
            Acquisition::Stream,
            &context.cancel,
            &allow,
        )
        .await;

    // No peer scoring from here. `NeighborRegistry` has `record_success` and
    // `record_failure`, and the Live dialer — the other component choosing who
    // to dial — feeds neither. A second component with its own opinion about
    // the same registry is a change worth making on purpose and for both, not
    // incidentally for one.
    match outcome {
        Ok(()) => Ended::Landed,
        // The set was not empty, so this is the peers answering and not having
        // it — the one `NoProvider` that is about the content.
        Err(Failure::NoProvider) => Ended::Unoffered,
        Err(_) => Ended::Refused,
    }
}

/// Connect up to [`MAX_PROVIDERS`] eligible Stations.
///
/// Opened per fetch and dropped when it returns. A playback cursor outlives any
/// one window but not a Station, and holding sessions open across a paused
/// screen would keep a swarm of connections alive for a picture nobody is
/// watching.
async fn dial(context: &SupplyContext, operation: [u8; 16]) -> Vec<Provider> {
    let mut providers = Vec::new();
    for station in (context.candidates)() {
        if providers.len() >= MAX_PROVIDERS {
            break;
        }
        if station == context.local {
            continue;
        }
        let connection = tokio::time::timeout(
            DIAL_PATIENCE,
            connect_provider(
                context.transport.as_ref(),
                &context.fetcher.space,
                &context.local,
                &station,
                operation,
            ),
        )
        .await;
        // A refusal and a timeout both leave this Station unreached, and
        // neither says anything about the content — which is why an empty set
        // answers `Unasked` rather than `Unoffered`.
        if let Ok(Ok(provider)) = connection {
            providers.push(provider);
        }
    }
    providers
}

/// How long one dial may take before the next candidate is tried.
///
/// A demand-paged read is behind a playhead, so a peer that has not answered in
/// this long has already cost more than it is worth — the next candidate is a
/// better use of the window than waiting out a transport deadline.
const DIAL_PATIENCE: Duration = Duration::from_secs(5);
