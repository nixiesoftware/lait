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
pub const MAX_PROGRAM_WINDOWS: usize = 32;
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
    /// Opaque native-media rendition or render-group published on this
    /// Orbit's lait-live plane. It is never a URL and never durable media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_resource: Option<String>,
    pub duration_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageWindow {
    pub id: String,
    pub window: schedule::Window,
    /// Ordered item identities selected while this window has precedence.
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageProgram {
    /// Canonical `BodyId` rendering, minted by the authenticated application
    /// adapter before the semantic World runs.
    pub id: String,
    pub name: String,
    pub cycle: ProgramCycle,
    pub items: Vec<SignageItem>,
    /// Empty preserves the authored program as an always-active playlist.
    #[serde(default)]
    pub windows: Vec<SignageWindow>,
}

pub struct ScheduledProgram<'a> {
    pub items: Vec<&'a SignageItem>,
    pub next_boundary_unix_ms: Option<u64>,
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
                || item.live_resource.as_ref().is_some_and(|resource| {
                    resource.is_empty()
                        || resource.len() > MAX_ITEM_ID_BYTES
                        || !resource.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'.' | b'-' | b'_')
                        })
                })
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
        if horizon > MAX_PROGRAM_HORIZON_MS || self.windows.len() > MAX_PROGRAM_WINDOWS {
            return false;
        }
        let mut window_ids = std::collections::BTreeSet::new();
        for scheduled in &self.windows {
            let valid_id = !scheduled.id.is_empty()
                && scheduled.id.len() <= MAX_ITEM_ID_BYTES
                && scheduled.id.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                });
            let mut selected = std::collections::BTreeSet::new();
            if !valid_id
                || !window_ids.insert(&scheduled.id)
                || scheduled.items.is_empty()
                || scheduled.items.len() > MAX_PROGRAM_ITEMS
                || scheduled.window.validate().is_err()
                || scheduled
                    .items
                    .iter()
                    .any(|item| !ids.contains(item) || !selected.insert(item))
                || scheduled.items.iter().enumerate().any(|(index, id)| {
                    self.items
                        .iter()
                        .find(|item| &item.id == id)
                        .is_some_and(|item| {
                            item.duration_ms.is_none() && index + 1 != scheduled.items.len()
                        })
                })
            {
                return false;
            }
        }
        true
    }

    pub fn scheduled_at(
        &self,
        now_unix_ms: u64,
    ) -> Result<ScheduledProgram<'_>, schedule::Invalid> {
        if self.windows.is_empty() {
            return Ok(ScheduledProgram {
                items: self.items.iter().collect(),
                next_boundary_unix_ms: None,
            });
        }
        let mut selected: Option<&SignageWindow> = None;
        let mut next_boundary = None;
        for scheduled in &self.windows {
            let (active, next) = scheduled.window.evaluate_at(now_unix_ms)?;
            if let Some(next) = next {
                next_boundary = Some(next_boundary.map_or(next, |current: u64| current.min(next)));
            }
            if active
                && selected.is_none_or(|current| {
                    scheduled.window.priority > current.window.priority
                        || (scheduled.window.priority == current.window.priority
                            && scheduled.id < current.id)
                })
            {
                selected = Some(scheduled);
            }
        }
        let items = selected.map_or_else(Vec::new, |scheduled| {
            scheduled
                .items
                .iter()
                .filter_map(|id| self.items.iter().find(|item| &item.id == id))
                .collect()
        });
        Ok(ScheduledProgram {
            items,
            next_boundary_unix_ms: next_boundary,
        })
    }

    pub fn schedule_overlaps_between(
        &self,
        range_start_unix_ms: u64,
        range_end_unix_ms: u64,
    ) -> Result<Vec<schedule::Overlap>, schedule::Invalid> {
        let windows: Vec<_> = self
            .windows
            .iter()
            .map(|scheduled| scheduled.window.clone())
            .collect();
        schedule::overlaps_between(&windows, range_start_unix_ms, range_end_unix_ms)
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

/// The things a Signage grant can be about.
///
/// Segments are matched byte-exactly, so these spellings are the grant format:
/// a holder granted on `screen/scr_a` is not granted on `screens/scr_a`, and
/// nothing warns. They are pinned by test for that reason.
///
/// There is no wildcard — Mechanics refuses one. A fleet-wide permission is a
/// grant on [`root_resource`]; "these screens" is a grant on a group, whose
/// membership this World resolves and Mechanics never learns.
pub mod resource {
    use super::{Resource, PRODUCT_WORLD};

    /// Everything this World owns. Fleet-wide.
    pub fn root() -> Resource {
        Resource::root(PRODUCT_WORLD)
    }

    /// One authored program.
    pub fn program(id: &str) -> Resource {
        segments("program", id)
    }

    /// One screen, by the identity the display coordinator enrolled it under.
    pub fn screen(id: &str) -> Resource {
        segments("screen", id)
    }

    /// A named set of screens. The indirection that replaces a wildcard.
    pub fn group(id: &str) -> Resource {
        segments("group", id)
    }

    /// One item in the media library.
    pub fn media(id: &str) -> Resource {
        segments("media", id)
    }

    /// An over-long or malformed id degrades to the root resource, which is
    /// *narrower* in effect: it demands fleet-wide standing rather than
    /// silently granting on a truncated name.
    fn segments(kind: &str, id: &str) -> Resource {
        Resource::segments(PRODUCT_WORLD, [kind, id]).unwrap_or_else(|_| root())
    }
}

fn root_resource() -> Resource {
    resource::root()
}

fn capability(name: &str) -> PolicyCapability {
    PolicyCapability::new(PRODUCT_WORLD, name)
}

/// Fleet-wide authority to author.
pub fn demand_manage() -> Vec<u8> {
    AuthorizationDemand::require(capability("space.signage.manage"), root_resource())
        .encode_canonical()
        .expect("canonical Signage manage demand")
}

/// Authority to author one program: granted on the program, or fleet-wide.
///
/// `Any` rather than a second capability, so a per-program grant is an
/// attenuation of the fleet-wide one rather than a parallel vocabulary that has
/// to be kept in step with it.
pub fn demand_manage_program(program: &str) -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(
            capability("space.signage.manage"),
            resource::program(program),
        ),
        AuthorizationDemand::require(capability("space.signage.manage"), root_resource()),
    ])
    .encode_canonical()
    .expect("canonical Signage program manage demand")
}

