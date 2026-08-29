use std::collections::BTreeSet;

use replica::body::{BodyKey, MutationModel, Op, Schema, SchemaId, WorldId};
use replica::content::ContentRef;
use replica::frontier::ReplicaFrontier;
use runtime::world::{
    Context, Descriptor, Effect, Intent, Limits, Projection, Query, Rejection, Version, World,
};

use crate::addressing::{Context as MatchContext, Match, SignageAudience};
use crate::contract::{
    self, AsRunIntent, AsRunProjection, AsRunQuery, AudienceIntent, AudienceProjection,
    AudienceQuery, BroadcastIntent, BroadcastProjection, BroadcastQuery, ChannelIntent,
    ChannelProjection, ChannelQuery, MediaIntent, MediaProjection, MediaQuery, PresetIntent,
    PresetProjection, PresetQuery, ScreenIntent, ScreenProjection, ScreenQuery, SignageAsRun,
    SignageIntent, SignageMedia, SignagePreset, SignageProgram, SignageProjection, SignageQuery,
};
use crate::fleet::{SignageBroadcast, SignageChannel, SignageScreen};

pub struct SignageWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}

impl SignageWorld {
    pub fn new() -> Self {
        Self {
            id: contract::world_id(),
            schemas: vec![
                atomic_json(contract::program_schema(), contract::PROGRAM_SCHEMA_VERSION),
                atomic_json(contract::media_schema(), contract::MEDIA_SCHEMA_VERSION),
                atomic_json(contract::screen_schema(), contract::SCREEN_SCHEMA_VERSION),
                atomic_json(contract::channel_schema(), contract::CHANNEL_SCHEMA_VERSION),
                atomic_json(
                    contract::audience_schema(),
                    contract::AUDIENCE_SCHEMA_VERSION,
                ),
                atomic_json(
                    contract::broadcast_schema(),
                    contract::BROADCAST_SCHEMA_VERSION,
                ),
                atomic_json(contract::preset_schema(), contract::PRESET_SCHEMA_VERSION),
                atomic_json(contract::asrun_schema(), contract::ASRUN_SCHEMA_VERSION),
            ],
        }
    }

    pub fn implementation_descriptor() -> runtime::world::Implementation {
        let world = Self::new();
        runtime::world::Implementation::from_registration(
            &world.descriptor(),
            2,
            *blake3::hash(
                b"lait.signage.policy-table.v5:media-screen-channel-audience-broadcast-preset-asrun",
            )
            .as_bytes(),
            *blake3::hash(
                b"lait.signage.schemas.v5:program:media:screen:channel:audience:broadcast:preset:asrun",
            )
            .as_bytes(),
        )
    }
}

/// Every Signage document is replaced whole. Nothing here merges, so nothing
/// here needs a collaborative type.
fn atomic_json(id: SchemaId, version: u32) -> Schema {
    Schema {
        id,
        version,
        encoding: contract::program_encoding(),
        mutation: MutationModel::Atomic,
        readable_predecessors: Vec::new(),
    }
}

/// Which document a request is about.
///
/// One resolution serves `submit` and `query`, so a schema can never be
/// writable and unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plane {
    Program,
    Media,
    Screen,
    Channel,
    Audience,
    Broadcast,
    Preset,
    AsRun,
}

impl Plane {
    fn of(schema: &SchemaId, version: u32) -> Result<Self, Rejection> {
        let (plane, current) = match schema.as_str() {
            contract::PROGRAM_SCHEMA => (Self::Program, contract::PROGRAM_SCHEMA_VERSION),
            contract::MEDIA_SCHEMA => (Self::Media, contract::MEDIA_SCHEMA_VERSION),
            contract::SCREEN_SCHEMA => (Self::Screen, contract::SCREEN_SCHEMA_VERSION),
            contract::CHANNEL_SCHEMA => (Self::Channel, contract::CHANNEL_SCHEMA_VERSION),
            contract::AUDIENCE_SCHEMA => (Self::Audience, contract::AUDIENCE_SCHEMA_VERSION),
            contract::BROADCAST_SCHEMA => (Self::Broadcast, contract::BROADCAST_SCHEMA_VERSION),
            contract::PRESET_SCHEMA => (Self::Preset, contract::PRESET_SCHEMA_VERSION),
            contract::ASRUN_SCHEMA => (Self::AsRun, contract::ASRUN_SCHEMA_VERSION),
            _ => return Err(Rejection::UnsupportedSchema),
        };
        if version != current {
            return Err(Rejection::UnsupportedSchema);
        }
        Ok(plane)
    }
}

impl Default for SignageWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl World for SignageWorld {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            id: self.id.clone(),
            implementation_version: Version(5),
            schemas: self.schemas.clone(),
            limits: Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: Vec::new(),
            find_extractors: Vec::new(),
            exec_specs: Vec::new(),
        }
    }

    fn id(&self) -> WorldId {
        self.id.clone()
    }

    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        match Plane::of(&intent.schema, intent.schema_version)? {
            Plane::Program => write_program(&intent.payload),
            Plane::Media => write_media(&intent.payload),
            Plane::Screen => write_screen(&intent.payload),
            Plane::Channel => write_channel(&intent.payload),
            Plane::Audience => self.write_audience(ctx, &intent.payload),
            Plane::Broadcast => write_broadcast(&intent.payload),
            Plane::Preset => write_preset(&intent.payload),
            Plane::AsRun => write_asrun(&intent.payload),
        }
    }

    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        let (bytes, demand) = match Plane::of(&query.schema, query.schema_version)? {
            Plane::Program => self.read_programs(ctx, &query.payload)?,
            Plane::Media => self.read_media(ctx, &query.payload)?,
            Plane::Screen => self.read_screens(ctx, &query.payload)?,
            Plane::Channel => self.read_channels(ctx, &query.payload)?,
            Plane::Audience => self.read_audiences(ctx, &query.payload)?,
            Plane::Broadcast => self.read_broadcasts(ctx, &query.payload)?,
            Plane::Preset => self.read_presets(ctx, &query.payload)?,
            Plane::AsRun => self.read_asrun(ctx, &query.payload)?,
        };
        Ok(Projection {
            schema: query.schema,
            schema_version: query.schema_version,
            bytes,
            frontier: ReplicaFrontier::EMPTY,
            publication: None,
            demand,
        })
    }
}

// ─── Writing ────────────────────────────────────────────────────────────────

fn write_program(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<SignageIntent>(payload)? {
        SignageIntent::Put { program } => {
            if !program.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = program.body_key().ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_program(&program.id);
            Ok(staged(key, replace(&program)?, None, demand))
        }
        SignageIntent::Delete { program } => {
            let key = contract::body_key(&program).ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_program(&program);
            Ok(staged(key, Op::Tombstone, None, demand))
        }
    }
}

fn write_media(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<MediaIntent>(payload)? {
        MediaIntent::Put { media } => {
            if !media.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = media.body_key().ok_or(Rejection::InvalidRequest)?;
            // Always a declaration, never an omission: an entry edited off the
            // content plane must release what it used to name, and absent
            // means unchanged.
            let content = match media.source.content() {
                Some(id) => vec![content_ref(id).ok_or(Rejection::InvalidRequest)?],
                None => Vec::new(),
            };
            let demand = contract::demand_manage_media(&media.id);
            Ok(staged(key, replace(&media)?, Some(content), demand))
        }
        MediaIntent::Delete { media } => {
            let key = contract::body_key(&media).ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_media(&media);
            // The entry was the only thing holding those bytes reachable.
            Ok(staged(key, Op::Tombstone, Some(Vec::new()), demand))
        }
    }
}

