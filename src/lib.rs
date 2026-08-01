#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! lait: an orbital shell for local-first, peer-to-peer collaboration.
//!
//! One binary, four roles:
//!   * `lait daemon` is the identity-scoped host: one local process endpoint,
//!     an Orbit directory/router, identity-keyed transport hubs, and zero or
//!     more in-process StationHosts.
//!   * `lait <cmd>` is the cwd-scoped navigation client, selecting an Orbit and
//!     driving the daemon or one installed World over an explicit route.
//!   * `lait serve` binds that same façade to loopback HTTP + SSE so a browser
//!     can be a client too ([`serve`], `docs/UI.md`). It owns no Station.
//!   * `lait mcp` exposes the same Layer-B façade as MCP tools for an agent.
//!
//! The crate is split lib + bin so integration tests, doctests, and the MCP/DTO
//! parity check can exercise the same code the binary runs. See `docs/`.
//!
//! Layering (see `docs/ARCHITECTURE.md` and `docs/DATA-CONTRACT.md`):
//!   * **The substrate** (`mechanics`, `fabric`, `replica`, `comms`,
//!     `runtime`): authority, convergence, the Body graph, transport, and the
//!     orbital lifecycle, each behind its own crate boundary.
//!   * **Bundled products** ([`world`]): the composition root that docks
//!     independently packaged Worlds and mounts their client interfaces.
//!   * **Layer B — control protocol** ([`control`]): a stable,
//!     versioned, hand-maintained projection over the local socket. Never a
//!     dump of storage internals.

pub mod app;
pub mod cli;
pub mod client_action;
pub mod cmdspec;
pub mod config;
pub mod control;
pub mod daemon;
pub mod daemon_spawn;
pub mod diagnose;
pub mod install;
pub mod list_picker;
pub mod mcp;
pub mod members_ui;
/// The product's adoption of the orbital lifecycle (hosts a World, drives
/// Sessions through the public `runtime` API).
pub mod orbital;
pub mod orbits;
pub mod registry;
pub mod serve;
/// The composition adapter for independently packaged Worlds.
pub mod world;

/// Clean-env test entrypoint (step 0 of the Agent Experience initiative).
///
/// A developer's shell profile may export `$LAIT_HOME`/`$LAIT_STORE` (we hit
/// exactly this while operating multi-node), and the store/identity resolver in
/// [`config`] consults them. Inherited into the unit-test process, a stray value
/// silently redirected `config::tests::discovery_never_creates_but_init_path_does`
/// to a foreign store and failed it — a poisoned run masquerading as a real
/// regression. This runs at process load, *before* the test harness spawns any
/// test thread, so every lib unit test starts from a clean slate by
/// construction. Tests that *want* these vars set them explicitly afterward
/// (serialized by their own `ENV_LOCK`), so scrubbing here is safe for them.
///
/// Scoped to the lib's own unit tests (`cfg(test)`). Integration binaries link
/// the non-test lib and pass env deliberately (`Command::env`), so they are
/// unaffected — and immune already.
#[cfg(test)]
#[ctor::ctor]
fn scrub_ambient_lait_env() {
    for key in ["LAIT_HOME", "LAIT_STORE", "LAIT_CONFIG_ROOT"] {
        std::env::remove_var(key);
    }
}
