//! Product-side hosts for the Worlds installed in one active Station.
//!
//! [`runtime::world::Catalog`] owns the immutable semantic implementations used
//! by a Station. This module owns the application-side half of that boundary:
//! a compile-time [`WorldPackage`] for each product, and one [`WorldHost`] per
//! package inside an active Space. A package carries the reviewed semantic
//! implementation plus its optional product-neutral call handler and its
//! Runtime-validated Exec package; orbital code never needs to name the
//! product behind any of them.
//!
//! A [`WorldRouter`] belongs to one active Station. It is not a process,
//! does not own a listener, and has no autonomous background loop.

use runtime::poison::LockRecovering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use mechanics::authorization::{PolicyCapability, Resource};
use mechanics::ids::{DeviceId, SpaceId};
use replica::body::WorldId;
use runtime::world::Refusal as RegistrationRefusal;
use runtime::Error as RuntimeFailure;
use runtime::{
    world::Builder, world::Catalog, world::LocalIdentity, world::World, Session, Station,
};

use runtime::world::call::{Access, Call, Code, Failure, Handler};
#[cfg(test)]
use runtime::world::call::{Context, Reply};

/// One World's Observation projection, in that World's own vocabulary.
///
/// Defined by `runtime` so a World supplied out of tree can name it without
/// depending on this shell.
pub use runtime::world::{Invalidation, RoutedInvalidation};

pub struct StatusProjection {
    pub items: usize,
    pub scopes: usize,
    pub name: String,
    pub description: String,
}

/// A World package's Observation projector. Implementations own their own
/// baselines; the Station host only fans generic observations out.
///
/// The invalidation vocabulary lives in `runtime`, so an out-of-tree World can
/// project without depending on this shell. The trait stays application-side
/// because it combines World projection with shell-facing status metadata and
/// projector registration.
pub trait ObservationProjector: Send + Sync {
    fn status(&self, session: &Session) -> Option<StatusProjection>;
    fn start(&self, session: &Session, space: &mechanics::ids::SpaceId);
    fn project(
        &self,
        session: &Session,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Invalidation;
}

/// One founder capability declared by a World package.
///
/// The shell installs these generic Mechanics facts without knowing the
/// product role or policy that produced them.
#[derive(Debug, Clone)]
pub struct FounderGrant {
    pub capability: PolicyCapability,
    pub resource: Resource,
    pub salt: [u8; 16],
}

/// A human-facing container a World wants created with a new Space.
///
/// `kind` is World-owned vocabulary. `key` and `name` are carried back to the
/// navigation catalog; the lifecycle host never interprets either one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialScope {
    pub kind: String,
    pub key: String,
    pub name: String,
}

/// The generic resources supplied to one World's formation hook.
pub struct BootstrapContext<'a> {
    pub store_root: &'a Path,
    pub space: &'a SpaceId,
    pub session: &'a Session,
    pub identity: &'a LocalIdentity,
    pub device: &'a str,
    pub display_name: &'a str,
    pub initial_scope: Option<&'a InitialScope>,
}

/// Product-owned policy invoked by the generic Space lifecycle.
///
/// Implementations are bound in the application composition root. The host
/// forms the Space, applies the returned Mechanics grants, docks the package's
/// own World, and supplies that Session here; it never names the product or
/// constructs one of its DTOs.
pub trait WorldLifecycle: Send + Sync {
    fn founder_grants(&self) -> anyhow::Result<Vec<FounderGrant>>;
    fn initial_scope(&self, display_name: &str) -> Option<InitialScope>;
    fn bootstrap(&self, context: BootstrapContext<'_>) -> anyhow::Result<()>;
}

/// One product package available to the application build.
#[derive(Clone)]
pub struct WorldPackage {
    world: WorldId,
    implementation: Arc<dyn World>,
    reviewed_implementation: [u8; 32],
    control: Option<Arc<dyn Handler>>,
    exec: runtime::exec::Package,
    projector: Option<Arc<dyn ObservationProjector>>,
    lifecycle: Option<Arc<dyn WorldLifecycle>>,
    /// The package an unformed Space activates by default. Historical exact
    /// packages remain installed for retained publication reads and existing
    /// Spaces whose authority still selects them.
    preferred: bool,
}

impl std::fmt::Debug for WorldPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldPackage")
            .field("world", &self.world)
            .field(
                "reviewed_implementation",
                &data_encoding::HEXLOWER.encode(&self.reviewed_implementation[..8]),
            )
            .field("has_call_handler", &self.control.is_some())
            .field("exec", &self.exec)
            .finish()
    }
}

