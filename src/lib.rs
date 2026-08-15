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
//! One binary, three processes — and no command surface. Everything a verb used
//! to do is a request one of these carries:
//!   * `lait daemon` is the identity-scoped host: one local process endpoint,
//!     an Orbit directory/router, identity-keyed transport hubs, and zero or
//!     more in-process StationHosts.
//!   * `lait mcp` exposes the Layer-B façade as MCP tools for an agent.
//!   * bare `lait` binds that same façade to loopback HTTP + SSE so a browser
//!     can be a client too ([`serve`], `docs/UI.md`), and starts the daemon
//!     under it. It owns no Station.
//!
//! The two heads are *clients* — see [`host_client`] for the plumbing they
//! share. The crate is split lib + bin so integration tests, doctests, and the
//! MCP/DTO parity check can exercise the same code the binary runs; the binary
//! itself is only a launcher. See `docs/`.
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

/// This build, in the form releases are identified by: `<version>` for a
/// tagged build, `<version>-dev+<sha> (<date>)` for anything else (`build.rs`).
///
/// One constant so the launcher's `--version` and the host plane's orientation
/// reply can never disagree about which binary is running — the question
/// support has to answer first the moment two builds are in the field.
pub const VERSION: &str = env!("LAIT_VERSION_LONG");

pub mod client_action;
pub mod composition;
pub mod config;
pub mod control;
pub mod daemon;
pub mod daemon_spawn;
pub mod diagnose;
pub mod display;
pub mod dto;
pub mod host_client;
pub mod install;
pub mod mcp;
/// The product's adoption of the orbital lifecycle (hosts a World, drives
/// Sessions through the public `runtime` API).
pub mod orbital;
pub mod orbits;
pub mod process;
pub mod registry;
pub mod serve;
pub mod update;
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
