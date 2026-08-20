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
pub const PROGRAM_SCHEMA_VERSION: u32 = 2;
pub const MAX_PROGRAM_ITEMS: usize = 16;
pub const MAX_PROGRAM_WINDOWS: usize = 32;
pub const MAX_SCREEN_WINDOWS: usize = 32;
pub const MAX_PROGRAM_NAME_CHARS: usize = 96;
pub const MAX_ITEM_TITLE_CHARS: usize = 160;
pub const MAX_ITEM_BODY_CHARS: usize = 800;
pub const MAX_ITEM_ID_BYTES: usize = 64;
pub const MIN_ITEM_DURATION_MS: u32 = 250;
pub const MAX_ITEM_DURATION_MS: u32 = 86_400_000;
pub const MAX_PROGRAM_HORIZON_MS: u64 = 86_400_000;

/// The media library: what a stored file is, in the product's terms.
pub const MEDIA_SCHEMA: &str = "signage.media";
pub const MEDIA_SCHEMA_VERSION: u32 = 2;
/// One screen's fleet intent. Never its grant, and never its device lifecycle.
pub const SCREEN_SCHEMA: &str = "signage.screen";
pub const SCREEN_SCHEMA_VERSION: u32 = 2;
/// A named set of screens — the indirection that stands in for a wildcard.
pub const GROUP_SCHEMA: &str = "signage.group";
pub const GROUP_SCHEMA_VERSION: u32 = 2;

pub const MAX_NAME_CHARS: usize = 160;
pub const MAX_MIME_CHARS: usize = 96;
pub const MAX_GROUP_SCREENS: usize = 512;
/// A ceiling on one stored file, so a library row cannot describe a petabyte.
pub const MAX_MEDIA_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Taken from the display contract rather than chosen, because an id this
/// World accepts and a receiver cannot be told about is a program that blanks
/// for a reason nobody can see.
pub use world_interface::display::MAX_SURFACE_ID_BYTES as MAX_CONTENT_ID_BYTES;
pub const MAX_CONFIG_SETTINGS: usize = 64;
pub const MAX_SETTING_CHARS: usize = 1024;

/// What an integration is configured with, once per Space.
pub const CONFIG_SCHEMA: &str = "signage.config";
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

pub fn world_id() -> WorldId {
    WorldId::parse(PRODUCT_WORLD).expect("reviewed Signage World id")
}

pub fn program_schema() -> SchemaId {
    SchemaId::parse(PROGRAM_SCHEMA).expect("reviewed Signage schema id")
}

pub fn program_encoding() -> EncodingId {
    EncodingId::parse("json").expect("reviewed Signage encoding id")
}

pub fn media_schema() -> SchemaId {
    SchemaId::parse(MEDIA_SCHEMA).expect("reviewed Signage media schema id")
}

pub fn screen_schema() -> SchemaId {
    SchemaId::parse(SCREEN_SCHEMA).expect("reviewed Signage screen schema id")
}

pub fn group_schema() -> SchemaId {
    SchemaId::parse(GROUP_SCHEMA).expect("reviewed Signage group schema id")
}

