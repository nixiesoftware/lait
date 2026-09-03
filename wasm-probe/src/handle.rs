//! The shippable packaging boundary: one `#[wasm_bindgen]` entry the viewer's
//! Worker calls to stand the whole browser engine up, and a handle it holds
//! across JS calls to drive it. Everything below this line is the composition
//! the `tests/dispatch.rs` and `tests/space_call.rs` claims proved; this module
//! is only its `#[wasm_bindgen]` skin, so the packaging risk — a handle holding
//! non-`Send` `Rc`/`RemoteClient` state surviving a return to JS and a later
//! call back in — is settled by this compiling and answering, not asserted.
//!
//! World-agnostic by construction: the World identity the runner is told
//! (`world`/`version`/`release`) and the serving `mount` are *inputs*, supplied
//! by the bootstrap that read them from the Library row and the pull params.
//! Nothing here names a World. A local copy (`local_issues`) and a release
//! (`com.lait.issues`) reach `boot` the same way, differing only in the strings
//! the caller passes.

use std::cell::RefCell;
use std::sync::Arc;

use comms::Transport;
use contact::authority::SharedLedgerAuthority;
use contact::pull::{pull_receive, Deadlines};
use mechanics::actor::device_from_seed;
use mechanics::ids::SpaceId;
use mechanics::station::{Epoch, Key};
use replica::transaction::{CommitContext, SeedSigner};
use runtime::world::{AuthorityView, Builder, World};
use runtime::{AffectedWorldPublication, ObservationStream};
use wasm_bindgen::prelude::*;
use world_runner::wasm_abi::GuestInit;
use world_sdk::{RemoteClient, RemoteWorld};

use crate::dispatch::BrowserEngine;
use crate::runner::{WebInstance, WebModule};
use crate::space_pull::{pull_space, unhex32};

/// A composed browser engine, owned by JS across the Worker's lifetime. Holds
/// the dispatch engine (the read/write/control seam) and, alongside it, the
/// live `Station` and the transport the pull stood on — so the same handle that
/// answers a frame can also install a peer's converged material (`repull`),
/// which is what makes a tab reflect a write it did not make.
#[wasm_bindgen]
pub struct BrowserEngineHandle {
    engine: BrowserEngine,
    station: runtime::browser::Station,
    transport: Arc<dyn Transport>,
    responder: Key,
    seed: [u8; 32],
    authority: SharedLedgerAuthority,
    space: SpaceId,
    /// The observe stream this engine's docked Session was opened on, drained
    /// one record at a time by the events lane. `RefCell` because `try_next`
    /// mutates the cursor and JS holds the handle by shared reference — sound
    /// on a single-threaded Worker, where no two calls overlap.
    ring: RefCell<ObservationStream>,
    /// The editor lane's open sessions, same borrow discipline as the ring:
    /// taken per call, released before any dispatch into the engine.
    sessions: RefCell<crate::session::SessionHost>,
    /// The tab's Live-plane session to the responder, for carets/presence.
    /// `None` when the peer's Live plane could not be joined at boot — carets
    /// are then simply absent, never a boot failure. Its methods take `&self`
    /// (interior mutability), so no borrow spans an await.
    live: Option<crate::live_client::LiveClient>,
    /// The World identity, for building the `BodyKey` a caret anchor names.
    world_id: replica::body::WorldId,
}

/// Fold any error into a `JsValue` the Worker sees as a rejected Promise or a
/// thrown value — the browser's own failure channel, not a Rust panic.
fn js_err(context: &str, error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

/// One doorbell frame in the viewer's own `SpaceDoorbell` wire shape
/// (`viewer/src/types.ts`), built from an [`runtime::Observation`]. A ring is a
/// dirty flag, never state: the consumer re-reads on any ring. `publications`
/// and `change` are the daemon's own wire types, so serializing them here is
/// byte-identical to what the head sends — no second grammar to drift.
#[derive(serde::Serialize)]
struct BrowserRing<'a> {
    space: &'a str,
    epoch: u64,
    seq: u64,
    reset: bool,
    /// Always empty in a tab. The per-scope routing that fills this is a
    /// hosted-World projection the daemon owns; the doorbell consumer
    /// (`doorbell.ts`) re-reads on any ring regardless — invalidations only
    /// retire optimistic guesses — so an empty set is honest, just coarser.
    invalidations: [(); 0],
    publications: &'a [AffectedWorldPublication],
    change: &'a runtime::change::DurableChange,
    authority_advanced: bool,
    activity_advanced: bool,
    presence_advanced: bool,
}