fn write_screen(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<ScreenIntent>(payload)? {
        ScreenIntent::Put { screen } => {
            if !screen.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = screen.body_key().ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_screen(&screen.id);
            Ok(staged(key, replace(&screen)?, None, demand))
        }
        ScreenIntent::Delete { screen } => {
            let key = contract::body_key(&screen).ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_screen(&screen);
            Ok(staged(key, Op::Tombstone, None, demand))
        }
    }
}

fn write_channel(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<ChannelIntent>(payload)? {
        ChannelIntent::Put { channel } => {
            if !channel.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = channel.body_key().ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_channel(&channel.id);
            Ok(staged(key, replace(&channel)?, None, demand))
        }
        ChannelIntent::Delete { channel } => {
            let key = contract::body_key(&channel).ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_channel(&channel);
            Ok(staged(key, Op::Tombstone, None, demand))
        }
    }
}

fn write_broadcast(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<BroadcastIntent>(payload)? {
        BroadcastIntent::Put { broadcast } => {
            if !broadcast.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = broadcast.body_key().ok_or(Rejection::InvalidRequest)?;
            // On the transmission and on the audience it names: sending to a
            // set of screens is authority over that set.
            let demand = contract::demand_manage_broadcast(&broadcast.id, &broadcast.audience);
            Ok(staged(key, replace(&broadcast)?, None, demand))
        }
        BroadcastIntent::Delete { broadcast } => {
            let key = contract::body_key(&broadcast).ok_or(Rejection::InvalidRequest)?;
            // A submission stages without reading, so the audience that would
            // widen this demand is not in hand. Fleet-wide, never narrower.
            let demand = contract::demand_manage();
            Ok(staged(key, Op::Tombstone, None, demand))
        }
    }
}

fn write_preset(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<PresetIntent>(payload)? {
        // No read-before-write, and no uniqueness scan. The old config plane
        // refused a second document for a kind so that a lookup *by kind* had
        // one answer; entries name their preset by id now, so a second one is
        // an ordinary row rather than an ambiguity to refuse.
        PresetIntent::Put { preset } => {
            if !preset.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = preset.body_key().ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_preset(&preset.id);
            Ok(staged(key, replace(&preset)?, None, demand))
        }
        PresetIntent::Delete { preset } => {
            let key = contract::body_key(&preset).ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_preset(&preset);
            Ok(staged(key, Op::Tombstone, None, demand))
        }
    }
}

fn write_asrun(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<AsRunIntent>(payload)? {
        AsRunIntent::Record { asrun } => {
            if !asrun.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = asrun.body_key().ok_or(Rejection::InvalidRequest)?;
            // Demanded on the screen, so the only principal who can attest
            // what a panel played is that panel.
            let demand = contract::demand_record_asrun(&asrun.screen);
            Ok(staged(key, replace(&asrun)?, None, demand))
        }
    }
}

impl SignageWorld {
    /// The one write that reads first.
    ///
    /// An audience may name another audience, and a cycle among them would
    /// make evaluation depend on where it started. Bounded hops already stop
    /// it from running forever; refusing the cycle at write is what keeps
    /// "who does this reach" from having two answers. It is a refusal an
    /// author can act on: this audience already reaches through that one.
    fn write_audience(&self, ctx: &mut Context<'_>, payload: &[u8]) -> Result<Effect, Rejection> {
        match decode::<AudienceIntent>(payload)? {
            AudienceIntent::Put { audience } => {
                if !audience.validate() {
                    return Err(Rejection::InvalidRequest);
                }
                let existing = self.audiences(ctx)?;
                if reaches_itself(&audience, &existing) {
                    return Err(Rejection::InvalidRequest);
                }
                let key = audience.body_key().ok_or(Rejection::InvalidRequest)?;
                let demand = contract::demand_manage_audience(&audience.id);
                Ok(staged(key, replace(&audience)?, None, demand))
            }
            AudienceIntent::Delete { audience } => {
                let key = contract::body_key(&audience).ok_or(Rejection::InvalidRequest)?;
                let demand = contract::demand_manage_audience(&audience);
                Ok(staged(key, Op::Tombstone, None, demand))
            }
        }
    }
}

/// Whether this audience, once written, would reach itself by reference.
fn reaches_itself(candidate: &SignageAudience, existing: &[SignageAudience]) -> bool {
    let mut rules: std::collections::BTreeMap<&str, &Match> = existing
        .iter()
        .filter(|other| other.id != candidate.id)
        .map(|other| (other.id.as_str(), &other.rule))
        .collect();
    rules.insert(candidate.id.as_str(), &candidate.rule);

    let mut seen = BTreeSet::new();
    let mut frontier = Vec::new();
    candidate.rule.referenced_audiences(&mut frontier);
    while let Some(next) = frontier.pop() {
        if next == candidate.id {
            return true;
        }
        if !seen.insert(next.clone()) {
            continue;
        }
        if let Some(rule) = rules.get(next.as_str()) {
            rule.referenced_audiences(&mut frontier);
        }
    }
    false
}

/// One row written, and what that row declares about content.
///
/// `Some` replaces the Body's content declaration — `Some(empty)` releases it;
/// `None` leaves it as it was.
fn staged(
    key: BodyKey,
    operation: Op,
    content: Option<Vec<ContentRef>>,
    demand: Vec<u8>,
) -> Effect {
    Effect {
        content_refs: content
            .map(|refs| vec![(key.clone(), refs)])
            .unwrap_or_default(),
        exec: Vec::new(),
        operations: vec![(key.clone(), operation)],
        bodies: vec![key],
        effect: Vec::new(),
        declarations: Vec::new(),
        demand,
    }
}

/// A content id as the upload route rendered it: 32 bytes of lowercase hex.
fn content_ref(raw: &str) -> Option<ContentRef> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    Some(ContentRef {
        content_id: <[u8; 32]>::try_from(bytes.as_slice()).ok()?,
    })
}

fn decode<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, Rejection> {
    serde_json::from_slice(payload).map_err(|_| Rejection::InvalidRequest)
}

fn replace<T: serde::Serialize>(value: &T) -> Result<Op, Rejection> {
    serde_json::to_vec(value)
        .map(|value| Op::ReplaceAtomic { value })
        .map_err(|_| Rejection::InvalidRequest)
}

// ─── Reading ────────────────────────────────────────────────────────────────

/// Projection bytes and the read demand they required.
type Answer = (Vec<u8>, Vec<u8>);

impl SignageWorld {
    fn read_programs(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<SignageQuery>(payload)? {
            SignageQuery::Program { program } => {
                let demand = contract::demand_read_program(&program);
                let key = contract::body_key(&program).ok_or(Rejection::InvalidRequest)?;
                let program = one(ctx, &key, SignageProgram::validate)?;
                let media = match &program {
                    Some(program) => named_media(ctx, program)?,
                    None => Vec::new(),
                };
                Ok((
                    encode(&SignageProjection::Program { program, media })?,
                    demand,
                ))
            }
            SignageQuery::Programs => Ok((
                encode(&SignageProjection::Programs {
                    programs: self.programs(ctx)?,
                })?,
                contract::demand_read(),
            )),
        }
    }

