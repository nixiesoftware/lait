#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "the reviewed contract uses compile-time identifiers and bounded item indices"
)]

use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use serde::{Deserialize, Serialize};

pub const PRODUCT_WORLD: &str = "com.lait.signage";
pub const PROGRAM_SCHEMA: &str = "signage.program";
pub const PROGRAM_SCHEMA_VERSION: u32 = 1;
pub const MAX_PROGRAM_ITEMS: usize = 16;
pub const MAX_PROGRAM_NAME_CHARS: usize = 96;
pub const MAX_ITEM_TITLE_CHARS: usize = 160;
pub const MAX_ITEM_BODY_CHARS: usize = 800;
pub const MAX_ITEM_ID_BYTES: usize = 64;
pub const MIN_ITEM_DURATION_MS: u32 = 250;
pub const MAX_ITEM_DURATION_MS: u32 = 86_400_000;
pub const MAX_PROGRAM_HORIZON_MS: u64 = 86_400_000;

pub fn world_id() -> WorldId {
    WorldId::parse(PRODUCT_WORLD).expect("reviewed Signage World id")
}

pub fn program_schema() -> SchemaId {
    SchemaId::parse(PROGRAM_SCHEMA).expect("reviewed Signage schema id")
}

pub fn program_encoding() -> EncodingId {
    EncodingId::parse("json").expect("reviewed Signage encoding id")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramCycle {
    HoldLast,
    Loop,
    PollAtEnd,
    BlankAtEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Lowercase six-digit RGB, without '#'.
    pub background: String,
    /// Lowercase six-digit RGB, without '#'.
    pub foreground: String,
    pub duration_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageProgram {
    /// Canonical `BodyId` rendering, minted by the authenticated application
    /// adapter before the semantic World runs.
    pub id: String,
    pub name: String,
    pub cycle: ProgramCycle,
    pub items: Vec<SignageItem>,
}

impl SignageProgram {
    pub fn validate(&self) -> bool {
        if BodyId::parse(&self.id).is_none()
            || self.name.trim().is_empty()
            || self.name.chars().count() > MAX_PROGRAM_NAME_CHARS
            || self.items.is_empty()
            || self.items.len() > MAX_PROGRAM_ITEMS
        {
            return false;
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut horizon = 0u64;
        for (index, item) in self.items.iter().enumerate() {
            let valid_id = !item.id.is_empty()
                && item.id.len() <= MAX_ITEM_ID_BYTES
                && item.id.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                });
            if !valid_id
                || !ids.insert(&item.id)
                || item.title.trim().is_empty()
                || item.title.chars().count() > MAX_ITEM_TITLE_CHARS
                || item.body.chars().count() > MAX_ITEM_BODY_CHARS
                || !valid_rgb(&item.background)
                || !valid_rgb(&item.foreground)
            {
                return false;
            }
            match item.duration_ms {
                Some(duration)
                    if (MIN_ITEM_DURATION_MS..=MAX_ITEM_DURATION_MS).contains(&duration) =>
                {
                    horizon = match horizon.checked_add(u64::from(duration)) {
                        Some(value) => value,
                        None => return false,
                    };
                }
                None if index + 1 == self.items.len()
                    && matches!(self.cycle, ProgramCycle::HoldLast) => {}
                _ => return false,
            }
        }
        horizon <= MAX_PROGRAM_HORIZON_MS
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        BodyId::parse(&self.id).map(|body| BodyKey::new(world_id(), body))
    }
}

fn valid_rgb(value: &str) -> bool {
    value.len() == 6
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignageIntent {
    Put { program: SignageProgram },
    Delete { program: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignageQuery {
    Program { program: String },
    Programs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignageProjection {
    Program { program: Option<SignageProgram> },
    Programs { programs: Vec<SignageProgram> },
}

pub fn body_key(program: &str) -> Option<BodyKey> {
    BodyId::parse(program).map(|body| BodyKey::new(world_id(), body))
}

fn root_resource() -> Resource {
    Resource::root(PRODUCT_WORLD)
}

fn capability(name: &str) -> PolicyCapability {
    PolicyCapability::new(PRODUCT_WORLD, name)
}

pub fn demand_manage() -> Vec<u8> {
    AuthorizationDemand::require(capability("space.signage.manage"), root_resource())
        .encode_canonical()
        .expect("canonical Signage manage demand")
}

pub fn demand_read() -> Vec<u8> {
    AuthorizationDemand::require(capability("space.signage.read"), root_resource())
        .encode_canonical()
        .expect("canonical Signage read demand")
}

pub fn founder_capabilities() -> Vec<(PolicyCapability, Resource)> {
    ["space.signage.manage", "space.signage.read"]
        .into_iter()
        .map(|name| (capability(name), root_resource()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program() -> SignageProgram {
        SignageProgram {
            id: BodyId::from_bytes([3; 16]).render(),
            name: "Lobby".into(),
            cycle: ProgramCycle::Loop,
            items: vec![SignageItem {
                id: "welcome".into(),
                title: "Welcome".into(),
                body: "Open house at 6".into(),
                background: "102030".into(),
                foreground: "ffffff".into(),
                duration_ms: Some(10_000),
            }],
        }
    }

    #[test]
    fn programs_are_bounded_and_have_one_canonical_identity() {
        assert!(program().validate());
        let mut bad = program();
        bad.items[0].duration_ms = None;
        assert!(!bad.validate());
        bad.cycle = ProgramCycle::HoldLast;
        assert!(bad.validate());
    }
}