#[wasm_bindgen]
impl BrowserEngineHandle {
    /// Answer one decoded frame the viewer's `workerLink` sent, as a JSON
    /// string in and a JSON string out — the frame vocabulary the daemon-backed
    /// link already speaks, so the Worker glue is a `postMessage` relay and
    /// nothing more. The one-shot rpc verbs answer with a reply frame; the
    /// streaming lanes (events / abort / close) carry no synchronous answer and
    /// come back as JSON `null`, for the Worker composition root to manage.
    #[wasm_bindgen(js_name = handleLink)]
    pub fn handle_link(&self, frame_json: &str) -> Result<String, JsValue> {
        let request =
            serde_json::from_str(frame_json).map_err(|e| js_err("the frame does not decode", e))?;
        match self.engine.handle_link(request) {
            Some(response) => serde_json::to_string(&response)
                .map_err(|e| js_err("the response does not encode", e)),
            None => Ok("null".to_string()),
        }
    }

    /// Answer one decoded session frame the viewer's `workerSession` sent —
    /// the editor lane, JSON string in, a JSON ARRAY of response frames out
    /// (one request can owe zero or several: `open` answers a liveness event,
    /// `watch`/`close` answer nothing, `mutate` answers one reply). The
    /// Worker's glue routes frames whose `lait` tag starts with `session:`
    /// here and everything else to [`Self::handle_link`], and relays each
    /// frame of the answered array as its own `postMessage`.
    #[wasm_bindgen(js_name = handleSession)]
    pub fn handle_session(&self, frame_json: &str) -> Result<String, JsValue> {
        let request = serde_json::from_str(frame_json)
            .map_err(|e| js_err("the session frame does not decode", e))?;
        // Take, decide, drop: the borrow is released before any engine
        // dispatch, the same discipline the ring keeps.
        let accepted = self.sessions.borrow_mut().accept(request);
        let responses = match accepted {
            crate::session::Accepted::Respond(responses) => responses,
            crate::session::Accepted::Mutate { sid, rid, request } => {
                vec![crate::session::mutate_reply(&self.engine, sid, rid, request)]
            }
        };
        serde_json::to_string(&responses)
            .map_err(|e| js_err("the session response does not encode", e))
    }

    /// Publish this tab's caret into an issue's field over the Live plane — the
    /// send half of live carets. The viewer gives an issue reff, a field, and a
    /// `u64` cursor position; the tab resolves the world-specific body id
    /// through the runner (`transient_body`, world-agnostic), mints a
    /// `fabric::Anchor` for the position against the same pinned publication its
    /// reads answer from, and sends it as a datagram bound to the Live session.
    /// `false` when the tab holds no Live session, the position is not
    /// anchorable, or the datagram did not fit — carets are best-effort by
    /// design. The subscribe is sent once per field, then steady-state carets
    /// are pure datagram sends.
    #[wasm_bindgen(js_name = publishCaret)]
    pub async fn publish_caret(
        &self,
        issue: String,
        field: String,
        position: u32,
    ) -> Result<bool, JsValue> {
        let Some(live) = self.live.as_ref() else {
            return Ok(false);
        };
        let body = self
            .engine
            .transient_body(&issue)
            .map_err(|e| js_err("the caret's body could not be resolved", e))?;
        let key = replica::body::BodyKey::new(
            self.world_id.clone(),
            replica::body::BodyId::from_bytes(body),
        );
        let anchor = self
            .station
            .anchor(&key, &field, position as u64)
            .map_err(|e| js_err("the caret anchor could not be minted", format!("{e:?}")))?
            .ok_or_else(|| JsValue::from_str("the caret position is not anchorable"))?;
        let scope = runtime::transient::Target::Field {
            world: self.world_id.as_str().to_string(),
            body,
            field,
        };
        if !live.is_subscribed(&scope) {
            live.subscribe(vec![scope.clone()])
                .await
                .map_err(|e| js_err("the caret scope could not be subscribed", e))?;
        }
        Ok(live.publish(
            scope,
            runtime::transient::TransientPayload::Caret {
                anchor: anchor.encode(),
            },
        ))
    }

