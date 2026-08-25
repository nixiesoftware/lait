#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "the reviewed contract uses compile-time identifiers and bounded item indices"
)]

use crate::addressing::{Context, Match, SignageAudience};
use crate::fleet::{self, Playback, SignageBroadcast, SignageChannel, SignageScreen};
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
pub const MEDIA_SCHEMA_VERSION: u32 = 3;
/// One panel: where it is, what it is called, and what it is tuned to. Never
/// its grant, and never its device lifecycle.
pub const SCREEN_SCHEMA: &str = "signage.screen";
pub const SCREEN_SCHEMA_VERSION: u32 = 3;

/// A standing stream a screen tunes to.
pub const CHANNEL_SCHEMA: &str = "signage.channel";
pub const CHANNEL_SCHEMA_VERSION: u32 = 1;

/// Who a broadcast reaches — a named predicate, not a membership list.
pub const AUDIENCE_SCHEMA: &str = "signage.audience";
pub const AUDIENCE_SCHEMA_VERSION: u32 = 1;

/// A transmission: audience, action, timing, priority.
pub const BROADCAST_SCHEMA: &str = "signage.broadcast";
pub const BROADCAST_SCHEMA_VERSION: u32 = 1;

/// What a screen says it actually played, signed by the screen.
pub const ASRUN_SCHEMA: &str = "signage.asrun";
pub const ASRUN_SCHEMA_VERSION: u32 = 1;

pub const MAX_NAME_CHARS: usize = 160;
pub const MAX_MIME_CHARS: usize = 96;
pub const MAX_SCREEN_LABELS: usize = 64;
pub const MAX_LABEL_CHARS: usize = 96;
pub const MAX_CHANNEL_WINDOWS: usize = 64;
/// A bound on `Match` nesting. Deep enough for any audience a person writes,
/// shallow enough that evaluation is obviously terminating on every replica.
pub const MAX_MATCH_DEPTH: u8 = 8;
pub const MAX_MATCH_TERMS: usize = 32;
/// A bound on `Audience` references resolved while evaluating one match.
pub const MAX_AUDIENCE_HOPS: u8 = 8;
pub const MAX_OBSERVATIONS: usize = 64;
pub const MAX_ASRUN_ENTRIES: usize = 512;
pub const MAX_SUPERSEDES: usize = 16;
/// A ceiling on one stored file, so a library row cannot describe a petabyte.
pub const MAX_MEDIA_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_CONFIG_SETTINGS: usize = 64;
pub const MAX_SETTING_CHARS: usize = 1024;

/// How a kind is *presented*. Named, reusable, and carrying nothing about
/// where it plays — which is what lets one preset serve every venue.
pub const PRESET_SCHEMA: &str = "signage.preset";
pub const PRESET_SCHEMA_VERSION: u32 = 1;

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

pub fn channel_schema() -> SchemaId {
    SchemaId::parse(CHANNEL_SCHEMA).expect("reviewed Signage channel schema")
}

pub fn audience_schema() -> SchemaId {
    SchemaId::parse(AUDIENCE_SCHEMA).expect("reviewed Signage audience schema")
}

pub fn broadcast_schema() -> SchemaId {
    SchemaId::parse(BROADCAST_SCHEMA).expect("reviewed Signage broadcast schema")
}

pub fn asrun_schema() -> SchemaId {
    SchemaId::parse(ASRUN_SCHEMA).expect("reviewed Signage as-run schema")
}