impl WorldPackage {
    pub fn new(implementation: Arc<dyn World>, reviewed_implementation: [u8; 32]) -> Self {
        let world = implementation.descriptor().id;
        Self {
            world,
            implementation,
            reviewed_implementation,
            control: None,
            exec: runtime::exec::Package::new(),
            projector: None,
            lifecycle: None,
            preferred: true,
        }
    }

    /// The version this package's World declares for itself.
    ///
    /// Read off the descriptor rather than taken as a second constructor
    /// argument, for the same reason `Implementation::from_registration` reads
    /// it off the registration: a version passed alongside the implementation
    /// is a second source of truth that can disagree with the one the id was
    /// hashed over, and the disagreement would be invisible.
    pub fn reviewed_version(&self) -> u32 {
        self.implementation.descriptor().implementation_version.0
    }

    pub fn with_control(mut self, control: Arc<dyn Handler>) -> Self {
        self.control = Some(control);
        self
    }

    pub fn with_exec(mut self, exec: runtime::exec::Package) -> Self {
        self.exec = exec;
        self
    }

    pub fn with_projector(mut self, projector: Arc<dyn ObservationProjector>) -> Self {
        self.projector = Some(projector);
        self
    }

    pub fn with_lifecycle(mut self, lifecycle: Arc<dyn WorldLifecycle>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    /// Install this exact implementation for historical/authority-selected
    /// use without making it the formation default for its World.
    pub fn historical(mut self) -> Self {
        self.preferred = false;
        self
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world
    }
}

/// Compile-time composition of the Worlds bundled by one application build.
///
/// The package set is cloned down the Daemon → Station placement →
/// StationHost call stack. Each Space freezes its own Runtime registry and
/// host objects from the same reviewed set.
#[derive(Clone, Default)]
pub struct WorldPackages {
    packages: Vec<WorldPackage>,
}

impl WorldPackages {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one reviewed World package.
    ///
    /// Duplicate ids and registration/implementation mismatches are rejected by
    /// the existing [`Builder`] validation when [`Self::build`] freezes
    /// the set.
    pub fn with_package(mut self, package: WorldPackage) -> Self {
        self.packages.push(package);
        self
    }

    /// Add a semantic-only World. Kept for independent Runtime adopters that
    /// need no application control adapter.
    pub fn register(
        self,
        implementation: Arc<dyn World>,
        reviewed_implementation: [u8; 32],
    ) -> Self {
        self.with_package(WorldPackage::new(implementation, reviewed_implementation))
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.packages
            .iter()
            .filter(|package| package.preferred)
            .map(WorldPackage::world_id)
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.packages
            .iter()
            .any(|package| package.world_id() == world)
    }

    pub fn reviewed_implementation(&self, world: &WorldId) -> Option<[u8; 32]> {
        self.packages
            .iter()
            .find(|package| package.world_id() == world && package.preferred)
            .map(|package| package.reviewed_implementation)
    }

    /// The reviewed id *and* the version its descriptor declares — the pair a
    /// catch-up needs to decide whether this build is ahead of the Space.
    pub fn reviewed_state(&self, world: &WorldId) -> Option<([u8; 32], u32)> {
        self.packages
            .iter()
            .find(|package| package.world_id() == world && package.preferred)
            .map(|package| (package.reviewed_implementation, package.reviewed_version()))
    }

    pub fn founder_policies(
        &self,
    ) -> anyhow::Result<Vec<(WorldId, [u8; 32], u32, Vec<FounderGrant>)>> {
        self.packages
            .iter()
            .filter(|package| package.preferred)
            .filter_map(|package| {
                package.lifecycle.as_deref().map(|lifecycle| {
                    lifecycle.founder_grants().map(|grants| {
                        (
                            package.world.clone(),
                            package.reviewed_implementation,
                            package.reviewed_version(),
                            grants,
                        )
                    })
                })
            })
            .collect()
    }

    pub fn initial_scopes(&self, display_name: &str) -> Vec<(WorldId, InitialScope)> {
        self.packages
            .iter()
            .filter(|package| package.preferred)
            .filter_map(|package| {
                package.lifecycle.as_deref().and_then(|lifecycle| {
                    lifecycle
                        .initial_scope(display_name)
                        .map(|scope| (package.world.clone(), scope))
                })
            })
            .collect()
    }