pub fn config_schema() -> SchemaId {
    SchemaId::parse(CONFIG_SCHEMA).expect("reviewed Signage config schema id")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramCycle {
    HoldLast,
    Loop,
    PollAtEnd,
    BlankAtEnd,
}

/// One entry of a program: which library entry, and for how long.
///
/// Everything a program shows is a library entry, so an item names one rather
/// than carrying a shape of its own. That is Medusa's model too — a playlist
/// row was always `(content_id, position, duration)` — and it is what lets the
/// editor's four-phase save collapse to one write of this array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageItem {
    pub id: String,
    /// A [`SignageMedia`] id.
    pub media: String,
    /// Overrides the library entry's own duration. `None` takes that default.
    ///
    /// It does not also mean "hold indefinitely" — [`ProgramCycle::HoldLast`]
    /// says that, about the program, once. Two meanings on one absent value is
    /// how a caller ends up encoding intent in whether a field is missing.
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
        for item in &self.items {
            let valid_id = !item.id.is_empty()
                && item.id.len() <= MAX_ITEM_ID_BYTES
                && item.id.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                });
            if !valid_id || !ids.insert(&item.id) || BodyId::parse(&item.media).is_none() {
                return false;
            }
            // A declared duration is bounded and counts toward the horizon. An
            // absent one defers to the library entry, which this validation
            // cannot see — the program alone is not enough to know it, and
            // pretending otherwise would reject a legal program.
            match item.duration_ms {
                Some(duration)
                    if (MIN_ITEM_DURATION_MS..=MAX_ITEM_DURATION_MS).contains(&duration) =>
                {
                    horizon = match horizon.checked_add(u64::from(duration)) {
                        Some(value) => value,
                        None => return false,
                    };
                }
                None => {}
                Some(_) => return false,
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
    /// The program, and the library entries its items name.
    ///
    /// Resolved here because nothing downstream can resolve it later: a display
    /// surface gets one round trip, chosen before it has seen the program, and
    /// its renderer holds no handle to ask a second question. An item carries a
    /// media id, so something has to turn those into entries, and this is the
    /// only place that can.
    ///
    /// It is also what a browser wants. Medusa fetched a playlist and then its
    /// rows one by one; returning the set closes that N+1 rather than moving it.
    Program {
        program: Option<SignageProgram>,
        #[serde(default)]
        media: Vec<SignageMedia>,
    },
    Programs {
        programs: Vec<SignageProgram>,
    },
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

    /// One integration's configuration.
    pub fn config(id: &str) -> Resource {
        segments("config", id)
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

// ─── The library ────────────────────────────────────────────────────────────

/// What a library entry actually is.
///
/// Four kinds with four different lifetimes — authored here, durable on the
/// content plane, resolved by an integration, or live. A caller cannot write an
/// entry that is two of them, which is the shape Medusa lacked: there, every
/// row was a "content" row and the difference lived in a `kind` string and a
/// `url` that meant different things depending on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MediaSource {
    /// Authored text this World renders to a frame.
    Card {
        title: String,
        #[serde(default)]
        body: String,
        /// Lowercase six-digit RGB, without '#'.
        background: String,
        /// Lowercase six-digit RGB, without '#'.
        foreground: String,
    },
    /// Uploaded bytes on the content plane.
    Stored {
        /// The content id these bytes were committed under.
        content: String,
        /// Plaintext length as the uploader reported it.
        ///
        /// Carried rather than derived: a World cannot ask the content plane,
        /// and asking would mean handing it plaintext it must never hold. A
        /// wrong value is a wrong number on a screen, never a wrong file — the
        /// descriptor the declaration names is what the substrate checks.
        size: u64,
        mime: String,
    },
    /// An integration instance: an app decides what plays.
    ///
    /// `settings` is this instance's own — the video to play, the location to
    /// compute for. Whatever the *kind* needs once for the whole Space lives in
    /// a [`SignageConfig`] found by kind, and is deliberately not copied here:
    /// an entry that snapshotted the account it bills to would need a fan-out
    /// to correct, which is a consistency problem to create rather than solve.
    ///
    /// So the two halves are addressed differently on purpose — this one by
    /// value because it varies per entry, that one by kind because it does not.
    Kind {
        kind: String,
        #[serde(default)]
        settings: Settings,
    },
    /// An opaque rendition or render-group on this Orbit's lait-live plane.
    /// Never a URL, and never durable media.
    Live { resource: String },
}

impl MediaSource {
    fn validate(&self) -> bool {
        match self {
            Self::Card {
                title,
                body,
                background,
                foreground,
            } => {
                !title.trim().is_empty()
                    && title.chars().count() <= MAX_ITEM_TITLE_CHARS
                    && body.chars().count() <= MAX_ITEM_BODY_CHARS
                    && valid_rgb(background)
                    && valid_rgb(foreground)
            }
            Self::Stored {
                content,
                size,
                mime,
            } => {
                valid_content_id(content)
                    && *size > 0
                    && *size <= MAX_MEDIA_BYTES
                    && !mime.trim().is_empty()
                    && mime.chars().count() <= MAX_MIME_CHARS
            }
            Self::Kind { kind, settings } => valid_kind(kind) && valid_settings(settings),
            Self::Live { resource } => valid_kind(resource),
        }
    }

    /// The bytes this entry names, if it names any.
    ///
    /// What a media Effect declares as a content reference — and the reason
    /// the other three declare none rather than an empty one.
    pub fn content(&self) -> Option<&str> {
        match self {
            Self::Stored { content, .. } => Some(content),
            Self::Card { .. } | Self::Kind { .. } | Self::Live { .. } => None,
        }
    }
}

/// One entry in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageMedia {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub source: MediaSource,
    /// How long this entry plays when an item does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl SignageMedia {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && self.source.validate()
            && self
                .duration_ms
                .is_none_or(|d| (MIN_ITEM_DURATION_MS..=MAX_ITEM_DURATION_MS).contains(&d))
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

/// What one integration is configured with, once, for the whole Space.
///
/// A kind is "configured" exactly when one of these exists for it. Medusa
/// carried a boolean per app and hid unconfigured ones from the picker; a
/// document that either exists or does not says the same thing without a flag
/// to fall out of step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageConfig {
    pub id: String,
    /// The integration definition this configures, and the only way an entry
    /// reaches it. One per kind per Space, refused at write — see
    /// [`SignageConfig::conflicts_with`].
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub settings: Settings,
}