pub fn preset_schema() -> SchemaId {
    SchemaId::parse(PRESET_SCHEMA).expect("reviewed Signage preset schema id")
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

    /// A standing stream screens tune to.
    pub fn channel(id: &str) -> Resource {
        segments("channel", id)
    }

    /// A named audience. Granting on one is how "these screens" is delegated
    /// now that a group no longer exists to stand in for a wildcard.
    pub fn audience(id: &str) -> Resource {
        segments("audience", id)
    }

    /// One transmission.
    pub fn broadcast(id: &str) -> Resource {
        segments("broadcast", id)
    }

    /// One item in the media library.
    pub fn media(id: &str) -> Resource {
        segments("media", id)
    }

    /// One kind's presentation.
    pub fn preset(id: &str) -> Resource {
        segments("preset", id)
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
    /// Three things decide what one of these draws, and they are addressed
    /// differently because they vary differently.
    ///
    /// `settings` is this entry's own, by value, because it varies per entry.
    /// `preset` names a [`SignagePreset`] by id — how the kind is *presented*,
    /// shared by every entry that points at it and editable in one place.
    /// Everything that varies by *venue* — where the screen is, what a
    /// congregation practises — is neither of these: it lives on the screen,
    /// and arrives at render time from wherever this is playing.
    ///
    /// That last split is what makes one entry correct in two cities. The
    /// previous model found configuration *by kind*, one per Space, so a
    /// second venue was a contradiction the contract had no room for.
    Kind {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
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
            Self::Kind {
                kind,
                preset,
                settings,
            } => {
                valid_kind(kind)
                    && preset.as_ref().is_none_or(|id| BodyId::parse(id).is_some())
                    && valid_settings(settings)
            }
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

    /// The stored id as a substrate reference, decoded here because this is the
    /// crate that decides the id's shape. `validate` checks shape only, so a
    /// declaration and a render must not each invent their own decode.
    pub fn content_ref(&self) -> Option<replica::content::ContentRef> {
        let bytes = data_encoding::HEXLOWER
            .decode(self.content()?.as_bytes())
            .ok()?;
        Some(replica::content::ContentRef {
            content_id: <[u8; 32]>::try_from(bytes.as_slice()).ok()?,
        })
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
    /// The catalog a stored entry serves under, derived at ingest.
    ///
    /// Canonical catalog JSON, exactly as the live plane's own
    /// `Catalog::encode_canonical` writes it. Derived once, where the person
    /// who uploaded the file is present to be told if it cannot be — a file
    /// that cannot meet the plane's baseline has no valid catalog, and that
    /// refusal at a render instead would reach a screen at three in the
    /// morning. The World validates the bytes decode canonically and asserts
    /// nothing else about them: what a catalog *means* is the plane's rule,
    /// asked rather than restated here.
    ///
    /// Only a `Stored` entry may carry one. A live rendition's catalog is
    /// announced by whatever is encoding; a card and a kind have no bytes to
    /// describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
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
            && self.valid_catalog()
    }

    /// A catalog is canonical or absent, and only bytes can carry one.
    ///
    /// The decode is the whole check. `Catalog::decode_canonical` re-encodes
    /// and compares, so a record cannot hold a catalog the packagers would
    /// refuse — and this World never learns what a catalog means, only that
    /// the plane accepts it.
    fn valid_catalog(&self) -> bool {
        match &self.catalog {
            None => true,
            Some(catalog) => {
                matches!(self.source, MediaSource::Stored { .. })
                    && runtime::plane::live::media::Catalog::decode_canonical(catalog.as_bytes())
                        .is_ok()
            }
        }
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

/// What one integration is configured with, once, for the whole Space.
/// How a kind is presented.
///
/// Named, reusable, and carrying nothing about where it plays — no
/// coordinates, no timezone, no congregation's practice. That absence is the
/// point: a preset with no venue in it is safe to point at from anywhere, and
/// the same one serves every site an operator runs.
///
/// There is deliberately no uniqueness rule. The old document was one per kind
/// per Space, enforced at write, and the only thing that rule bought was a
/// well-defined lookup *by kind* — which this replaces with a reference from
/// the entry that uses it. Two mosques on different calculation methods under
/// one operator stopped being a contradiction the moment the lookup changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignagePreset {
    pub id: String,
    /// The integration definition this presents.
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub settings: Settings,
}

impl SignagePreset {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && valid_kind(&self.kind)
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && valid_settings(&self.settings)
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

pub type Settings = std::collections::BTreeMap<String, String>;

pub fn valid_settings(settings: &Settings) -> bool {
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
#[derive(Debug, Default)]
pub struct Boundary {
    soonest: Option<u64>,
}

impl Boundary {
    pub fn soonest(&self) -> Option<u64> {
        self.soonest
    }

    pub fn saw(&mut self, moment: Option<u64>) {
        if let Some(moment) = moment {
            self.soonest = Some(self.soonest.map_or(moment, |current| current.min(moment)));
        }
    }
}

/// An integration definition name, or a live rendition id.
pub fn valid_kind(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ITEM_ID_BYTES
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// A committed content id, as the upload route rendered it: exactly 32 bytes of
/// lowercase hex.
///
/// Whether the descriptor *exists* is the substrate's question and it refuses
/// the declaration if it does not. But the id's own shape is this World's, and
/// it used to accept any lowercase token up to 96 bytes — so an entry could be
/// written, validate, replicate, and then render as a blank forever, because
/// nothing that could decode it ever saw it. Admitted and renderable are the
/// same set now.
fn valid_content_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a screen says it actually played.
///
/// Written by the panel, under the panel's own identity, and replicated like
/// everything else. In broadcast this is the as-run log and it is the
/// substrate billing rests on; in every signage CMS it is a telemetry table
/// the server writes *about* a player. The difference matters: a record the
/// controller authored attests only that the controller believes something,
/// and proof-of-play that the proving party cannot sign is not proof.
///
/// It is also what stops a screen being purely a sink. A panel that can write
/// is a peer, and the observations it reports here are the honest source of
/// the context its own audiences are evaluated against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignageAsRun {
    /// The screen, and the body id: one document per panel, appended to.
    pub id: String,
    pub screen: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<AsRunEntry>,
    /// What the screen last reported about itself. Fed back into resolution as
    /// the context an `Observed` audience matches on.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub observations: Settings,
}

/// One thing that was on the glass, and for how long.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsRunEntry {
    pub program: String,
    pub item: String,
    pub started_unix_ms: u64,
    pub ended_unix_ms: u64,
    /// Which broadcast or channel put it there, so the record answers "why"
    /// as well as "what" — the same question `Resolved` answers live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl SignageAsRun {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && BodyId::parse(&self.screen).is_some()
            && self.entries.len() <= MAX_ASRUN_ENTRIES
            && self.observations.len() <= MAX_OBSERVATIONS
            && valid_settings(&self.observations)
            && self.entries.iter().all(|entry| {
                BodyId::parse(&entry.program).is_some()
                    && !entry.item.is_empty()
                    && entry.item.len() <= MAX_ITEM_ID_BYTES
                    && entry.ended_unix_ms >= entry.started_unix_ms
            })
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Which screens a program reaches — the reverse index Medusa computed
    /// with an N+1 fetch per screen. Now a question about addressing rather
    /// than about membership, so it is answered by evaluating audiences.
    Showing {
        program: String,
    },
    /// Everything resolution needs for one screen, in one round trip: the
    /// screen, every channel, every live broadcast, and the audiences those
    /// broadcasts name.
    ///
    /// The World holds no clock, so it returns the inputs rather than the
    /// answer and the caller resolves against its own. That is not a
    /// limitation worked around: a receiver deciding what to show from a
    /// coordinator's clock would keep showing yesterday's broadcast through a
    /// partition, which is the case this whole ladder exists to survive.
    Plays {
        screen: String,
    },
    /// Which screens an audience reaches, right now. The count an operator
    /// must see before sending — an expressive audience without a preview of
    /// its blast radius is the dangerous kind.
    Reaches {
        audience: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// The inputs to [`fleet::resolve`], answered together so no caller has to
    /// pair a screen with documents it was not answered with.
    Plays {
        screen: Option<SignageScreen>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        channels: Vec<SignageChannel>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        broadcasts: Vec<SignageBroadcast>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        audiences: Vec<SignageAudience>,
    },
    Reaches {
        screens: Vec<String>,
    },
}

impl ScreenProjection {
    /// Resolve a `Plays` answer against the caller's clock and observations.
    ///
    /// Here rather than on the caller so every consumer resolves the same way
    /// against the same documents. Assembling this by hand is how a screen
    /// gets resolved against an audience it was not answered with.
    pub fn playback(&self, cx: &Context) -> Option<Playback> {
        match self {
            Self::Plays {
                screen,
                channels,
                broadcasts,
                audiences,
            } => {
                let lookup: std::collections::BTreeMap<String, Match> = audiences
                    .iter()
                    .map(|audience| (audience.id.clone(), audience.rule.clone()))
                    .collect();
                screen
                    .as_ref()
                    .map(|screen| fleet::resolve(screen, channels, broadcasts, cx, &lookup))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelIntent {
    Put { channel: SignageChannel },
    Delete { channel: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelQuery {
    Channel { channel: String },
    Channels,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelProjection {
    Channel { channel: Option<SignageChannel> },
    Channels { channels: Vec<SignageChannel> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudienceIntent {
    Put { audience: SignageAudience },
    Delete { audience: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudienceQuery {
    Audience { audience: String },
    Audiences,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudienceProjection {
    Audience { audience: Option<SignageAudience> },
    Audiences { audiences: Vec<SignageAudience> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BroadcastIntent {
    Put { broadcast: SignageBroadcast },
    Delete { broadcast: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BroadcastQuery {
    Broadcast { broadcast: String },
    Broadcasts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BroadcastProjection {
    Broadcast { broadcast: Option<SignageBroadcast> },
    Broadcasts { broadcasts: Vec<SignageBroadcast> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresetIntent {
    Put { preset: SignagePreset },
    Delete { preset: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresetQuery {
    Preset { preset: String },
    Presets,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresetProjection {
    Preset { preset: Option<SignagePreset> },
    Presets { presets: Vec<SignagePreset> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AsRunIntent {
    /// Appended by the screen, under the screen's own identity.
    Record { asrun: SignageAsRun },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AsRunQuery {
    AsRun { screen: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AsRunProjection {
    AsRun { asrun: Option<SignageAsRun> },
}

// ─── What each of them demands ──────────────────────────────────────────────

/// Authority over one library entry: granted on it, or fleet-wide.
pub fn demand_manage_media(media: &str) -> Vec<u8> {
    scoped("space.signage.manage", resource::media(media))
}

pub fn demand_read_media(media: &str) -> Vec<u8> {
    scoped("space.signage.read", resource::media(media))
}

/// Authority over one screen: granted on it, or fleet-wide.
///
/// A group used to be nameable here, because Mechanics refuses a wildcard and
/// a group was the indirection that stood in for one. Audiences are that
/// indirection now — see [`demand_manage_audience`] — and a screen's own grant
/// stopped depending on which set somebody had filed it under.
pub fn demand_manage_screen(screen: &str) -> Vec<u8> {
    any_of("space.signage.manage", resource::screen(screen))
}

pub fn demand_read_screen(screen: &str) -> Vec<u8> {
    any_of("space.signage.read", resource::screen(screen))
}

pub fn demand_manage_channel(channel: &str) -> Vec<u8> {
    any_of("space.signage.manage", resource::channel(channel))
}

pub fn demand_read_channel(channel: &str) -> Vec<u8> {
    any_of("space.signage.read", resource::channel(channel))
}

/// Authority over one audience.
///
/// This is where "these screens" is delegated. A tenant holding manage on an
/// audience can address exactly the screens it reaches and nothing else, which
/// is the multi-stakeholder case expressed as a grant rather than as a schema.
pub fn demand_manage_audience(audience: &str) -> Vec<u8> {
    any_of("space.signage.manage", resource::audience(audience))
}

pub fn demand_read_audience(audience: &str) -> Vec<u8> {
    any_of("space.signage.read", resource::audience(audience))
}

/// Authority to transmit.
///
/// Demanded on the broadcast *and* on the audience it names: sending to a set
/// of screens is authority over that set, so a grant on the transmission alone
/// would let a holder address a fleet by pointing at somebody else's audience.
pub fn demand_manage_broadcast(broadcast: &str, audience: &str) -> Vec<u8> {
    AuthorizationDemand::All(vec![
        AuthorizationDemand::Any(vec![
            AuthorizationDemand::require(
                capability("space.signage.manage"),
                resource::broadcast(broadcast),
            ),
            AuthorizationDemand::require(capability("space.signage.manage"), root_resource()),
        ]),
        AuthorizationDemand::Any(vec![
            AuthorizationDemand::require(
                capability("space.signage.manage"),
                resource::audience(audience),
            ),
            AuthorizationDemand::require(capability("space.signage.manage"), root_resource()),
        ]),
    ])
    .encode_canonical()
    .expect("canonical Signage broadcast manage demand")
}

pub fn demand_read_broadcast(broadcast: &str) -> Vec<u8> {
    any_of("space.signage.read", resource::broadcast(broadcast))
}

pub fn demand_manage_preset(preset: &str) -> Vec<u8> {
    any_of("space.signage.manage", resource::preset(preset))
}

pub fn demand_read_preset(preset: &str) -> Vec<u8> {
    any_of("space.signage.read", resource::preset(preset))
}

/// A screen recording what it played.
///
/// Demanded on the screen itself, so the only principal who can write a
/// panel's as-run is that panel — the attestation is worth exactly as much as
/// the identity behind it, and a coordinator writing it on the screen's behalf
/// would be worth nothing.
pub fn demand_record_asrun(screen: &str) -> Vec<u8> {
    AuthorizationDemand::require(capability("space.signage.manage"), resource::screen(screen))
        .encode_canonical()
        .expect("canonical Signage as-run demand")
}

/// Granted on the thing, or fleet-wide. `Any` rather than a second capability,
/// so a scoped grant is an attenuation of the fleet-wide one rather than a
/// parallel vocabulary that has to be kept in step with it.
fn any_of(name: &str, on: Resource) -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(capability(name), on),
        AuthorizationDemand::require(capability(name), root_resource()),
    ])
    .encode_canonical()
    .expect("canonical Signage scoped demand")
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

    #[test]
    fn a_kind_entry_names_its_preset_and_keeps_its_own_settings() {
        let entry = SignageMedia {
            id: BodyId::from_bytes([3; 16]).render(),
            name: "Athan".into(),
            duration_ms: Some(60_000),
            width: None,
            height: None,
            catalog: None,
            source: MediaSource::Kind {
                kind: "athan".into(),
                preset: Some(BodyId::from_bytes([4; 16]).render()),
                settings: Settings::new(),
            },
        };
        assert!(entry.validate());
        let MediaSource::Kind { preset, .. } = &entry.source else {
            panic!("a kind entry stays a kind");
        };
        assert!(preset.is_some(), "the preset is addressed by reference");
    }

    /// Two presets for one kind. The old contract refused the second at write,
    /// and that refusal is the whole reason a Space could hold one venue.
    #[test]
    fn a_kind_may_have_more_than_one_preset() {
        let one = SignagePreset {
            id: BodyId::from_bytes([5; 16]).render(),
            kind: "athan".into(),
            name: "House style".into(),
            settings: Settings::from([("theme".into(), "emerald".into())]),
        };
        let two = SignagePreset {
            id: BodyId::from_bytes([6; 16]).render(),
            kind: "athan".into(),
            name: "Ramadan".into(),
            settings: Settings::from([("theme".into(), "night".into())]),
        };
        assert!(one.validate() && two.validate());
        assert_ne!(one.id, two.id);
    }

    #[test]
    fn an_as_run_entry_cannot_end_before_it_started() {
        let mut asrun = SignageAsRun {
            id: BodyId::from_bytes([7; 16]).render(),
            screen: BodyId::from_bytes([8; 16]).render(),
            entries: vec![AsRunEntry {
                program: BodyId::from_bytes([9; 16]).render(),
                item: "itm".into(),
                started_unix_ms: 1_000,
                ended_unix_ms: 2_000,
                source: None,
            }],
            observations: Settings::new(),
        };
        assert!(asrun.validate());
        if let Some(entry) = asrun.entries.first_mut() {
            entry.ended_unix_ms = 500;
        }
        assert!(
            !asrun.validate(),
            "time does not run backwards on the glass"
        );
    }
}
