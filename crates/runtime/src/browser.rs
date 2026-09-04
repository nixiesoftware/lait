//! The browser composition root: the daemon's own Station machinery —
//! [`StationCore`], [`Session`], the one shared dock path — composed by the
//! embedding Worker instead of the native lifecycle.
//!
//! There is no lifecycle here: no Orbit, no store custody chain, no drivers,
//! no planes. The Worker owns everything the composition stands on — the
//! Replica it opened on a browser medium, the ledger it pulled over Contact,
//! the runner it instantiated under the browser's own WebAssembly — and this
//! module wires those into the *same* query machinery the daemon runs, so a
//! browser read and a daemon read are one code path. A parallel composition
//! that re-derived publications or principal facts by hand would be a second
//! model disagreeing with the first exactly when an answer mattered.
//!
//! wasm32-only by design. On native, the lifecycle's custody chain (flock,
//! epoch bump, crash recovery) is the only door to a store; offering this
//! constructor there would be a second door around it.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use mechanics::ids::DeviceId;
use replica::body::WorldId;

pub use crate::session::DockRefusal;
use crate::session::{dock_session, DockPoint, ReadMemoryGovernor, Session, StationCore};
use crate::world::{AuthorityView, LocalIdentity, PrincipalResolution};

/// Why the composition was refused.
#[derive(Debug)]
pub enum ComposeRefusal {
    /// The read-memory governor refused the Station's resident snapshot.
    ReadCapacity,
}

/// A Station composed in the browser: the daemon's read/query machinery over
/// a Worker-owned Replica, with the Worker's own ledger as the authority.
pub struct Station {
    space: mechanics::ids::SpaceId,
    core: Arc<StationCore>,
    authority: Arc<dyn AuthorityView>,
    registry: crate::registry::Catalog,
    epoch: mechanics::station::Epoch,
    find_policy: crate::find::Policy,
    alive: Arc<AtomicBool>,
}

/// Declare the registry's schemas on a Replica BEFORE remote material
/// arrives. Convergence classifies each body at incorporation: an undeclared
/// `(world, schema, version)` takes the opaque-retention branch, and only
/// re-receipt upgrades it — a later declaration reinterprets nothing. The
/// native Station declares at activation, before its Contact driver ever
/// pulls; a browser composition that pulls before composing must make the
/// same declaration through this seam, or the pull lands unreadable and the
/// dock's corpus build refuses, loudly but avoidably.
/// Whether a caret may anchor into this body: `Ok(true)` collaborative and
/// readable, `Ok(false)` not held (a drift, not a failure), `Err` opaque or
/// non-collaborative — the same gate `ReplicaReader::anchor_in_body` applies,
/// factored so the mint and resolve halves share it.
fn anchorable(
    replica: &replica::Replica,
    key: &replica::body::BodyKey,
) -> Result<bool, crate::world::BodyReadFailure> {
    let Some(binding) = replica.binding(key) else {
        return Ok(false);
    };
    let coordinate = crate::world::BodyReadCoordinate::new(key.clone(), None);
    if crate::exec::is_reserved_schema(&binding.schema) || replica.is_opaque(key) {
        return Err(crate::world::BodyReadFailure::Opaque(coordinate));
    }
    if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
        return Err(crate::world::BodyReadFailure::NotCollaborative(coordinate));
    }
    Ok(true)
}

pub fn declare_schemas(replica: &mut replica::Replica, registry: &crate::registry::Catalog) {
    replica.set_supported(registry.supported_schemas());
}

