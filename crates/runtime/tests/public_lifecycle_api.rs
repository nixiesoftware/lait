//! C0.1 — the public lifecycle API freeze.
//!
//! Every target lifecycle method from `docs/plans/01-orbital-architecture.md`
//! is pinned here **by signature**: each binding below only compiles if the
//! method exists on the public surface with exactly the stated parameter and
//! result/error types. A signature drift is a compile error, not a silent
//! contract change.
//!
//! Two surfaces are documented incomplete until their completion packages land
//! (C2: `Station::neighbors`/`Station::contact` operational behavior; C3:
//! `Session::observe` streaming); their *signatures* are still frozen here so
//! the completion work implements the pinned shape rather than inventing one.

use runtime::contact::Failure as ContactFailure;
use runtime::lifecycle::{Exit, Failure as LifecycleFailure, Interruption};
use runtime::session::Failure as SessionFailure;
use runtime::{
    ActivationOptions, CommittedEffect, ContactOutcome, Intent, LocalIdentity, Neighbor,
    ObservationCursor, Orbit, OrbitStatus, Projection, Query, Rejection, RemovalConfirmation,
    RequestId, Runtime, Session, SignedWorldAction, Station,
};

use mechanics::{ids::SpaceId, station::Key};
use replica::ids::WorldId;

/// The frozen lifecycle surface. Each `let _: fn(..) -> ..` binding is a
/// compile-time assertion of the method's exact public signature.
#[test]
fn the_target_lifecycle_methods_have_their_frozen_signatures() {
    // Runtime
    let _: fn(&Runtime) -> Result<Orbit, LifecycleFailure> = Runtime::create;
    let _: fn(&Runtime, &runtime::SignedCoordinates) -> Result<Orbit, LifecycleFailure> =
        Runtime::materialize;
    let _: fn(&Runtime, &SpaceId) -> Result<Orbit, LifecycleFailure> = Runtime::acquire;
    let _: fn(&Runtime, &SpaceId) -> Result<OrbitStatus, LifecycleFailure> = Runtime::inspect;
    let _: fn(&Runtime) -> Vec<OrbitStatus> = Runtime::list;
    let _: fn(&[u8; 32]) -> LocalIdentity = Runtime::identity_from_seed;

    // Orbit
    let _: fn(Orbit, ActivationOptions) -> Result<Station, LifecycleFailure> = Station::open;
    let _: fn(Orbit, RemovalConfirmation) -> Result<(), LifecycleFailure> = Orbit::remove;

    // Station
    let _: fn(&Station, &WorldId, &LocalIdentity) -> Result<Session, LifecycleFailure> =
        Station::dock;
    let _: fn(&Station) -> Vec<Neighbor> = Station::neighbors;
    let _: fn(&Station, &Key) -> Result<ContactOutcome, ContactFailure> = Station::contact;
    let _: fn(Station) -> Result<Orbit, Interruption> = Station::vacate;
    let _: fn(Station) -> Exit = Station::wait;

    // Action signing + Session
    let _: fn() -> RequestId = RequestId::mint;
    let _: fn(&LocalIdentity, &Session, RequestId, Intent) -> Result<SignedWorldAction, Rejection> =
        LocalIdentity::sign_action;
    let _: fn(&Session, SignedWorldAction) -> Result<CommittedEffect, SessionFailure> =
        Session::submit;
    let _: fn(&Session, Query) -> Result<Projection, SessionFailure> = Session::query;
    let _: fn(&Session, Option<ObservationCursor>) -> runtime::ObservationStream = Session::observe;
    let _: fn(Session) = Session::close;
}

/// The stable error taxonomies backing those results are public, cloneable,
/// comparable types (so callers can match on them and tests can assert them).
#[test]
fn the_error_taxonomies_are_stable_typed_categories() {
    fn assert_error<E: std::error::Error + Clone + PartialEq + Send + Sync + 'static>() {}
    assert_error::<LifecycleFailure>();
    assert_error::<Interruption>();
    assert_error::<ContactFailure>();
    assert_error::<Rejection>();
    assert_error::<SessionFailure>();

    // The unknown-World dock failure is a stable typed category (C0.1), not a
    // stage-progress placeholder.
    let world = WorldId::parse("dev.example.none").unwrap();
    let err = LifecycleFailure::UnknownWorld(world.clone());
    assert_eq!(err, LifecycleFailure::UnknownWorld(world));
}