    pub fn lifecycle_world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.packages
            .iter()
            .filter(|package| package.preferred && package.lifecycle.is_some())
            .map(WorldPackage::world_id)
    }

    pub fn bootstrap(&self, world: &WorldId, context: BootstrapContext<'_>) -> anyhow::Result<()> {
        let package = self
            .packages
            .iter()
            .find(|package| package.world_id() == world && package.preferred)
            .ok_or_else(|| anyhow::anyhow!("World '{world}' is not bundled"))?;
        let lifecycle = package
            .lifecycle
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("World '{world}' has no formation lifecycle"))?;
        lifecycle.bootstrap(context)
    }

    pub fn accepts_call(&self, call: &Call) -> bool {
        self.call_access(call).is_ok()
    }

    pub fn call_access(&self, call: &Call) -> Result<Access, Failure> {
        let control = self
            .packages
            .iter()
            .find(|package| package.world_id() == call.world() && package.preferred)
            .and_then(|package| package.control.as_deref())
            .ok_or_else(|| {
                Failure::new(
                    Code::UnsupportedOperation,
                    format!("World '{}' has no application call handler", call.world()),
                )
            })?;
        control.access(call)
    }

    /// Freeze the semantic registry and create one application host per
    /// registered World.
    pub fn build(&self) -> Result<(Catalog, WorldRouter), RegistrationRefusal> {
        let mut runtime = Builder::new();
        let mut hosts = Vec::with_capacity(self.packages.len());
        let mut preferred = BTreeMap::new();
        for package in &self.packages {
            let descriptor = package.implementation.descriptor();
            package
                .exec
                .validate(
                    &package.world,
                    &package.reviewed_implementation,
                    &descriptor,
                )
                .map_err(|reason| RegistrationRefusal::InvalidExecPackage {
                    world: package.world.clone(),
                    reason,
                })?;
            hosts.push((
                package.world.clone(),
                package.reviewed_implementation,
                package.reviewed_version(),
                package.control.clone(),
                package.exec.clone(),
                package.projector.clone(),
            ));
            if package.preferred
                && preferred
                    .insert(package.world.clone(), package.reviewed_implementation)
                    .is_some()
            {
                return Err(RegistrationRefusal::AmbiguousWorldDefault(
                    package.world.clone(),
                ));
            }
            runtime = runtime.register_reviewed(
                package.implementation.clone(),
                package.reviewed_implementation,
            );
        }
        for world in self
            .packages
            .iter()
            .map(WorldPackage::world_id)
            .collect::<std::collections::BTreeSet<_>>()
        {
            if !preferred.contains_key(world) {
                return Err(RegistrationRefusal::AmbiguousWorldDefault(world.clone()));
            }
        }
        let registry = runtime.build()?;
        Ok((
            registry,
            WorldRouter::new(
                hosts
                    .into_iter()
                    .map(|(world, reviewed, version, control, exec, projector)| {
                        (
                            (world.clone(), reviewed),
                            WorldHost::new(world, reviewed, version, control, exec, projector),
                        )
                    })
                    .collect(),
                preferred,
            ),
        ))
    }
}

/// The sole product-side entrance to one World in one active Space.
///
/// Primary and sponsored-agent Sessions cannot be reused across Worlds because
/// each host owns only the Sessions docked to its own [`WorldId`].
pub struct WorldHost {
    world: WorldId,
    reviewed_implementation: [u8; 32],
    /// The version the reviewed descriptor declares. Carried beside the id
    /// because a `WorldHost` never sees the `World` itself, so it cannot go
    /// back to the descriptor to ask.
    reviewed_version: u32,
    control: Option<Arc<dyn Handler>>,
    exec: runtime::exec::Package,
    projector: Option<Arc<dyn ObservationProjector>>,
    primary_session: Mutex<Option<Arc<Session>>>,
    agent_sessions: Mutex<HashMap<DeviceId, Session>>,
}

impl std::fmt::Debug for WorldHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldHost")
            .field("world", &self.world)
            .finish_non_exhaustive()
    }
}

