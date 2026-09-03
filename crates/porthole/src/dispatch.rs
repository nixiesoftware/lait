//! The browser Worker's world-agnostic dispatch: a product World RPC reaches
//! the runner through the `ClientAdapter`/`ClientHost` seam, exactly as the
//! daemon's `world_rpc` does — never naming a World.
//!
//! TWO runner instances of the one module, because a wasm instance cannot be
//! re-entered mid-callback (a single instance traps — proven). Every Issues
//! app call routes `RemoteClient::execute` → the guest's `client.host.world`
//! callback → the control `Handler` → `APPLICATION_CALL` back into a runner.
//! If that landed on the SAME instance the `execute` is suspended in, it is a
//! nested activation of one instance on one stack, and it traps. So:
//!
//! - the **control** `Arc<RemoteWorld>` wears two hats — the `World` the
//!   Catalog registers (so `Session::query`/`submit` reach it) and the control
//!   `Handler` [`BrowserClientHost::call_world`] drives — and its
//!   `APPLICATION_CALL` reads via the Context's semantic callbacks straight
//!   from the Replica, so it re-enters nothing;
//! - the **client** `Arc<RemoteWorld>` is a separate instance backing the
//!   `ClientAdapter` (`RemoteClient`), which forwards a product request in with
//!   `parse_web`/`execute`.
//!
//! `execute` (client instance) → `call_world` → `APPLICATION_CALL` (control
//! instance) crosses between two distinct instances, so neither is re-entered.

use std::sync::Arc;
use std::sync::Mutex;

use runtime::world::call::{Call, Context, Handler, Reply};
use runtime::Session;
use world_interface::{
    ClientAdapter, ClientFuture, ClientHost, Failure, HostContentRequest, HostControlRequest,
    PresentationHandle, PresentationResolution,
};
use world_sdk::{RemoteClient, RemoteWorld};

/// A browser ClientHost over the composed Session: it answers the runner's
/// callbacks synchronously, drives world sub-calls back through the control
/// Handler, keeps caller-local state in memory, and refuses — honestly, with
/// a typed failure — every capability a tab does not have (a filesystem, the
/// content plane, the exec drain, an address book).
pub struct BrowserClientHost<'a> {
    session: &'a Session,
    control: Arc<RemoteWorld>,
    identity: &'a runtime::world::LocalIdentity,
    actor: String,
    device: String,
    local: Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
}

impl<'a> BrowserClientHost<'a> {
    fn context(&self) -> Context<'_> {
        Context {
            session: self.session,
            identity: self.identity,
            actor: &self.actor,
            device: &self.device,
        }
    }
}

impl ClientHost for BrowserClientHost<'_> {
    fn local_root(&self) -> &std::path::Path {
        // A browser host has no filesystem; nothing should read this, but the
        // trait demands a path. A stable sentinel that no real op touches.
        std::path::Path::new("/browser")
    }

    fn local_get(&self, key: &str) -> Option<Vec<u8>> {
        self.local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    fn local_put(&self, key: &str, bytes: &[u8]) -> Result<(), Failure> {
        self.local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }

    fn save_user_file(&self, _destination: &str, _bytes: &[u8]) -> Result<(), Failure> {
        Err(Failure::new(
            "this browser session cannot write files to the device",
        ))
    }

    fn call_world<'b>(&'b self, call: Call) -> ClientFuture<'b, Reply> {
        // The re-entrant seam: run the product call through the control Handler
        // over the composed Session — a nested dispatch back into the runner.
        let reply = self.control.call(&call, &self.context());
        Box::pin(async move { Ok(reply) })
    }

    fn call_find<'b>(
        &'b self,
        _world: replica::body::WorldId,
        query: runtime::find::Query,
    ) -> ClientFuture<'b, serde_json::Value> {
        let answer = runtime::world::call::SessionAccess::find(self.session, query);
        Box::pin(async move {
            match answer {
                Ok(answer) => serde_json::to_value(answer)
                    .map_err(|e| Failure::new(format!("encode Find answer: {e}"))),
                Err(failure) => Err(Failure::new(format!("Find refused: {failure:?}"))),
            }
        })
    }

    fn call_work<'b>(
        &'b self,
        _request: runtime::exec::WorkRequest,
    ) -> ClientFuture<'b, serde_json::Value> {
        Box::pin(async move {
            Err(Failure::new(
                "durable Run lifecycle is not available in this browser session",
            ))
        })
    }

    fn call_control<'b>(
        &'b self,
        _request: HostControlRequest,
    ) -> ClientFuture<'b, serde_json::Value> {
        Box::pin(async move {
            Err(Failure::new(
                "Space control is not available in this browser session",
            ))
        })
    }

    fn call_content<'b>(
        &'b self,
        _request: HostContentRequest,
    ) -> ClientFuture<'b, serde_json::Value> {
        Box::pin(async move {
            Err(Failure::new(
                "the content plane is not available in this browser session",
            ))
        })
    }

    fn call_identity<'b>(
        &'b self,
        _handles: Vec<PresentationHandle>,
    ) -> ClientFuture<'b, PresentationResolution> {
        // No address book in a tab: an honest empty resolution, not an error.
        Box::pin(async move { Ok(PresentationResolution::unavailable()) })
    }
}