impl SignageConfig {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && valid_kind(&self.kind)
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && valid_settings(&self.settings)
    }

    /// Whether `other` already configures this kind under a different identity.
    ///
    /// The uniqueness a lookup by kind depends on. Two documents for one kind
    /// would make "is this kind configured" answerable two ways, and which one
    /// an entry got would depend on iteration order.
    pub fn conflicts_with(&self, other: &Self) -> bool {
        self.kind == other.kind && self.id != other.id
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

/// What an integration was given: an instance's parameters, or a kind's.
///
/// Values stay strings because this World does not know any integration's
/// field types — [`SignageConfig`] carries what an app was told, not what the
/// app means by it. Typing them here would put every integration's schema in
/// the substrate, which is the coupling the kind indirection exists to avoid.
pub type Settings = std::collections::BTreeMap<String, String>;

fn valid_settings(settings: &Settings) -> bool {
    settings.len() <= MAX_CONFIG_SETTINGS
        && settings.iter().all(|(key, value)| {
            !key.is_empty()
                && key.chars().count() <= MAX_NAME_CHARS
                && value.chars().count() <= MAX_SETTING_CHARS
        })
}

/// The grammar every window id shares.
fn valid_window_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ITEM_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

/// The soonest of the moments an answer could change on its own.
#[derive(Default)]
struct Boundary {
    soonest: Option<u64>,
}

impl Boundary {
    fn saw(&mut self, moment: Option<u64>) {
        if let Some(moment) = moment {
            self.soonest = Some(self.soonest.map_or(moment, |current| current.min(moment)));
        }
    }
}

/// An integration definition name, or a live rendition id.
fn valid_kind(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ITEM_ID_BYTES
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// A committed content id, as the upload route rendered it.
///
/// Checked for shape only. Whether the descriptor exists is the substrate's
/// question and it refuses the declaration if it does not.
fn valid_content_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CONTENT_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

// ─── The fleet ──────────────────────────────────────────────────────────────

/// What one screen should be showing, and nothing about whether it is.
///
/// Intent is replicated; the grant that lets a receiver fetch stays with the
/// coordinator. A row carrying both would make revocation a thing two planes
/// disagreed about, and the coordinator is the one holding the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageScreen {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Which program is in force, and an override that ends without a writer.
    #[serde(default)]
    pub intent: register::Slot,
    /// Windows that put a *different program* on this screen while they are
    /// open — distinct from a program's own windows, which choose among its
    /// items. Both exist because reusing one program across screens with
    /// unlike hours is the case that collapsing them would cost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedule: Vec<ProgramWindow>,
}

/// Which program a window puts on a screen, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramWindow {
    pub id: String,
    #[serde(flatten)]
    pub window: schedule::Window,
    pub program: String,
}

