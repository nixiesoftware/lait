//! How a local Orbit comes into existence, and into standing.
//!
//! An Orbit is a store directory on this machine bound to one Space. Founding
//! and joining are the only two ways a row enters the registry next door, so
//! the policy that decides which directory may become an Orbit — and what is
//! written when it does — lives with the registry it populates.
//!
//! Everything here takes explicit arguments and returns values. None of it
//! prints, exits, or writes the process environment: these bodies used to be
//! CLI handlers, and a head that serves several callers out of one process can
//! do none of those things on behalf of one of them.
//!
//! The daemon is the intended host. It is identity-scoped and is built from an
//! identity directory before any store exists, which is what lets it own
//! formation at all — and, since it is the process that would otherwise hold
//! the Orbit lock, it is the only one that can form or rebuild a store without
//! racing itself for it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::config;
use crate::control::{ControlRoute, HostReply, Request, Response};
use crate::daemon::{LocalOrbitId, OrbitAddress};
use crate::orbits::{self, Entry, Origin, Router};

/// Why a store directory cannot host the Space a formation verb was aimed at.
///
/// Typed rather than a sentence: these carry different `ErrorKind`s on the
/// wire, and a head has to distinguish "you are already in a space" from "this
/// directory belongs to a different one" to offer the right next step.
#[derive(Debug)]
pub enum TargetRefusal {
    /// The directory already holds an initialized space store.
    Occupied { home: PathBuf, space: String },
    /// The directory holds a Space other than the one the invite names.
    WrongSpace {
        home: PathBuf,
        holds: String,
        invite: String,
    },
    /// The directory holds a pre-orbital store. A clean break, never migrated.
    Unsupported { home: PathBuf, detail: String },
    /// The directory holds several Spaces. Not an empty target and not a
    /// re-entry either: nothing can say which Space it is aimed at.
    Ambiguous { home: PathBuf },
}

impl std::fmt::Display for TargetRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetRefusal::Occupied { home, space } => write!(
                f,
                "{} already holds space {space} — found another space in a different directory",
                home.display()
            ),
            TargetRefusal::WrongSpace {
                home,
                holds,
                invite,
            } => write!(
                f,
                "{} holds space {holds} — the invite is for {invite}",
                home.display()
            ),
            TargetRefusal::Unsupported { home, detail } => {
                write!(f, "{detail} (aimed at {})", home.display())
            }
            TargetRefusal::Ambiguous { home } => write!(
                f,
                "{} holds more than one space — a directory holds one, so aim at one of them",
                home.display()
            ),
        }
    }
}
impl std::error::Error for TargetRefusal {}

/// A Space founded here, and where it landed.
#[derive(Debug, Clone)]
pub struct Founded {
    pub space: String,
    pub home: PathBuf,
    pub device: String,
    pub name: String,
    pub project: orbits::ProjectBrief,
}

/// A joiner's store bootstrapped from an invite. Admission has not run yet.
#[derive(Debug, Clone)]
pub struct Entered {
    pub space: String,
    /// The same Space, typed — so a caller can address the Orbit it just bound
    /// without re-parsing the string it was handed.
    pub space_id: mechanics::ids::SpaceId,
    pub home: PathBuf,
    pub device: String,
    pub approach: String,
    pub host_nick: String,
    /// False when the store already held this Space (a re-join).
    pub fresh: bool,
}

/// Found a Space into `home` and register the resulting Orbit.
///
/// `identity_dir` supplies (or mints) the device seed. The store directory is
/// created if absent; an existing space store there is a refusal, because one
/// directory holds one Space and silently forming beside another is how a node
/// ends up with two.
pub fn found(
    packages: &crate::orbital::WorldPackages,
    home: &Path,
    identity_dir: &Path,
    name: &str,
    nick: Option<&str>,
) -> Result<Founded> {
    let home = config::prepare_store_dir(home)?;
    refuse_occupied(&home)?;

    if let Some(nick) = nick {
        config::config_set(Some(&home), "user.nick", nick, false)?;
    }
    let seed = config::load_or_create_identity(identity_dir)?;
    let device = mechanics::actor::device_from_seed(&seed)
        .as_str()
        .to_string();
    let (space, project) = crate::world::lifecycle::found_space(packages, &home, &seed, name)?;

    register(Entry {
        space: space.to_string(),
        name: name.to_string(),
        path: home.display().to_string(),
        origin: Origin::Founded,
        host_nick: String::new(),
        last_opened: mechanics::wallclock::now_secs(),
    });

    Ok(Founded {
        space: space.to_string(),
        home,
        device,
        name: name.to_string(),
        project,
    })
}

