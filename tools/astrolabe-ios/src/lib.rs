//! The Swift boundary of the native iOS client.
//!
//! One model, two shells: the Rust core owns client state, and this crate is a
//! generated bridge over it — UniFFI to Swift. It is the only generated
//! boundary left; the desktop one (flutter_rust_bridge to Dart) went with the
//! deprecated Flutter client, and Tauri's host links the core directly and
//! destructures its view exhaustively instead. The generated Swift is checked
//! in beside the application and exactly one Swift file may call through it.
//! (`build-core.sh` says CI fails on drift; that check is still unwired.)
//!
//! The view below is the interface design's shape rendered honestly at this
//! build's capability: the bundled World list is compile-time truth — the
//! Library, as on desktop — and the link state says *why* it is absent.
//! Chats and the Inbox render from nothing at all yet: correspondence rides
//! the mailbox primitive (a payload sealed once, unlocked per device), and
//! until that contract is issued their surfaces say so rather than carrying
//! empty collections pretending to be measured.

uniffi::setup_scaffolding!();

mod node;
pub use node::*;

use lait::composition;
use lait::orbits;

/// Whole immutable projection out — the only thing a surface may render.
#[derive(uniffi::Record)]
pub struct IosView {
    /// The engine build this application embeds.
    pub core_version: String,
    /// This phone's standing in the identity's linked-device set.
    pub link: LinkState,
    /// What this signed build bundles — the Library facts, compiled in.
    pub bundled_worlds: Vec<BundledWorld>,
    /// One row per joined Space, from the Orbit registry — advisory
    /// navigation state, never truth.
    pub spaces: Vec<SpaceRow>,
    /// The in-process head, when the node has started. A World cannot open
    /// without it, and its absence renders as "starting", not as an error.
    pub head: Option<HeadReady>,
}

/// The typed absence rule applies to the link itself: "cannot yet" and "has
/// not yet" are different facts and the interface words them differently.
#[derive(uniffi::Enum)]
pub enum LinkState {
    /// This build cannot link: the pairing contract (`lait/pair/1`) is not
    /// yet issued, so no ceremony can begin.
    Unavailable,
    /// Linking is possible and has not happened.
    Unlinked,
    /// This phone is a linked device of the selected identity.
    Linked { device_name: String, did: String },
}

/// A World this build carries, drawn from the compiled-in registry.
#[derive(uniffi::Record)]
pub struct BundledWorld {
    /// The published namespace key — machine input, never renamed.
    pub mount: String,
    /// What the World calls itself.
    pub name: String,
    /// One line under the name, when the World declared one.
    pub tagline: Option<String>,
    /// Packed 0xRRGGBB accent seed, when declared.
    pub accent: Option<u32>,
    /// Whether `Open` has anywhere to land. False is a real answer.
    pub openable: bool,
}

/// One Space, one row, however many providers advertise it.
#[derive(uniffi::Record)]
pub struct SpaceRow {
    pub space_id: String,
    pub name: String,
    /// The store path — the handle invite-minting keys on.
    pub path: String,
    /// The head's address for this Space (`orb_…`), when the store is
    /// present: `/spaces/{orbit_id}` in the served shell.
    pub orbit_id: Option<String>,
    pub status: SpaceStatus,
    pub worlds: Vec<SpaceWorldRow>,
}

/// The state vocabulary from the interface design — measured facts and typed
/// absences, never a bare "offline".
#[derive(uniffi::Enum)]
pub enum SpaceStatus {
    /// Measured: a named provider answered just now.
    Serving { provider: String },
    /// Measured: this phone's own node can serve it while foregrounded.
    ServingLocally,
    /// Measured: joined, and the inviter has not yet redeemed admission. The
    /// node keeps driving; the state ends itself.
    AdmissionPending,
    /// Measured: providers answered; none serves it.
    NotRunning,
    /// Unmeasured: no provider reachable. Says nothing about the Space.
    CouldNotBeAsked,
    /// The registry names a store this phone no longer holds.
    StoreMissing,
}

/// A World within a Space's disclosure.
#[derive(uniffi::Record)]
pub struct SpaceWorldRow {
    pub mount: String,
    pub name: String,
    pub accent: Option<u32>,
    /// False renders as the typed "not resident" absence.
    pub resident: bool,
}

/// The one read. Whole view out; no partial asks.
#[uniffi::export]
pub fn client_view() -> IosView {
    let registry = composition::bundled_client_packages();
    let bundled_worlds: Vec<BundledWorld> = registry
        .packages()
        .map(|package| {
            let display = package.display();
            BundledWorld {
                mount: package.mount().to_owned(),
                name: display.name().to_owned(),
                tagline: display.tagline().map(str::to_owned),
                accent: display.accent(),
                openable: display.entry_path().is_some(),
            }
        })
        .collect();

    // The head's *current* announcement: a paused head reads as absent, and
    // the tab surface renders that as its own state. Never a startup-frozen
    // copy — after a foreground restart the old one is a dead port.
    let head = node::node()
        .and_then(|node| node.head_ready())
        .map(|ready| HeadReady {
            url: ready.url.clone(),
            token: ready.token.clone(),
            port: ready.port,
        });

    // The registry is navigation state, never truth: names are advisory
    // snapshots, and a corrupt file degrades to "no known spaces".
    let spaces = orbits::list()
        .into_iter()
        .map(|entry| {
            let present = matches!(orbits::presence(&entry), orbits::Presence::Present);
            let path_buf = std::path::PathBuf::from(&entry.path);
            let status = if !present {
                SpaceStatus::StoreMissing
            } else {
                // Membership is asked passively: a placed Station answers, a
                // vacant one refuses, and nothing is placed just to draw a row.
                // The persisted pending invite is the row's measured "still
                // waiting on the inviter" — it outlives the driver's pass, so
                // the wait stays visible instead of silently expiring.
                let pending = path_buf.join(node::PENDING_INVITE).exists();
                match node::membership_of(&path_buf).as_deref() {
                    Some("member" | "admin") => SpaceStatus::ServingLocally,
                    Some(_) => SpaceStatus::AdmissionPending,
                    None if pending => SpaceStatus::AdmissionPending,
                    None if head.is_some() => SpaceStatus::ServingLocally,
                    None => SpaceStatus::NotRunning,
                }
            };
            let orbit_id = match lait::orbital::discover_space(&path_buf) {
                lait::orbital::SpaceStore::One(space_id) => Some(
                    lait::control::OrbitAddress::for_store(&path_buf, space_id)
                        .orbit
                        .to_string(),
                ),
                _ => None,
            };
            let worlds = bundled_worlds
                .iter()
                .filter(|world| world.openable)
                .map(|world| SpaceWorldRow {
                    mount: world.mount.clone(),
                    name: world.name.clone(),
                    accent: world.accent,
                    resident: present,
                })
                .collect();
            SpaceRow {
                space_id: entry.space.clone(),
                name: if entry.name.is_empty() {
                    entry.space
                } else {
                    entry.name
                },
                path: entry.path,
                orbit_id,
                status,
                worlds,
            }
        })
        .collect();

    IosView {
        core_version: lait::VERSION.to_owned(),
        link: LinkState::Unavailable,
        bundled_worlds,
        spaces,
        head,
    }
}
