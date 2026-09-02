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

use std::sync::Arc;

use comms::Transport;
use contact::authority::SharedLedgerAuthority;
use contact::pull::{pull_receive, Deadlines};
use mechanics::actor::device_from_seed;
use mechanics::ids::SpaceId;
use mechanics::station::{Epoch, Key};
use replica::transaction::{CommitContext, SeedSigner};
use runtime::world::{AuthorityView, Builder, World};
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
}

/// Fold any error into a `JsValue` the Worker sees as a rejected Promise or a
/// thrown value — the browser's own failure channel, not a Rust panic.
fn js_err(context: &str, error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
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

    /// Re-receive over the same transport and install the converged material
    /// into the live core through the Station's own writer — the exact seam the
    /// native Contact driver installs through, and the one reactivity is built
    /// on. Idempotent by convergence: nothing new is a no-op that leaves the
    /// live core intact. Returns the bytes moved, so the caller can tell a
    /// quiet poll from one that landed a peer's write.
    pub async fn repull(&self) -> Result<u32, JsValue> {
        let holdings = self.station.published_root();
        let received = pull_receive(
            self.transport.as_ref(),
            &self.responder,
            &self.space,
            &self.seed,
            &self.authority.bundle(),
            holdings,
            Deadlines::default(),
        )
        .await
        .map_err(|e| js_err("the live re-receive failed", format!("{e:?}")))?;
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

    Ok(BrowserEngineHandle {
        engine,
        station,
        transport,
        responder,
        seed,
        authority: ledger,
        space,
    })
}