/// Bootstrap a joiner's store from an invite link and register the Orbit.
///
/// Idempotent for a re-join: a store that already holds this Space is left
/// exactly as it is, and only the registry row is refreshed.
pub fn enter(
    packages: &crate::orbital::WorldPackages,
    home: &Path,
    identity_dir: &Path,
    link: &str,
    nick: Option<&str>,
) -> Result<Entered> {
    let coordinates = runtime::coordinates::SignedCoordinates::parse_link(link.trim())
        .map_err(|error| anyhow!("invalid invite link: {error}"))?;
    let verified = coordinates
        .verify()
        .map_err(|error| anyhow!("invite: {error}"))?;
    let space_id = verified.space.clone();
    let space = space_id.as_str().to_string();
    let approach = verified.approach_station.as_str().to_string();

    let home = config::prepare_store_dir(home)?;
    refuse_unsupported(&home)?;
    // A directory bound to another Space is a wrong-directory signal, never a
    // reason to form a second store beside the first.
    let fresh = match crate::orbital::discover_space(&home) {
        crate::orbital::SpaceStore::One(existing) if existing.as_str() == space => false,
        crate::orbital::SpaceStore::One(existing) => {
            return Err(TargetRefusal::WrongSpace {
                home,
                holds: existing.to_string(),
                invite: space,
            }
            .into())
        }
        crate::orbital::SpaceStore::Several => return Err(TargetRefusal::Ambiguous { home }.into()),
        crate::orbital::SpaceStore::Absent => true,
    };

    if let Some(nick) = nick {
        config::config_set(Some(&home), "user.nick", nick, false)?;
    }
    let seed = config::load_or_create_identity(identity_dir)?;
    let device = mechanics::actor::device_from_seed(&seed)
        .as_str()
        .to_string();
    if fresh {
        crate::world::lifecycle::enter_space(packages, &home, &seed, link)?;
    }

    register(Entry {
        space: space.clone(),
        name: verified.approach_nick_hint.clone(),
        path: home.display().to_string(),
        origin: Origin::Joined,
        host_nick: verified.approach_nick_hint.clone(),
        last_opened: mechanics::wallclock::now_secs(),
    });

    Ok(Entered {
        space,
        space_id,
        home,
        device,
        approach,
        host_nick: verified.approach_nick_hint,
        fresh,
    })
}

/// Sign this machine's consent to join an existing actor.
///
/// Store-free by construction, and it has to be: the machine running this has
/// no membership anywhere yet, which is the entire situation device enrolment
/// exists for. It reads (or mints) one seed and signs.
pub fn device_consent(identity_dir: &Path, token: &str) -> Result<String> {
    let mut parts = token.split_whitespace();
    let actor = parts
        .next()
        .and_then(mechanics::ids::ActorId::parse)
        .ok_or_else(|| anyhow!("invalid device token (expected `<actor_id> <space_id>`)"))?;
    let space = parts
        .next()
        .filter(|w| w.starts_with("ws_"))
        .ok_or_else(|| anyhow!("invalid device token (missing space id)"))?
        .to_string();
    let seed = config::load_or_create_identity(identity_dir)?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|error| anyhow!("getrandom: {error}"))?;
    let binding = mechanics::actor::consent_sign(
        &seed,
        &space,
        nonce,
        &mechanics::actor::ConsentCtx::Member { actor: &actor },
    );
    Ok(data_encoding::HEXLOWER.encode(&postcard::to_stdvec(&binding)?))
}

/// Rebuild one Orbit's implicit prior representation, releasing this daemon's
/// own placement first.
///
/// A generation build takes the Orbit lock exclusively, so run from a client it
/// was a race against whatever the daemon had open — and losing that race is
/// how "Orbit is active; stop the daemon before rebuilding" became a routine
/// answer. Inside the daemon there is nobody left to race: the placement is
/// stopped here, and the next routed request lazily re-places it.
pub async fn rebuild(
    router: &Router,
    selector: &str,
) -> Result<crate::orbital::generation::Rebuild> {
    let store = orbits::select(selector)?;
    let resolved = admit(router, &store).map_err(|error| anyhow!("{selector}: {error}"))?;
    let seed = config::load_or_create_identity(&resolved.identity_dir)?;
    let home = resolved.home.clone();
    // The vacancy is held across the build, not just across the handoff: a
    // placement that has left the slot is still draining and still holds the
    // Orbit lock, and `rebuild_prior` takes that lock exclusively. Releasing
    // the slot first left room for the next routed request to re-place the
    // Orbit underneath the build — which is how "Orbit is active; stop the
    // daemon before rebuilding" survived being moved inside the daemon.
    let vacancy = router.vacate(&resolved.address.orbit).await?;
    let rebuilt = tokio::task::spawn_blocking(move || {
        crate::orbital::generation::rebuild_prior(&home, &seed)
    })
    .await
    .map_err(|error| anyhow!("rebuild task failed: {error}"))?;
    drop(vacancy);
    rebuilt
}

/// Whether a locally admitted Orbit still selects the implicit prior
/// representation. This is a read-only classifier: unknown prior bytes are not
/// called compatible here; [`rebuild`] will either authenticate the one
/// supported source generation and replace it atomically, or refuse it.
pub fn needs_representation_rebuild(router: &Router, selector: &str) -> Result<bool> {
    let store = orbits::select(selector)?;
    let resolved = admit(router, &store).map_err(|error| anyhow!("{selector}: {error}"))?;
    let active = runtime::generation::Active::read(
        crate::orbital::orbital_store_root(&resolved.home).join(resolved.address.space.as_str()),
    )
    .map_err(|error| anyhow!("read active generation for {selector}: {error}"))?;
    Ok(active.generation().is_none())
}

/// The local Orbit a host request named, admitted for this daemon to act on.
///
/// Two questions, and both belong on this side. Is the path a store this daemon
/// serves at all — a host request carries a caller-chosen path, and without this
/// a settings write lands in any directory on the machine and a rebuild reaches
/// any store on it. And would acting on it spend a seed that is not this
/// daemon's own — the identity a caller's credential stands for is this
/// daemon's, so a signature made with a key it merely hosts is not theirs to
/// ask for, however wide their own grants are or become.
fn admit(router: &Router, home: &Path) -> Result<crate::orbits::ResolvedOrbit> {
    let resolved = router.resolve(LocalOrbitId::for_store(home).as_str())?;
    if !router.catalog().signs_with_own_seed(&resolved) {
        return Err(anyhow!(
            "{} is hosted here under its own key — act on it through that identity's own node",
            resolved.home.display()
        ));
    }
    Ok(resolved)
}

