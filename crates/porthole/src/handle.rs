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
    /// The serving mount — the id the viewer keys its selected Space on
    /// (`ServedSpaceRow.id`), and therefore the id a doorbell ring must carry so
    /// the viewer routes it to the current Space. The daemon carries its orbit id
    /// here for the same reason; the tab's analog is the mount, NOT the raw
    /// `SpaceId` (`self.space`), which the viewer never uses as a selection key.
    mount: String,
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
                vec![crate::session::mutate_reply(
                    &self.engine,
                    sid,
                    rid,
                    request,
                )]
            }
        };
        serde_json::to_string(&responses)
            .map_err(|e| js_err("the session response does not encode", e))
    }

    /// Publish this tab's caret into an issue's field over the Live plane — the
    /// send half of live carets. The viewer gives an issue reff, a field, and a
    /// `u64` cursor position; the tab resolves the world-specific body id
    /// through the runner (`transient_body`, world-agnostic), mints a
    /// `fabric::Anchor` for the position against the LIVE Replica (the pinned
    /// publication a daemon anchors against is never built on wasm), and sends
    /// it as a datagram bound to the Live session.
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

    /// Drive carets from the viewer's own `session:watch` question — the real
    /// frame the editor sends (`{issue, cursor:{field, anchor}}`), rather than a
    /// bespoke call. When the question carries a cursor over an issue, the tab
    /// publishes that caret; a question with no cursor (just watching) publishes
    /// nothing. This is the send half's integration point; the Worker relays the
    /// viewer's `session:watch` here.
    #[wasm_bindgen(js_name = watchCaret)]
    pub async fn watch_caret(&self, question_json: &str) -> Result<bool, JsValue> {
        let question: serde_json::Value = serde_json::from_str(question_json)
            .map_err(|e| js_err("the watch question does not decode", e))?;
        let (Some(issue), Some(cursor)) = (
            question.get("issue").and_then(serde_json::Value::as_str),
            question.get("cursor"),
        ) else {
            return Ok(false);
        };
        let (Some(field), Some(position)) = (
            cursor.get("field").and_then(serde_json::Value::as_str),
            cursor.get("anchor").and_then(serde_json::Value::as_u64),
        ) else {
            return Ok(false);
        };
        self.publish_caret(issue.to_string(), field.to_string(), position as u32)
            .await
    }

    /// Drain the next caret a peer published to this tab, as the viewer's own
    /// `{kind:"live", …}` `SocketEvent` payload — the receive half. Awaits the
    /// next datagram, resolves its anchor to a position against the live core,
    /// and shapes one `LiveEntry` in the exact wire form the daemon's socket
    /// sends, so the viewer's facepile/caret UI draws it with no branch on which
    /// backend answered. `None` when the Live session has ended. The `sid` is
    /// the Worker's to attach when it wraps this in a `session:event` for the
    /// watching session — one Live connection can answer several sessions.
    ///
    /// Received carets are the responder's own local editors: the daemon
    /// publishes only its local presence (it does not relay other peers'), so
    /// the author is the responder's actor. A future daemon that relays would
    /// carry the station per item.
    #[wasm_bindgen(js_name = drainCaret)]
    pub async fn drain_caret(&self) -> Result<Option<String>, JsValue> {
        let Some(live) = self.live.as_ref() else {
            return Ok(None);
        };
        let Some((origin, item)) = live.next_item().await else {
            return Ok(None);
        };
        let Some(entry) = self.live_entry(origin, &item)? else {
            // A payload kind this tab does not surface (residency/preview).
            return Ok(None);
        };
        // `issue: null` is the honest whole-table answer: one Live connection
        // may carry carets for several issues, and the Worker attaches the exact
        // reff a given watching session asked for when it wraps this.
        let event = serde_json::json!({
            "kind": "live",
            "space": self.space.as_str(),
            "issue": serde_json::Value::Null,
            "view": { "kind": "live", "generation": 0, "partial": false, "entries": [entry] },
        });
        serde_json::to_string(&event)
            .map(Some)
            .map_err(|e| js_err("the live event does not encode", e))
    }

    /// One received transient item as the viewer's `LiveEntry` — resolving a
    /// caret/selection anchor to a position against the live core, in the exact
    /// wire shape `hosting::live_entry` builds on the daemon. `None` for a kind
    /// the tab does not surface.
    ///
    /// `origin` is the caret's true author when a supporter relayed it (its
    /// station key); `None` means the responder this tab dialed is the author (a
    /// bare, un-relayed item). Attributing to the origin is what lets one tab draw
    /// ANOTHER tab's caret rather than mislabel every peer as the supporter.
    fn live_entry(
        &self,
        origin: Option<[u8; 32]>,
        item: &runtime::transient::TransientItem,
    ) -> Result<Option<serde_json::Value>, JsValue> {
        use runtime::transient::TransientPayload;
        let author = match origin {
            Some(bytes) => Key::from_key_bytes(bytes),
            None => self.responder.clone(),
        };
        let actor = runtime::browser::LedgerAuthorityView(self.authority.clone())
            .resolve(&author.as_device())
            .map(|resolution| resolution.actor.as_str().to_string());
        let Some(actor) = actor else {
            return Ok(None);
        };
        let (kind, caret, focus) = match &item.payload {
            TransientPayload::Presence => {
                ("presence", serde_json::Value::Null, serde_json::Value::Null)
            }
            TransientPayload::Typing => {
                ("typing", serde_json::Value::Null, serde_json::Value::Null)
            }
            TransientPayload::Caret { anchor } => (
                "caret",
                self.caret_position(&item.scope, anchor),
                serde_json::Value::Null,
            ),
            TransientPayload::Selection { anchor, focus } => (
                "selection",
                self.caret_position(&item.scope, anchor),
                self.caret_position(&item.scope, focus),
            ),
            // Preview and residency are not drawn as carets here.
            TransientPayload::Preview { .. } | TransientPayload::Residency { .. } => {
                return Ok(None)
            }
        };
        Ok(Some(serde_json::json!({
            "actor": actor,
            "scope": live_scope(&item.scope),
            "kind": kind,
            "age_ms": 0,
            "uncertain": false,
            "caret": caret,
            "focus": focus,
        })))
    }

    /// Resolve a peer's caret anchor to a `CaretPosition` against the live
    /// core — `at` a position, or `drifted` when the material it named is gone.
    fn caret_position(
        &self,
        scope: &runtime::transient::Target,
        anchor_bytes: &[u8],
    ) -> serde_json::Value {
        let (Some(field), Some(body)) = (scope_field(scope), scope_body_id(scope)) else {
            return serde_json::json!({ "caret": "unresolved" });
        };
        let _ = field;
        let key = replica::body::BodyKey::new(
            self.world_id.clone(),
            replica::body::BodyId::from_bytes(body),
        );
        let Ok(anchor) = fabric::Anchor::decode_canonical(anchor_bytes) else {
            return serde_json::json!({ "caret": "unresolved" });
        };
        match self.station.resolve_anchor(&key, &anchor) {
            Ok(fabric::AnchorResolution::Resolved(position)) => {
                serde_json::json!({ "caret": "at", "position": position })
            }
            Ok(fabric::AnchorResolution::Drifted) => serde_json::json!({ "caret": "drifted" }),
            Err(_) => serde_json::json!({ "caret": "unresolved" }),
        }
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
        // Steady-state deadlines, not the ENTER pull's 120s/20s: this runs on a
        // ~2s poll, so a stalled dial must give up in seconds and yield the
        // single Worker thread to the next attempt (the keep-alive keeps the
        // path warm; a fresh dial re-establishes it) rather than pinning the
        // worker for 20s and stacking repulls behind it.
        let deadlines = Deadlines {
            whole: std::time::Duration::from_secs(6),
            progress: std::time::Duration::from_secs(3),
        };
        let received = pull_receive(
            self.transport.as_ref(),
            &self.responder,
            &self.space,
            &self.seed,
            &self.authority.bundle(),
            holdings,
            Some(excess),
            deadlines,
        )
        .await
        .map_err(|e| js_err("the live converge failed", format!("{e:?}")))?;
        let bundle = self.authority.bundle();
        let signer = SeedSigner(&self.seed);
        // Authority can advance INSIDE convergence (validate_contact incorporates
        // a revocation/admission that arrived over Contact), so measure the
        // frontier around the whole call, exactly as the native driver does.
        let authority_before = (bundle.frontier)();
        let outcome = self
            .station
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
        // Ring the doorbell for what arrived. `with_replica_convergence` only
        // incorporates and rebuilds publications; publishing the Observation is
        // the caller's job (the native Contact driver does it too), and without
        // it a pulled peer edit reaches the Replica but the viewer never re-reads
        // — the change stays invisible until the next boot.
        let authority_advanced = (self.authority.bundle().frontier)() != authority_before;
        self.station
            .publish_convergence(&outcome, authority_advanced);
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
                    // The mount, not the raw SpaceId: the viewer keys its
                    // selected Space on `ServedSpaceRow.id` (= the mount), and its
                    // doorbell drops any ring whose `space` is not that key. The
                    // daemon carries its orbit id here for the same reason.
                    space: self.mount.as_str(),
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
        mount.clone(),
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
        mount,
    })
}