    fn read_media(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<MediaQuery>(payload)? {
            MediaQuery::Media { media } => {
                let demand = contract::demand_read_media(&media);
                let key = contract::body_key(&media).ok_or(Rejection::InvalidRequest)?;
                let media = one(ctx, &key, SignageMedia::validate)?;
                Ok((encode(&MediaProjection::Media { media })?, demand))
            }
            MediaQuery::Library => {
                let media = all(ctx, &self.id, &contract::media_schema(), |media| {
                    SignageMedia::validate(media).then_some((&media.name, &media.id))
                })?;
                Ok((
                    encode(&MediaProjection::Library { media })?,
                    contract::demand_read(),
                ))
            }
            MediaQuery::UsedBy { media } => {
                let programs = self
                    .programs(ctx)?
                    .into_iter()
                    .filter(|program| program.items.iter().any(|item| item.media == media))
                    .map(|program| program.id)
                    .collect();
                Ok((
                    encode(&MediaProjection::UsedBy { programs })?,
                    contract::demand_read(),
                ))
            }
        }
    }

    fn read_screens(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<ScreenQuery>(payload)? {
            ScreenQuery::Screen { screen } => {
                let demand = contract::demand_read_screen(&screen);
                let key = contract::body_key(&screen).ok_or(Rejection::InvalidRequest)?;
                let screen = one(ctx, &key, SignageScreen::validate)?;
                Ok((encode(&ScreenProjection::Screen { screen })?, demand))
            }
            ScreenQuery::Screens => Ok((
                encode(&ScreenProjection::Screens {
                    screens: self.screens(ctx)?,
                })?,
                contract::demand_read(),
            )),
            // One answer carrying every input resolution takes. It is a scan
            // now, where it used to be two reads, because addressing stopped
            // being a pointer the screen holds and became a predicate other
            // documents make about it — you cannot know which broadcasts
            // reach a screen without looking at the broadcasts.
            ScreenQuery::Plays { screen } => {
                let demand = contract::demand_read_screen(&screen);
                let key = contract::body_key(&screen).ok_or(Rejection::InvalidRequest)?;
                let screen = one::<SignageScreen>(ctx, &key, SignageScreen::validate)?;
                let channels = self.channels(ctx)?;
                let broadcasts = self.broadcasts(ctx)?;
                let audiences = self.audiences(ctx)?;
                // Bounded by reachability, not by a clock: every program this
                // screen could land on, whatever the hour, plus the library
                // those name. Sending the whole library instead would make a
                // prepare cost the Space rather than the screen.
                let (programs, media) = match &screen {
                    None => (Vec::new(), Vec::new()),
                    Some(screen) => {
                        let lookup: std::collections::BTreeMap<String, Match> = audiences
                            .iter()
                            .map(|entry| (entry.id.clone(), entry.rule.clone()))
                            .collect();
                        let wanted = reachable_programs(screen, &channels, &broadcasts, &lookup);
                        let programs: Vec<SignageProgram> = self
                            .programs(ctx)?
                            .into_iter()
                            .filter(|program| wanted.contains(&program.id))
                            .collect();
                        let named: BTreeSet<&str> = programs
                            .iter()
                            .flat_map(|program| {
                                program.items.iter().map(|item| item.media.as_str())
                            })
                            .collect();
                        let media = self
                            .library(ctx)?
                            .into_iter()
                            .filter(|entry| named.contains(entry.id.as_str()))
                            .collect();
                        (programs, media)
                    }
                };
                let presets = all(ctx, &self.id, &contract::preset_schema(), |preset| {
                    SignagePreset::validate(preset).then_some((&preset.name, &preset.id))
                })?;
                Ok((
                    encode(&ScreenProjection::Plays {
                        screen,
                        channels,
                        broadcasts,
                        audiences,
                        programs,
                        media,
                        presets,
                    })?,
                    demand,
                ))
            }
            // The blast radius, before anybody presses send. Answered here
            // rather than assembled by a caller so the count an operator is
            // shown is produced by the same evaluator that will decide.
            ScreenQuery::Reaches { audience } => {
                let demand = contract::demand_read_audience(&audience);
                let audiences = self.audiences(ctx)?;
                let lookup: std::collections::BTreeMap<String, Match> = audiences
                    .iter()
                    .map(|entry| (entry.id.clone(), entry.rule.clone()))
                    .collect();
                let screens = match audiences.iter().find(|entry| entry.id == audience) {
                    None => Vec::new(),
                    Some(entry) => {
                        // The World holds no clock, so an `Observed` term is
                        // matched against nothing here and reaches nobody. A
                        // preview is a lower bound, and honestly so.
                        let cx = MatchContext::default();
                        self.screens(ctx)?
                            .into_iter()
                            .filter(|screen| entry.rule.reaches(screen, &cx, &lookup))
                            .map(|screen| screen.id)
                            .collect()
                    }
                };
                Ok((encode(&ScreenProjection::Reaches { screens })?, demand))
            }
            ScreenQuery::Showing { program } => {
                let channels = self.channels(ctx)?;
                let broadcasts = self.broadcasts(ctx)?;
                let audiences: std::collections::BTreeMap<String, Match> = self
                    .audiences(ctx)?
                    .into_iter()
                    .map(|entry| (entry.id, entry.rule))
                    .collect();
                let screens = self
                    .screens(ctx)?
                    .into_iter()
                    .filter(|screen| {
                        could_reach(screen, &program, &channels, &broadcasts, &audiences)
                    })
                    .map(|screen| screen.id)
                    .collect();
                Ok((
                    encode(&ScreenProjection::Showing { screens })?,
                    contract::demand_read(),
                ))
            }
        }
    }

    fn read_channels(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<ChannelQuery>(payload)? {
            ChannelQuery::Channel { channel } => {
                let demand = contract::demand_read_channel(&channel);
                let key = contract::body_key(&channel).ok_or(Rejection::InvalidRequest)?;
                let channel = one(ctx, &key, SignageChannel::validate)?;
                Ok((encode(&ChannelProjection::Channel { channel })?, demand))
            }
            ChannelQuery::Channels => Ok((
                encode(&ChannelProjection::Channels {
                    channels: self.channels(ctx)?,
                })?,
                contract::demand_read(),
            )),
        }
    }

    fn read_audiences(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<AudienceQuery>(payload)? {
            AudienceQuery::Audience { audience } => {
                let demand = contract::demand_read_audience(&audience);
                let key = contract::body_key(&audience).ok_or(Rejection::InvalidRequest)?;
                let audience = one(ctx, &key, SignageAudience::validate)?;
                Ok((encode(&AudienceProjection::Audience { audience })?, demand))
            }
            AudienceQuery::Audiences => Ok((
                encode(&AudienceProjection::Audiences {
                    audiences: self.audiences(ctx)?,
                })?,
                contract::demand_read(),
            )),
        }
    }

    fn read_broadcasts(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<BroadcastQuery>(payload)? {
            BroadcastQuery::Broadcast { broadcast } => {
                let demand = contract::demand_read_broadcast(&broadcast);
                let key = contract::body_key(&broadcast).ok_or(Rejection::InvalidRequest)?;
                let broadcast = one(ctx, &key, SignageBroadcast::validate)?;
                Ok((
                    encode(&BroadcastProjection::Broadcast { broadcast })?,
                    demand,
                ))
            }
            BroadcastQuery::Broadcasts => Ok((
                encode(&BroadcastProjection::Broadcasts {
                    broadcasts: self.broadcasts(ctx)?,
                })?,
                contract::demand_read(),
            )),
        }
    }

    fn read_presets(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<PresetQuery>(payload)? {
            PresetQuery::Preset { preset } => {
                let demand = contract::demand_read_preset(&preset);
                let key = contract::body_key(&preset).ok_or(Rejection::InvalidRequest)?;
                let preset = one(ctx, &key, SignagePreset::validate)?;
                Ok((encode(&PresetProjection::Preset { preset })?, demand))
            }
            PresetQuery::Presets => {
                let presets = all(ctx, &self.id, &contract::preset_schema(), |preset| {
                    SignagePreset::validate(preset).then_some((&preset.name, &preset.id))
                })?;
                Ok((
                    encode(&PresetProjection::Presets { presets })?,
                    contract::demand_read(),
                ))
            }
        }
    }

