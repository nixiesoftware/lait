//! The product's orbital World adapter (C4): the frozen contract packet, the
//! parsed state/projection layer, and the registered `IssuesWorld`.
//!
//! See `docs/plans/04-product-world-contract.md` for the normative mapping.

use std::sync::Arc;

use crate::orbital::{
    LegacyWorldCodec, WorldCall, WorldCallError, WorldCallErrorCode, WorldPackage, WorldPackages,
    WorldReply,
};
use world_interface::WorldClientRegistry;

pub mod lifecycle;

pub use issues::{
    contract, roles, views, workflow, IssueEffect, IssueIntent, IssueQuery, IssuesWorld,
    PRODUCT_WORLD,
};

/// Temporary translation for historical typed SpaceBridge processes.
///
/// Product execution lives in `issues-app`; this adapter exists only at the
/// root control-protocol boundary and can disappear with that legacy protocol.
#[derive(Debug, Default)]
struct IssuesLegacyCodec;

impl IssuesLegacyCodec {
    fn product_request(
        request: &crate::control::Request,
    ) -> Result<issues_app::IssuesRequest, WorldCallError> {
        serde_json::from_value(serde_json::to_value(request).map_err(|error| {
            WorldCallError::new(
                WorldCallErrorCode::InvalidCall,
                format!("encode legacy Issues request: {error}"),
            )
        })?)
        .map_err(|error| {
            WorldCallError::new(
                WorldCallErrorCode::UnsupportedOperation,
                format!("request is not owned by IssuesWorld: {error}"),
            )
        })
    }
}

impl LegacyWorldCodec for IssuesLegacyCodec {
    fn handles(&self, request: &crate::control::Request) -> bool {
        Self::product_request(request).is_ok()
    }

    fn encode_call(&self, request: crate::control::Request) -> Result<WorldCall, WorldCallError> {
        issues_app::encode_call(&Self::product_request(&request)?)
    }

    fn decode_call(&self, call: &WorldCall) -> Result<crate::control::Request, WorldCallError> {
        let request = issues_app::decode_call(call)?;
        serde_json::from_value(serde_json::to_value(request).map_err(|error| {
            WorldCallError::new(
                WorldCallErrorCode::InvalidCall,
                format!("encode Issues compatibility request: {error}"),
            )
        })?)
        .map_err(|error| {
            WorldCallError::new(
                WorldCallErrorCode::InvalidCall,
                format!("decode Issues compatibility request: {error}"),
            )
        })
    }
}

/// The issue tracker's complete compile-time World package.
///
/// Keeping semantic implementation, reviewed identity, and application control
/// adapter in one value makes this module the product composition root. The
/// daemon and SpaceBridge receive packages by injection and do not construct or
/// name IssuesWorld themselves.
pub fn package() -> WorldPackage {
    let control = Arc::new(issues_app::IssuesCallHandler);
    let legacy = Arc::new(IssuesLegacyCodec);
    WorldPackage::new(
        IssuesWorld::registration(),
        Arc::new(IssuesWorld::new()),
        implementation_id(),
    )
    .with_control(control)
    .with_legacy_codec(legacy)
}

/// Every product World bundled by the issue-tracker application.
pub fn packages() -> WorldPackages {
    WorldPackages::new().with_package(package())
}

/// Every client-facing World package mounted by the navigation shell.
pub fn client_packages() -> WorldClientRegistry {
    WorldClientRegistry::new()
        .with_package(issues_app::package().expect("valid bundled Issues client package"))
        .expect("non-conflicting bundled World client packages")
}

/// Select the product World for one request emitted by this application.
///
/// Host routing consumes the explicit result and never infers a World from the
/// current directory or from daemon-global state.
pub fn request_world(request: &crate::control::Request) -> Option<replica::ids::WorldId> {
    IssuesLegacyCodec.handles(request).then(contract::world_id)
}

/// Encode the issue tracker's historical typed request as a generic World call.
pub fn encode_call(request: crate::control::Request) -> Result<WorldCall, WorldCallError> {
    IssuesLegacyCodec.encode_call(request)
}

/// Decode a generic Issues reply for the historical CLI/MCP/viewer surfaces.
pub fn decode_reply(reply: WorldReply) -> crate::control::Response {
    IssuesLegacyCodec.decode_reply(reply)
}

/// The reviewed IssuesWorld implementation id shipped by this build.
pub fn implementation_id() -> [u8; 32] {
    issues_app::lifecycle::implementation_id()
}