/// Which rung of the ladder answered.
///
/// Carried out with the answer because "why is this screen showing that" is
/// the question asked when it is showing the wrong thing, and a bare program
/// id cannot answer it. Medusa returned this string for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSource {
    Override,
    Schedule,
    Direct,
    Group,
}

/// What a screen plays, why, and when that changes on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playback {
    /// `None` is a screen with nothing to show, which is a state to draw
    /// rather than an error to report.
    pub program: Option<String>,
    /// `Some` exactly when `program` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PlaybackSource>,
    /// The earliest moment this answer changes with nobody writing, so a
    /// receiver can sleep until then instead of polling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_boundary_unix_ms: Option<u64>,
}

impl SignageScreen {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && self
                .group
                .as_ref()
                .is_none_or(|id| BodyId::parse(id).is_some())
            && self.intent.validate().is_ok()
            && self
                .intended_program()
                .is_none_or(|id| BodyId::parse(id).is_some())
            && self.schedule.len() <= MAX_SCREEN_WINDOWS
            && {
                let mut seen = std::collections::BTreeSet::new();
                self.schedule.iter().all(|scheduled| {
                    valid_window_id(&scheduled.id)
                        && seen.insert(&scheduled.id)
                        && BodyId::parse(&scheduled.program).is_some()
                        && scheduled.window.validate().is_ok()
                })
            }
    }

    /// The program this screen should show at `now`, override included.
    ///
    /// Resolution rather than storage: an override lapses because the clock
    /// passed it, so the answer changes with nobody writing.
    pub fn intended_at(&self, now_unix_ms: u64) -> register::Resolution {
        self.intent.resolve_at(now_unix_ms)
    }

    /// The member named anywhere in this slot, for validation.
    fn intended_program(&self) -> Option<&String> {
        self.intent
            .over
            .as_ref()
            .map(|over| &over.choice.member)
            .or(self.intent.base.as_ref().map(|base| &base.member))
    }

    /// What this screen plays at `now`, and which rung said so.
    ///
    /// Override, then schedule, then this screen's own choice, then its
    /// group's. A pure function taking the clock as an argument because the
    /// World ABI supplies none — and because a receiver deciding what to show
    /// should be reading its own clock rather than trusting a coordinator's.
    ///
    /// `group` is the screen's group when it has one; passing an unrelated
    /// group is a caller error this cannot detect, so [`Self::group`] is the
    /// only thing that should choose it.
    pub fn plays_at(
        &self,
        group: Option<&SignageGroup>,
        now_unix_ms: u64,
    ) -> Result<Playback, schedule::Invalid> {
        let mut boundary = Boundary::default();

        let overridden = self
            .intent
            .over
            .as_ref()
            .filter(|over| now_unix_ms < over.until_unix_ms);
        if let Some(over) = overridden {
            return Ok(Playback {
                program: Some(over.choice.member.clone()),
                source: Some(PlaybackSource::Override),
                next_boundary_unix_ms: Some(over.until_unix_ms),
            });
        }

        let mut open: Option<&ProgramWindow> = None;
        for window in &self.schedule {
            let (active, next) = window.window.evaluate_at(now_unix_ms)?;
            boundary.saw(next);
            if active
                && open.is_none_or(|current| {
                    window.window.priority > current.window.priority
                        || (window.window.priority == current.window.priority
                            && window.id < current.id)
                })
            {
                open = Some(window);
            }
        }
        if let Some(window) = open {
            return Ok(Playback {
                program: Some(window.program.clone()),
                source: Some(PlaybackSource::Schedule),
                next_boundary_unix_ms: boundary.soonest,
            });
        }

        // A screen with no direct choice falls through to its group, and the
        // group's own override can lapse too.
        let (program, source) = match self.intent.base.as_ref() {
            Some(base) => (Some(base.member.clone()), Some(PlaybackSource::Direct)),
            None => match group {
                Some(group) => {
                    let resolved = group.intent.resolve_at(now_unix_ms);
                    boundary.saw(resolved.next_boundary_unix_ms);
                    let source = resolved.member.is_some().then_some(PlaybackSource::Group);
                    (resolved.member, source)
                }
                None => (None, None),
            },
        };
        Ok(Playback {
            program,
            source,
            next_boundary_unix_ms: boundary.soonest,
        })
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

/// A named set of screens.
///
/// It exists because Mechanics refuses a wildcard segment: "these screens" is a
/// grant on a group, and resolving which screens that is belongs here, where
/// the membership already lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageGroup {
    pub id: String,
    pub name: String,
    /// What its screens play when they choose nothing themselves — Medusa's
    /// network playlist, the bottom rung of [`SignageScreen::plays_at`].
    #[serde(default)]
    pub intent: register::Slot,
    #[serde(default)]
    pub screens: Vec<String>,
}