    fn read_asrun(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<AsRunQuery>(payload)? {
            AsRunQuery::AsRun { screen } => {
                let demand = contract::demand_read_screen(&screen);
                let asrun = self
                    .all_asrun(ctx)?
                    .into_iter()
                    .find(|record| record.screen == screen);
                Ok((encode(&AsRunProjection::AsRun { asrun })?, demand))
            }
        }
    }

    fn library(&self, ctx: &Context<'_>) -> Result<Vec<SignageMedia>, Rejection> {
        all(ctx, &self.id, &contract::media_schema(), |entry| {
            SignageMedia::validate(entry).then_some((&entry.name, &entry.id))
        })
    }

    fn channels(&self, ctx: &Context<'_>) -> Result<Vec<SignageChannel>, Rejection> {
        all(ctx, &self.id, &contract::channel_schema(), |channel| {
            SignageChannel::validate(channel).then_some((&channel.name, &channel.id))
        })
    }

    fn audiences(&self, ctx: &Context<'_>) -> Result<Vec<SignageAudience>, Rejection> {
        all(ctx, &self.id, &contract::audience_schema(), |audience| {
            SignageAudience::validate(audience).then_some((&audience.name, &audience.id))
        })
    }

    fn broadcasts(&self, ctx: &Context<'_>) -> Result<Vec<SignageBroadcast>, Rejection> {
        all(ctx, &self.id, &contract::broadcast_schema(), |broadcast| {
            SignageBroadcast::validate(broadcast).then_some((&broadcast.name, &broadcast.id))
        })
    }

    fn all_asrun(&self, ctx: &Context<'_>) -> Result<Vec<SignageAsRun>, Rejection> {
        all(ctx, &self.id, &contract::asrun_schema(), |record| {
            SignageAsRun::validate(record).then_some((&record.screen, &record.id))
        })
    }

    fn programs(&self, ctx: &Context<'_>) -> Result<Vec<SignageProgram>, Rejection> {
        all(ctx, &self.id, &contract::program_schema(), |program| {
            SignageProgram::validate(program).then_some((&program.name, &program.id))
        })
    }

    fn screens(&self, ctx: &Context<'_>) -> Result<Vec<SignageScreen>, Rejection> {
        all(ctx, &self.id, &contract::screen_schema(), |screen| {
            SignageScreen::validate(screen).then_some((&screen.name, &screen.id))
        })
    }
}

/// The library entries a program's items name, in item order, once each.
///
/// An id that resolves to nothing is skipped rather than fatal: a dangling
/// reference is a program that is still mostly playable, and the surface that
/// asked has no second round trip in which to discover that.
fn named_media(
    ctx: &Context<'_>,
    program: &SignageProgram,
) -> Result<Vec<SignageMedia>, Rejection> {
    let mut seen = BTreeSet::new();
    let mut library = Vec::new();
    for item in &program.items {
        if !seen.insert(&item.media) {
            continue;
        }
        let Some(key) = contract::body_key(&item.media) else {
            continue;
        };
        if let Some(entry) = one(ctx, &key, SignageMedia::validate)? {
            library.push(entry);
        }
    }
    Ok(library)
}

/// Every program this screen could land on, ignoring time.
///
/// The inverse of [`could_reach`], and bounded the same way: a channel it is
/// tuned to contributes its base and every window, and a broadcast whose
/// audience reaches it contributes whatever it plays.
fn reachable_programs(
    screen: &SignageScreen,
    channels: &[SignageChannel],
    broadcasts: &[SignageBroadcast],
    audiences: &std::collections::BTreeMap<String, Match>,
) -> BTreeSet<String> {
    let mut wanted = BTreeSet::new();
    let mut take = |channel: &SignageChannel, into: &mut BTreeSet<String>| {
        if let Some(base) = &channel.base {
            into.insert(base.clone());
        }
        for window in &channel.schedule {
            into.insert(window.program.clone());
        }
    };
    if let Some(id) = screen.tuned.as_deref() {
        if let Some(channel) = channels.iter().find(|candidate| candidate.id == id) {
            take(channel, &mut wanted);
        }
    }
    let cx = MatchContext::default();
    for broadcast in broadcasts {
        let reaches = audiences
            .get(&broadcast.audience)
            .is_some_and(|rule| rule.reaches(screen, &cx, audiences));
        if !reaches {
            continue;
        }
        match &broadcast.action {
            crate::fleet::Action::Play { program } => {
                wanted.insert(program.clone());
            }
            crate::fleet::Action::Tune { channel: id } => {
                if let Some(channel) = channels.iter().find(|candidate| &candidate.id == id) {
                    take(channel, &mut wanted);
                }
            }
            _ => {}
        }
    }
    wanted
}

/// Which screens a program could reach, ignoring time.
///
/// A World callback gets no clock, so this answers *reachability* rather than
/// what is on the glass now: a channel that carries the program anywhere in
/// its schedule counts, and so does a broadcast that plays it, whatever its
/// window says. That is the question a rename or a delete needs answered —
/// "who would notice if this went away" — and answering it with a clock would
/// make a program look unused because nobody happened to be showing it.
fn could_reach(
    screen: &SignageScreen,
    program: &str,
    channels: &[SignageChannel],
    broadcasts: &[SignageBroadcast],
    audiences: &std::collections::BTreeMap<String, Match>,
) -> bool {
    let tuned_to_it = screen.tuned.as_deref().is_some_and(|id| {
        channels.iter().any(|channel| {
            channel.id == id
                && (channel.base.as_deref() == Some(program)
                    || channel
                        .schedule
                        .iter()
                        .any(|window| window.program == program))
        })
    });
    if tuned_to_it {
        return true;
    }
    // Reachability, so `Observed` terms are matched against nothing and a
    // reactive broadcast is reported only when the rest of its audience
    // already reaches this screen.
    let cx = MatchContext::default();
    broadcasts.iter().any(|broadcast| {
        let plays_it = match &broadcast.action {
            crate::fleet::Action::Play { program: played } => played == program,
            crate::fleet::Action::Tune { channel: id } => channels.iter().any(|channel| {
                &channel.id == id
                    && (channel.base.as_deref() == Some(program)
                        || channel
                            .schedule
                            .iter()
                            .any(|window| window.program == program))
            }),
            _ => false,
        };
        plays_it
            && audiences
                .get(&broadcast.audience)
                .is_some_and(|rule| rule.reaches(screen, &cx, audiences))
    })
}

/// One row, decoded and re-checked against its own contract. A row that no
/// longer satisfies it is corrupt state, never a caller's mistake.
fn one<T: serde::de::DeserializeOwned>(
    ctx: &Context<'_>,
    key: &BodyKey,
    valid: fn(&T) -> bool,
) -> Result<Option<T>, Rejection> {
    let Some(bytes) = ctx.read_body(key)? else {
        return Ok(None);
    };
    let row = serde_json::from_slice::<T>(&bytes).map_err(|_| Rejection::StateCorrupt)?;
    if !valid(&row) {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(row))
}