impl WorldHost {
    fn new(
        world: WorldId,
        reviewed_implementation: [u8; 32],
        reviewed_version: u32,
        control: Option<Arc<dyn Handler>>,
        exec: runtime::exec::Package,
        projector: Option<Arc<dyn ObservationProjector>>,
    ) -> Self {
        Self {
            world,
            reviewed_version,
            reviewed_implementation,
            control,
            exec,
            projector,
            primary_session: Mutex::new(None),
            agent_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world
    }

    pub fn reviewed_implementation(&self) -> &[u8; 32] {
        &self.reviewed_implementation
    }

    /// The version this build declares for the World it hosts.
    pub fn reviewed_version(&self) -> u32 {
        self.reviewed_version
    }

    pub fn control(&self) -> Option<&dyn Handler> {
        self.control.as_deref()
    }

    pub fn exec(&self) -> &runtime::exec::Package {
        &self.exec
    }

    /// Observe committed unresolved Runs and perform one local Attempt pass.
    pub fn perform(
        &self,
        session: &Session,
        put_output: impl FnMut(&[u8]) -> Result<replica::content::ContentRef, runtime::world::Failure>,
    ) -> Result<runtime::exec::PerformReport, runtime::world::Failure> {
        session.perform(&self.exec, put_output)
    }

    /// Ensure the Space's primary identity has a Session for this World.
    ///
    /// Docking remains lazy: an unadmitted joiner can keep its StationHost
    /// active to drive Contact before Mechanics grants it standing.
    pub fn ensure_primary(
        &self,
        station: &Station,
        identity: &LocalIdentity,
    ) -> Result<(), RuntimeFailure> {
        let mut session = self.primary_session.lock_recovering();
        if session.is_none() {
            *session = Some(Arc::new(station.dock(&self.world, identity)?));
        }
        Ok(())
    }

    pub fn with_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        let session = self.primary_session.lock_recovering().clone();
        session.as_ref().map(|session| f(session.as_ref()))
    }