/// The browser engine: the composed Session plus the runner as adapter and
/// control Handler, ready to answer a world RPC world-agnostically.
pub struct BrowserEngine {
    session: Session,
    control: Arc<RemoteWorld>,
    client: RemoteClient,
    identity: runtime::world::LocalIdentity,
    actor: String,
    device: String,
    ledger: contact::authority::SharedLedgerAuthority,
    space: String,
    mount: String,
}

/// One answer over the frame link, mirroring the viewer's `LinkReply`
/// (`viewer/src/link.ts`): a reply body, a refusal as clone-safe data, or a
/// confirmation question. Serialized `{kind, …}`.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkReply {
    Reply { body: serde_json::Value },
    Refusal { refusal: browser_control::Refusal },
    Confirm { question: String },
}

/// A frame the viewer's `workerLink` sends (`viewer/src/workerLink.ts`
/// `WorkerLinkRequest`). The Worker glue decodes one of these per message and
/// hands it to [`BrowserEngine::handle_link`].
#[derive(serde::Deserialize)]
#[serde(tag = "lait", rename_all = "snake_case")]
pub enum WorkerLinkRequest {
    Rpc {
        id: u64,
        verb: LinkVerb,
        #[serde(default)]
        space: Option<String>,
        #[serde(default)]
        world: Option<String>,
        #[serde(default)]
        request: Option<serde_json::Value>,
        #[serde(default)]
        confirm: bool,
    },
    Abort {
        id: u64,
    },
    Events {
        id: u64,
    },
    Close {
        id: u64,
    },
}

#[derive(serde::Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LinkVerb {
    Spaces,
    Host,
    Space,
    World,
}

/// A frame the engine sends back (`WorkerLinkResponse`). The one-shot rpc
/// verbs answer with `reply`; the events lane's `ring`/`liveness` frames are
/// pushed by the Worker composition root over time, not returned here.
#[derive(serde::Serialize)]
#[serde(tag = "lait", rename_all = "snake_case")]
pub enum WorkerLinkResponse {
    Reply { id: u64, reply: LinkReply },
}