/// The custody gate on the directory a formation verb is aimed at.
///
/// Founding and entering legitimately name a path this daemon has never
/// served — that is the situation the host plane exists for — so neither is
/// admitted against the catalog the way a settings write is. What neither may
/// do is spend, or plant beside, a key that is not this daemon's. A target can
/// be an identity this daemon merely hosts: [`enter`] accepts a directory that
/// already holds the invite's Space, and it creates one where the directory
/// holds nothing yet; [`found`] plants a store in whatever directory it is
/// handed. Either way the write lands in `<home>/config.json` — the write
/// `admit_settings_home` refuses for the same directory — and entering then
/// drives `Request::Connect` through a Station that loads its seed from that
/// home, so the admission handshake goes out on the wire under somebody else's
/// signature. `/api/spaces/{id}/rpc` already refuses `Connect` for that Orbit;
/// this is the same refusal on the route that reaches it first.
///
/// The question is asked of the path when the registry cannot answer it, because
/// forming is what writes the registry row: an agent home is one by where it
/// lives, row or no row.
///
/// And it is asked unconditionally. Whether a Space happens to sit in the
/// directory yet is not the custody question — *whose key lives here* is. A
/// provisioned agent identity directory holds a seed before it holds a Space, so
/// treating "no Space here" as "blank, therefore mine" admitted exactly the
/// directory this gate exists to refuse, and the first `Connect` went out under
/// that agent's signature. A directory that holds neither a Space nor an
/// identity this daemon does not own still passes, which is what keeps founding
/// and entering into a blank target working.
///
/// It is asked of a directory that already exists. The caller materializes the
/// target first and hands the same [`PathBuf`] to the verb behind it, so no
/// spelling reaches this that the filesystem cannot resolve — and the gate and
/// the write cannot end up meaning two different directories.
fn admit_formation_target(router: &Router, home: &Path) -> Result<()> {
    let own = match router.resolve(LocalOrbitId::for_store(home).as_str()) {
        Ok(resolved) => router.catalog().signs_with_own_seed(&resolved),
        Err(_) => router.catalog().path_signs_with_own_seed(home),
    };
    if !own {
        return Err(anyhow!(
            "{} is hosted here under its own key — form it through that identity's own node",
            home.display()
        ));
    }
    Ok(())
}

/// Materialize the directory a formation request named, so the custody gate and
/// the write that follows it are asked about the same place on disk.
///
/// A caller-named path is a *spelling* until something resolves it. Gating the
/// spelling and then creating the directory let the two disagree — a component
/// that does not exist yet is unresolvable, so the gate compared text while
/// `create_dir_all` walked `..` through to a directory the text did not name.
/// Creating first collapses the spelling into one answer both halves read.
fn materialize_target(home: &str) -> Result<PathBuf, Response> {
    // Only what the custody gate needs: a real directory to resolve, and a
    // resolved spelling to compare. Store preparation stays on the far side of
    // the gate, in `found`/`enter`, so a request that is about to be REFUSED
    // does not leave a `.gitignore` behind in a directory it was never allowed
    // to name.
    let named = Path::new(home);
    std::fs::create_dir_all(named).map_err(|error| Response::err(format!("{home}: {error}")))?;
    config::resolved(named).ok_or_else(|| Response::err(format!("{home}: cannot be resolved")))
}

/// How long entering a Space keeps reaching for the inviter before answering.
///
/// Bounded rather than open-ended: an inviter who is merely offline is not an
/// error — the store is bootstrapped either way — and a caller waiting on an
/// HTTP response needs an answer, not a hang. The reply says which of the two
/// happened, so a surface can offer "retry" instead of pretending.
const ADMISSION_DEADLINE: Duration = Duration::from_secs(30);

/// What a joiner learned while waiting to be admitted.
#[derive(Debug, Clone)]
pub struct Admission {
    pub admitted: bool,
    pub contacted: bool,
    pub last_error: Option<String>,
}