    /// Dock or reuse a sponsored local agent's Session, then run `f` with it.
    pub fn with_agent<R>(
        &self,
        station: &Station,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, RuntimeFailure> {
        let mut sessions = self.agent_sessions.lock_recovering();
        let device = identity.device().clone();
        if !sessions.contains_key(&device) {
            sessions.insert(device.clone(), station.dock(&self.world, identity)?);
        }
        sessions
            .get(&device)
            .map(f)
            .ok_or_else(|| RuntimeFailure::UnknownWorld(self.world.clone()))
    }
}

/// The World hosts enabled inside one active StationHost.
pub struct WorldRouter {
    hosts: BTreeMap<(WorldId, [u8; 32]), WorldHost>,
    preferred: BTreeMap<WorldId, [u8; 32]>,
}

impl std::fmt::Debug for WorldRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldRouter")
            .field("worlds", &self.preferred.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl WorldRouter {
    fn new(
        hosts: BTreeMap<(WorldId, [u8; 32]), WorldHost>,
        preferred: BTreeMap<WorldId, [u8; 32]>,
    ) -> Self {
        Self { hosts, preferred }
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.preferred.keys()
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.preferred.contains_key(world)
    }

    pub fn host(&self, world: &WorldId) -> Option<&WorldHost> {
        let active = self
            .hosts
            .iter()
            .filter(|((candidate, _), _)| candidate == world)
            .find_map(|(_, host)| {
                host.primary_session
                    .lock_recovering()
                    .is_some()
                    .then_some(host)
            });
        active.or_else(|| self.preferred_host(world))
    }

    pub fn host_for(&self, world: &WorldId, implementation: [u8; 32]) -> Option<&WorldHost> {
        self.hosts.get(&(world.clone(), implementation))
    }

    pub fn preferred_host(&self, world: &WorldId) -> Option<&WorldHost> {
        self.host_for(world, *self.preferred.get(world)?)
    }

    pub fn reviewed_implementations(&self) -> impl Iterator<Item = (&WorldId, &[u8; 32])> {
        self.preferred
            .iter()
            .map(|(world, implementation)| (world, implementation))
    }

    /// Every hosted World's reviewed id with the version beside it.
    pub fn reviewed_states(&self) -> impl Iterator<Item = (&WorldId, [u8; 32], u32)> {
        self.preferred.iter().filter_map(|(world, implementation)| {
            let host = self.host_for(world, *implementation)?;
            Some((
                world,
                *host.reviewed_implementation(),
                host.reviewed_version(),
            ))
        })
    }

    pub fn ensure_primary(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<(), RuntimeFailure> {
        let implementation = station.active_implementation(world, identity)?;
        self.host_for(world, implementation)
            .ok_or_else(|| RuntimeFailure::UnknownWorld(world.clone()))?
            .ensure_primary(station, identity)
    }

    pub fn with_primary<R>(&self, world: &WorldId, f: impl FnOnce(&Session) -> R) -> Option<R> {
        self.host(world)?.with_primary(f)
    }

    /// Run `f` with any docked primary Session.
    ///
    /// Station observations and authority doorbells are shared across Worlds,
    /// so Space-level adapters need exactly one Session to publish that plane.
    pub fn with_any_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        for host in self.hosts.values() {
            let session = host.primary_session.lock_recovering().clone();
            if let Some(session) = session {
                return Some(f(session.as_ref()));
            }
        }
        None
    }

    pub fn start_projectors(&self, space: &mechanics::ids::SpaceId) {
        for host in self.hosts.values() {
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host.primary_session.lock_recovering().clone();
            if let Some(session) = session {
                projector.start(session.as_ref(), space);
            }
        }
    }

    pub fn status(&self) -> Option<StatusProjection> {
        let mut combined: Option<StatusProjection> = None;
        for host in self.hosts.values() {
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host.primary_session.lock_recovering().clone();
            // A host with nothing to say is skipped, never propagated: with a
            // second bundled World this loop visits hosts that hold no session
            // on this Space at all (Signage on a board-only Space), and an
            // early `?` here let that silence erase the answer of the World
            // that had one — the joiner's board synced and its status still
            // read "no board data", which the join diagnosis renders as a
            // sync that never completes.
            let Some(status) = session
                .as_ref()
                .and_then(|session| projector.status(session.as_ref()))
            else {
                continue;
            };
            match &mut combined {
                Some(total) => {
                    total.items = total.items.saturating_add(status.items);
                    total.scopes = total.scopes.saturating_add(status.scopes);
                    if total.name.is_empty() {
                        total.name = status.name;
                    }
                    if total.description.is_empty() {
                        total.description = status.description;
                    }
                }
                None => combined = Some(status),
            }
        }
        combined
    }

    /// Fan one Observation out to every hosted World, preserving the World
    /// boundary around each package's vocabulary.
    pub fn project(
        &self,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Vec<RoutedInvalidation> {
        let mut projected = Vec::new();
        for host in self.hosts.values() {
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host.primary_session.lock_recovering().clone();
            let Some(session) = session else {
                continue;
            };
            let next = projector.project(session.as_ref(), space, observation);
            if !next.dirty.is_empty() || !next.planes.is_empty() {
                projected.push(RoutedInvalidation {
                    world: host.world.clone(),
                    dirty: next.dirty,
                    planes: next.planes,
                });
            }
        }
        projected
    }

    pub fn with_agent<R>(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, RuntimeFailure> {
        let implementation = station.active_implementation(world, identity)?;
        self.host_for(world, implementation)
            .ok_or_else(|| RuntimeFailure::UnknownWorld(world.clone()))?
            .with_agent(station, identity, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::body::Schema;
    use replica::body::WorldId;
    use runtime::{
        world::Context as WorldContext, world::Effect, world::Intent, world::Projection,
        world::Query,
    };

    struct NoopWorld {
        id: WorldId,
        schemas: Vec<Schema>,
        exec_specs: Vec<runtime::exec::Spec>,
    }

    struct ProjectControl;

    impl Handler for ProjectControl {
        fn access(&self, call: &Call) -> Result<Access, Failure> {
            if call.operation() == "projects.control" && call.version() == 1 {
                Ok(Access::Query)
            } else {
                Err(Failure::new(
                    Code::UnsupportedOperation,
                    "unsupported project call",
                ))
            }
        }

        fn call(&self, call: &Call, _context: &super::Context<'_>) -> Reply {
            Reply::ok(call, b"{}".to_vec())
        }
    }

    impl World for NoopWorld {
        fn id(&self) -> WorldId {
            self.id.clone()
        }

        fn schemas(&self) -> &[Schema] {
            &self.schemas
        }

        fn exec_specs(&self) -> &[runtime::exec::Spec] {
            &self.exec_specs
        }

        fn submit(
            &self,
            _ctx: &mut WorldContext<'_>,
            _intent: Intent,
        ) -> Result<Effect, runtime::world::Rejection> {
            unreachable!("registry tests never execute the World")
        }

        fn query(
            &self,
            _ctx: &WorldContext<'_>,
            _query: Query,
        ) -> Result<Projection, runtime::world::Rejection> {
            unreachable!("registry tests never execute the World")
        }
    }

    fn package(id: &str, marker: u8) -> (Arc<dyn World>, [u8; 32]) {
        let id = WorldId::parse(id).expect("test World id");
        let schemas = Vec::new();
        (
            Arc::new(NoopWorld {
                id,
                schemas,
                exec_specs: Vec::new(),
            }),
            [marker; 32],
        )
    }

    fn exec_demand(capability: &str) -> Vec<u8> {
        mechanics::authorization::AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("com.example.jobs", capability),
            mechanics::authorization::Resource::root("com.example.jobs"),
        )
        .encode_canonical()
        .unwrap()
    }

    fn exec_spec() -> runtime::exec::Spec {
        let payload = |name: &str| runtime::exec::PayloadSpec {
            schema: runtime::exec::SchemaRef {
                name: replica::body::SchemaId::parse(name).unwrap(),
                version: 1,
            },
            max_inline_bytes: 1_024,
            max_content_refs: 0,
            max_content_bytes: 0,
            read: exec_demand("payload.read"),
            max_additional_input_bytes: 0,
        };
        runtime::exec::Spec {
            name: replica::body::SchemaId::parse("job").unwrap(),
            version: 1,
            access: runtime::exec::Access {
                start: exec_demand("start"),
                offer: exec_demand("offer"),
                control: exec_demand("control"),
                accept: exec_demand("accept"),
            },
            input: payload("job.input"),
            output: payload("job.output"),
            mode: runtime::exec::Mode::Unary,
            resume: runtime::exec::Resume::Restart,
            effects: runtime::exec::Effects::Pure,
            accept: runtime::exec::AcceptRule::World,
            queries: Vec::new(),
            service: None,
            links: Vec::new(),
            limits: runtime::exec::Limits {
                attempts: 2,
                events: 32,
                checkpoints: 0,
                child_runs: 1,
                progress_bytes: 1_024,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
        }
    }

    struct CheckHandler {
        binding: runtime::exec::HandlerBinding,
    }

    impl runtime::exec::Handler for CheckHandler {
        fn binding(&self) -> &runtime::exec::HandlerBinding {
            &self.binding
        }

        fn handle(
            &self,
            _context: &mut runtime::exec::Context<'_>,
        ) -> Result<runtime::exec::Candidate, runtime::exec::Failure> {
            Ok(runtime::exec::Candidate {
                output: runtime::exec::SchemaRef {
                    name: replica::body::SchemaId::parse("job.output").unwrap(),
                    version: 1,
                },
                inline: Vec::new(),
                content: Vec::new(),
                content_bytes: 0,
                terminal: runtime::exec::TerminalClass::Succeeded,
                usage: Vec::new(),
                evidence: Vec::new(),
            })
        }
    }

    fn executable_package() -> (Arc<dyn World>, [u8; 32], runtime::exec::Package) {
        let world = WorldId::parse("com.example.jobs").unwrap();
        let reviewed = [0x77; 32];
        let spec = exec_spec();
        let seed = [0x78; 32];
        let build = runtime::exec::Build {
            id: runtime::exec::BuildId::from_bytes([0; 32]),
            world: world.clone(),
            world_build: reviewed,
            spec: runtime::exec::SchemaRef {
                name: spec.name.clone(),
                version: spec.version,
            },
            handler: replica::content::ContentRef {
                content_id: [0x79; 32],
            },
            dependencies: None,
            environment: [0x7a; 32],
            config: Vec::new(),
            checkpoint: None,
            replay_commands: None,
            compatible_from: Vec::new(),
            publisher: mechanics::ids::ActorId::from_incept_hash(&"a".repeat(64)),
            signature: runtime::exec::Signature {
                signer: mechanics::actor::device_from_seed(&seed),
                algorithm: 1,
                bytes: [0; 64],
            },
        }
        .sign(&seed)
        .unwrap();
        let handler = Arc::new(CheckHandler {
            binding: runtime::exec::HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: build.handler,
                role: None,
                links: Vec::new(),
            },
        });
        let exec = runtime::exec::Package::new()
            .with_spec(spec.clone())
            .with_build(build)
            .with_handler(handler);
        (
            Arc::new(NoopWorld {
                id: world,
                schemas: Vec::new(),
                exec_specs: vec![spec],
            }),
            reviewed,
            exec,
        )
    }

    #[test]
    fn one_space_has_one_host_per_registered_world() {
        let a = package("com.example.files", 1);
        let b = package("com.example.notes", 2);
        let (registry, hosts) = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1))
            .with_package(WorldPackage::new(b.0, b.1))
            .build()
            .unwrap();

        let ids: Vec<_> = hosts.world_ids().map(WorldId::as_str).collect();
        assert_eq!(ids, ["com.example.files", "com.example.notes"]);
        assert_eq!(registry.len(), 2);
        assert_eq!(
            hosts
                .reviewed_implementations()
                .map(|(id, implementation)| (id.as_str(), implementation[0]))
                .collect::<Vec<_>>(),
            [("com.example.files", 1), ("com.example.notes", 2)]
        );
        assert_ne!(
            hosts
                .host(&WorldId::parse("com.example.files").unwrap())
                .unwrap() as *const WorldHost,
            hosts
                .host(&WorldId::parse("com.example.notes").unwrap())
                .unwrap() as *const WorldHost,
            "each World must have a distinct host object"
        );
    }

    #[test]
    fn multiple_current_defaults_for_one_world_are_rejected() {
        let a = package("com.example.files", 1);
        let b = package("com.example.files", 2);
        let err = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1))
            .with_package(WorldPackage::new(b.0, b.1))
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            RegistrationRefusal::AmbiguousWorldDefault(
                WorldId::parse("com.example.files").expect("test World id")
            )
        );
    }