pub fn demand_read() -> Vec<u8> {
    AuthorizationDemand::require(capability("space.signage.read"), root_resource())
        .encode_canonical()
        .expect("canonical Signage read demand")
}

/// Authority to read one program: granted on the program, or fleet-wide.
pub fn demand_read_program(program: &str) -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(capability("space.signage.read"), resource::program(program)),
        AuthorizationDemand::require(capability("space.signage.read"), root_resource()),
    ])
    .encode_canonical()
    .expect("canonical Signage program read demand")
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
                live_resource: None,
                duration_ms: Some(10_000),
            }],
            windows: Vec::new(),
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

        bad.items.insert(
            0,
            SignageItem {
                id: "intro".into(),
                title: "Introduction".into(),
                body: String::new(),
                background: "102030".into(),
                foreground: "ffffff".into(),
                live_resource: None,
                duration_ms: Some(1_000),
            },
        );
        bad.windows.push(SignageWindow {
            id: "reordered".into(),
            window: schedule::Window {
                start_local: "2026-08-15T10:00:00".into(),
                duration_ms: 60_000,
                recurrence: schedule::Recurrence::None,
                until_unix_ms: None,
                priority: 0,
                enabled: true,
                timezone: "UTC".into(),
                exceptions: Vec::new(),
            },
            items: vec!["welcome".into(), "intro".into()],
        });
        assert!(!bad.validate());
    }

    #[test]
    fn highest_priority_active_window_selects_items_and_exposes_the_next_boundary() {
        let mut program = program();
        program.items.push(SignageItem {
            id: "urgent".into(),
            title: "Urgent".into(),
            body: String::new(),
            background: "901010".into(),
            foreground: "ffffff".into(),
            live_resource: None,
            duration_ms: Some(10_000),
        });
        program.windows = vec![
            SignageWindow {
                id: "regular".into(),
                window: schedule::Window {
                    start_local: "2026-08-15T10:00:00".into(),
                    duration_ms: 60 * 60 * 1_000,
                    recurrence: schedule::Recurrence::None,
                    until_unix_ms: None,
                    priority: 0,
                    enabled: true,
                    timezone: "America/Chicago".into(),
                    exceptions: Vec::new(),
                },
                items: vec!["welcome".into()],
            },
            SignageWindow {
                id: "override".into(),
                window: schedule::Window {
                    start_local: "2026-08-15T10:30:00".into(),
                    duration_ms: 10 * 60 * 1_000,
                    recurrence: schedule::Recurrence::None,
                    until_unix_ms: None,
                    priority: 10,
                    enabled: true,
                    timezone: "America/Chicago".into(),
                    exceptions: Vec::new(),
                },
                items: vec!["urgent".into()],
            },
        ];
        assert!(program.validate());
        let now = u64::try_from(
            "2026-08-15T15:35:00Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
                .as_millisecond(),
        )
        .unwrap();
        let selected = program.scheduled_at(now).unwrap();
        assert_eq!(selected.items.len(), 1);
        assert_eq!(selected.items[0].id, "urgent");
        assert_eq!(
            selected.next_boundary_unix_ms,
            Some(
                u64::try_from(
                    "2026-08-15T15:40:00Z"
                        .parse::<jiff::Timestamp>()
                        .unwrap()
                        .as_millisecond()
                )
                .unwrap()
            )
        );
    }

    /// The grant format, pinned.
    ///
    /// Segments match byte-exactly, so a rename here silently stops matching
    /// every grant already minted against the old spelling. Nothing else in the
    /// system would report it.
    #[test]
    fn the_resource_spellings_are_the_grant_format() {
        assert_eq!(resource::root().segments, Vec::<String>::new());
        assert_eq!(resource::program("prg_1").segments, ["program", "prg_1"]);
        assert_eq!(resource::screen("scr_1").segments, ["screen", "scr_1"]);
        assert_eq!(
            resource::group("grp_lobby").segments,
            ["group", "grp_lobby"]
        );
        assert_eq!(resource::media("med_1").segments, ["media", "med_1"]);
        for r in [
            resource::program("p"),
            resource::screen("s"),
            resource::group("g"),
            resource::media("m"),
        ] {
            assert_eq!(r.world, PRODUCT_WORLD);
            assert!(r.validate().is_ok());
        }
    }

    #[test]
    fn an_unusable_id_demands_fleet_wide_standing_rather_than_a_truncated_grant() {
        let absurd = "x".repeat(mechanics::authorization::MAX_SEGMENT_BYTES + 1);
        assert_eq!(
            resource::screen(&absurd),
            resource::root(),
            "narrower, not wider"
        );
    }

    #[test]
    fn a_program_grant_and_a_fleet_grant_both_satisfy_writing_that_program() {
        let demand = AuthorizationDemand::decode_canonical(&demand_manage_program("prg_7"))
            .expect("canonical");
        let AuthorizationDemand::Any(options) = demand else {
            panic!("a scoped write is satisfied either way");
        };
        assert_eq!(options.len(), 2);
        assert!(options.contains(&AuthorizationDemand::require(
            capability("space.signage.manage"),
            resource::program("prg_7"),
        )));
        assert!(options.contains(&AuthorizationDemand::require(
            capability("space.signage.manage"),
            resource::root(),
        )));
    }

    #[test]
    fn one_programs_grant_does_not_reach_another() {
        assert_ne!(
            demand_manage_program("prg_1"),
            demand_manage_program("prg_2")
        );
        assert_ne!(demand_manage_program("prg_1"), demand_manage());
    }
}