/// Drive Contact to the invite's approach Station until admission lands.
///
/// The joiner's Contact registers it as a pending Neighbor on the inviter,
/// whose driver reciprocally dials back to redeem the admission — so repeated
/// Connects converge to membership with no manual step for an auto-approving
/// invite.
///
/// Run inside the daemon, right behind the bootstrap that made the store, so
/// entering a Space is one request rather than a bootstrap plus a polling loop
/// every head would have to reimplement. Progress goes to the daemon's own log:
/// there is no terminal to beat at any more.
async fn await_admission(router: &Router, address: OrbitAddress, approach: &str) -> Admission {
    let route = ControlRoute::Orbit { address };
    let started = tokio::time::Instant::now();
    let mut contacted = false;
    let mut last_error: Option<String> = None;
    while started.elapsed() < ADMISSION_DEADLINE {
        match router
            .request_routed(
                route.clone(),
                &Request::Connect {
                    ticket: approach.to_string(),
                },
                None,
            )
            .await
        {
            Ok(Response::Ok { .. }) => contacted = true,
            Ok(Response::Error { message, .. }) => last_error = Some(message),
            _ => {}
        }
        if let Ok(Response::Status(info)) = router
            .request_routed(route.clone(), &Request::Status, None)
            .await
        {
            if info.membership == "member" {
                return Admission {
                    admitted: true,
                    contacted: true,
                    last_error,
                };
            }
        }
        tracing::debug!(
            contacted,
            elapsed_ms = started.elapsed().as_millis(),
            last_error = last_error.as_deref().unwrap_or(""),
            "still reaching the inviter"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Admission {
        admitted: false,
        contacted,
        last_error,
    }
}

/// Refuse a directory that already holds a store of any vintage.
fn refuse_occupied(home: &Path) -> Result<()> {
    refuse_unsupported(home)?;
    match crate::orbital::discover_space(home) {
        crate::orbital::SpaceStore::One(space) => Err(TargetRefusal::Occupied {
            home: home.to_path_buf(),
            space: space.to_string(),
        }
        .into()),
        crate::orbital::SpaceStore::Several => Err(TargetRefusal::Ambiguous {
            home: home.to_path_buf(),
        }
        .into()),
        crate::orbital::SpaceStore::Absent => Ok(()),
    }
}

fn refuse_unsupported(home: &Path) -> Result<()> {
    if let Some(error) = crate::orbital::unsupported_store_at(home) {
        return Err(TargetRefusal::Unsupported {
            home: home.to_path_buf(),
            detail: error.to_string(),
        }
        .into());
    }
    Ok(())
}

/// Register a store in the catalog. Best-effort: the registry is navigation
/// state, and a Space that formed is formed whether or not it was indexed.
fn register(entry: Entry) {
    if let Err(error) = orbits::upsert(entry) {
        tracing::warn!(%error, "Orbit registry update failed");
    }
}

/// Serve one daemon-scoped host request, or decline it.
///
/// `None` means "not mine", which is what keeps the daemon's own arm for
/// unhandled daemon-scoped requests honest instead of turning it into a
/// catch-all.
pub(crate) async fn dispatch(router: &Router, request: Request) -> Option<Response> {
    Some(match request {
        Request::HostSpaceFound { home, name, nick } => {
            // Same gate, same position as the entry below: founding writes a
            // store and a `config.json` into whatever directory it is handed,
            // and a directory holding somebody else's key is not a blank target
            // however empty of Spaces it looks.
            let home = match materialize_target(&home) {
                Ok(home) => home,
                Err(refusal) => return Some(refusal),
            };
            if let Err(refusal) = admit_formation_target(router, &home) {
                return Some(Response::err(format!("{refusal:#}")));
            }
            let identity = router.catalog().identity().to_path_buf();
            let packages = router.packages();
            blocking(move || {
                found(&packages, &home, &identity, &name, nick.as_deref()).map(|founded| {
                    HostReply::Founded {
                        space: founded.space,
                        home: founded.home.display().to_string(),
                        device: founded.device,
                        name: founded.name,
                        project_key: founded.project.key,
                        project_name: founded.project.name,
                    }
                })
            })
            .await
        }
        Request::HostSpaceEnter { link, home, nick } => {
            // Before the nick write and before the first Connect: an entry may
            // not spend a seed this daemon only hosts, whether or not the
            // directory it names holds a Space yet.
            let home = match materialize_target(&home) {
                Ok(home) => home,
                Err(refusal) => return Some(refusal),
            };
            if let Err(refusal) = admit_formation_target(router, &home) {
                return Some(Response::err(format!("{refusal:#}")));
            }
            let identity = router.catalog().identity().to_path_buf();
            let packages = router.packages();
            let bootstrapped = tokio::task::spawn_blocking(move || {
                enter(&packages, &home, &identity, &link, nick.as_deref())
            })
            .await;
            match bootstrapped {
                Ok(Ok(entered)) => {
                    // Entering is not finished until the joiner holds standing:
                    // the store is bound to the Space, but every Body in it is
                    // still encrypted to a key admission delivers. Driving that
                    // here is what keeps "entered" from meaning "blank board".
                    let address = OrbitAddress::for_store(&entered.home, entered.space_id.clone());
                    let admission = await_admission(router, address, &entered.approach).await;
                    Response::Host(HostReply::Entered {
                        space: entered.space,
                        home: entered.home.display().to_string(),
                        device: entered.device,
                        approach: entered.approach,
                        host_nick: entered.host_nick,
                        fresh: entered.fresh,
                        admitted: admission.admitted,
                        contacted: admission.contacted,
                        last_error: admission.last_error,
                    })
                }
                Ok(Err(error)) => target_error(error),
                Err(error) => Response::err(format!("host task failed: {error}")),
            }
        }
        Request::HostDeviceConsent { token } => {
            let identity = router.catalog().identity().to_path_buf();
            blocking(move || {
                device_consent(&identity, &token)
                    .map(|consent| HostReply::DeviceConsent { consent })
            })
            .await
        }
        Request::HostConfigList { home } => match admit_settings_home(router, home.as_deref()) {
            Err(refusal) => refusal,
            Ok(target) => Response::Host(HostReply::Config {
                rows: config::config_list(target.as_deref()),
            }),
        },
        // The answer to `HostConfigGet` is the *value*, not a row: a caller
        // substitutes it straight into a field or a prompt, and a
        // key/origin/help sentence in that position is a sentence where a nick
        // should be. The row form is what `HostConfigList` is for.
        Request::HostConfigGet { key, home } => {
            match admit_settings_home(router, home.as_deref()) {
                Err(refusal) => refusal,
                Ok(target) => match config::config_get(target.as_deref(), &key) {
                    Ok(row) => Response::Text {
                        text: config_value(&row),
                    },
                    Err(error) => config_error(error),
                },
            }
        }
        Request::HostConfigSet {
            key,
            value,
            global,
            home,
        } => match admit_settings_home(router, home.as_deref()) {
            Err(refusal) => refusal,
            Ok(target) => match config::config_set(target.as_deref(), &key, &value, global) {
                Ok(write) => config_written(router, write, home.as_deref()).await,
                Err(error) => config_error(error),
            },
        },
        Request::HostConfigUnset { key, global, home } => {
            match admit_settings_home(router, home.as_deref()) {
                Err(refusal) => refusal,
                Ok(target) => match config::config_unset(target.as_deref(), &key, global) {
                    Ok(write) => config_written(router, write, home.as_deref()).await,
                    Err(error) => config_error(error),
                },
            }
        }
        Request::HostOrbitForget { selector } => match orbits::forget(&selector) {
            Ok(entries) if entries.is_empty() => {
                Response::not_found(format!("nothing in the registry matches '{selector}'"))
            }
            Ok(entries) => Response::Host(HostReply::Forgotten { entries }),
            Err(error) => Response::err(format!("{error:#}")),
        },
        Request::HostOrbitPrune => match orbits::prune() {
            Ok(entries) => Response::Host(HostReply::Pruned { entries }),
            Err(error) => Response::err(format!("{error:#}")),
        },
        Request::HostOrbitRebuild { orbit } => match rebuild(router, &orbit).await {
            Ok(rebuilt) => Response::Host(HostReply::Rebuilt {
                generation: rebuilt.generation.to_string(),
                effects: rebuilt.effects,
                bodies: rebuilt.bodies,
                receipts: rebuilt.receipts,
                evidence: data_encoding::HEXLOWER.encode(&rebuilt.evidence),
            }),
            Err(error) => selector_error(error),
        },
        Request::HostInstallMcp {
            client,
            scope,
            name,
            agent,
            no_agent,
            print,
            dir,
            world,
        } => {
            blocking(move || {
                crate::install::install_mcp(
                    client,
                    scope,
                    &name,
                    agent.as_deref(),
                    no_agent,
                    print,
                    Path::new(&dir),
                    world.as_deref(),
                )
                .map(|installed| HostReply::McpInstalled {
                    path: installed.path.display().to_string(),
                    detail: installed.detail,
                    note: installed.note,
                    replaced: installed.replaced,
                    agent: installed.agent,
                })
            })
            .await
        }
        // Blocking by nature (HTTP, archive extract, file swap) and slow enough
        // that it must not hold the reactor while it runs.
        Request::HostUpdate => {
            // The standing is read here rather than inside `update::run`,
            // because it is a fact about the *installation* — written by the
            // resident watcher on its own period — and not a result of this
            // request. A sidecar's `run` refuses to replace itself and says
            // who owns it; what the person actually needs to know in that
            // case is whether their client has a release waiting.
            let identity = router.catalog().identity().to_path_buf();
            blocking(move || {
                let standing = crate::update::watch::standing(&identity);
                crate::update::run().map(|updated| HostReply::Updated {
                    from: updated.from,
                    to: updated.to,
                    replaced: updated.replaced,
                    channel: updated.channel,
                    available: updated.available,
                    floor: updated.floor,
                    managed_by: updated.managed_by,
                    standing,
                })
            })
            .await
        }
        Request::HostWorldUpdate { world } => {
            let Some(world_id) = replica::body::WorldId::parse(&world) else {
                return Some(Response::invalid("invalid World id"));
            };
            if router.reviewed_world_implementation(&world_id).is_none() {
                return Some(Response::not_found(format!(
                    "World '{world}' is not installed for this identity"
                )));
            }
            let worlds = crate::serve::head::installations_root(router.catalog().identity());
            let world_for_job = world.clone();
            match router
                .run_blocking(move || {
                    crate::update::consent::enqueue(
                        &worlds,
                        &world_for_job,
                        mechanics::wallclock::now_secs(),
                    )
                })
                .await
            {
                Ok(job) => Response::Host(HostReply::WorldUpdate {
                    world,
                    job: Some(job),
                }),
                Err(crate::orbits::BlockingFailure::Capacity) => Response::capacity(
                    "the bounded host lane cannot admit World update consent right now",
                ),
                Err(error) => world_update_state_failure("persist consent", error),
            }
        }
        Request::HostWorldUpdateStatus { world } => {
            let Some(world_id) = replica::body::WorldId::parse(&world) else {
                return Some(Response::invalid("invalid World id"));
            };
            if router.reviewed_world_implementation(&world_id).is_none() {
                return Some(Response::not_found(format!(
                    "World '{world}' is not installed for this identity"
                )));
            }
            let worlds = crate::serve::head::installations_root(router.catalog().identity());
            let world_for_job = world.clone();
            match router
                .run_blocking(move || crate::update::consent::load(&worlds, &world_for_job))
                .await
            {
                Ok(job) => Response::Host(HostReply::WorldUpdate { world, job }),
                Err(crate::orbits::BlockingFailure::Capacity) => Response::capacity(
                    "the bounded host lane cannot admit World update status right now",
                ),
                Err(error) => world_update_state_failure("read status", error),
            }
        }
        Request::DevicePairEnter { code } => {
            match router
                .pair()
                .enter(&code, crate::daemon::pair::now_ms())
                .await
            {
                Ok(crate::daemon::pair::SponsorOutcome::Offer(offer)) => {
                    Response::Host(HostReply::DevicePairOffer {
                        pairing: offer.pairing,
                        device: offer.device,
                        name: offer.name,
                        phrase: offer.phrase,
                        expires_at_ms: offer.expires_at_ms,
                    })
                }
                Ok(crate::daemon::pair::SponsorOutcome::Paired { device }) => {
                    Response::Host(HostReply::DevicePaired {
                        device: device.as_str().to_owned(),
                    })
                }
                Err(error) => Response::err(format!("{error:#}")),
            }
        }
        Request::DevicePairConfirm { pairing, accept } => {
            match router
                .pair()
                .confirm(&pairing, accept, crate::daemon::pair::now_ms())
                .await
            {
                Ok(Some(device)) => Response::Host(HostReply::DevicePaired {
                    device: device.as_str().to_owned(),
                }),
                Ok(None) => Response::Ok {
                    message: Some("the offer was rejected; nothing was written".into()),
                },
                Err(error) => Response::err(format!("{error:#}")),
            }
        }
        Request::HostContext => match config::list_identities() {
            Ok(identities) => Response::Host(HostReply::Context {
                version: crate::VERSION.to_string(),
                identity_home: router.catalog().identity().display().to_string(),
                spaces_root: config::spaces_root().display().to_string(),
                worlds: router
                    .packages()
                    .world_ids()
                    .map(ToString::to_string)
                    .collect(),
                identities,
                orbits: orbits::list(),
                asks: router.asks().list(),
                pairing: router.pair().status(crate::daemon::pair::now_ms()),
                pair_offers: router.pair().offers(crate::daemon::pair::now_ms()),
            }),
            Err(error) => Response::err(format!("{error:#}")),
        },
        _ => return None,
    })
}

fn world_update_state_failure(
    operation: &'static str,
    failure: crate::orbits::BlockingFailure,
) -> Response {
    match failure {
        crate::orbits::BlockingFailure::Capacity => Response::capacity(
            "the bounded host lane cannot admit this World update operation right now",
        ),
        crate::orbits::BlockingFailure::Join(error) => {
            tracing::warn!(operation, %error, "World update blocking worker failed");
            Response::err("the World update worker is temporarily unavailable")
        }
        crate::orbits::BlockingFailure::Work(error) => {
            let io = error
                .chain()
                .any(|cause| cause.downcast_ref::<std::io::Error>().is_some());
            if io {
                tracing::warn!(operation, %error, "World update state I/O failed");
                Response::err("World update state is temporarily unavailable")
            } else {
                tracing::error!(operation, %error, "World update state failed integrity validation");
                Response::invalid("stored World update state failed integrity validation")
            }
        }
    }
}

/// Run one blocking host operation off the reactor and shape its answer.
async fn blocking<F>(work: F) -> Response
where
    F: FnOnce() -> Result<HostReply> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(reply)) => Response::Host(reply),
        Ok(Err(error)) => target_error(error),
        Err(error) => Response::err(format!("host task failed: {error}")),
    }
}