impl BrowserEngine {
    /// Compose over an already-docked Session, the control runner (the same
    /// `Arc<RemoteWorld>` the Session's Catalog registered, which also serves
    /// `call_world`), and a SEPARATE `client_runner` instance backing the
    /// `ClientAdapter`. The two instances are what keep `execute`'s callback
    /// into `call_world` from re-entering the instance `execute` runs in.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: Session,
        control: Arc<RemoteWorld>,
        client_runner: Arc<RemoteWorld>,
        identity: runtime::world::LocalIdentity,
        actor: String,
        device: String,
        ledger: contact::authority::SharedLedgerAuthority,
        space: String,
        mount: String,
    ) -> anyhow::Result<Self> {
        let client = RemoteClient::connect(client_runner)?;
        Ok(Self {
            session,
            control,
            client,
            identity,
            actor,
            device,
            ledger,
            space,
            mount,
        })
    }

    /// The spaces reply a browser backend gives: the single served Space, in a
    /// shape that carries no daemon probe readings (see
    /// `browser_control::reply`).
    pub fn spaces(&self) -> LinkReply {
        let row = browser_control::reply::ServedSpaceRow {
            kind: browser_control::reply::ServedKind::Served,
            id: self.mount.clone(),
            space: self.space.clone(),
            // The live catalog name is a later refinement (a status read); a
            // served row is honest with it absent.
            name: None,
            identity: browser_control::reply::ServedIdentity::Own,
        };
        let reply = browser_control::reply::ServedSpacesReply {
            spaces: vec![row],
            world: self.mount.clone(),
        };
        reply_of(serde_json::to_value(reply))
    }

    /// A control-plane request (host or Space plane), answered
    /// world-agnostically: `whoami`/`members` from the pulled ledger, and every
    /// daemon-only or not-yet command refused legibly with `not_hosted`.
    pub fn control_rpc(&self, cmd: &str) -> LinkReply {
        use browser_control::Disposition;
        match browser_control::disposition(cmd) {
            Some(Disposition::Answered) => match cmd {
                "whoami" => reply_of(serde_json::to_value(browser_control::answer::whoami(
                    &self.ledger,
                ))),
                "members" => reply_of(serde_json::to_value(browser_control::answer::members(
                    &self.ledger,
                ))),
                // The classification names a command Answered that this
                // dispatcher has not wired — treat as not-yet rather than
                // pretend. (The completeness test keeps this arm empty in
                // practice.)
                other => LinkReply::Refusal {
                    refusal: browser_control::Refusal::not_yet(other),
                },
            },
            Some(Disposition::DaemonOnly) => LinkReply::Refusal {
                refusal: browser_control::Refusal::daemon_only(cmd),
            },
            Some(Disposition::NotYet) => LinkReply::Refusal {
                refusal: browser_control::Refusal::not_yet(cmd),
            },
            None => LinkReply::Refusal {
                refusal: browser_control::Refusal::unclassified(cmd),
            },
        }
    }

    fn host(&self) -> BrowserClientHost<'_> {
        BrowserClientHost {
            session: &self.session,
            control: self.control.clone(),
            identity: &self.identity,
            actor: self.actor.clone(),
            device: self.device.clone(),
            local: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// Run one product World request through the world-agnostic seam:
    /// `parse_web` classifies it, `execute` forwards it into the runner, and
    /// the runner's callbacks come back through the browser ClientHost — the
    /// re-entrant path when a call reaches `call_world`. Returns the raw
    /// product value (or its Failure); [`Self::world_link`] wraps it as a
    /// frame reply.
    pub fn world_rpc(&self, request: serde_json::Value) -> Result<serde_json::Value, Failure> {
        let host = self.host();
        let invocation = self.client.parse_web(request)?;
        futures_lite::future::block_on(self.client.execute(&host, invocation))
    }

    /// The world-specific body id for a document, asked of the runner — the
    /// world-agnostic way a caret finds which Body it is in (issue-reff →
    /// `[u8; 16]`), exactly as the daemon's socket asks its package's
    /// `transient_body`. A tab must not compute a World's own hashing itself.
    pub fn transient_body(&self, document: &str) -> Result<[u8; 16], String> {
        use world_interface::ClientAdapter;
        self.client
            .transient_body(document)
            .map_err(|failure| format!("{failure:?}"))
    }

    /// Route one decoded frame to the right verb and answer it. The one-shot
    /// rpc verbs (spaces / host / space / world) answer with a `reply` frame;
    /// the streaming lanes (events / abort / close) are the Worker composition
    /// root's to manage — they carry no synchronous answer, so this returns
    /// `None` for them.
    pub fn handle_link(&self, request: WorkerLinkRequest) -> Option<WorkerLinkResponse> {
        let WorkerLinkRequest::Rpc {
            id, verb, request, ..
        } = request
        else {
            return None;
        };
        let reply = match verb {
            LinkVerb::Spaces => self.spaces(),
            LinkVerb::Host | LinkVerb::Space => {
                // A control request carries its command in the `cmd` tag, the
                // one fact the world-agnostic dispatcher needs to classify it.
                match request
                    .as_ref()
                    .and_then(|value| value.get("cmd"))
                    .and_then(|cmd| cmd.as_str())
                {
                    Some(cmd) => self.control_rpc(cmd),
                    None => LinkReply::Refusal {
                        refusal: browser_control::Refusal {
                            status: 400,
                            message: "a control request must name its cmd".to_string(),
                            error_kind: "error".to_string(),
                        },
                    },
                }
            }
            LinkVerb::World => match request {
                Some(product) => self.world_link(product),
                None => LinkReply::Refusal {
                    refusal: browser_control::Refusal {
                        status: 400,
                        message: "a world request carries no product payload".to_string(),
                        error_kind: "error".to_string(),
                    },
                },
            },
        };
        Some(WorkerLinkResponse::Reply { id, reply })
    }

    /// The world verb as a frame `LinkReply`: a product answer, or the
    /// runner's Failure carried across as a refusal (never the native head's
    /// wrong-mount refusal — a browser refusal is `not_hosted`-family, and a
    /// World's own Failure keeps its diagnostic).
    pub fn world_link(&self, request: serde_json::Value) -> LinkReply {
        match self.world_rpc(request) {
            Ok(body) => LinkReply::Reply { body },
            Err(failure) => LinkReply::Refusal {
                refusal: browser_control::Refusal {
                    status: 400,
                    message: failure
                        .diagnostic()
                        .unwrap_or("the World refused the call")
                        .to_string(),
                    error_kind: "error".to_string(),
                },
            },
        }
    }
}

/// A `serde_json` result into a reply frame, folding an encode error into a
/// legible refusal.
fn reply_of(result: Result<serde_json::Value, serde_json::Error>) -> LinkReply {
    match result {
        Ok(body) => LinkReply::Reply { body },
        Err(error) => LinkReply::Refusal {
            refusal: browser_control::Refusal {
                status: 500,
                message: format!("could not encode the reply: {error}"),
                error_kind: "error".to_string(),
            },
        },
    }
}
