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
#[cfg(feature = "proof")]
use runtime::world::Catalog;
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
    /// The same registry the Station composed over, kept so a snapshot restore
    /// declares the identical schema set on its fresh Replica. Convergence
    /// classifies at import and never reinterprets, so a restore that declares
    /// a different set (or none) retains every body opaquely.
    #[cfg(feature = "proof")]
    registry: Catalog,
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
        // A structural write — a new project, a milestone, a rename, a member
        // change — reaches the engine through this lane and touches a catalog or
        // an authority plane the tab cannot route a per-plane doorbell for. So
        // bracket the call with BOTH frontiers: the replica frontier moves on a
        // World/catalog write, the authority frontier on a membership or grant
        // change. If either advanced, the write was real, and a coarse RESET
        // re-reads every active resource — sidebar, roster, and all — keeping the
        // app alive. A query (or a refused write) moves neither and rings
        // nothing; a collaborative edit takes the session lane, not this one.
        let replica_before = self.station.frontier();
        let authority_before = (self.authority.bundle().frontier)();
        let answer = match self.engine.handle_link(request) {
            Some(response) => serde_json::to_string(&response)
                .map_err(|e| js_err("the response does not encode", e)),
            None => Ok("null".to_string()),
        };
        let replica_moved = replica_before.is_some() && self.station.frontier() != replica_before;
        let authority_moved = (self.authority.bundle().frontier)() != authority_before;
        if replica_moved || authority_moved {
            self.station.ring_reset(authority_moved);
        }
        answer
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
        // Subscribe the whole issue (Body) AND this field together: the Body scope
        // is what makes a supporter relay OTHER peers' carets in this issue back to
        // this tab (a passive reader watches the Body), and the Field scope is what
        // lets the supporter accept THIS tab's own published caret. One set, so an
        // editor both speaks and hears.
        if !live.is_subscribed(&scope) {
            let body_scope = runtime::transient::Target::Body {
                world: self.world_id.as_str().to_string(),
                body,
            };
            live.subscribe(vec![body_scope, scope.clone()])
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

    /// Drive the realtime lane from the viewer's own `session:watch` question:
    /// `{issue, cursor?:{field,anchor}, preview?:{field,base,result,index,delete,
    /// insert,anchor?,focus?}}`.
    ///
    /// Subscribes the whole issue (a `Body` scope) so a supporter relays peers'
    /// carets AND previews back here even when this tab is only watching, plus this
    /// tab's own caret field and preview field so the supporter accepts what IT
    /// publishes — one union, because a subscribe replaces the set. Then publishes
    /// this tab's caret (a minted anchor) and its preview (a scalar splice from a
    /// durable base). The preview is the INSTANT text lane a peer sees in
    /// milliseconds — the durable text follows over Contact underneath. This is the
    /// send half's integration point; the Worker relays every `session:watch` here.
    #[wasm_bindgen(js_name = watchCaret)]
    pub async fn watch_caret(&self, question_json: &str) -> Result<bool, JsValue> {
        use runtime::transient::{Target, TextPreview, TransientPayload};
        let question: serde_json::Value = serde_json::from_str(question_json)
            .map_err(|e| js_err("the watch question does not decode", e))?;
        let Some(issue) = question.get("issue").and_then(serde_json::Value::as_str) else {
            return Ok(false);
        };
        let Some(live) = self.live.as_ref() else {
            return Ok(false);
        };
        let body = self
            .engine
            .transient_body(issue)
            .map_err(|e| js_err("the watched body could not be resolved", e))?;
        let world = self.world_id.as_str().to_string();

        let cursor = question.get("cursor");
        let caret_field = cursor
            .and_then(|c| c.get("field").and_then(serde_json::Value::as_str))
            .map(str::to_string);
        let preview = question.get("preview");
        let preview_field = preview
            .and_then(|p| p.get("field").and_then(serde_json::Value::as_str))
            .map(str::to_string);

        // The subscription UNION: the issue (to hear peers' carets and previews),
        // plus our own caret and preview fields (so the supporter accepts what we
        // publish). One set, because a subscribe replaces — a passive viewer gets
        // just the Body, an editor gets all three.
        let mut scopes = vec![Target::Body {
            world: world.clone(),
            body,
        }];
        if let Some(field) = &caret_field {
            scopes.push(Target::Field {
                world: world.clone(),
                body,
                field: field.clone(),
            });
        }
        if let Some(field) = &preview_field {
            scopes.push(Target::Preview {
                world: world.clone(),
                body,
                field: field.clone(),
            });
        }
        if scopes.iter().any(|scope| !live.is_subscribed(scope)) {
            live.subscribe(scopes)
                .await
                .map_err(|e| js_err("the live scopes could not be subscribed", e))?;
        }

        // Our caret: an anchor minted against the live Replica so it survives
        // concurrent edits.
        if let (Some(field), Some(position)) = (
            caret_field,
            cursor.and_then(|c| c.get("anchor").and_then(serde_json::Value::as_u64)),
        ) {
            let key = replica::body::BodyKey::new(
                self.world_id.clone(),
                replica::body::BodyId::from_bytes(body),
            );
            if let Some(anchor) = self
                .station
                .anchor(&key, &field, position)
                .map_err(|e| js_err("the caret anchor could not be minted", format!("{e:?}")))?
            {
                live.publish(
                    Target::Field {
                        world: world.clone(),
                        body,
                        field,
                    },
                    TransientPayload::Caret {
                        anchor: anchor.encode(),
                    },
                );
            }
        }

        // Our preview: the instant text lane. Scalar offsets from a durable base,
        // copied straight through — no anchor minting, the receiver applies it
        // against the base/result revisions it carries.
        if let (Some(field), Some(p)) = (preview_field, preview) {
            if let (Some(base), Some(result), Some(index), Some(delete), Some(insert)) = (
                p.get("base").and_then(serde_json::Value::as_str),
                p.get("result").and_then(serde_json::Value::as_str),
                p.get("index").and_then(serde_json::Value::as_u64),
                p.get("delete").and_then(serde_json::Value::as_u64),
                p.get("insert").and_then(serde_json::Value::as_str),
            ) {
                live.publish(
                    Target::Preview { world, body, field },
                    TransientPayload::Preview {
                        preview: TextPreview {
                            base: base.to_string(),
                            result: result.to_string(),
                            index,
                            delete,
                            insert: insert.to_string(),
                            anchor: p.get("anchor").and_then(serde_json::Value::as_u64),
                            focus: p.get("focus").and_then(serde_json::Value::as_u64),
                        },
                    },
                );
            }
        }
        Ok(true)
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
    /// A received caret is attributed to its true author: a supporter that
    /// relays (`feature::PRESENCE_RELAY`, which this tab negotiates on connect)
    /// carries each item's origin station in a `RelayedPresence`, so `live_entry`
    /// draws ANOTHER peer's caret under that peer rather than under the supporter.
    /// A bare, un-relayed item (no origin) is the responder's own.
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
            // The mount, not the raw SpaceId: the viewer keys its live slots on the
            // Space id it selected (= the mount), and drops a live view whose space
            // is not that key — the same reason the doorbell ring carries the mount.
            "space": self.mount.as_str(),
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
        let null = serde_json::Value::Null;
        let (kind, caret, focus, preview) = match &item.payload {
            TransientPayload::Presence => ("presence", null.clone(), null.clone(), null.clone()),
            TransientPayload::Typing => ("typing", null.clone(), null.clone(), null.clone()),
            TransientPayload::Caret { anchor } => (
                "caret",
                self.caret_position(&item.scope, anchor),
                null.clone(),
                null.clone(),
            ),
            TransientPayload::Selection { anchor, focus } => (
                "selection",
                self.caret_position(&item.scope, anchor),
                self.caret_position(&item.scope, focus),
                null.clone(),
            ),
            // A preview is the INSTANT text lane: scalar offsets from a durable
            // base, copied straight through (no anchor resolution — the receiver
            // applies it against the `base`/`result` revisions it carries). This is
            // what a peer sees within milliseconds, before durable convergence.
            TransientPayload::Preview { preview } => (
                "preview",
                null.clone(),
                null.clone(),
                serde_json::json!({
                    "base": preview.base,
                    "result": preview.result,
                    "index": preview.index,
                    "delete": preview.delete,
                    "insert": preview.insert,
                    "anchor": preview.anchor,
                    "focus": preview.focus,
                }),
            ),
            // Residency is not a thing the viewer draws in the editor.
            TransientPayload::Residency { .. } => return Ok(None),
        };
        Ok(Some(serde_json::json!({
            "actor": actor,
            "scope": live_scope(&item.scope),
            "kind": kind,
            "age_ms": 0,
            "uncertain": false,
            "caret": caret,
            "focus": focus,
            "preview": preview,
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

    /// Capture the whole live Space — ledger and World — as a portable snapshot
    /// the Worker can persist to a bucket: the durability half of daemon-less
    /// hosting. Composes over the real pulled Space, so the ledger's sealed
    /// epoch keys ride along and a cold restore decrypts. The authority section
    /// is the FULL export every time; the store's own retention keeps the World
    /// half compact.
    pub fn snapshot(&self) -> Result<Vec<u8>, JsValue> {
        let guard = self.authority.0.lock().unwrap_or_else(|p| p.into_inner());
        let ledger = &guard.ledger;
        let genesis = ledger.genesis().clone();
        let frontier =
            replica::frontier::AuthorityFrontier::from_canonical_bytes(ledger.frontier());
        let founding = genesis
            .founding_actors
            .first()
            .cloned()
            .ok_or_else(|| js_err("snapshot", "the ledger names no founding actor"))?;
        let founder = ledger
            .actor_events()
            .into_iter()
            .find(|e| mechanics::ids::ActorId::from_incept_hash(&e.hash()) == founding)
            .ok_or_else(|| js_err("snapshot", "the founder inception is not on the ledger"))?;
        let founder_bytes =
            postcard::to_stdvec(&founder).map_err(|e| js_err("founder inception encode", e))?;
        let snapshot = self
            .station
            .with_replica_read(|replica| {
                contact::snapshot::capture(
                    &self.space,
                    &self.seed,
                    genesis.clone(),
                    founder_bytes.clone(),
                    ledger,
                    frontier.clone(),
                    replica,
                )
                .map_err(|f| {
                    replica::transaction::commit::Failure::Illegitimate(format!("{f:?}").into())
                })
            })
            .map_err(|e| js_err("capture snapshot", format!("{e:?}")))?;
        Ok(snapshot.encode())
    }

    /// The bucket object key this Space's snapshot lives at — a capability
    /// digest of the Space id, not the id itself (see
    /// [`contact::gateway::object_key`]). The Worker fetches `GET <bucket>/<key>`
    /// to bootstrap and derives the gateway path `PUT <gateway>/s/<basename>`
    /// from it, so nothing but the tab that already holds the Space id can
    /// locate its blob.
    #[wasm_bindgen(js_name = objectKey)]
    pub fn object_key(&self) -> String {
        contact::gateway::object_key(&self.space)
    }

    /// Capture the live Space and sign a write of it that replaces
    /// `expected_generation` — the publish half of the bucket loop. Returns the
    /// encoded [`contact::gateway::WriteEnvelope`] the Worker PUTs to the
    /// gateway. On a `412` the Worker re-reads the current generation and calls
    /// this again with it: the signature binds the generation, so a stale
    /// envelope cannot be replayed against a moved object.
    ///
    /// `expected_generation` is `0` for the very first publish (the object must
    /// not yet exist) and otherwise the generation the tab last saw — never a
    /// predicted one, because GCS generations are not sequential.
    #[wasm_bindgen(js_name = publishEnvelope)]
    pub fn publish_envelope(&self, expected_generation: u64) -> Result<Vec<u8>, JsValue> {
        let blob = self.snapshot()?;
        let request =
            contact::gateway::sign_write(&self.seed, &self.space, expected_generation, &blob);
        Ok(contact::gateway::WriteEnvelope { request, blob }.encode())
    }

    /// Absorb a snapshot the Worker downloaded from the bucket into the LIVE
    /// Space — the bootstrap/catch-up half. Idempotent and order-independent
    /// (see [`contact::snapshot::converge`]): absorbing an older or already-held
    /// blob changes nothing, absorbing a newer one merges it. Returns whether
    /// the replica frontier advanced, so the Worker can tell "the bucket had
    /// something new" from "the bucket was behind us".
    #[wasm_bindgen(js_name = absorbSnapshot)]
    pub fn absorb_snapshot(&self, snapshot_bytes: &[u8]) -> Result<bool, JsValue> {
        let snapshot = contact::snapshot::SpaceSnapshot::decode(snapshot_bytes)
            .map_err(|f| js_err("decode bucket snapshot", format!("{f:?}")))?;
        let outcome = self
            .station
            .with_replica_convergence(|replica| {
                contact::snapshot::converge(snapshot, &self.seed, replica, &self.authority).map_err(
                    |f| {
                        replica::transaction::commit::Failure::Illegitimate(format!("{f:?}").into())
                    },
                )
            })
            .map_err(|e| js_err("absorb bucket snapshot", format!("{e:?}")))?;
        Ok(outcome.advanced())
    }

    /// Prove the daemon-less round trip end to end (adversary R1): capture the
    /// live Space, cold-restore it into fresh in-memory storage with this
    /// device's seed, and DECRYPT every collaborative body on the restored
    /// replica. Each successful read means the sealed epoch key survived the
    /// snapshot and unsealed against the seed — the whole ledger → sealed-key →
    /// unseal → decrypt path a member cold-reload depends on. Returns
    /// `restored/total` decrypted-body counts; the caller asserts they match
    /// the live Space and are non-zero.
    ///
    /// The failure message carries the epoch geometry of all three — live
    /// authority, restored authority, and the snapshot's own material — because
    /// "restored 0 of 25" names nothing on its own. Its first reading blamed the
    /// epoch-key layer; the geometry showed one epoch, sealed to this device, in
    /// the restored keyring, and 25 bodies opaque anyway. What was actually
    /// missing was the schema declaration on the restored Replica. Keep the
    /// geometry: it is what separates "cannot decrypt" from "did not classify".
    /// Restore a snapshot the Worker downloaded from the bucket into FRESH
    /// in-memory storage and count how many collaborative bodies decrypt — the
    /// read half of the bucket acceptance proof (slice 4). The harness publishes
    /// this tab's Space through the real gateway, GETs the bytes back from the
    /// real bucket, and hands them here: a non-zero count matching the live
    /// Space is the whole write → gateway → bucket → read → decrypt path closing
    /// on infrastructure, not in memory.
    #[cfg(feature = "proof")]
    pub fn bootstrap_and_count(&self, snapshot_bytes: &[u8]) -> Result<String, JsValue> {
        let snapshot = contact::snapshot::SpaceSnapshot::decode(snapshot_bytes)
            .map_err(|f| js_err("decode downloaded snapshot", format!("{f:?}")))?;
        let (restored, _authority) = contact::snapshot::restore(
            snapshot,
            self.seed,
            std::sync::Arc::new(journal::MemMedium::new()),
            std::sync::Arc::new(journal::MemMedium::new()),
            |replica| runtime::browser::declare_schemas(replica, &self.registry),
        )
        .map_err(|f| js_err("restore downloaded snapshot", format!("{f:?}")))?;
        let live = self
            .station
            .with_replica_read(|replica| Ok(count_decrypted(replica)))
            .map_err(|e| js_err("read live replica", format!("{e:?}")))?;
        let restored_keys = restored.body_keys().len();
        let restored_decrypted = count_decrypted(&restored);
        if restored_decrypted != live || restored_decrypted == 0 {
            return Err(js_err(
                "bucket round trip",
                format!(
                    "downloaded snapshot decrypted {restored_decrypted} of {restored_keys}, live \
                     decrypts {live} — {}",
                    body_report(&restored)
                ),
            ));
        }
        Ok(format!(
            "downloaded from the bucket: {restored_decrypted}/{restored_keys} bodies decrypted, \
             matching the live Space"
        ))
    }

    #[cfg(feature = "proof")]
    pub fn verify_snapshot_roundtrip(&self) -> Result<String, JsValue> {
        let bytes = self.snapshot()?;
        let snapshot = contact::snapshot::SpaceSnapshot::decode(&bytes)
            .map_err(|f| js_err("decode snapshot", format!("{f:?}")))?;
        let me = device_from_seed(&self.seed);
        let snapshot_diag = snapshot_epoch_geometry(&snapshot, &me);
        let (restored, restored_authority) = contact::snapshot::restore(
            snapshot,
            self.seed,
            std::sync::Arc::new(journal::MemMedium::new()),
            std::sync::Arc::new(journal::MemMedium::new()),
            |replica| runtime::browser::declare_schemas(replica, &self.registry),
        )
        .map_err(|f| js_err("restore snapshot", format!("{f:?}")))?;

        let (live_decrypted, live_bodies) = self
            .station
            .with_replica_read(|replica| Ok((count_decrypted(replica), body_report(replica))))
            .map_err(|e| js_err("read live replica", format!("{e:?}")))?;
        let restored_keys = restored.body_keys().len();
        let restored_decrypted = count_decrypted(&restored);

        let diag = format!(
            "live[{} {live_bodies}] restored[{} {}] snapshot[{snapshot_diag}]",
            authority_epochs(&self.authority),
            authority_epochs(&restored_authority),
            body_report(&restored),
        );
        if restored_decrypted != live_decrypted || restored_decrypted == 0 {
            return Err(js_err(
                "snapshot round trip",
                format!(
                    "live decrypted {live_decrypted}, restored decrypted {restored_decrypted} of \
                     {restored_keys} — {diag}"
                ),
            ));
        }
        Ok(format!(
            "restored {restored_decrypted}/{restored_keys} bodies, decrypted, matching the live \
             Space — {diag}"
        ))
    }

    /// Prove the bucket's read-merge-write algebra (slice 2): given an OLDER
    /// snapshot `earlier` the harness captured before more material arrived,
    /// and the current Space as the newer one, show that absorbing them in
    /// either order lands the same decrypted state, and that re-absorbing held
    /// material changes nothing:
    ///
    /// - restore fresh from `earlier`, converge current in — forward: the
    ///   merged store decrypts exactly what the live Space does;
    /// - restore fresh from current, converge `earlier` in — backward: the
    ///   stale snapshot regresses nothing (the replica frontier is unmoved);
    /// - converge current into the forward store AGAIN — idempotent: unmoved.
    ///
    /// The harness makes `earlier` genuinely older by writing through the
    /// responder between the capture and this call; the report says how far
    /// apart the two snapshots were, so a run where nothing arrived (proving
    /// only idempotence) is legible as that and not as the full claim.
    #[cfg(feature = "proof")]
    pub fn verify_snapshot_converge(&self, earlier: &[u8]) -> Result<String, JsValue> {
        let older = contact::snapshot::SpaceSnapshot::decode(earlier)
            .map_err(|f| js_err("decode the earlier snapshot", format!("{f:?}")))?;
        let current_bytes = self.snapshot()?;
        let current = contact::snapshot::SpaceSnapshot::decode(&current_bytes)
            .map_err(|f| js_err("decode the current snapshot", format!("{f:?}")))?;
        let older_bodies = older.staged.bodies.len();
        let current_bodies = current.staged.bodies.len();

        let fresh = |snapshot: contact::snapshot::SpaceSnapshot| {
            contact::snapshot::restore(
                snapshot,
                self.seed,
                std::sync::Arc::new(journal::MemMedium::new()),
                std::sync::Arc::new(journal::MemMedium::new()),
                |replica| runtime::browser::declare_schemas(replica, &self.registry),
            )
            .map_err(|f| js_err("restore snapshot", format!("{f:?}")))
        };

        // Forward: old store absorbs the newer snapshot.
        let (mut forward, forward_authority) = fresh(older.clone())?;
        contact::snapshot::converge(
            current.clone(),
            &self.seed,
            &mut forward,
            &forward_authority,
        )
        .map_err(|f| js_err("converge newer into older", format!("{f:?}")))?;

        // Backward: new store absorbs the stale snapshot — nothing may move.
        let (mut backward, backward_authority) = fresh(current.clone())?;
        let regress =
            contact::snapshot::converge(older, &self.seed, &mut backward, &backward_authority)
                .map_err(|f| js_err("converge older into newer", format!("{f:?}")))?;
        if regress.advanced() {
            return Err(js_err(
                "snapshot converge",
                "a STALE snapshot moved the newer store's frontier — the merge is not monotonic",
            ));
        }

        // Idempotent: the forward store absorbs the same snapshot again.
        let again =
            contact::snapshot::converge(current, &self.seed, &mut forward, &forward_authority)
                .map_err(|f| js_err("re-converge the same snapshot", format!("{f:?}")))?;
        if again.advanced() {
            return Err(js_err(
                "snapshot converge",
                "re-absorbing an already-held snapshot moved the frontier — the merge is not \
                 idempotent",
            ));
        }

        let live_decrypted = self
            .station
            .with_replica_read(|replica| Ok(count_decrypted(replica)))
            .map_err(|e| js_err("read live replica", format!("{e:?}")))?;
        let forward_decrypted = count_decrypted(&forward);
        let backward_decrypted = count_decrypted(&backward);
        if forward_decrypted != live_decrypted
            || backward_decrypted != live_decrypted
            || live_decrypted == 0
        {
            return Err(js_err(
                "snapshot converge",
                format!(
                    "orders disagree: live decrypts {live_decrypted}, older⊔newer \
                     {forward_decrypted}, newer⊔older {backward_decrypted} — \
                     forward[{}] backward[{}]",
                    body_report(&forward),
                    body_report(&backward)
                ),
            ));
        }
        Ok(format!(
            "both orders decrypt {live_decrypted} bodies matching the live Space; a stale \
             snapshot moves nothing; re-absorption moves nothing (earlier snapshot carried \
             {older_bodies} bodies, current {current_bodies})"
        ))
    }
}

/// How many of a replica's collaborative bodies read back decrypted — a body
/// that will not `read_collaborative` is one whose epoch key this authority
/// cannot open.
#[cfg(feature = "proof")]
fn count_decrypted(replica: &replica::Replica) -> usize {
    replica
        .body_keys()
        .iter()
        .filter(|key| replica.read_collaborative(key).is_ok())
        .count()
}

/// First four bytes of an epoch id, as the diagnostic's short name for it.
#[cfg(feature = "proof")]
fn short_epoch(id: &[u8; 16]) -> String {
    id[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// One authority's epoch geometry: the keyring it can open, the ACL's
/// authorized epoch set, and which epochs the ledger holds sealed envelopes
/// for (marking this device's own). The three sets side by side are what
/// decides whether the epoch-key-history gap is a retention problem (fix in
/// the ledger) or a capture problem (re-seal at capture).
#[cfg(feature = "proof")]
fn authority_epochs(authority: &SharedLedgerAuthority) -> String {
    let mut inner = authority.0.lock().unwrap_or_else(|p| p.into_inner());
    let me = inner.me.clone();
    let keyring: Vec<String> = inner.keyring.keys().map(short_epoch).collect();
    let acl: Vec<String> = match inner.ledger.acl_state() {
        Ok(state) => {
            let mut epochs = state.epochs();
            epochs.sort_by_key(|e| (e.gen, e.id));
            epochs
                .iter()
                .map(|e| format!("{}#g{}", short_epoch(&e.id), e.gen))
                .collect()
        }
        Err(f) => vec![format!("unreplayable:{f:?}")],
    };
    let mut per_epoch: std::collections::BTreeMap<[u8; 16], (usize, bool)> =
        std::collections::BTreeMap::new();
    for bytes in inner.ledger.export_sealed() {
        if let Ok(rec) = mechanics::authorization::SealedKeyRecord::decode(&bytes) {
            let entry = per_epoch.entry(rec.epoch).or_insert((0, false));
            entry.0 += 1;
            entry.1 |= rec.device == me;
        }
    }
    let sealed: Vec<String> = per_epoch
        .iter()
        .map(|(id, (n, mine))| {
            format!(
                "{}x{}{}",
                short_epoch(id),
                n,
                if *mine { "+me" } else { "" }
            )
        })
        .collect();
    format!(
        "keyring=[{}] acl=[{}] sealed=[{}]",
        keyring.join(","),
        acl.join(","),
        sealed.join(",")
    )
}

/// The epochs the snapshot's material actually references: per-epoch artifact
/// counts from every body transaction's descriptors, beside the sealed
/// envelopes the authority section carries (marking this device's own).
#[cfg(feature = "proof")]
fn snapshot_epoch_geometry(
    snapshot: &contact::snapshot::SpaceSnapshot,
    me: &mechanics::ids::DeviceId,
) -> String {
    use contact::authority::AuthorityRecord;
    let mut body_epochs: std::collections::BTreeMap<[u8; 16], usize> =
        std::collections::BTreeMap::new();
    let mut sealed: std::collections::BTreeMap<[u8; 16], (usize, bool)> =
        std::collections::BTreeMap::new();
    for record in &snapshot.staged.authority_records {
        // Canonical transaction decode first: it requires exact re-encode
        // equality, so it cannot false-positive on an AuthorityRecord, while
        // postcard's tolerant enum decode could mistake a transaction.
        if let Ok(tx) = replica::transaction::Transaction::decode_canonical(record) {
            for descriptor in &tx.core.descriptors {
                for reference in descriptor.artifact_refs() {
                    *body_epochs.entry(reference.epoch).or_insert(0) += 1;
                }
            }
            continue;
        }
        if let Some(AuthorityRecord::SealedKey(bytes)) = AuthorityRecord::decode(record) {
            if let Ok(rec) = mechanics::authorization::SealedKeyRecord::decode(&bytes) {
                let entry = sealed.entry(rec.epoch).or_insert((0, false));
                entry.0 += 1;
                entry.1 |= rec.device == *me;
            }
        }
    }
    let body: Vec<String> = body_epochs
        .iter()
        .map(|(id, n)| format!("{}x{}", short_epoch(id), n))
        .collect();
    let sealed: Vec<String> = sealed
        .iter()
        .map(|(id, (n, mine))| {
            format!(
                "{}x{}{}",
                short_epoch(id),
                n,
                if *mine { "+me" } else { "" }
            )
        })
        .collect();
    format!(
        "body_epochs=[{}] sealed=[{}]",
        body.join(","),
        sealed.join(",")
    )
}

/// Per-body presence classes plus how many read back collaboratively — the
/// split that distinguishes "imported opaque" from "readable but not
/// collaborative" at a glance.
#[cfg(feature = "proof")]
fn body_report(replica: &replica::Replica) -> String {
    let keys = replica.body_keys();
    let mut opaque = 0usize;
    let mut collab_ok = 0usize;
    for key in &keys {
        if replica.is_opaque(key) {
            opaque += 1;
        }
        if replica.read_collaborative(key).is_ok() {
            collab_ok += 1;
        }
    }
    format!(
        "bodies={} opaque={opaque} collab_ok={collab_ok}",
        keys.len()
    )
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
    .await?;

    compose_over(
        pulled,
        registry,
        control,
        client_runner,
        world_id,
        seed,
        mount,
    )
    .await
}

/// Compose the Station, engine, and handle over a Space that is already stood
/// up — the shared tail of every entry, whether the Space arrived by a pull
/// ([`boot`]) or was minted in the tab ([`found`]). Everything World-specific
/// (the runners, the registry) is settled by the caller; this is the
/// composition the `tests/dispatch.rs`/`tests/space_call.rs` claims proved.
async fn compose_over(
    pulled: crate::space_pull::PulledSpace,
    registry: runtime::world::Catalog,
    control: Arc<RemoteWorld>,
    client_runner: Arc<RemoteWorld>,
    world_id: replica::body::WorldId,
    seed: [u8; 32],
    mount: String,
) -> Result<BrowserEngineHandle, JsValue> {
    // Resolve the caller's actor/device from the ledger before it moves into
    // the composed Station.
    let device = device_from_seed(&seed);
    let ledger = pulled.authority.clone();
    let authority_view = runtime::browser::LedgerAuthorityView(pulled.authority.clone());
    let actor = authority_view
        .resolve(&device)
        .map(|resolution| resolution.actor.as_str().to_string())
        .ok_or_else(|| JsValue::from_str("the ledger does not admit this device"))?;

    let transport = pulled.transport.clone();
    let responder = pulled.responder.clone();
    let space = pulled.space.clone();

    // Cloned only where the proof's restore needs to declare the same schema
    // set later; the shipped build hands the registry straight over.
    #[cfg(feature = "proof")]
    let kept_registry = registry.clone();
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

    // Join the responder's Live plane for carets/presence — best-effort, but
    // RETRIED: the Live session rides the same relay the ENTER pull does, and a
    // single transient failure at boot would otherwise leave carets/presence
    // absent for the WHOLE session (nothing retries this connect elsewhere) — the
    // likeliest reason a room shows no cursors even when convergence works. A few
    // short attempts clear the same transient relay/peer-presence race the pull
    // loop rides. A founder with no responder, or a peer whose Live plane is
    // genuinely down, still ends at None — carets absent, never a boot failure.
    let local_station = mechanics::station::Key::from_device(&device);
    let live = match local_station {
        Some(local) => {
            let mut client = None;
            for attempt in 0..5u32 {
                match crate::live_client::LiveClient::connect(
                    transport.as_ref(),
                    &space,
                    &local,
                    &responder,
                )
                .await
                {
                    Ok(connected) => {
                        client = Some(connected);
                        break;
                    }
                    Err(_) if attempt + 1 < 5 => {
                        n0_future::time::sleep(n0_future::time::Duration::from_millis(500)).await;
                    }
                    Err(_) => {}
                }
            }
            client
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
        #[cfg(feature = "proof")]
        registry: kept_registry,
        mount,
    })
}

/// Found a NEW Space in the tab and stand the engine up over it — the
/// daemon-less FOUNDING entry, the bare-visit counterpart to [`boot`]. Mints
/// the Space, activates this World with its declared founder grants (read from
/// the runner), and composes the same engine `boot` does. `nick` is the
/// founder's display name; the rest of the arguments are `boot`'s, minus the
/// join ticket (there is nothing to join).
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub async fn found(
    relay: String,
    seed_hex: String,
    runner_wasm: Vec<u8>,
    world: String,
    version: String,
    release: String,
    mount: String,
) -> Result<BrowserEngineHandle, JsValue> {
    let seed = unhex32(&seed_hex);
    let PreparedWorld {
        registry,
        control,
        client_runner,
        world_id,
        implementation,
        implementation_version,
        founder_grants,
    } = prepare_world(&runner_wasm, &world, version, release)?;

    let pulled = match crate::space_pull::found_space(
        &relay,
        seed,
        &world,
        implementation,
        implementation_version,
        founder_grants,
        |replica| runtime::browser::declare_schemas(replica, &registry),
    )
    .await
    {
        Ok(pulled) => pulled,
        // A local store an older build wrote cannot be reopened by this build.
        // Do not crash — signal it so the Worker fetches the durable bucket copy
        // and calls `recover` to adopt it (or re-found if nothing was published).
        Err(crate::space_pull::ResumeIncompatible) => {
            return Err(JsValue::from_str("RESUME_INCOMPATIBLE"));
        }
    };

    compose_over(
        pulled,
        registry,
        control,
        client_runner,
        world_id,
        seed,
        mount,
    )
    .await
}

/// Everything a founding needs off the World runner, read world-agnostically
/// before registration moves it — shared by [`found`] and [`recover`].
/// `implementation` is `Copy`, so it feeds both `register_reviewed` and the
/// founding call.
struct PreparedWorld {
    registry: runtime::world::Catalog,
    control: Arc<RemoteWorld>,
    client_runner: Arc<RemoteWorld>,
    world_id: replica::body::WorldId,
    implementation: [u8; 32],
    implementation_version: u32,
    founder_grants: Vec<contact::founding::FounderGrant>,
}

fn prepare_world(
    runner_wasm: &[u8],
    world: &str,
    version: String,
    release: String,
) -> Result<PreparedWorld, JsValue> {
    use world_sdk::WorldApplication;
    let module = WebModule::compile(runner_wasm);
    let init = GuestInit {
        world: world.to_string(),
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
    // Everything founding needs comes off the runner, world-agnostically — the
    // implementation to activate and the founder GRANTS the World declares —
    // read BEFORE the runner is registered (which moves it). The grants ride the
    // same synchronous `dispatch` seam every other runner call does, so nothing
    // World-specific and nothing native leaks into this substrate entry.
    let implementation = world_runner.reviewed_implementation();
    let implementation_version = world_runner.descriptor().implementation_version.0;
    let world_id = world_runner.descriptor().id.clone();
    let founder_grants: Vec<contact::founding::FounderGrant> = world_runner
        .founder_grants()
        .map_err(|e| {
            js_err(
                "the runner does not declare its founder grants",
                format!("{e:?}"),
            )
        })?
        .into_iter()
        .map(|grant| contact::founding::FounderGrant {
            capability: grant.capability,
            resource: grant.resource,
            salt: grant.salt,
        })
        .collect();
    let registry = Builder::new()
        .register_reviewed(world_runner, implementation)
        .build()
        .map_err(|e| js_err("the runner's contract does not register", format!("{e:?}")))?;

    let control = launch("control")?;
    let client_runner = launch("client")?;
    Ok(PreparedWorld {
        registry,
        control,
        client_runner,
        world_id,
        implementation,
        implementation_version,
        founder_grants,
    })
}

/// Recover a bare-visit founder whose local store [`found`] could not reopen
/// (it returned `RESUME_INCOMPATIBLE`): the Worker fetches the durable copy from
/// the bucket and calls this with those bytes — or `None` if the bucket held
/// nothing. [`crate::space_pull::recover_space`] clears the unreadable store and
/// ADOPTS the snapshot (or re-founds when there is none), then composes the same
/// engine `found` does.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub async fn recover(
    relay: String,
    seed_hex: String,
    snapshot: Option<Vec<u8>>,
    runner_wasm: Vec<u8>,
    world: String,
    version: String,
    release: String,
    mount: String,
) -> Result<BrowserEngineHandle, JsValue> {
    let seed = unhex32(&seed_hex);
    let PreparedWorld {
        registry,
        control,
        client_runner,
        world_id,
        implementation,
        implementation_version,
        founder_grants,
    } = prepare_world(&runner_wasm, &world, version, release)?;

    let pulled = crate::space_pull::recover_space(
        &relay,
        seed,
        snapshot,
        &world,
        implementation,
        implementation_version,
        founder_grants,
        |replica| runtime::browser::declare_schemas(replica, &registry),
    )
    .await;

    compose_over(
        pulled,
        registry,
        control,
        client_runner,
        world_id,
        seed,
        mount,
    )
    .await
}

/// The bucket object key a bare-visit founder's Space publishes to, from the
/// device seed alone (the Space id is deterministic in it) — so the Worker can
/// fetch the durable copy during recovery, before any handle exists.
#[wasm_bindgen]
pub fn object_key_for_seed(seed_hex: String) -> String {
    let seed = unhex32(&seed_hex);
    let space = contact::founding::founding_identity(&seed).space;
    contact::gateway::object_key(&space)
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