/// Push a daemon-read key change to a running Station so it applies now rather
/// than at the next restart — and report which of the two happened.
///
/// Passive dispatch (`request_running`), never a placement: delivering an
/// advisory reload is not a reason to wake a Station nobody asked for.
async fn config_written(
    router: &Router,
    write: config::ConfigWrite,
    home: Option<&str>,
) -> Response {
    let mut applied = None;
    if write.daemon_read {
        if let Some(home) = home {
            let orbit = LocalOrbitId::for_store(Path::new(home));
            if let Ok(resolved) = router.resolve(orbit.as_str()) {
                let route = crate::control::station_route(resolved.address);
                applied = Some(matches!(
                    router.request_running(route, &Request::ConfigReload).await,
                    Ok(Response::Ok { .. })
                ));
            }
        }
    }
    Response::Host(HostReply::ConfigWritten { write, applied })
}

/// The store layer a settings request may address, or the refusal to send back.
///
/// A settings key lives in `<home>/config.json`, and `home` is whatever the
/// caller put in the request. Unadmitted, a write lands in any directory on the
/// machine and a read returns any `config.json` on it — the verb this replaced
/// could do neither, because a client derived its own home from a store it had
/// already found. Admission moved with the request: the client's filter is a
/// convenience now, this is the rule.
///
/// Reads go through it too. `user.nick` and `project.default` out of a directory
/// the caller merely names is still a directory they were never granted, and the
/// read is the reconnaissance the write needs.
fn admit_settings_home(router: &Router, home: Option<&str>) -> Result<Option<PathBuf>, Response> {
    let Some(home) = home else {
        return Ok(None);
    };
    match admit(router, Path::new(home)) {
        Ok(resolved) => Ok(Some(resolved.home)),
        Err(error) => Err(Response::err(format!("{home}: {error:#}"))),
    }
}

