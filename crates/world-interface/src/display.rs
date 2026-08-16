//! Package-owned display surfaces consumed by the generic coordinator.
//!
//! This stops at rendered host-side output. Receiver authentication,
//! assignments, assets, and wire programs belong to the coordinator.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use replica::body::WorldId;
use runtime::world::call::Access;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ClientInvocation, Failure};

pub const MAX_CANONICAL_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_SURFACE_ID_BYTES: usize = 96;
pub const MAX_SURFACE_TITLE_CHARS: usize = 96;
pub const MAX_LOCALE_BYTES: usize = 35;
pub const MAX_RENDERED_PROGRAM_ITEMS: usize = 16;
pub const MAX_RENDERED_ASSET_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RENDERED_STAGED_BYTES: usize = 48 * 1024 * 1024;
pub const MAX_RENDERED_ITEM_ID_BYTES: usize = 128;
pub const MAX_SPOKEN_SUMMARY_BYTES: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisplaySurfaceId(String);

impl DisplaySurfaceId {
    pub fn new(value: impl Into<String>) -> Result<Self, Failure> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_SURFACE_ID_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            });
        if !valid {
            return Err(Failure::new(format!(
                "display surface id must be 1..={MAX_SURFACE_ID_BYTES} lowercase ASCII letters, digits, '.', '_' or '-'"
            )));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayOutputKind {
    Frame,
    Media,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySurfaceDescriptor {
    pub id: DisplaySurfaceId,
    pub title: String,
    pub runtime_implementation: [u8; 32],
    pub contract_version: u32,
    pub input_contract_digest: [u8; 32],
    /// Identity of the package-owned rendering implementation. This is
    /// semantic, unlike the human title: changing pixels under the same World
    /// reply requires a new assignment grant.
    pub renderer_identity: [u8; 32],
    pub contract_digest: [u8; 32],
    pub outputs: BTreeSet<DisplayOutputKind>,
}

impl DisplaySurfaceDescriptor {
    /// Digest semantic fields. Display text is deliberately absent, so a title
    /// correction cannot retarget an assignment.
    pub fn expected_contract_digest(&self, world: &WorldId) -> [u8; 32] {
        let mut digest = Sha256::new();
        commit(&mut digest, b"astrolabe-display-surface-v1");
        commit(&mut digest, world.as_str().as_bytes());
        commit(&mut digest, self.id.as_str().as_bytes());
        commit(&mut digest, &self.runtime_implementation);
        commit(&mut digest, &self.contract_version.to_be_bytes());
        commit(&mut digest, &self.input_contract_digest);
        commit(&mut digest, &self.renderer_identity);
        for output in &self.outputs {
            commit(
                &mut digest,
                match output {
                    DisplayOutputKind::Frame => b"frame",
                    DisplayOutputKind::Media => b"media",
                },
            );
        }
        digest.finalize().into()
    }

    pub fn validate(&self, world: &WorldId) -> Result<(), Failure> {
        if self.title.trim().is_empty() || self.title.chars().count() > MAX_SURFACE_TITLE_CHARS {
            return Err(Failure::new(format!(
                "display surface '{}' title must be 1..={MAX_SURFACE_TITLE_CHARS} characters",
                self.id.as_str()
            )));
        }
        if self.runtime_implementation == [0; 32] {
            return Err(Failure::new(format!(
                "display surface '{}' has no runtime implementation",
                self.id.as_str()
            )));
        }
        if self.contract_version == 0 {
            return Err(Failure::new(format!(
                "display surface '{}' contract version must be non-zero",
                self.id.as_str()
            )));
        }
        if self.input_contract_digest == [0; 32] {
            return Err(Failure::new(format!(
                "display surface '{}' has no input contract digest",
                self.id.as_str()
            )));
        }
        if self.renderer_identity == [0; 32] {
            return Err(Failure::new(format!(
                "display surface '{}' has no renderer identity",
                self.id.as_str()
            )));
        }
        if self.outputs.is_empty() {
            return Err(Failure::new(format!(
                "display surface '{}' declares no output kind",
                self.id.as_str()
            )));
        }
        if self.contract_digest != self.expected_contract_digest(world) {
            return Err(Failure::new(format!(
                "display surface '{}' contract digest does not match its descriptor",
                self.id.as_str()
            )));
        }
        Ok(())
    }
}

fn commit(digest: &mut Sha256, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    digest.update(len.to_be_bytes());
    digest.update(bytes);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalDisplayInput(Vec<u8>);

impl CanonicalDisplayInput {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Failure> {
        if bytes.len() > MAX_CANONICAL_INPUT_BYTES {
            return Err(Failure::new(format!(
                "canonical display input is {} bytes; limit is {MAX_CANONICAL_INPUT_BYTES}",
                bytes.len()
            )));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayTheme {
    Light,
    Dark,
    HighContrast,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayRequest {
    pub surface: DisplaySurfaceId,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u16,
    pub theme: DisplayTheme,
    pub locale: String,
    pub window_start_unix: u64,
    pub window_horizon_ms: u32,
    pub input: CanonicalDisplayInput,
}

impl DisplayRequest {
    pub fn validate(&self) -> Result<(), Failure> {
        if self.width == 0 || self.height == 0 || self.scale_milli == 0 {
            return Err(Failure::new(
                "display request dimensions and scale must be non-zero",
            ));
        }
        if self.locale.is_empty() || self.locale.len() > MAX_LOCALE_BYTES || !self.locale.is_ascii()
        {
            return Err(Failure::new(format!(
                "display locale must be 1..={MAX_LOCALE_BYTES} ASCII bytes"
            )));
        }
        if self.window_horizon_ms == 0 {
            return Err(Failure::new("display request horizon must be non-zero"));
        }
        Ok(())
    }
}

pub type DisplayPrepare = fn(&DisplayRequest) -> Result<ClientInvocation, Failure>;
pub type DisplayCanonicalizeInput = fn(Value) -> Result<CanonicalDisplayInput, Failure>;
pub type DisplayProjectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DisplayProjection, Failure>> + Send + 'a>>;

pub trait DisplayRenderer: Send + Sync {
    fn project<'a>(&'a self, value: Value, request: &'a DisplayRequest)
        -> DisplayProjectFuture<'a>;
}

#[derive(Clone)]
pub struct DisplaySurface {
    pub descriptor: DisplaySurfaceDescriptor,
    pub canonicalize_input: DisplayCanonicalizeInput,
    pub prepare: DisplayPrepare,
    pub renderer: Arc<dyn DisplayRenderer>,
}

#[derive(Debug, Clone)]
pub struct DisplayProjection {
    pub program: RenderedProgram,
    pub assessment: DisplayAssessment,
    pub spoken_summary: Option<String>,
}

impl DisplayProjection {
    /// Enforce package-bound output kinds and allocation ceilings before the
    /// generic coordinator derives receiver identifiers or caches any bytes.
    pub fn validate_for(
        &self,
        descriptor: &DisplaySurfaceDescriptor,
        request: &DisplayRequest,
    ) -> Result<(), Failure> {
        if self.program.items.is_empty() || self.program.items.len() > MAX_RENDERED_PROGRAM_ITEMS {
            return Err(Failure::new(
                "rendered display program item count is out of bounds",
            ));
        }
        if self
            .program
            .refresh_after_ms
            .is_some_and(|refresh| refresh == 0 || refresh > request.window_horizon_ms)
        {
            return Err(Failure::new(
                "rendered display refresh boundary is out of bounds",
            ));
        }
        bounded_summary(self.spoken_summary.as_deref())?;
        let mut staged = 0usize;
        let mut ids = BTreeSet::new();
        for item in &self.program.items {
            if item.id.is_empty()
                || item.id.len() > MAX_RENDERED_ITEM_ID_BYTES
                || !ids.insert(item.id.as_str())
            {
                return Err(Failure::new("rendered display item identity is invalid"));
            }
            bounded_summary(item.spoken_summary.as_deref())?;
            match &item.scene {
                RenderedScene::Frame(frame) => {
                    if !descriptor.outputs.contains(&DisplayOutputKind::Frame)
                        || frame.width != request.width
                        || frame.height != request.height
                        || frame.bytes.is_empty()
                        || frame.bytes.len() > MAX_RENDERED_ASSET_BYTES
                    {
                        return Err(Failure::new(
                            "rendered display frame violates its surface contract",
                        ));
                    }
                    staged = staged.checked_add(frame.bytes.len()).ok_or_else(|| {
                        Failure::new("rendered display assets exceed their bound")
                    })?;
                }
                RenderedScene::Media(_) => {
                    if !descriptor.outputs.contains(&DisplayOutputKind::Media) {
                        return Err(Failure::new(
                            "rendered display media violates its surface contract",
                        ));
                    }
                }
                RenderedScene::Blank(_) => {}
            }
        }
        if staged > MAX_RENDERED_STAGED_BYTES {
            return Err(Failure::new(
                "rendered display assets exceed their staging bound",
            ));
        }
        Ok(())
    }
}

fn bounded_summary(summary: Option<&str>) -> Result<(), Failure> {
    if summary.is_some_and(|value| {
        value.is_empty()
            || value.len() > MAX_SPOKEN_SUMMARY_BYTES
            || value.chars().any(char::is_control)
    }) {
        return Err(Failure::new("display spoken summary is invalid"));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RenderedProgram {
    pub items: Vec<RenderedProgramItem>,
    pub cycle: ProgramCycle,
    pub refresh_after_ms: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct RenderedProgramItem {
    pub id: String,
    pub duration_ms: Option<u32>,
    pub scene: RenderedScene,
    pub assessment: DisplayAssessment,
    pub spoken_summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramCycle {
    HoldLast,
    Loop,
    PollAtEnd,
    BlankAtEnd,
}

#[derive(Debug, Clone)]
pub enum DisplayAssessment {
    Current,
    Partial(BTreeSet<DisplayPartialReason>),
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayPartialReason {
    ProvisionalData,
    CorruptRecords,
    IncompleteProjection,
    DegradedSource,
}

#[derive(Debug, Clone)]
pub enum RenderedScene {
    Frame(RenderedFrame),
    Media(RenderedMedia),
    Blank(BlankReason),
}

#[derive(Debug, Clone)]
pub struct RenderedFrame {
    pub media_type: FrameMediaType,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameMediaType {
    Png,
    Jpeg,
    WebP,
}

#[derive(Debug, Clone)]
pub struct RenderedMedia {
    pub resource: DisplayResourceId,
    pub protocol: MediaProtocol,
    pub live: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaProtocol {
    Mse,
    Hls,
    Dash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayResourceId(String);

impl DisplayResourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, Failure> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_SURFACE_ID_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            });
        if !valid {
            return Err(Failure::new("invalid display resource id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlankReason {
    SourceUnavailable,
    Unsupported,
    ProgramEnded,
}

/// The trusted daemon classifier enforces this independently of the outer
/// package declaration.
pub const REQUIRED_WORLD_ACCESS: Access = Access::Query;

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> (WorldId, DisplaySurfaceDescriptor) {
        let world = WorldId::parse("com.example.signage").unwrap();
        let mut descriptor = DisplaySurfaceDescriptor {
            id: DisplaySurfaceId::new("signage.program").unwrap(),
            title: "Program".into(),
            runtime_implementation: [7; 32],
            contract_version: 1,
            input_contract_digest: [9; 32],
            renderer_identity: [10; 32],
            contract_digest: [0; 32],
            outputs: BTreeSet::from([DisplayOutputKind::Frame]),
        };
        descriptor.contract_digest = descriptor.expected_contract_digest(&world);
        (world, descriptor)
    }

    #[test]
    fn descriptor_digest_binds_semantics_but_not_title() {
        let (world, mut descriptor) = descriptor();
        descriptor.validate(&world).unwrap();
        let digest = descriptor.contract_digest;
        descriptor.title = "Renamed".into();
        assert_eq!(descriptor.expected_contract_digest(&world), digest);
        descriptor.contract_version = 2;
        assert_ne!(descriptor.expected_contract_digest(&world), digest);
    }

    #[test]
    fn canonical_input_and_surface_ids_are_bounded() {
        assert!(DisplaySurfaceId::new("Signage Program").is_err());
        assert!(CanonicalDisplayInput::new(vec![0; MAX_CANONICAL_INPUT_BYTES + 1]).is_err());
    }
}