/// Every row of a schema, in one pass, ordered by the key `sort` extracts.
/// `sort` answers `None` for a row that fails its own contract, so validity and
/// ordering stay one decision.
fn all<T: serde::de::DeserializeOwned>(
    ctx: &Context<'_>,
    world: &WorldId,
    schema: &SchemaId,
    sort: for<'a> fn(&'a T) -> Option<(&'a String, &'a String)>,
) -> Result<Vec<T>, Rejection> {
    let mut rows = Vec::new();
    for key in ctx.bodies_with_schema(world, schema) {
        let Some(bytes) = ctx.read_body(&key)? else {
            continue;
        };
        let row = serde_json::from_slice::<T>(&bytes).map_err(|_| Rejection::StateCorrupt)?;
        if sort(&row).is_none() {
            return Err(Rejection::StateCorrupt);
        }
        rows.push(row);
    }
    rows.sort_by(|left, right| sort(left).cmp(&sort(right)));
    Ok(rows)
}

fn encode<T: serde::Serialize>(projection: &T) -> Result<Vec<u8>, Rejection> {
    serde_json::to_vec(projection).map_err(|_| Rejection::ContractViolation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addressing::Compare;
    use crate::contract::{MediaSource, ProgramCycle, SignageItem};
    use crate::fleet::{Action, Resolved, Showing, Timing};
    use mechanics::authorization::AuthorizationDemand;
    use replica::body::BodyId;
    use std::collections::BTreeMap;

    #[test]
    fn reviewed_implementation_is_stable_and_nonzero() {
        let first = SignageWorld::implementation_descriptor().id().unwrap();
        let second = SignageWorld::implementation_descriptor().id().unwrap();
        assert_eq!(first, second);
        assert_ne!(first, [0; 32]);
    }

    /// A schema declared but not dispatched is a Body nobody can write, and
    /// nothing else in the system would report it.
    #[test]
    fn every_declared_schema_dispatches() {
        for schema in SignageWorld::new().schemas() {
            assert!(
                Plane::of(&schema.id, schema.version).is_ok(),
                "{} is declared but not dispatched",
                schema.id
            );
        }
    }

    #[test]
    fn a_stored_entry_declares_the_content_it_names() {
        let media = stored("a");
        let effect = put_media(&media);
        assert_eq!(
            effect.content_refs,
            vec![(
                media.body_key().unwrap(),
                vec![ContentRef {
                    content_id: [0xa1; 32]
                }]
            )]
        );
    }

    /// An entry off the content plane declares an empty set, not nothing: the
    /// same id may have been `Stored` a moment ago, and absent would leave that
    /// declaration standing over bytes nothing names any more.
    #[test]
    fn an_entry_that_names_no_bytes_declares_none() {
        let media = card("b");
        let effect = put_media(&media);
        assert_eq!(
            effect.content_refs,
            vec![(media.body_key().unwrap(), Vec::new())]
        );
    }

    /// The shape check the substrate's descriptor check sits behind: an entry
    /// whose content id is not a rendered id never reaches it.
    #[test]
    fn a_stored_entry_naming_an_unrenderable_content_id_is_refused() {
        let mut media = stored("a");
        media.source = MediaSource::Stored {
            content: "notarenderedid".into(),
            size: 1,
            mime: "video/mp4".into(),
        };
        assert_eq!(
            submit(&media_intent(MediaIntent::Put { media })),
            Err(Rejection::InvalidRequest)
        );
    }

    /// A screen's own grant, or the fleet's. There is no third arm now: a
    /// group used to widen this demand, and which set somebody had filed a
    /// panel under stopped being part of who may write to it.
    #[test]
    fn a_screen_put_is_satisfied_by_the_screen_or_the_fleet() {
        let screen = screen("a", None, Some(program_id()));
        let effect = submit(&screen_intent(ScreenIntent::Put {
            screen: screen.clone(),
        }))
        .unwrap();
        assert_eq!(
            granted_on(&effect.demand),
            BTreeSet::from([vec!["screen".to_owned(), screen.id], Vec::new()])
        );
    }

    /// Without a group there is no group arm — a screen in no group must not be
    /// reachable by a grant on some group.
    #[test]
    fn an_ungrouped_screen_demands_itself_or_the_fleet() {
        let effect = submit(&screen_intent(ScreenIntent::Put {
            screen: screen("a", None, Some(program_id())),
        }))
        .unwrap();
        assert_eq!(
            granted_on(&effect.demand),
            BTreeSet::from([vec!["screen".to_owned(), screen_id("a")], Vec::new()])
        );
    }

    /// The whole ladder, in one place: a broadcast outranks the channel a
    /// screen is tuned to, and a cancelled one stops outranking anything.
    #[test]
    fn a_broadcast_outranks_the_channel_and_a_cancellation_gives_it_back() {
        let menus = channel("menus", Some(program_id()));
        let evacuate = body_id("prg", 21);
        let everyone = audience("all", Match::All);
        let screen = tuned_screen("lobby", Some(menus.id.clone()), &["role:menu"]);

        let lookup: BTreeMap<String, Match> = [(everyone.id.clone(), everyone.rule.clone())].into();
        let cx = MatchContext::at(1_700_000_000_000);

        let quiet = crate::fleet::resolve(&screen, &[menus.clone()], &[], &cx, &lookup);
        assert_eq!(
            quiet.showing,
            Showing::Program {
                program: program_id()
            },
            "with nothing broadcast, the tuned channel answers"
        );

        let mut alert = broadcast(
            "evac",
            &everyone.id,
            Action::Play {
                program: evacuate.clone(),
            },
        );
        let loud = crate::fleet::resolve(
            &screen,
            &[menus.clone()],
            std::slice::from_ref(&alert),
            &cx,
            &lookup,
        );
        assert_eq!(
            loud.showing,
            Showing::Program { program: evacuate },
            "a broadcast interrupts the channel"
        );
        let Some(Resolved::Broadcast { name, .. }) = loud.source else {
            panic!("the answer names the broadcast that won");
        };
        assert_eq!(name, "evac", "why it is showing that, in words");

        alert.cancelled_at_unix_ms = Some(cx.now_unix_ms - 1);
        let restored = crate::fleet::resolve(
            &screen,
            &[menus],
            std::slice::from_ref(&alert),
            &cx,
            &lookup,
        );
        assert_eq!(
            restored.showing,
            Showing::Program {
                program: program_id()
            },
            "an all-clear travels faster than an expiry"
        );
    }

    /// Blank and unaddressed are different facts. Folding them together is the
    /// defect this codebase names everywhere else.
    #[test]
    fn a_blanked_screen_is_not_an_unaddressed_one() {
        let dark = audience(
            "dark",
            Match::Label {
                label: "role:office".into(),
            },
        );
        let lookup: BTreeMap<String, Match> = [(dark.id.clone(), dark.rule.clone())].into();
        let cx = MatchContext::at(1_700_000_000_000);
        let office = tuned_screen("office", None, &["role:office"]);
        let blanked = crate::fleet::resolve(
            &office,
            &[],
            &[broadcast("lights", &dark.id, Action::Blank)],
            &cx,
            &lookup,
        );
        assert_eq!(blanked.showing, Showing::Blank);
        assert!(blanked.source.is_some(), "somebody chose this darkness");

        let nobody = tuned_screen("spare", None, &[]);
        let unaddressed = crate::fleet::resolve(&nobody, &[], &[], &cx, &lookup);
        assert_eq!(unaddressed.showing, Showing::Unaddressed);
        assert!(unaddressed.source.is_none(), "nothing chose this darkness");
    }

    /// Two mosques under one operator, on different reckonings, addressed by
    /// what is true of them rather than by which set somebody filed them under.
    #[test]
    fn an_audience_reaches_by_fact_without_anybody_maintaining_a_label() {
        let makkah = audience(
            "makkah",
            Match::Fact {
                kind: "athan".into(),
                key: "method".into(),
                value: "makkah".into(),
            },
        );
        let lookup: BTreeMap<String, Match> = [(makkah.id.clone(), makkah.rule.clone())].into();
        let cx = MatchContext::at(1_700_000_000_000);

        let mut one = tuned_screen("one", None, &[]);
        one.facts = [(
            "athan".to_string(),
            BTreeMap::from([("method".to_string(), "makkah".to_string())]),
        )]
        .into();
        let mut two = tuned_screen("two", None, &[]);
        two.facts = [(
            "athan".to_string(),
            BTreeMap::from([("method".to_string(), "isna".to_string())]),
        )]
        .into();

        assert!(makkah.rule.reaches(&one, &cx, &lookup));
        assert!(!makkah.rule.reaches(&two, &cx, &lookup));
    }

    /// An observation the screen never reported fails the comparison. Absent
    /// is not zero, and a reactive broadcast must not fire on silence.
    #[test]
    fn an_unreported_observation_reaches_nobody() {
        let busy = Match::Observed {
            key: "queue".into(),
            compare: Compare::Above,
            value: "5".into(),
        };
        let screen = tuned_screen("till", None, &[]);
        assert!(!busy.reaches(&screen, &MatchContext::at(0), &()));
        assert!(busy.reaches(
            &screen,
            &MatchContext::observing(0, [("queue".to_string(), "9".to_string())].into()),
            &()
        ));
        assert!(
            !busy.reaches(
                &screen,
                &MatchContext::observing(0, [("queue".to_string(), "busy".to_string())].into()),
                &()
            ),
            "unparseable is absent, never zero"
        );
    }

    /// An audience that reaches itself would make "who does this reach"
    /// depend on where evaluation started.
    #[test]
    fn an_audience_that_reaches_itself_is_refused() {
        let first = body_id("aud", 41);
        let second = body_id("aud", 42);
        let existing = SignageAudience {
            id: second.clone(),
            name: "second".into(),
            rule: Match::Audience {
                audience: first.clone(),
            },
        };
        let candidate = SignageAudience {
            id: first.clone(),
            name: "first".into(),
            rule: Match::Audience {
                audience: second.clone(),
            },
        };
        let reader = Reader::default().audience(&existing);
        let refused = submit_against(
            &reader,
            &audience_intent(AudienceIntent::Put {
                audience: candidate.clone(),
            }),
        );
        assert!(matches!(refused, Err(Rejection::InvalidRequest)));

        let straight = SignageAudience {
            id: first,
            name: "first".into(),
            rule: Match::All,
        };
        assert!(submit_against(
            &reader,
            &audience_intent(AudienceIntent::Put { audience: straight })
        )
        .is_ok());
    }

    #[test]
    fn showing_finds_the_screens_tuned_to_a_channel_carrying_it() {
        let menus = channel("menus", Some(program_id()));
        let other = channel("other", Some(BodyId::from_bytes([9; 16]).render()));
        let mine = tuned_screen("a", Some(menus.id.clone()), &[]);
        let theirs = tuned_screen("b", Some(other.id.clone()), &[]);
        let idle = tuned_screen("c", None, &[]);
        let reader = Reader::default()
            .screen(&mine)
            .screen(&theirs)
            .screen(&idle)
            .channel(&menus)
            .channel(&other);

        let ScreenProjection::Showing { screens } = ask(
            &reader,
            contract::screen_schema(),
            contract::SCREEN_SCHEMA_VERSION,
            &ScreenQuery::Showing {
                program: program_id(),
            },
        ) else {
            panic!("Showing answers Showing");
        };
        assert_eq!(screens, vec![mine.id]);
    }

    /// A program reached only by a broadcast is still in use, and the index
    /// must see it: "who would notice if this went away" is the question a
    /// delete needs answered, and a program nobody is tuned to can still be
    /// the one an emergency plays.
    #[test]
    fn showing_sees_a_program_reached_only_by_a_broadcast() {
        let everyone = audience("all", Match::All);
        let alert = broadcast(
            "evac",
            &everyone.id,
            Action::Play {
                program: program_id(),
            },
        );
        let unattached = tuned_screen("a", None, &[]);
        let reader = Reader::default()
            .screen(&unattached)
            .audience(&everyone)
            .broadcast(&alert);

        let ScreenProjection::Showing { screens } = ask(
            &reader,
            contract::screen_schema(),
            contract::SCREEN_SCHEMA_VERSION,
            &ScreenQuery::Showing {
                program: program_id(),
            },
        ) else {
            panic!("Showing answers Showing");
        };
        assert_eq!(screens, vec![unattached.id]);
    }

    #[test]
    fn used_by_finds_the_programs_whose_items_name_an_entry() {
        let playing = program(&[("one", &stored("a").id), ("two", &card("b").id)]);
        let mut other = program(&[("one", &card("b").id)]);
        other.id = BodyId::from_bytes([8; 16]).render();
        let reader = Reader::default().program(&playing).program(&other);

        let MediaProjection::UsedBy { programs } = ask(
            &reader,
            contract::media_schema(),
            contract::MEDIA_SCHEMA_VERSION,
            &MediaQuery::UsedBy {
                media: stored("a").id,
            },
        ) else {
            panic!("UsedBy answers UsedBy");
        };
        assert_eq!(programs, vec![playing.id]);
    }

    /// The display surface gets one round trip, so the entries the items name
    /// come back with the program — once each, and a dangling id is skipped
    /// rather than sinking the whole answer.
    #[test]
    fn a_program_arrives_with_the_entries_its_items_name() {
        let missing = BodyId::from_bytes([7; 16]).render();
        let authored = program(&[
            ("one", &stored("a").id),
            ("two", &stored("a").id),
            ("three", &missing),
        ]);
        let reader = Reader::default()
            .program(&authored)
            .media(&stored("a"))
            .media(&card("b"));

        let SignageProjection::Program { program, media } = ask(
            &reader,
            contract::program_schema(),
            contract::PROGRAM_SCHEMA_VERSION,
            &SignageQuery::Program {
                program: authored.id.clone(),
            },
        ) else {
            panic!("Program answers Program");
        };
        assert_eq!(program.map(|program| program.id), Some(authored.id));
        assert_eq!(
            media.iter().map(|row| &row.id).collect::<Vec<_>>(),
            vec![&stored("a").id],
            "once each, and only what the items name"
        );
    }

    /// A kind is configured exactly when a config document exists for it, so
    /// the list is the answer and there is no flag to disagree with it.
    /// One kind, one configuration — because an entry finds it by kind.
    ///
    /// The second document is refused rather than merged: merging would make
    /// the answer depend on which arrived first, which is exactly the ambiguity
    /// the lookup cannot carry.
    /// The refusal this port exists to remove. Two presets for one kind used
    /// to be a contract violation, which is what made a second venue in one
    /// Space impossible to express.
    #[test]
    fn a_second_preset_for_one_kind_is_ordinary() {
        let house = preset("house", "athan");
        let ramadan = preset("ramadan", "athan");
        let reader = Reader::default().preset(&house);
        assert!(
            submit_against(
                &reader,
                &preset_intent(PresetIntent::Put {
                    preset: ramadan.clone()
                })
            )
            .is_ok(),
            "a kind may be presented more than one way"
        );
        assert_ne!(house.id, ramadan.id);
    }

    #[test]
    fn two_entries_of_one_kind_carry_their_own_settings() {
        let mut first = card("a");
        first.source = MediaSource::Kind {
            kind: "youtube".into(),
            preset: None,
            settings: BTreeMap::from([("video_id".to_owned(), "aaaaaaaaaaa".to_owned())]),
        };
        let mut second = card("b");
        second.source = MediaSource::Kind {
            kind: "youtube".into(),
            preset: None,
            settings: BTreeMap::from([("video_id".to_owned(), "bbbbbbbbbbb".to_owned())]),
        };
        assert!(first.validate() && second.validate());
        assert_ne!(first.source, second.source);

        let reader = Reader::default().media(&first).media(&second);
        let MediaProjection::Library { media } = ask(
            &reader,
            contract::media_schema(),
            contract::MEDIA_SCHEMA_VERSION,
            &MediaQuery::Library,
        ) else {
            panic!("Library answers Library");
        };
        assert_eq!(media.len(), 2);
        assert_ne!(media[0].source, media[1].source);
    }

    #[test]
    fn presets_come_back_in_the_list() {
        let house = preset("house", "athan");
        let reader = Reader::default().preset(&house);
        let PresetProjection::Presets { presets } = ask(
            &reader,
            contract::preset_schema(),
            contract::PRESET_SCHEMA_VERSION,
            &PresetQuery::Presets,
        ) else {
            panic!("Presets answers Presets");
        };
        assert_eq!(presets.len(), 1);
        assert_eq!(presets.first().map(|row| row.kind.as_str()), Some("athan"));
    }

    #[test]
    fn deleting_any_document_tombstones_its_row() {
        let deletions = [
            (
                program_intent(SignageIntent::Delete {
                    program: program_id(),
                }),
                program_id(),
            ),
            (
                media_intent(MediaIntent::Delete { media: media_id() }),
                media_id(),
            ),
            (
                screen_intent(ScreenIntent::Delete {
                    screen: screen_id("a"),
                }),
                screen_id("a"),
            ),
            (
                channel_intent(ChannelIntent::Delete {
                    channel: channel("menus", None).id,
                }),
                channel("menus", None).id,
            ),
            (
                audience_intent(AudienceIntent::Delete {
                    audience: audience("all", Match::All).id,
                }),
                audience("all", Match::All).id,
            ),
            (
                broadcast_intent(BroadcastIntent::Delete {
                    broadcast: broadcast("evac", &audience("all", Match::All).id, Action::Blank).id,
                }),
                broadcast("evac", &audience("all", Match::All).id, Action::Blank).id,
            ),
            (
                preset_intent(PresetIntent::Delete {
                    preset: preset("house", "athan").id,
                }),
                preset("house", "athan").id,
            ),
        ];
        for (intent, id) in deletions {
            let effect = submit(&intent).unwrap();
            assert_eq!(
                effect.operations,
                vec![(contract::body_key(&id).unwrap(), Op::Tombstone)]
            );
        }
    }

    /// A tombstoned entry declares no content, which is what makes the bytes
    /// collectable. An unchanged declaration would keep them signed state on
    /// every peer forever.
    #[test]
    fn deleting_a_library_entry_releases_its_content() {
        let effect = submit(&media_intent(MediaIntent::Delete { media: media_id() })).unwrap();
        assert_eq!(
            effect.content_refs,
            vec![(contract::body_key(&media_id()).unwrap(), Vec::new())]
        );
    }

    #[test]
    fn the_library_lists_what_was_put_there() {
        let reader = Reader::default().media(&stored("b")).media(&card("a"));
        let MediaProjection::Library { media } = ask(
            &reader,
            contract::media_schema(),
            contract::MEDIA_SCHEMA_VERSION,
            &MediaQuery::Library,
        ) else {
            panic!("Library answers Library");
        };
        assert_eq!(
            media
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["entry a", "entry b"]
        );
    }

    /// The program plane is unchanged by the other three.
    #[test]
    fn a_program_put_still_stages_one_atomic_replacement() {
        let authored = program(&[("one", &stored("a").id)]);
        let effect = submit(&program_intent(SignageIntent::Put {
            program: authored.clone(),
        }))
        .unwrap();
        assert!(effect.content_refs.is_empty());
        assert_eq!(effect.demand, contract::demand_manage_program(&authored.id));
        assert!(matches!(
            effect.operations.as_slice(),
            [(_, Op::ReplaceAtomic { .. })]
        ));
    }

    #[test]
    fn a_schema_this_world_does_not_declare_is_refused_both_ways() {
        let foreign = SchemaId::parse("signage.playlist").unwrap();
        assert_eq!(
            Plane::of(&foreign, 1),
            Err(Rejection::UnsupportedSchema),
            "an unknown schema"
        );
        assert_eq!(
            Plane::of(
                &contract::media_schema(),
                contract::MEDIA_SCHEMA_VERSION + 1
            ),
            Err(Rejection::UnsupportedSchema),
            "a known schema at an unknown version"
        );
    }

    // ─── fixtures ───────────────────────────────────────────────────────────

    fn body_id(tag: &str, salt: u8) -> String {
        let mut raw = [salt; 16];
        raw[0] = tag.as_bytes()[0];
        BodyId::from_bytes(raw).render()
    }

    fn program_id() -> String {
        BodyId::from_bytes([3; 16]).render()
    }

    fn media_id() -> String {
        BodyId::from_bytes([4; 16]).render()
    }

    fn channel(tag: &str, base: Option<String>) -> SignageChannel {
        SignageChannel {
            id: body_id(tag, 60),
            name: tag.into(),
            base,
            schedule: Vec::new(),
        }
    }

    fn audience(tag: &str, rule: Match) -> SignageAudience {
        SignageAudience {
            id: body_id(tag, 61),
            name: tag.into(),
            rule,
        }
    }

    /// A broadcast that is always open, so a test says what it is testing:
    /// resolution order, not window arithmetic.
    fn broadcast(tag: &str, audience: &str, action: Action) -> SignageBroadcast {
        SignageBroadcast {
            id: body_id(tag, 62),
            name: tag.into(),
            audience: audience.to_string(),
            action,
            timing: Timing::When {
                of: Match::All,
                priority: 10,
            },
            supersedes: Vec::new(),
            cancelled_at_unix_ms: None,
        }
    }

    fn preset(tag: &str, kind: &str) -> SignagePreset {
        SignagePreset {
            id: body_id(tag, 63),
            kind: kind.into(),
            name: tag.into(),
            settings: BTreeMap::new(),
        }
    }

    fn tuned_screen(tag: &str, tuned: Option<String>, labels: &[&str]) -> SignageScreen {
        SignageScreen {
            id: screen_id(tag),
            name: tag.into(),
            place: None,
            facts: BTreeMap::new(),
            sync: None,
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            tuned,
        }
    }

    fn group_id() -> String {
        BodyId::from_bytes([5; 16]).render()
    }

    fn screen_id(tag: &str) -> String {
        body_id(tag, 6)
    }

    fn entry(tag: &str, source: MediaSource) -> SignageMedia {
        SignageMedia {
            id: body_id(tag, 4),
            name: format!("entry {tag}"),
            source,
            duration_ms: None,
            width: None,
            height: None,
            catalog: None,
        }
    }

    fn stored(tag: &str) -> SignageMedia {
        entry(
            tag,
            MediaSource::Stored {
                content: data_encoding::HEXLOWER.encode(&[0xa1; 32]),
                size: 4_096,
                mime: "video/mp4".into(),
            },
        )
    }

    fn card(tag: &str) -> SignageMedia {
        entry(
            tag,
            MediaSource::Card {
                title: "Welcome".into(),
                body: String::new(),
                background: "102030".into(),
                foreground: "ffffff".into(),
            },
        )
    }

    /// A screen tuned to a channel. The second argument used to be a group
    /// and a program in one; a panel now names only what it is tuned to, and
    /// what plays there is the channel's business.
    fn screen(tag: &str, _unused: Option<String>, tuned: Option<String>) -> SignageScreen {
        tuned_screen(tag, tuned, &[])
    }

    fn program(items: &[(&str, &str)]) -> SignageProgram {
        SignageProgram {
            id: program_id(),
            name: "Lobby".into(),
            cycle: ProgramCycle::Loop,
            items: items
                .iter()
                .map(|(id, media)| SignageItem {
                    id: (*id).into(),
                    media: (*media).into(),
                    duration_ms: Some(10_000),
                })
                .collect(),
            windows: Vec::new(),
        }
    }

    // ─── driving the World ──────────────────────────────────────────────────

    fn program_intent(intent: SignageIntent) -> Intent {
        wire(
            contract::program_schema(),
            contract::PROGRAM_SCHEMA_VERSION,
            &intent,
        )
    }

    fn media_intent(intent: MediaIntent) -> Intent {
        wire(
            contract::media_schema(),
            contract::MEDIA_SCHEMA_VERSION,
            &intent,
        )
    }

    fn screen_intent(intent: ScreenIntent) -> Intent {
        wire(
            contract::screen_schema(),
            contract::SCREEN_SCHEMA_VERSION,
            &intent,
        )
    }

    fn channel_intent(intent: ChannelIntent) -> Intent {
        wire(
            contract::channel_schema(),
            contract::CHANNEL_SCHEMA_VERSION,
            &intent,
        )
    }

    fn audience_intent(intent: AudienceIntent) -> Intent {
        wire(
            contract::audience_schema(),
            contract::AUDIENCE_SCHEMA_VERSION,
            &intent,
        )
    }

    fn broadcast_intent(intent: BroadcastIntent) -> Intent {
        wire(
            contract::broadcast_schema(),
            contract::BROADCAST_SCHEMA_VERSION,
            &intent,
        )
    }

    fn preset_intent(intent: PresetIntent) -> Intent {
        wire(
            contract::preset_schema(),
            contract::PRESET_SCHEMA_VERSION,
            &intent,
        )
    }

    fn wire<T: serde::Serialize>(schema: SchemaId, schema_version: u32, body: &T) -> Intent {
        Intent {
            schema,
            schema_version,
            payload: serde_json::to_vec(body).unwrap(),
        }
    }

    fn submit(intent: &Intent) -> Result<Effect, Rejection> {
        submit_against(&Reader::default(), intent)
    }

    fn submit_against(reader: &Reader, intent: &Intent) -> Result<Effect, Rejection> {
        let facts = facts();
        let mut ctx = Context::with_reads(&facts, reader, [0u8; 32]);
        SignageWorld::new().submit(&mut ctx, intent.clone())
    }

    fn put_media(media: &SignageMedia) -> Effect {
        submit(&media_intent(MediaIntent::Put {
            media: media.clone(),
        }))
        .unwrap()
    }

    fn ask<Q: serde::Serialize, P: serde::de::DeserializeOwned>(
        reader: &Reader,
        schema: SchemaId,
        schema_version: u32,
        query: &Q,
    ) -> P {
        let facts = facts();
        let ctx = Context::with_reads(&facts, reader, [0u8; 32]);
        let request = Query {
            schema,
            schema_version,
            payload: serde_json::to_vec(query).unwrap(),
            publication: None,
        };
        let projection = SignageWorld::new().query(&ctx, request).unwrap();
        serde_json::from_slice(&projection.bytes).unwrap()
    }

    /// The resources an `Any` demand's options are granted on. A set, because
    /// any one of them satisfies it and canonical encoding picks the order.
    fn granted_on(demand: &[u8]) -> BTreeSet<Vec<String>> {
        let AuthorizationDemand::Any(options) =
            AuthorizationDemand::decode_canonical(demand).unwrap()
        else {
            panic!("a scoped write is satisfied any of several ways");
        };
        options
            .iter()
            .map(|option| match option {
                AuthorizationDemand::Require { resource, .. } => resource.segments.clone(),
                _ => panic!("each option names one resource"),
            })
            .collect()
    }

    fn facts() -> runtime::world::PrincipalFacts {
        let device = mechanics::actor::device_from_seed(&[3u8; 32]);
        runtime::world::PrincipalFacts {
            actor: mechanics::ids::ActorId::from_incept_hash(&"cd".repeat(32)),
            station: mechanics::station::Key::from_device(&device).unwrap(),
            device,
            space: mechanics::ids::SpaceId::from_digest([5u8; 16]),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
        }
    }

    /// A committed snapshot, indexed by schema the way the durable directory
    /// indexes it, so a projection is exercised over more than one row.
    #[derive(Default)]
    struct Reader {
        rows: BTreeMap<BodyKey, Vec<u8>>,
        by_schema: BTreeMap<String, Vec<BodyKey>>,
    }

    impl Reader {
        fn program(self, row: &SignageProgram) -> Self {
            self.put(contract::PROGRAM_SCHEMA, &row.id, row)
        }

        fn media(self, row: &SignageMedia) -> Self {
            self.put(contract::MEDIA_SCHEMA, &row.id, row)
        }

        fn screen(self, row: &SignageScreen) -> Self {
            self.put(contract::SCREEN_SCHEMA, &row.id, row)
        }

        fn channel(self, row: &SignageChannel) -> Self {
            self.put(contract::CHANNEL_SCHEMA, &row.id, row)
        }

        fn audience(self, row: &SignageAudience) -> Self {
            self.put(contract::AUDIENCE_SCHEMA, &row.id, row)
        }

        fn broadcast(self, row: &SignageBroadcast) -> Self {
            self.put(contract::BROADCAST_SCHEMA, &row.id, row)
        }

        fn preset(self, row: &SignagePreset) -> Self {
            self.put(contract::PRESET_SCHEMA, &row.id, row)
        }

        fn put<T: serde::Serialize>(mut self, schema: &str, id: &str, row: &T) -> Self {
            let key = contract::body_key(id).unwrap();
            self.rows
                .insert(key.clone(), serde_json::to_vec(row).unwrap());
            self.by_schema.entry(schema.into()).or_default().push(key);
            self
        }
    }

    impl runtime::world::BodyReader for Reader {
        fn read_body(
            &self,
            key: &BodyKey,
        ) -> Result<Option<runtime::world::BodyBytes>, runtime::world::BodyReadFailure> {
            Ok(self
                .rows
                .get(key)
                .cloned()
                .map(runtime::world::BodyBytes::owned))
        }
        fn read_collaborative_body(
            &self,
            _key: &BodyKey,
        ) -> Result<Option<runtime::world::CollaborativeBody>, runtime::world::BodyReadFailure>
        {
            Ok(None)
        }
        fn bodies_with_schema(&self, _world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
            self.by_schema
                .get(schema.as_str())
                .cloned()
                .unwrap_or_default()
        }
        fn body_version(&self, _key: &BodyKey) -> Option<fabric::Version> {
            None
        }
        fn anchor_in_body(
            &self,
            _key: &BodyKey,
            _path: &str,
            _position: u64,
        ) -> Result<Option<fabric::Anchor>, runtime::world::BodyReadFailure> {
            Ok(None)
        }
        fn resolve_anchor(
            &self,
            _key: &BodyKey,
            _anchor: &fabric::Anchor,
        ) -> Result<fabric::AnchorResolution, runtime::world::BodyReadFailure> {
            Ok(fabric::AnchorResolution::Drifted)
        }
        fn content_status(&self, _content: &ContentRef) -> Option<runtime::world::ContentStatus> {
            None
        }
    }
}