/// One setting's value as a scalar. A value nobody configured is marked
/// `(default)`, so a reader can still tell an answer that came from a layer
/// apart from one that came from the build.
fn config_value(row: &config::ConfigRow) -> String {
    let value = row.value.as_deref().unwrap_or_default();
    match row.origin {
        config::ConfigOrigin::Default => format!("{value} (default)"),
        _ => value.to_string(),
    }
}

/// A settings failure, keeping "unset" distinguishable from "bad key".
fn config_error(error: anyhow::Error) -> Response {
    match error.downcast_ref::<config::ConfigUnset>() {
        Some(unset) => Response::not_found(unset.to_string()),
        None => Response::err(format!("{error:#}")),
    }
}

/// A formation failure. A directory bound to another Space (or holding a store
/// this build cannot read) is a *lookup* answer — the caller aimed at the wrong
/// place — while an occupied or ambiguous directory is a plain refusal: the aim
/// was right and the answer is no.
fn target_error(error: anyhow::Error) -> Response {
    match error.downcast_ref::<TargetRefusal>() {
        Some(TargetRefusal::WrongSpace { .. } | TargetRefusal::Unsupported { .. }) => {
            Response::not_found(format!("{error}"))
        }
        _ => Response::err(format!("{error:#}")),
    }
}