/// The base32 render of a Body id — the form the tree renders Body ids in, and
/// the form the viewer's `LiveScope.body` carries.
fn render_body(body: &[u8; 16]) -> String {
    replica::body::BodyId::from_bytes(*body).render()
}

/// The field a scope names, when it names one.
fn scope_field(scope: &runtime::transient::Target) -> Option<&str> {
    use runtime::transient::Target;
    match scope {
        Target::Field { field, .. }
        | Target::Preview { field, .. }
        | Target::Typing { field, .. } => Some(field),
        _ => None,
    }
}

/// The Body id a scope names, when it names one.
fn scope_body_id(scope: &runtime::transient::Target) -> Option<[u8; 16]> {
    use runtime::transient::Target;
    match scope {
        Target::Body { body, .. }
        | Target::Material { body, .. }
        | Target::Field { body, .. }
        | Target::Preview { body, .. }
        | Target::Typing { body, .. } => Some(*body),
        _ => None,
    }
}

/// One transient scope as the viewer's `LiveScope` JSON — the exact wire shape
/// `hosting::live_scope` builds, so a caret drawn in a tab is byte-identical to
/// one from the daemon.
fn live_scope(scope: &runtime::transient::Target) -> serde_json::Value {
    use runtime::transient::Target;
    match scope {
        Target::Body { world, body } => {
            serde_json::json!({ "scope": "issue_view", "world": world, "body": render_body(body) })
        }
        Target::Material { world, body } => serde_json::json!({
            "scope": "document_view", "world": world, "body": render_body(body)
        }),
        Target::Field { world, body, field } => serde_json::json!({
            "scope": "text_caret", "world": world, "body": render_body(body), "field": field
        }),
        Target::Preview { world, body, field } => serde_json::json!({
            "scope": "text_preview", "world": world, "body": render_body(body), "field": field
        }),
        Target::Typing { world, body, field } => serde_json::json!({
            "scope": "typing", "world": world, "body": render_body(body), "field": field
        }),
        Target::Content { content } => {
            let hex: String = content.iter().map(|b| format!("{b:02x}")).collect();
            serde_json::json!({ "scope": "content_residency", "content": hex })
        }
        Target::World { world, schema, key } => serde_json::json!({
            "scope": "custom_world", "world": world, "schema": schema, "key": key
        }),
    }
}