impl SignageGroup {
    pub fn validate(&self) -> bool {
        if BodyId::parse(&self.id).is_none()
            || self.name.trim().is_empty()
            || self.name.chars().count() > MAX_NAME_CHARS
            || self.screens.len() > MAX_GROUP_SCREENS
            || self.intent.validate().is_err()
            || !self
                .intended_program()
                .is_none_or(|id| BodyId::parse(id).is_some())
        {
            return false;
        }
        let mut seen = std::collections::BTreeSet::new();
        self.screens
            .iter()
            .all(|id| BodyId::parse(id).is_some() && seen.insert(id))
    }

    /// The member named anywhere in this slot, for validation.
    fn intended_program(&self) -> Option<&String> {
        self.intent
            .over
            .as_ref()
            .map(|over| &over.choice.member)
            .or(self.intent.base.as_ref().map(|base| &base.member))
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

// ─── What a caller asks for ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaIntent {
    Put { media: SignageMedia },
    Delete { media: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaQuery {
    Media {
        media: String,
    },
    Library,
    /// Which programs contain an item naming this entry.
    ///
    /// Asked before deleting. Medusa did not ask, and deleted content out from
    /// under playlists that were showing it.
    UsedBy {
        media: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaProjection {
    Media { media: Option<SignageMedia> },
    Library { media: Vec<SignageMedia> },
    UsedBy { programs: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScreenIntent {
    Put { screen: SignageScreen },
    Delete { screen: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScreenQuery {
    Screen {
        screen: String,
    },
    Screens,
    /// Which screens intend a program — the reverse index Medusa computed with
    /// an N+1 fetch per screen.
    Showing {
        program: String,
    },
    /// Everything [`SignageScreen::plays_at`] needs, in one round trip.
    ///
    /// The World holds no clock, so it returns the inputs rather than the
    /// answer and the caller resolves against its own. That is not a
    /// limitation worked around: a receiver deciding what to show from a
    /// coordinator's clock would keep showing yesterday's override through a
    /// partition, which is the case this whole ladder exists to survive.
    Plays {
        screen: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScreenProjection {
    Screen {
        screen: Option<SignageScreen>,
    },
    Screens {
        screens: Vec<SignageScreen>,
    },
    Showing {
        screens: Vec<String>,
    },
    /// `group` is present only when the screen names one that resolves.
    Plays {
        screen: Option<SignageScreen>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<SignageGroup>,
    },
}

impl ScreenProjection {
    /// Resolve a `Plays` answer against the caller's clock.
    ///
    /// Here rather than on the caller so that every consumer pairs the screen
    /// with the same group it was answered with. Pairing them by hand is how
    /// one gets resolved against a group it does not belong to.
    pub fn playback(&self, now_unix_ms: u64) -> Option<Result<Playback, schedule::Invalid>> {
        match self {
            Self::Plays { screen, group } => screen
                .as_ref()
                .map(|screen| screen.plays_at(group.as_ref(), now_unix_ms)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupIntent {
    Put { group: SignageGroup },
    Delete { group: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupQuery {
    Group { group: String },
    Groups,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupProjection {
    Group { group: Option<SignageGroup> },
    Groups { groups: Vec<SignageGroup> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigIntent {
    Put { config: SignageConfig },
    Delete { config: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigQuery {
    Config { config: String },
    Configs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigProjection {
    Config { config: Option<SignageConfig> },
    Configs { configs: Vec<SignageConfig> },
}

// ─── What each of them demands ──────────────────────────────────────────────

/// Authority over one library entry: granted on it, or fleet-wide.
pub fn demand_manage_media(media: &str) -> Vec<u8> {
    scoped("space.signage.manage", resource::media(media))
}

pub fn demand_read_media(media: &str) -> Vec<u8> {
    scoped("space.signage.read", resource::media(media))
}

/// Authority over one screen: granted on the screen, on its group, or
/// fleet-wide.
///
/// The group arm is why a group is a resource rather than a label. A grant on
/// `group/lobby` reaches every screen in it without naming them, which is what
/// a wildcard would have done if Mechanics allowed one.
pub fn demand_manage_screen(screen: &str, group: Option<&str>) -> Vec<u8> {
    let mut options = vec![
        AuthorizationDemand::require(capability("space.signage.manage"), resource::screen(screen)),
        AuthorizationDemand::require(capability("space.signage.manage"), root_resource()),
    ];
    if let Some(group) = group {
        options.insert(
            1,
            AuthorizationDemand::require(
                capability("space.signage.manage"),
                resource::group(group),
            ),
        );
    }
    AuthorizationDemand::Any(options)
        .encode_canonical()
        .expect("canonical Signage screen manage demand")
}

pub fn demand_read_screen(screen: &str) -> Vec<u8> {
    scoped("space.signage.read", resource::screen(screen))
}

pub fn demand_manage_group(group: &str) -> Vec<u8> {
    scoped("space.signage.manage", resource::group(group))
}

pub fn demand_read_group(group: &str) -> Vec<u8> {
    scoped("space.signage.read", resource::group(group))
}

pub fn demand_manage_config(config: &str) -> Vec<u8> {
    scoped("space.signage.manage", resource::config(config))
}

pub fn demand_read_config(config: &str) -> Vec<u8> {
    scoped("space.signage.read", resource::config(config))
}

/// Granted on the thing, or fleet-wide. The shape every scoped demand takes.
fn scoped(name: &str, on: Resource) -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(capability(name), on),
        AuthorizationDemand::require(capability(name), root_resource()),
    ])
    .encode_canonical()
    .expect("canonical scoped Signage demand")
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
                media: BodyId::from_bytes([4; 16]).render(),
                duration_ms: Some(10_000),
            }],
            windows: Vec::new(),
        }
    }

    fn item(id: &str, duration_ms: Option<u32>) -> SignageItem {
        SignageItem {
            id: id.into(),
            media: BodyId::from_bytes([4; 16]).render(),
            duration_ms,
        }
    }

    fn window(id: &str, items: Vec<String>) -> SignageWindow {
        SignageWindow {
            id: id.into(),
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
            items,
        }
    }

    #[test]
    fn programs_are_bounded_and_have_one_canonical_identity() {
        assert!(program().validate());

        for bad in [
            SignageProgram {
                id: "not-a-body-id".into(),
                ..program()
            },
            SignageProgram {
                name: "  ".into(),
                ..program()
            },
            SignageProgram {
                items: Vec::new(),
                ..program()
            },
            SignageProgram {
                items: vec![item("welcome", Some(10_000)), item("welcome", Some(1_000))],
                ..program()
            },
            SignageProgram {
                items: vec![SignageItem {
                    media: "not-a-body-id".into(),
                    ..item("welcome", Some(10_000))
                }],
                ..program()
            },
            SignageProgram {
                items: vec![item("Welcome", Some(10_000))],
                ..program()
            },
        ] {
            assert!(!bad.validate(), "{bad:?}");
        }
    }

    /// An item with no duration defers to the library entry it names, wherever
    /// it sits and whatever the cycle does at the end.
    ///
    /// This validation cannot see the library, so it cannot know what that
    /// resolves to — and refusing what it cannot check would reject programs
    /// the editor writes by default.
    #[test]
    fn an_absent_duration_is_legal_anywhere_and_a_declared_one_is_bounded() {
        for cycle in [ProgramCycle::Loop, ProgramCycle::HoldLast] {
            let deferred = SignageProgram {
                cycle,
                items: vec![item("intro", None), item("welcome", None)],
                ..program()
            };
            assert!(deferred.validate(), "{cycle:?}");

            let selected = SignageProgram {
                windows: vec![window("both", vec!["intro".into(), "welcome".into()])],
                ..deferred.clone()
            };
            assert!(selected.validate(), "{cycle:?}");
        }

        for duration in [MIN_ITEM_DURATION_MS - 1, MAX_ITEM_DURATION_MS + 1] {
            let bad = SignageProgram {
                items: vec![item("welcome", Some(duration))],
                ..program()
            };
            assert!(!bad.validate(), "{duration}");
        }
    }

    #[test]
    fn a_window_selects_from_the_items_the_program_actually_has() {
        let base = SignageProgram {
            items: vec![item("intro", Some(1_000)), item("welcome", Some(10_000))],
            ..program()
        };
        assert!(SignageProgram {
            windows: vec![window("regular", vec!["welcome".into()])],
            ..base.clone()
        }
        .validate());

        for bad in [
            window("unknown", vec!["absent".into()]),
            window("twice", vec!["welcome".into(), "welcome".into()]),
            window("empty", Vec::new()),
        ] {
            let program = SignageProgram {
                windows: vec![bad.clone()],
                ..base.clone()
            };
            assert!(!program.validate(), "{:?}", bad.id);
        }

        let duplicated = SignageProgram {
            windows: vec![
                window("regular", vec!["welcome".into()]),
                window("regular", vec!["intro".into()]),
            ],
            ..base
        };
        assert!(!duplicated.validate());
    }

    fn program_id(tag: u8) -> String {
        BodyId::from_bytes([tag; 16]).render()
    }

    fn choice(member: &str) -> register::Choice {
        register::Choice {
            member: member.into(),
            chosen_unix_ms: 1,
            chooser: "someone".into(),
        }
    }

    fn at(moment: &str) -> u64 {
        u64::try_from(
            moment
                .parse::<jiff::Timestamp>()
                .expect("a reviewed test moment")
                .as_millisecond(),
        )
        .expect("a moment after the epoch")
    }

    fn open_window(id: &str, program: &str, start_local: &str, priority: i16) -> ProgramWindow {
        ProgramWindow {
            id: id.into(),
            window: schedule::Window {
                start_local: start_local.into(),
                duration_ms: 60 * 60 * 1_000,
                recurrence: schedule::Recurrence::None,
                until_unix_ms: None,
                priority,
                enabled: true,
                timezone: "UTC".into(),
                exceptions: Vec::new(),
            },
            program: program.into(),
        }
    }

    fn screen(group: Option<&str>) -> SignageScreen {
        SignageScreen {
            id: BodyId::from_bytes([1; 16]).render(),
            name: "Lobby screen".into(),
            group: group.map(str::to_owned),
            intent: register::Slot::default(),
            schedule: Vec::new(),
        }
    }

    fn group(program: Option<&str>) -> SignageGroup {
        SignageGroup {
            id: BodyId::from_bytes([2; 16]).render(),
            name: "Lobbies".into(),
            intent: register::Slot {
                base: program.map(choice),
                over: None,
            },
            screens: Vec::new(),
        }
    }

    /// Each rung answers when the ones above it do not, and says it did.
    #[test]
    fn playback_falls_through_override_schedule_direct_then_group() {
        let now = at("2026-08-15T10:30:00Z");
        let direct = program_id(3);
        let scheduled = program_id(4);
        let overriding = program_id(5);
        let inherited = program_id(6);
        let group = group(Some(&inherited));

        let bare = screen(Some(&group.id));
        assert_eq!(
            bare.plays_at(Some(&group), now).unwrap(),
            Playback {
                program: Some(inherited.clone()),
                source: Some(PlaybackSource::Group),
                next_boundary_unix_ms: None,
            }
        );

        let mut chosen = bare.clone();
        chosen.intent.base = Some(choice(&direct));
        assert_eq!(
            chosen.plays_at(Some(&group), now).unwrap().source,
            Some(PlaybackSource::Direct)
        );

        let mut timed = chosen.clone();
        timed.schedule = vec![open_window("morning", &scheduled, "2026-08-15T10:00:00", 0)];
        let playing = timed.plays_at(Some(&group), now).unwrap();
        assert_eq!(playing.program.as_ref(), Some(&scheduled));
        assert_eq!(playing.source, Some(PlaybackSource::Schedule));

        let mut overridden = timed.clone();
        overridden.intent.over = Some(register::Override {
            choice: choice(&overriding),
            until_unix_ms: now + 60_000,
        });
        assert_eq!(
            overridden.plays_at(Some(&group), now).unwrap(),
            Playback {
                program: Some(overriding),
                source: Some(PlaybackSource::Override),
                next_boundary_unix_ms: Some(now + 60_000),
            }
        );

        // A lapsed override is not an override, and the rung below answers.
        assert_eq!(
            overridden
                .plays_at(Some(&group), now + 120_000)
                .unwrap()
                .program
                .as_ref(),
            Some(&scheduled)
        );

        for screen in [screen(None), screen(Some(&group.id))] {
            assert_eq!(
                screen.plays_at(None, now).unwrap(),
                Playback {
                    program: None,
                    source: None,
                    next_boundary_unix_ms: None,
                },
                "nothing to show is a state, not a failure"
            );
        }
    }

    /// A closed window still says when it opens, so a receiver can sleep.
    #[test]
    fn a_screen_reports_when_its_answer_changes_with_nobody_writing() {
        let before = at("2026-08-15T09:00:00Z");
        let mut screen = screen(None);
        screen.intent.base = Some(choice(&program_id(3)));
        screen.schedule = vec![
            open_window("evening", &program_id(4), "2026-08-15T18:00:00", 0),
            open_window("morning", &program_id(5), "2026-08-15T10:00:00", 0),
        ];

        let playing = screen.plays_at(None, before).unwrap();
        assert_eq!(playing.source, Some(PlaybackSource::Direct));
        assert_eq!(
            playing.next_boundary_unix_ms,
            Some(at("2026-08-15T10:00:00Z")),
            "the soonest of the two, not the first listed"
        );
    }

    /// Priority decides an overlap; the id breaks a tie, so the answer does not
    /// depend on the order the windows happen to be stored in.
    #[test]
    fn overlapping_screen_windows_resolve_by_priority_then_id() {
        let now = at("2026-08-15T10:30:00Z");
        let urgent = program_id(4);
        let mut screen = screen(None);
        screen.schedule = vec![
            open_window("regular", &program_id(3), "2026-08-15T10:00:00", 0),
            open_window("urgent", &urgent, "2026-08-15T10:15:00", 10),
        ];
        assert_eq!(
            screen.plays_at(None, now).unwrap().program.as_ref(),
            Some(&urgent)
        );

        screen.schedule.reverse();
        assert_eq!(
            screen.plays_at(None, now).unwrap().program.as_ref(),
            Some(&urgent)
        );

        let first = program_id(7);
        screen.schedule = vec![
            open_window("b-later", &program_id(8), "2026-08-15T10:00:00", 5),
            open_window("a-first", &first, "2026-08-15T10:00:00", 5),
        ];
        assert_eq!(
            screen.plays_at(None, now).unwrap().program.as_ref(),
            Some(&first),
            "a tie resolves by id"
        );
    }

    #[test]
    fn a_screen_schedule_is_validated_like_the_rest() {
        let mut valid = screen(None);
        valid.schedule = vec![open_window(
            "morning",
            &program_id(3),
            "2026-08-15T10:00:00",
            0,
        )];
        assert!(valid.validate());

        let mut duplicate = valid.clone();
        duplicate.schedule.push(open_window(
            "morning",
            &program_id(4),
            "2026-08-15T12:00:00",
            0,
        ));
        assert!(!duplicate.validate(), "two windows cannot share an id");

        let mut dangling = valid.clone();
        dangling.schedule[0].program = "not-a-body-id".into();
        assert!(!dangling.validate());

        let mut shouty = valid;
        shouty.schedule[0].id = "Morning".into();
        assert!(!shouty.validate());
    }

    #[test]
    fn highest_priority_active_window_selects_items_and_exposes_the_next_boundary() {
        let mut program = program();
        program.items.push(SignageItem {
            id: "urgent".into(),
            media: BodyId::from_bytes([4; 16]).render(),
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
