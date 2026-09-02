//! The two traits every World runner backend shares — a native process and a
//! wasm module alike. They name no transport, so they compile on every target
//! the runner stack reaches.

use std::sync::Arc;

use anyhow::Result;

/// The product-defined behavior of one World generation: it answers named
/// operations, calling back into its host as needed.
pub trait Service: Send + Sync + 'static {
    fn descriptor(&self) -> crate::ServiceDescriptor;

    fn call(
        &self,
        operation: &str,
        payload: &[u8],
        host: Arc<dyn Host>,
    ) -> Result<Vec<u8>, String> {
        let _ = (operation, payload, host);
        Err("unsupported World operation".to_string())
    }
}

/// The only route from a World back into its supervising host.
///
/// Operations and payloads are package-defined, while framing, correlation,
/// authentication, and bounds remain runner-owned.
pub trait Host: Send + Sync + 'static {
    fn call(&self, operation: &str, payload: &[u8]) -> Result<Vec<u8>, String>;
}
