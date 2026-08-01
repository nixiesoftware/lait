//! Product-side hosts for the Worlds installed in one active Station.
//!
//! [`runtime::world::Catalog`] owns the immutable semantic implementations used
//! by a Station. This module owns the application-side half of that boundary:
//! a compile-time [`WorldPackage`] for each product, and one [`WorldHost`] per
//! package inside an active Space. A package carries the reviewed semantic
//! implementation plus its optional product-neutral call handler; orbital code
//! never needs to name the product behind either one.
//!
//! A [`WorldRouter`] belongs to one active Station. It is not a process,
//! does not own a listener, and has no autonomous background loop.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use mechanics::ids::DeviceId;
use replica::body::WorldId;
use runtime::world::Refusal as RegistrationRefusal;
use runtime::Error as RuntimeFailure;
use runtime::{
    world::Builder, world::Catalog, world::LocalIdentity, world::World, Session, Station,
};

use runtime::world::call::{Access, Call, Code, Failure, Handler};
#[cfg(test)]
use runtime::world::call::{Context, Reply};

/// Product-owned projection of a generic runtime Observation into the local
/// invalidations understood by the shell protocol.
#[derive(Debug, Default)]
pub struct Invalidation {
    pub dirty_by_project: Vec<issues::dto::DirtyProject>,
    pub dirty_catalog: Vec<crate::control::CatalogScope>,
}

pub struct StatusProjection {
    pub items: usize,
    pub groups: usize,
    pub name: String,
    pub description: String,
}

/// A World package's Observation projector. Implementations own their own
/// baselines; the Station host only fans generic observations out.
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

/// One product package available to the application build.
#[derive(Clone)]
pub struct WorldPackage {
    world: WorldId,
    implementation: Arc<dyn World>,
    reviewed_implementation: [u8; 32],
    control: Option<Arc<dyn Handler>>,
    projector: Option<Arc<dyn ObservationProjector>>,
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
            projector: None,
        }
    }

    pub fn with_control(mut self, control: Arc<dyn Handler>) -> Self {
        self.control = Some(control);
        self
    }

    pub fn with_projector(mut self, projector: Arc<dyn ObservationProjector>) -> Self {
        self.projector = Some(projector);
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
        self.packages.iter().map(WorldPackage::world_id)
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.packages
            .iter()
            .any(|package| package.world_id() == world)
    }

    pub fn accepts_call(&self, call: &Call) -> bool {
        self.call_access(call).is_ok()
    }

    pub fn call_access(&self, call: &Call) -> Result<Access, Failure> {
        let control = self
            .packages
            .iter()
            .find(|package| package.world_id() == call.world())
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
        for package in &self.packages {
            hosts.push((
                package.world.clone(),
                package.reviewed_implementation,
                package.control.clone(),
                package.projector.clone(),
            ));
            runtime = runtime.register(package.implementation.clone());
        }
        let registry = runtime.build()?;
        Ok((
            registry,
            WorldRouter::new(
                hosts
                    .into_iter()
                    .map(|(world, reviewed, control, projector)| {
                        (
                            world.clone(),
                            WorldHost::new(world, reviewed, control, projector),
                        )
                    })
                    .collect(),
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
    control: Option<Arc<dyn Handler>>,
    projector: Option<Arc<dyn ObservationProjector>>,
    primary_session: Mutex<Option<Session>>,
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
        control: Option<Arc<dyn Handler>>,
        projector: Option<Arc<dyn ObservationProjector>>,
    ) -> Self {
        Self {
            world,
            reviewed_implementation,
            control,
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

    pub fn control(&self) -> Option<&dyn Handler> {
        self.control.as_deref()
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
        let mut session = self
            .primary_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if session.is_none() {
            *session = Some(station.dock(&self.world, identity)?);
        }
        Ok(())
    }

    pub fn with_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        let session = self
            .primary_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        session.as_ref().map(f)
    }

    /// Dock or reuse a sponsored local agent's Session, then run `f` with it.
    pub fn with_agent<R>(
        &self,
        station: &Station,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, RuntimeFailure> {
        let mut sessions = self
            .agent_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    hosts: BTreeMap<WorldId, WorldHost>,
}

impl std::fmt::Debug for WorldRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldRouter")
            .field("worlds", &self.hosts.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl WorldRouter {
    fn new(hosts: BTreeMap<WorldId, WorldHost>) -> Self {
        Self { hosts }
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.hosts.keys()
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.hosts.contains_key(world)
    }

    pub fn host(&self, world: &WorldId) -> Option<&WorldHost> {
        self.hosts.get(world)
    }

    pub fn reviewed_implementations(&self) -> impl Iterator<Item = (&WorldId, &[u8; 32])> {
        self.hosts
            .iter()
            .map(|(world, host)| (world, host.reviewed_implementation()))
    }

    pub fn ensure_primary(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<(), RuntimeFailure> {
        self.host(world)
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
            let session = host
                .primary_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(session) = session.as_ref() {
                return Some(f(session));
            }
        }
        None
    }

    pub fn start_projectors(&self, space: &mechanics::ids::SpaceId) {
        for host in self.hosts.values() {
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host
                .primary_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(session) = session.as_ref() {
                projector.start(session, space);
            }
        }
    }

    pub fn status(&self) -> Option<StatusProjection> {
        for host in self.hosts.values() {
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host
                .primary_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(status) = session
                .as_ref()
                .and_then(|session| projector.status(session))
            {
                return Some(status);
            }
        }
        None
    }

    pub fn project(
        &self,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Invalidation {
        let mut projected = Invalidation::default();
        for host in self.hosts.values() {
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host
                .primary_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(session) = session.as_ref() else {
                continue;
            };
            let next = projector.project(session, space, observation);
            projected.dirty_by_project.extend(next.dirty_by_project);
            projected.dirty_catalog.extend(next.dirty_catalog);
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
        self.host(world)
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
        (Arc::new(NoopWorld { id, schemas }), [marker; 32])
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
    fn duplicate_worlds_still_fail_through_the_runtime_registry_contract() {
        let a = package("com.example.files", 1);
        let b = package("com.example.files", 2);
        let err = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1))
            .with_package(WorldPackage::new(b.0, b.1))
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            RegistrationRefusal::DuplicateWorld(
                WorldId::parse("com.example.files").expect("test World id")
            )
        );
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
}