    /// Converge with the responder over the same transport: pull its material
    /// AND push this tab's own excess on the same connection (symmetric
    /// convergence), then install what arrived into the live core through the
    /// Station's own writer. The push is what carries a tab's write OUT —
    /// nothing dials a tab, so it pushes on the dial it makes; an old responder
    /// without `RECIPROCAL_CONVERGE` never receives it and this is a plain pull.
    /// Idempotent by convergence. Called both on a poll and right after a local
    /// write (push-at-commit). Returns the bytes moved.
    pub async fn repull(&self) -> Result<u32, JsValue> {
        let holdings = self.station.published_root();
        // Build this tab's excess to push alongside the pull.
        let excess = self
            .station
            .export_excess(&self.seed, &self.authority.bundle())
            .map_err(|e| js_err("the tab could not build its push", e))?;
        let received = pull_receive(
            self.transport.as_ref(),
            &self.responder,
            &self.space,
            &self.seed,
            &self.authority.bundle(),
            holdings,
            Some(excess),
            Deadlines::default(),
        )
        .await
        .map_err(|e| js_err("the live converge failed", format!("{e:?}")))?;
        let bundle = self.authority.bundle();
        let signer = SeedSigner(&self.seed);
        self.station
            .with_replica_convergence(|replica| {
                let ctx = CommitContext {
                    space: &self.space,
                    signer: &signer,
                    authority_frontier: (bundle.frontier)(),
                };
                let mut incorporator = bundle
                    .incorporator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let validated = replica.validate_contact(
                    &received.staged,
                    bundle.source.as_ref(),
                    &mut *incorporator,
                )?;
                replica.incorporate_bundle(&ctx, validated, bundle.source.as_ref())
            })
            .map_err(|e| js_err("the live install failed", format!("{e:?}")))?;
        Ok(received.bytes_moved as u32)
    }

    /// Drain the next doorbell frame, non-blocking: a JSON `SpaceDoorbell`
    /// string when a record is pending, `undefined` when the stream is caught
    /// up. The events lane polls this — after a `repull` that moved bytes, and
    /// after a local write — and the Worker relays each frame as a `ring` to
    /// the viewer's `workerLink`. A ring is a dirty flag; the consumer re-reads.
    /// The first drain after boot is a `reset` record, which rebaselines.
    #[wasm_bindgen(js_name = drainRing)]
    pub fn drain_ring(&self) -> Result<Option<String>, JsValue> {
        let next = self
            .ring
            .borrow_mut()
            .try_next()
            .map_err(|e| js_err("the observe stream went dormant", format!("{e:?}")))?;
        match next {
            Some(observation) => {
                let ring = BrowserRing {
                    space: self.space.as_str(),
                    epoch: observation.epoch.as_u64(),
                    seq: observation.sequence,
                    reset: observation.reset,
                    invalidations: [],
                    publications: &observation.publications,
                    change: &observation.change,
                    authority_advanced: observation.authority,
                    // No activity/presence plane in a tab, and the consumer
                    // reads neither; authority news is the one true flag.
                    activity_advanced: false,
                    presence_advanced: observation.authority,
                };
                serde_json::to_string(&ring)
                    .map(Some)
                    .map_err(|e| js_err("the ring does not encode", e))
            }
            None => Ok(None),
        }
    }
}