    #[test]
    fn historical_and_preferred_packages_are_exact_and_default_is_explicit() {
        let old = package("com.example.files", 1);
        let current = package("com.example.files", 2);
        let (registry, hosts) = WorldPackages::new()
            .with_package(WorldPackage::new(old.0, old.1).historical())
            .with_package(WorldPackage::new(current.0, current.1))
            .build()
            .unwrap();
        let world = WorldId::parse("com.example.files").unwrap();

        assert!(registry.world_for(&world, [1; 32]).is_some());
        assert!(registry.world_for(&world, [2; 32]).is_some());
        assert_eq!(
            hosts
                .preferred_host(&world)
                .unwrap()
                .reviewed_implementation(),
            &[2; 32]
        );
        assert!(hosts.host_for(&world, [1; 32]).is_some());
        assert!(hosts.host_for(&world, [2; 32]).is_some());
    }

    #[test]
    fn call_handlers_are_owned_by_the_registered_package() {
        let files = package("com.example.files", 1);
        let notes = package("com.example.notes", 2);
        let control = Arc::new(ProjectControl);
        let packages = WorldPackages::new()
            .with_package(WorldPackage::new(files.0, files.1))
            .with_package(WorldPackage::new(notes.0, notes.1).with_control(control));
        let files = WorldId::parse("com.example.files").unwrap();
        let notes = WorldId::parse("com.example.notes").unwrap();

        let call = Call::new(notes.clone(), "projects.control", 1, Vec::new()).unwrap();
        assert!(packages.accepts_call(&call));
        let (_, hosts) = packages.build().unwrap();
        assert!(hosts.host(&files).unwrap().control().is_none());
        assert!(hosts.host(&notes).unwrap().control().is_some());
    }

    #[test]
    fn executable_packages_are_validated_and_retained_by_the_world_host() {
        let (world, reviewed, exec) = executable_package();
        let world_id = world.id();
        let (_, hosts) = WorldPackages::new()
            .with_package(WorldPackage::new(world, reviewed).with_exec(exec))
            .build()
            .unwrap();

        let installed = hosts.host(&world_id).unwrap().exec();
        assert_eq!(installed.specs().len(), 1);
        assert_eq!(installed.builds().len(), 1);
        assert_eq!(installed.handlers().len(), 1);
    }

    #[test]
    fn a_world_exec_declaration_without_its_application_package_is_refused() {
        let (world, reviewed, _) = executable_package();
        let err = WorldPackages::new()
            .with_package(WorldPackage::new(world, reviewed))
            .build()
            .unwrap_err();

        assert_eq!(
            err,
            RegistrationRefusal::InvalidExecPackage {
                world: WorldId::parse("com.example.jobs").unwrap(),
                reason: runtime::exec::PackageInvalid::SpecRegistrationMismatch,
            }
        );
    }
}
