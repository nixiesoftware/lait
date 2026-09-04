//! porthole — the lait engine, packaged to run one World in a browser tab.
//!
//! A person opens an invite link in a tab on a device with no daemon installed,
//! and reaches a Space through this crate: [`handle::boot`] stands the daemon's
//! own engine up behind one `#[wasm_bindgen]` call — pull/enter onto OPFS,
//! compose [`runtime::browser::Station`], dock, wire the world-agnostic dispatch
//! and the Live-plane client — and hands back a [`handle::BrowserEngineHandle`]
//! the viewer's Worker drives. Nothing here names a World: the identity and
//! mount are inputs.
//!
//! The whole crate is `wasm32`-only. Its modules were proven as the `wasm-probe`
//! engine modules and moved here verbatim (internal `crate::` paths resolve the
//! same, since they were always one crate's worth of code); `wasm-probe` now
//! re-exports them so its headless-Chrome claims keep testing this exact code.
#![cfg(target_arch = "wasm32")]

pub mod dispatch;
pub mod handle;
pub mod live_client;
pub mod runner;
pub mod session;
pub mod space_pull;

/// Installed once at module load, before any `boot`/`found` runs: a Rust panic
/// then surfaces as a console error carrying its message and source location,
/// instead of the bare `RuntimeError: unreachable` the wasm abort otherwise
/// shows (which names nothing and cannot be traced from a user's report). The
/// hook only formats the panic — it changes no behaviour and the process still
/// unwinds to the same abort.
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn __install_panic_hook() {
    console_error_panic_hook::set_once();
}