/// Stand the whole engine up in the Worker: compile the runner once, instantiate
/// the three nested layers, pull the Space over the relay onto OPFS, compose the
/// daemon's own Station over what arrived, dock the member device, and wire the
/// world-agnostic dispatch engine on top. The 39 MiB runner arrives as `bytes`,
/// not linked — served same-origin as a `.wasm` asset. Returns a handle JS
/// keeps for the tab's life.
///
/// `world` / `version` / `release` are the identity the runner is told, and
/// `mount` is the serving mount — all supplied by the caller, never a literal,
/// which is what keeps this world-agnostic.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub async fn boot(
    relay: String,
    seed_hex: String,
    ticket: String,
    runner_wasm: Vec<u8>,
    world: String,
    version: String,
    release: String,
    mount: String,
) -> Result<BrowserEngineHandle, JsValue> {
    let seed = unhex32(&seed_hex);

    // The runner, compiled once and instantiated per nested guest layer — a
    // wasm instance cannot be re-entered mid-callback, so the three activations
    // (client execute → control handler → session query) each need their own.
    let module = WebModule::compile(&runner_wasm);
    let init = GuestInit {
        world: world.clone(),
        version,
        release,
    };
    let launch = |tag: &str| -> Result<Arc<RemoteWorld>, JsValue> {
        let instance = WebInstance::launch_from(&module, init.clone())
            .map_err(|e| js_err(&format!("the {tag} runner instance"), e))?;
        RemoteWorld::connect_runner(Box::new(instance))
            .map(Arc::new)
            .map_err(|e| js_err(&format!("the {tag} runner connect"), e))
    };

    let world_runner = launch("world")?;
    let implementation = world_runner.reviewed_implementation();
    let world_id = world_runner.descriptor().id.clone();
    let registry = Builder::new()
        .register_reviewed(world_runner, implementation)
        .build()
        .map_err(|e| js_err("the runner's contract does not register", format!("{e:?}")))?;

    let control = launch("control")?;
    let client_runner = launch("client")?;

    // The pull, schemas declared on the fresh Replica before incorporation.
    let pulled = pull_space(&relay, seed, &ticket, |replica| {
        runtime::browser::declare_schemas(replica, &registry);
    })
    .await;

    // Resolve the caller's actor/device from the pulled ledger before it moves
    // into the composed Station.
    let device = device_from_seed(&seed);
    let ledger = pulled.authority.clone();
    let authority_view = runtime::browser::LedgerAuthorityView(pulled.authority.clone());
    let actor = authority_view
        .resolve(&device)
        .map(|resolution| resolution.actor.as_str().to_string())
        .ok_or_else(|| JsValue::from_str("the pulled ledger does not admit this device"))?;

    let transport = pulled.transport.clone();
    let responder = pulled.responder.clone();
    let space = pulled.space.clone();

    let station = runtime::browser::Station::compose(
        pulled.space.clone(),
        pulled.replica,
        Arc::new(authority_view),
        registry,
        Epoch::from_u64(1),
    )
    .map_err(|e| js_err("the browser Station does not compose", format!("{e:?}")))?;
    let identity = runtime::browser::Station::identity_from_seed(&seed);
    let session = station
        .dock(&world_id, &identity)
        .map_err(|e| js_err("the member does not dock", format!("{e:?}")))?;

    // Open the observe stream on the docked Session before it moves into the
    // engine — the stream holds its own Arc to the broadcaster, so it outlives
    // this borrow and sees every later commit and re-pull convergence.
    let ring = RefCell::new(session.observe(None));

    // A client DESCRIBE settles the client instance before it backs the adapter.
    let _ = RemoteClient::connect(client_runner.clone())
        .map_err(|e| js_err("the client runner does not answer DESCRIBE", e))?;

    let engine = BrowserEngine::new(
        session,
        control,
        client_runner,
        identity,
        actor,
        device.as_str().to_string(),
        ledger.clone(),
        space.as_str().to_string(),
        mount,
    )
    .map_err(|e| js_err("the browser engine does not compose", format!("{e:?}")))?;

    // Join the responder's Live plane for carets/presence — best-effort: a peer
    // whose Live plane is down (or an old peer without one) leaves carets
    // absent, never a boot failure. The tab dials the SAME peer the Contact
    // pull dialed, admitted the same way.
    let local_station = mechanics::station::Key::from_device(&device);
    let live = match local_station {
        Some(local) => {
            crate::live_client::LiveClient::connect(transport.as_ref(), &space, &local, &responder)
                .await
                .ok()
        }
        None => None,
    };

    Ok(BrowserEngineHandle {
        engine,
        station,
        transport,
        responder,
        seed,
        authority: ledger,
        space,
        ring,
        sessions: RefCell::new(crate::session::SessionHost::default()),
        live,
        world_id,
    })
}