/// A selector failure resolves to nothing, which is `NotFound` on every
/// surface — the same answer a missing ref or label gets.
fn selector_error(error: anyhow::Error) -> Response {
    match error.downcast_ref::<orbits::Unresolved>() {
        Some(selection) => Response::not_found(selection.to_string()),
        None => Response::err(format!("{error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two refusals a formation request answers with, and the kinds they
    /// carry. `WrongSpace` is `NotFound` because the caller aimed somewhere
    /// else; `Occupied` is a plain error because the aim was right and the
    /// answer is no.
    #[test]
    fn a_misaimed_formation_is_a_lookup_answer_and_an_occupied_one_is_not() {
        let wrong = target_error(
            TargetRefusal::WrongSpace {
                home: PathBuf::from("/tmp/x"),
                holds: "ws_a".into(),
                invite: "ws_b".into(),
            }
            .into(),
        );
        assert!(matches!(
            wrong,
            Response::Error {
                error_kind: crate::control::ErrorKind::NotFound,
                ..
            }
        ));
        let occupied = target_error(
            TargetRefusal::Occupied {
                home: PathBuf::from("/tmp/x"),
                space: "ws_a".into(),
            }
            .into(),
        );
        assert!(matches!(
            occupied,
            Response::Error {
                error_kind: crate::control::ErrorKind::Error,
                ..
            }
        ));
    }

    /// A settings request names a store, so the store has to be one this daemon
    /// serves — whether the request reads or writes.
    ///
    /// **The failure this prevents:** a caller-chosen `home` reaching
    /// `Settings::load` unadmitted, which turns a host-plane read into "print
    /// `<any path>/config.json`" for anything holding a loopback token.
    #[test]
    fn a_settings_request_cannot_name_a_directory_this_daemon_does_not_serve() {
        let served = PathBuf::from("/served-for-tests");
        let router = Router::new(
            orbits::Catalog::with_entries(
                served.clone(),
                PathBuf::from("/agents-for-tests"),
                false,
                vec![Entry {
                    space: mechanics::ids::SpaceId::from_digest([3; 16]).to_string(),
                    name: "Served".into(),
                    path: served.display().to_string(),
                    origin: Origin::Founded,
                    host_nick: String::new(),
                    last_opened: 0,
                }],
            ),
            crate::world::packages(),
        );

        // Naming no store is the global/user layers, which need no admission.
        assert!(admit_settings_home(&router, None).is_ok());
        assert!(admit_settings_home(&router, Some(&served.display().to_string())).is_ok());
        assert!(admit_settings_home(&router, Some("/somewhere-else-entirely")).is_err());
    }

    /// Entering a directory whose key is not this daemon's must not spend that
    /// key — whether or not a Space is sitting in it yet.
    ///
    /// **The failure this prevents:** `enter` supports a re-join — a store that
    /// already holds the invite's Space is left exactly as it is — so a host
    /// request aimed at an agent's home wrote that home's `config.json` and then
    /// drove `Connect` through a Station that loads its seed from it. The
    /// admission handshake went out on the wire over the agent's signature,
    /// which is the very thing `/api/spaces/{id}/rpc` refuses for that Orbit.
    ///
    /// **And the one that escaped:** a provisioned agent home holds its seed
    /// before it holds a Space. Asking "is there a Space here?" first answered
    /// "blank directory, therefore mine" for exactly that home — the gate waved
    /// through the case it was written for, and the very first `Connect` of the
    /// join went out over the agent's key.
    #[test]
    fn re_entering_a_store_this_daemon_only_hosts_is_refused() {
        let base = std::env::temp_dir().join(format!("lait-reenter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let identity = base.join("identity");
        let agents = base.join("agents");
        let space = mechanics::ids::SpaceId::from_digest([5; 16]);
        std::fs::create_dir_all(&identity).expect("identity dir");

        let with_store = |home: PathBuf| {
            std::fs::create_dir_all(
                crate::orbital::orbital_store_root(&home).join(space.to_string()),
            )
            .expect("plant a store");
            config::canonical(&home)
        };
        let mine = with_store(base.join("mine"));
        let hosted = with_store(agents.join("scout"));
        let unregistered = with_store(agents.join("ghost"));
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).expect("empty dir");
        // A provisioned agent identity: a seed on disk, no Space yet. This is
        // the shape a fresh sponsored agent has right up until it joins one.
        let provisioned = agents.join("fresh");
        std::fs::create_dir_all(&provisioned).expect("agent identity dir");
        config::load_or_create_identity(&provisioned).expect("mint the agent's seed");
        let provisioned = config::canonical(&provisioned);

        let entry = |home: &Path| Entry {
            space: space.to_string(),
            name: "Re".into(),
            path: home.display().to_string(),
            origin: Origin::Joined,
            host_nick: String::new(),
            last_opened: 0,
        };
        let router = Router::new(
            orbits::Catalog::with_entries(
                config::canonical(&identity),
                config::canonical(&agents),
                false,
                vec![entry(&mine), entry(&hosted)],
            ),
            crate::world::packages(),
        );

        // A store this daemon signs for re-enters, and a directory that holds
        // neither a Space nor a key of somebody else's is the blank target
        // formation exists for.
        assert!(admit_formation_target(&router, &mine).is_ok());
        assert!(admit_formation_target(&router, &empty).is_ok());
        // The agent's home does not — registered or not. Entering is what writes
        // the registry row, so the row cannot be the only thing asked.
        assert!(admit_formation_target(&router, &hosted).is_err());
        assert!(admit_formation_target(&router, &unregistered).is_err());
        // Nor does an agent home that holds only its seed. "No Space here" is
        // not "no key here", and this is the directory a first join names.
        assert!(
            admit_formation_target(&router, &provisioned).is_err(),
            "a seed with no Space beside it is still somebody else's seed"
        );
        // And nor does another spelling of the same directory. The predicate
        // answers from the filesystem, so a path that walks out and back in
        // reaches the same custody answer as the plain one — where comparing
        // the text answered "not under agents/, therefore mine".
        let sibling = base.join("mine").join("..").join("agents").join("scout");
        assert!(
            admit_formation_target(&router, &sibling).is_err(),
            "'..' is not a different directory"
        );
        // And a spelling that resolves to nothing at all is refused rather than
        // compared as text. `<base>/nope/../agents/newcomer` does not begin with
        // `agents/` as a string, and is a home in the agents area the moment
        // anything creates it.
        let unresolvable = base.join("nope").join("..").join("agents").join("newcomer");
        assert!(
            admit_formation_target(&router, &unresolvable).is_err(),
            "a path this cannot resolve is not a path it can answer for"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A daemon whose identity is its own and which hosts one agent home, with
    /// nothing in the registry: the state a formation request arrives in.
    fn hosting_one_agent(label: &str) -> (PathBuf, Router, PathBuf) {
        let base = std::env::temp_dir().join(format!("lait-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let identity = base.join("identity");
        let agents = base.join("agents");
        std::fs::create_dir_all(&identity).expect("identity dir");
        // A provisioned agent: its seed is on disk before any Space is.
        let agent = agents.join("scout");
        std::fs::create_dir_all(&agent).expect("agent identity dir");
        config::load_or_create_identity(&agent).expect("mint the agent's seed");
        let router = Router::new(
            orbits::Catalog::with_entries(
                config::canonical(&identity),
                config::canonical(&agents),
                false,
                vec![],
            ),
            crate::world::packages(),
        );
        (base, router, config::canonical(&agent))
    }

    fn refusal_message(response: Option<Response>) -> String {
        match response {
            Some(Response::Error { message, .. }) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Founding is gated on custody exactly as entering is.
    ///
    /// **The failure this prevents:** `HostSpaceFound` took the same
    /// caller-named directory and ran no custody check at all, so anything
    /// holding this server's loopback token could plant a Space store and a
    /// `config.json` inside another identity's home — and the Station that then
    /// signs for that home is the agent's, not the caller's.
    #[tokio::test]
    async fn founding_into_a_home_this_daemon_only_hosts_is_refused() {
        let (base, router, agent) = hosting_one_agent("found-custody");
        let refusal = refusal_message(
            dispatch(
                &router,
                Request::HostSpaceFound {
                    home: agent.display().to_string(),
                    name: "Borrowed".into(),
                    nick: None,
                },
            )
            .await,
        );
        assert!(
            refusal.contains("hosted here under its own key"),
            "founding into an agent home must be refused for custody, got: {refusal}"
        );
        assert!(
            !crate::orbital::orbital_store_root(&agent).exists(),
            "the refusal must land before anything is planted"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The gate and the write have to mean the same directory, and the gate has
    /// to keep letting a blank target through.
    ///
    /// **The failure this prevents:** the target was gated as a *string* and
    /// created afterwards. `<base>/nope/../agents/newcomer` resolves to nothing
    /// while it does not exist, so the gate compared text, saw a path that does
    /// not begin with `agents/`, and admitted it — and then `create_dir_all`
    /// walked it straight into the agents area and bound an Orbit whose Station
    /// signs with a seed sitting in there.
    ///
    /// **And why refusing the unresolvable is not enough on its own:** the
    /// second half of this test is the ordinary case — entering a directory that
    /// does not exist yet, which is most joins. Creating it first is what lets
    /// the gate be strict without refusing every legitimate blank target.
    #[tokio::test]
    async fn a_formation_target_is_resolved_before_it_is_gated() {
        let (base, router, _) = hosting_one_agent("materialise");
        let enter_into = |home: PathBuf| {
            dispatch(
                &router,
                Request::HostSpaceEnter {
                    // Deliberately unparseable: an invite error means the path
                    // got past custody, which is the distinction under test.
                    link: "lait://not-an-invite".into(),
                    home: home.display().to_string(),
                    nick: None,
                },
            )
        };

        let spelled = base
            .join("does-not-exist-yet")
            .join("..")
            .join("agents")
            .join("newcomer");
        let refusal = refusal_message(enter_into(spelled).await);
        assert!(
            refusal.contains("hosted here under its own key"),
            "a spelling that lands in the agents area is a home in the agents area, got: {refusal}"
        );

        let blank = refusal_message(enter_into(base.join("blank")).await);
        assert!(
            blank.contains("invalid invite link"),
            "a directory that does not exist yet is the ordinary join target, got: {blank}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unset_key_is_a_lookup_answer_and_an_unknown_one_is_a_usage_error() {
        let unset = config_error(
            config::ConfigUnset {
                key: "project.default".into(),
                in_layer: false,
            }
            .into(),
        );
        assert!(matches!(
            unset,
            Response::Error {
                error_kind: crate::control::ErrorKind::NotFound,
                ..
            }
        ));
        let unknown = config_error(anyhow!("unknown config key 'nope'"));
        assert!(matches!(
            unknown,
            Response::Error {
                error_kind: crate::control::ErrorKind::Error,
                ..
            }
        ));
    }

    #[test]
    fn world_update_state_hides_mechanical_detail_but_types_integrity() {
        let integrity = world_update_state_failure(
            "read status",
            crate::orbits::BlockingFailure::Work(anyhow!("secret decoder detail")),
        );
        assert!(matches!(
            integrity,
            Response::Error {
                error_kind: crate::control::ErrorKind::Invalid,
                ref message,
            } if message == "stored World update state failed integrity validation"
        ));

        let io = world_update_state_failure(
            "read status",
            crate::orbits::BlockingFailure::Work(
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "private path").into(),
            ),
        );
        assert!(matches!(
            io,
            Response::Error {
                error_kind: crate::control::ErrorKind::Error,
                ref message,
            } if message == "World update state is temporarily unavailable"
        ));
    }
}