impl Station {
    /// Compose the query layer over an already-opened Replica. Declares the
    /// registry's schemas on the Replica exactly as the native activation
    /// does, so Convergence classifies remote material identically — though
    /// material that already arrived undeclared stays opaque; see
    /// [`declare_schemas`]. Corpus image acceleration is native-only (`None`
    /// here): the wasm store seam refuses `sync_dir` rather than pretending.
    pub fn compose(
        space: mechanics::ids::SpaceId,
        mut replica: replica::Replica,
        authority: Arc<dyn AuthorityView>,
        registry: crate::registry::Catalog,
        epoch: mechanics::station::Epoch,
    ) -> Result<Self, ComposeRefusal> {
        declare_schemas(&mut replica, &registry);
        let core = Arc::new(
            StationCore::new(
                epoch,
                crate::session::DEFAULT_OBSERVATION_CAPACITY,
                replica,
                ReadMemoryGovernor::process_default(),
                None,
            )
            .map_err(|()| ComposeRefusal::ReadCapacity)?,
        );
        Ok(Self {
            space,
            core,
            authority,
            registry,
            epoch,
            find_policy: crate::find::Policy::default(),
            alive: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Attach a local caller to a hosted World — the same derivation of
    /// principal facts, activation, and publication the native Station docks
    /// through.
    pub fn dock(
        &self,
        world_id: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<Session, DockRefusal> {
        dock_session(
            DockPoint {
                space: &self.space,
                core: &self.core,
                authority: &self.authority,
                registry: &self.registry,
                epoch: self.epoch,
                find_policy: self.find_policy,
                alive: &self.alive,
            },
            world_id,
            identity,
        )
    }

    /// Proof-of-possession of a device seed, minted here because the native
    /// minting door (`Runtime::identity_from_seed`) does not exist on wasm.
    pub fn identity_from_seed(seed: &[u8; 32]) -> LocalIdentity {
        LocalIdentity::from_seed(seed)
    }

    /// This live Station's current published root — the holdings root a
    /// re-pull signs into its Offer. See [`StationCore::published_root`].
    pub fn published_root(&self) -> [u8; 32] {
        self.core.published_root()
    }

    /// Mint an anchor for a caret: the viewer sends a `u64` cursor position in a
    /// field, and a live caret rides a `fabric::Anchor` bound to that position so
    /// it survives concurrent edits. `None` when the position is not anchorable
    /// (the body is not held, is opaque, or is not a collaborative field).
    ///
    /// Reads the LIVE Replica, not the pinned publication the native Live plane
    /// anchors against: on wasm the publication worker never runs, so the
    /// Station's snapshot is never built and would report every body absent —
    /// the same asymmetry that makes the query path build inline. The Replica
    /// holds the pulled bodies directly, so a tab anchors against it, mirroring
    /// the `ReplicaReader` gate (bound-out reserved/opaque/non-collaborative
    /// exactly as a read does).
    pub fn anchor(
        &self,
        key: &replica::body::BodyKey,
        field: &str,
        position: u64,
    ) -> Result<Option<fabric::Anchor>, crate::world::BodyReadFailure> {
        let dormant = || {
            crate::world::BodyReadFailure::Interrupted(crate::world::BodyReadCoordinate::new(
                key.clone(),
                None,
            ))
        };
        match self.core.with_replica_read(|replica| {
            let inner = match anchorable(replica, key) {
                Ok(true) => Ok(replica.anchor(key, field, position)),
                Ok(false) => Ok(None),
                Err(failure) => Err(failure),
            };
            Ok(inner)
        }) {
            Ok(inner) => inner,
            Err(_) => Err(dormant()),
        }
    }

    /// Resolve a peer's caret anchor to a position this tab can draw — the
    /// receive half. A deleted position is `Ok(Drifted)`. Reads the live
    /// Replica for the same reason [`Self::anchor`] does.
    pub fn resolve_anchor(
        &self,
        key: &replica::body::BodyKey,
        anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, crate::world::BodyReadFailure> {
        let dormant = || {
            crate::world::BodyReadFailure::Interrupted(crate::world::BodyReadCoordinate::new(
                key.clone(),
                None,
            ))
        };
        match self.core.with_replica_read(|replica| {
            let inner = match anchorable(replica, key) {
                Ok(true) => Ok(replica.resolve_anchor(key, anchor)),
                // An unheld body cannot resolve a peer's anchor to a position;
                // that is drift, not a failure.
                Ok(false) => Ok(fabric::AnchorResolution::Drifted),
                Err(failure) => Err(failure),
            };
            Ok(inner)
        }) {
            Ok(inner) => inner,
            Err(_) => Err(dormant()),
        }
    }

    /// Install converged Contact material into this LIVE Station's Replica: the
    /// seam a browser re-pull commits through, so the new snapshot reaches the
    /// docked Session and the doorbell fires — the same `with_replica_
    /// convergence` the native Contact driver installs through, exposed for the
    /// Worker that owns both ends of the pull. The closure runs the
    /// `validate_contact` + `incorporate_bundle` a `contact::pull_receive`'s
    /// staged material feeds, under the Station's own writer lock.
    pub fn with_replica_convergence<F>(
        &self,
        f: F,
    ) -> Result<replica::convergence::ConvergenceOutcome, replica::transaction::commit::Failure>
    where
        F: FnOnce(
            &mut replica::Replica,
        ) -> Result<
            replica::convergence::ConvergenceOutcome,
            replica::transaction::commit::Failure,
        >,
    {
        self.core.with_replica_convergence(f)
    }

    /// Read this live Station's Replica — the capture side of a daemon-less
    /// snapshot. The Worker owns both ends of the pull, so it also owns the
    /// export that persists the pulled Space to a bucket.
    pub fn with_replica_read<T>(
        &self,
        f: impl FnOnce(&replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        self.core.with_replica_read(f)
    }

    /// Ring the doorbell for material a Contact re-pull just converged, so the
    /// docked Session's viewer re-reads. `with_replica_convergence` only
    /// incorporates the material and rebuilds publications — emitting the ring is
    /// the CALLER's job (the native Contact driver does it too), and on wasm the
    /// caller is the Worker's `repull`. Without it a pulled peer edit reaches the
    /// Replica but nothing tells the UI to re-read, so the edit lands invisibly
    /// until the next boot.
    ///
    /// The native host rings a *routed* invalidation: it projects the Observation
    /// through the hosted World's own container/plane vocabulary so only the
    /// touched rows re-read. A browser tab holds the World as an opaque runner
    /// and cannot run that projection, so a convergence that changed anything
    /// rings a single coarse RESET instead — the viewer re-reads its active
    /// resources, correct if broader than the routed form. Nothing rings when the
    /// pull converged nothing (a poll that moved only already-held material, or a
    /// tab's own just-pushed write echoing back), so steady-state polling is
    /// silent.
    pub fn publish_convergence(
        &self,
        outcome: &replica::convergence::ConvergenceOutcome,
        authority_advanced: bool,
    ) {
        let changed = !outcome.bodies.is_empty() || !outcome.changes.is_empty();
        if changed || authority_advanced {
            self.core
                .broadcaster
                .publish_reset(outcome.current, authority_advanced);
        }
        if authority_advanced {
            self.core.note_authority_advanced();
        }
    }

    /// Ring a coarse RESET for the current durable state — the tab's only honest
    /// signal for a STRUCTURAL local write (a project, a milestone, a rename)
    /// that changes a catalog the tab, holding the World as an opaque runner,
    /// cannot route a per-plane invalidation for. Every active resource re-reads
    /// and the app stays alive; the daemon does this precisely, a tab
    /// necessarily coarsely. Collaborative document edits do NOT use this — they
    /// ride the session lane and carry their own exact per-document observation,
    /// so keystrokes never trigger a whole-view refresh.
    pub fn ring_reset(&self) {
        if let Ok(frontier) = self
            .core
            .with_replica_read(|replica| Ok(replica.frontier()))
        {
            self.core.broadcaster.publish_reset(frontier, false);
        }
    }

    /// The current replica frontier — the cheap before/after probe a caller
    /// uses to tell whether a link-lane RPC actually wrote (advancing the
    /// frontier) or only read.
    pub fn frontier(&self) -> Option<replica::frontier::ReplicaFrontier> {
        self.core
            .with_replica_read(|replica| Ok(replica.frontier()))
            .ok()
    }

    /// Build this tab's outbound excess — the transfer it PUSHES to a responder
    /// under symmetric convergence so its writes converge OUT (nothing dials a
    /// tab, so it pushes on the dial it makes). Mirrors the native driver's
    /// `build_outbound`: read the composed core once, export the authorized
    /// manifest and Bodies, prefixed by the authority records the `authority`
    /// exposes. A tab holds no mechanics records to serve
    /// (`SharedLedgerAuthority`'s export is empty), so its WRITES ride the
    /// material; the `advertise` gate is `signer_can_write`, which a contributor
    /// device passes. An unadmitted joiner exports authority-only.
    pub fn export_excess(
        &self,
        seed: &[u8; 32],
        authority: &contact::Authority,
    ) -> Result<contact::OutboundTransfer, String> {
        let signer = LocalIdentity::from_seed(seed);
        let station_key = mechanics::actor::device_from_seed(seed)
            .key_bytes()
            .ok_or_else(|| "the tab's station device key is unavailable".to_string())?;
        let frontier = (authority.frontier)();
        let advertise = authority.source.signer_authorized(&station_key, &frontier);
        let held = std::collections::BTreeSet::new();
        let (material, manifest) = if advertise {
            self.core
                .with_replica_read(|replica| {
                    let commit_ctx = replica::transaction::CommitContext {
                        space: &self.space,
                        signer: &signer,
                        authority_frontier: frontier.clone(),
                    };
                    let material = replica.export_material_excluding(&held)?;
                    let manifest = replica.export_manifest(&commit_ctx)?;
                    Ok((material, manifest))
                })
                .map_err(|e| format!("the tab could not export its excess: {e:?}"))?
        } else {
            (Vec::new(), (Vec::new(), Vec::new()))
        };
        let mut authority_records = (authority.export)();
        let mut bodies = Vec::new();
        for (tx, closures) in &material {
            authority_records.push(tx.encode());
            for (key, artifact_pack) in closures {
                bodies.push((tx.id(), key.clone(), artifact_pack.clone()));
            }
        }
        Ok(contact::OutboundTransfer {
            authority_frontier: frontier.as_bytes().to_vec(),
            authority_records,
            manifest_root_bytes: manifest.0,
            manifest_nodes: manifest.1,
            bodies,
        })
    }
}

/// The pulled ledger as the Session's authority: every answer delegates to
/// the same `mechanics::space::Authority` calls the daemon's `SpaceAuthority`
/// makes — one ledger, one evaluation, minus the daemon. In production the
/// browser never authors founder policy; activation and capability grants
/// arrive already-signed in the ledger it pulled over Contact.
pub struct LedgerAuthorityView(pub contact::authority::SharedLedgerAuthority);

impl LedgerAuthorityView {
    fn lock(&self) -> std::sync::MutexGuard<'_, contact::authority::LedgerAuthority> {
        self.0
             .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl AuthorityView for LedgerAuthorityView {
    fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution> {
        let mut guard = self.lock();
        let actor = guard
            .ledger
            .actor_plane()
            .actor_of_device(device)
            .cloned()?;
        // An unreplayable ledger refuses rather than resolving from a clean
        // slate — the same fail-closed stance the daemon takes.
        let acl = guard.ledger.acl_state().ok()?;
        if !acl.is_member(&actor) {
            return None;
        }
        Some(PrincipalResolution {
            actor,
            authority_frontier: guard.frontier(),
        })
    }

    fn active_implementation(
        &self,
        world: &WorldId,
        authority_frontier: &replica::frontier::AuthorityFrontier,
    ) -> Result<Option<[u8; 32]>, String> {
        // A ledger error is carried, not flattened into "no activation".
        let mut guard = self.lock();
        guard
            .ledger
            .active_implementation_at(authority_frontier.as_bytes(), world.as_str())
            .map_err(|e| format!("activation state at the pinned frontier: {e}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn authorize_mutation(
        &self,
        _space: &mechanics::ids::SpaceId,
        world: &WorldId,
        actor: &mechanics::ids::ActorId,
        device: &DeviceId,
        authority_frontier: &replica::frontier::AuthorityFrontier,
        parent_manifest_root: [u8; 32],
        implementation_id: [u8; 32],
        intent_digest: [u8; 32],
        demand: &[u8],
        operations_digest: [u8; 32],
        core_digest: [u8; 32],
    ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        let mut guard = self.lock();
        let receipt = guard
            .ledger
            .authorize(&mechanics::authorization::AuthorizationRequest {
                world: world.as_str(),
                actor: actor.as_str(),
                device: device
                    .key_bytes()
                    .ok_or(mechanics::authorization::Refusal::Denied(
                        mechanics::authorization::DenialReason::Internal(
                            "device key bytes unavailable",
                        ),
                    ))?,
                authority_frontier: authority_frontier.as_bytes(),
                parent_manifest_root,
                implementation_id,
                intent_digest,
                demand,
                effect_operations_digest: operations_digest,
                body_transaction_core_digest: core_digest,
            })?;
        Ok(receipt.encode())
    }

    fn evaluate_read(
        &self,
        actor: &mechanics::ids::ActorId,
        authority_frontier: &replica::frontier::AuthorityFrontier,
        demand: &[u8],
    ) -> Result<bool, String> {
        // Failure to evaluate is not a denial: a malformed demand is a World
        // bug and an unmaterializable frontier is a ledger problem.
        let parsed = mechanics::authorization::AuthorizationDemand::decode_canonical(demand)
            .map_err(|e| format!("the read demand does not decode (a World bug): {e}"))?;
        let mut guard = self.lock();
        let view = guard
            .ledger
            .state_at(authority_frontier.as_bytes())
            .map_err(|e| format!("authority state at the pinned frontier: {e}"))?;
        Ok(view.acl.evaluate_demand(actor, &parsed).is_some())
    }
}
