use std::collections::BTreeSet;

use replica::body::{BodyKey, MutationModel, Op, Schema, SchemaId, WorldId};
use replica::content::ContentRef;
use replica::frontier::ReplicaFrontier;
use runtime::world::{
    Context, Descriptor, Effect, Intent, Limits, Projection, Query, Rejection, Version, World,
};

use crate::contract::{
    self, ConfigIntent, ConfigProjection, ConfigQuery, GroupIntent, GroupProjection, GroupQuery,
    MediaIntent, MediaProjection, MediaQuery, ScreenIntent, ScreenProjection, ScreenQuery,
    SignageConfig, SignageGroup, SignageIntent, SignageMedia, SignageProgram, SignageProjection,
    SignageQuery, SignageScreen,
};

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
                atomic_json(contract::group_schema(), contract::GROUP_SCHEMA_VERSION),
                atomic_json(contract::config_schema(), contract::CONFIG_SCHEMA_VERSION),
            ],
        }
    }

    pub fn implementation_descriptor() -> runtime::world::Implementation {
        let world = Self::new();
        runtime::world::Implementation::from_registration(
            &world.descriptor(),
            2,
            *blake3::hash(b"lait.signage.policy-table.v4:media-screen-group-config-resources")
                .as_bytes(),
            *blake3::hash(b"lait.signage.schemas.v4:program:media:screen:group:config").as_bytes(),
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
    Group,
    Config,
}

impl Plane {
    fn of(schema: &SchemaId, version: u32) -> Result<Self, Rejection> {
        let (plane, current) = match schema.as_str() {
            contract::PROGRAM_SCHEMA => (Self::Program, contract::PROGRAM_SCHEMA_VERSION),
            contract::MEDIA_SCHEMA => (Self::Media, contract::MEDIA_SCHEMA_VERSION),
            contract::SCREEN_SCHEMA => (Self::Screen, contract::SCREEN_SCHEMA_VERSION),
            contract::GROUP_SCHEMA => (Self::Group, contract::GROUP_SCHEMA_VERSION),
            contract::CONFIG_SCHEMA => (Self::Config, contract::CONFIG_SCHEMA_VERSION),
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
            implementation_version: Version(4),
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
            Plane::Group => write_group(&intent.payload),
            Plane::Config => self.write_config(ctx, &intent.payload),
        }
    }

    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        let (bytes, demand) = match Plane::of(&query.schema, query.schema_version)? {
            Plane::Program => self.read_programs(ctx, &query.payload)?,
            Plane::Media => self.read_media(ctx, &query.payload)?,
            Plane::Screen => self.read_screens(ctx, &query.payload)?,
            Plane::Group => self.read_groups(ctx, &query.payload)?,
            Plane::Config => self.read_configs(ctx, &query.payload)?,
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
            let demand = contract::demand_manage_screen(&screen.id, screen.group.as_deref());
            Ok(staged(key, replace(&screen)?, None, demand))
        }
        ScreenIntent::Delete { screen } => {
            let key = contract::body_key(&screen).ok_or(Rejection::InvalidRequest)?;
            // No group arm: a submission stages without reading, so the group
            // that would widen this demand is not in hand. Narrower, not wider.
            let demand = contract::demand_manage_screen(&screen, None);
            Ok(staged(key, Op::Tombstone, None, demand))
        }
    }
}

fn write_group(payload: &[u8]) -> Result<Effect, Rejection> {
    match decode::<GroupIntent>(payload)? {
        GroupIntent::Put { group } => {
            if !group.validate() {
                return Err(Rejection::InvalidRequest);
            }
            let key = group.body_key().ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_group(&group.id);
            Ok(staged(key, replace(&group)?, None, demand))
        }
        GroupIntent::Delete { group } => {
            let key = contract::body_key(&group).ok_or(Rejection::InvalidRequest)?;
            let demand = contract::demand_manage_group(&group);
            Ok(staged(key, Op::Tombstone, None, demand))
        }
    }
}

impl SignageWorld {
    /// The one write that reads first.
    ///
    /// A library entry reaches its kind's configuration by kind, so two
    /// documents claiming one kind would make "how is weather configured"
    /// answerable two ways. Refusing the second is cheaper than resolving the
    /// ambiguity at every read, and it is a refusal an author can act on:
    /// there is already a document for this kind, edit that one.
    fn write_config(&self, ctx: &mut Context<'_>, payload: &[u8]) -> Result<Effect, Rejection> {
        match decode::<ConfigIntent>(payload)? {
            ConfigIntent::Put { config } => {
                if !config.validate() {
                    return Err(Rejection::InvalidRequest);
                }
                let existing = all(ctx, &self.id, &contract::config_schema(), |config| {
                    SignageConfig::validate(config).then_some((&config.name, &config.id))
                })?;
                if existing.iter().any(|other| config.conflicts_with(other)) {
                    return Err(Rejection::InvalidRequest);
                }
                let key = config.body_key().ok_or(Rejection::InvalidRequest)?;
                let demand = contract::demand_manage_config(&config.id);
                Ok(staged(key, replace(&config)?, None, demand))
            }
            ConfigIntent::Delete { config } => {
                let key = contract::body_key(&config).ok_or(Rejection::InvalidRequest)?;
                let demand = contract::demand_manage_config(&config);
                Ok(staged(key, Op::Tombstone, None, demand))
            }
        }
    }
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
            // Two reads, never a scan: the screen names its group.
            ScreenQuery::Plays { screen } => {
                let demand = contract::demand_read_screen(&screen);
                let key = contract::body_key(&screen).ok_or(Rejection::InvalidRequest)?;
                let screen = one::<SignageScreen>(ctx, &key, SignageScreen::validate)?;
                let group = match screen.as_ref().and_then(|screen| screen.group.as_ref()) {
                    Some(id) => match contract::body_key(id) {
                        Some(key) => one(ctx, &key, SignageGroup::validate)?,
                        None => None,
                    },
                    None => None,
                };
                Ok((encode(&ScreenProjection::Plays { screen, group })?, demand))
            }
            ScreenQuery::Showing { program } => {
                let screens = self
                    .screens(ctx)?
                    .into_iter()
                    .filter(|screen| intends(screen, &program))
                    .map(|screen| screen.id)
                    .collect();
                Ok((
                    encode(&ScreenProjection::Showing { screens })?,
                    contract::demand_read(),
                ))
            }
        }
    }

    fn read_groups(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<GroupQuery>(payload)? {
            GroupQuery::Group { group } => {
                let demand = contract::demand_read_group(&group);
                let key = contract::body_key(&group).ok_or(Rejection::InvalidRequest)?;
                let group = one(ctx, &key, SignageGroup::validate)?;
                Ok((encode(&GroupProjection::Group { group })?, demand))
            }
            GroupQuery::Groups => {
                let groups = all(ctx, &self.id, &contract::group_schema(), |group| {
                    SignageGroup::validate(group).then_some((&group.name, &group.id))
                })?;
                Ok((
                    encode(&GroupProjection::Groups { groups })?,
                    contract::demand_read(),
                ))
            }
        }
    }

    fn read_configs(&self, ctx: &Context<'_>, payload: &[u8]) -> Result<Answer, Rejection> {
        match decode::<ConfigQuery>(payload)? {
            ConfigQuery::Config { config } => {
                let demand = contract::demand_read_config(&config);
                let key = contract::body_key(&config).ok_or(Rejection::InvalidRequest)?;
                let config = one(ctx, &key, SignageConfig::validate)?;
                Ok((encode(&ConfigProjection::Config { config })?, demand))
            }
            // Which kinds are configured is this list and nothing else, so it
            // stays cheap enough to ask before drawing a picker.
            ConfigQuery::Configs => {
                let configs = all(ctx, &self.id, &contract::config_schema(), |config| {
                    SignageConfig::validate(config).then_some((&config.name, &config.id))
                })?;
                Ok((
                    encode(&ConfigProjection::Configs { configs })?,
                    contract::demand_read(),
                ))
            }
        }
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

/// Whether a screen's slot names `program` at all.
///
/// A World callback gets no clock, so `intended_at` cannot be asked here and a
/// lapsed override still answers: this index says which screens *name* a
/// program, which is the question a rename or a delete needs answered.
fn intends(screen: &SignageScreen, program: &str) -> bool {
    screen
        .intent
        .base
        .as_ref()
        .is_some_and(|base| base.member == program)
        || screen
            .intent
            .over
            .as_ref()
            .is_some_and(|over| over.choice.member == program)
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
    use crate::contract::{MediaSource, ProgramCycle, SignageItem};
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

    #[test]
    fn a_screen_put_is_satisfied_by_the_screen_its_group_or_the_fleet() {
        let screen = screen("a", Some(group_id()), Some(program_id()));
        let effect = submit(&screen_intent(ScreenIntent::Put {
            screen: screen.clone(),
        }))
        .unwrap();
        assert_eq!(
            granted_on(&effect.demand),
            BTreeSet::from([
                vec!["screen".to_owned(), screen.id],
                vec!["group".to_owned(), group_id()],
                Vec::new(),
            ])
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

    /// The ladder's inputs arrive together, and resolve at the caller's clock.
    ///
    /// One round trip, and the group comes back with the screen that named it
    /// — pairing them at the caller is how a screen gets resolved against a
    /// group it does not belong to.
    #[test]
    fn plays_returns_the_screen_with_the_group_it_inherits_from() {
        let inherited = body_id("inherited", 4);
        let group = SignageGroup {
            id: body_id("lobbies", 5),
            name: "Lobbies".into(),
            intent: register::Slot {
                base: Some(register::Choice {
                    member: inherited.clone(),
                    chosen_unix_ms: 1,
                    chooser: "someone".into(),
                }),
                over: None,
            },
            screens: Vec::new(),
        };
        let attached = screen("a", Some(group.id.clone()), None);
        let reader = Reader::default().screen(&attached).group(&group);

        let answer: ScreenProjection = ask(
            &reader,
            contract::screen_schema(),
            contract::SCREEN_SCHEMA_VERSION,
            &ScreenQuery::Plays {
                screen: attached.id.clone(),
            },
        );
        let ScreenProjection::Plays {
            screen: got,
            group: from,
        } = &answer
        else {
            panic!("Plays answers Plays");
        };
        assert_eq!(got.as_ref().map(|row| &row.id), Some(&attached.id));
        assert_eq!(from.as_ref().map(|row| &row.id), Some(&group.id));

        let playback = answer.playback(1_000).expect("the screen exists").unwrap();
        assert_eq!(playback.program.as_ref(), Some(&inherited));
        assert_eq!(playback.source, Some(contract::PlaybackSource::Group));

        // A screen in no group answers with none, and resolves to nothing
        // rather than failing.
        let alone = screen("b", None, None);
        let answer: ScreenProjection = ask(
            &Reader::default().screen(&alone),
            contract::screen_schema(),
            contract::SCREEN_SCHEMA_VERSION,
            &ScreenQuery::Plays {
                screen: alone.id.clone(),
            },
        );
        assert!(matches!(
            &answer,
            ScreenProjection::Plays { group: None, .. }
        ));
        assert_eq!(answer.playback(1_000).unwrap().unwrap().program, None);

        // A screen that is not there is absent, never a corrupt read.
        let answer: ScreenProjection = ask(
            &Reader::default(),
            contract::screen_schema(),
            contract::SCREEN_SCHEMA_VERSION,
            &ScreenQuery::Plays {
                screen: body_id("missing", 9),
            },
        );
        assert!(matches!(
            answer,
            ScreenProjection::Plays { screen: None, .. }
        ));
        assert!(answer.playback(1_000).is_none());
    }

    #[test]
    fn showing_finds_the_screens_that_intend_that_program() {
        let mine = screen("a", None, Some(program_id()));
        let theirs = screen("b", None, Some(BodyId::from_bytes([9; 16]).render()));
        let idle = screen("c", None, None);
        let reader = Reader::default()
            .screen(&mine)
            .screen(&theirs)
            .screen(&idle);

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

    /// An override names a program the base does not, and the index must see
    /// it: a program under a live override is still in use.
    #[test]
    fn showing_sees_a_program_named_only_by_an_override() {
        let mut overridden = screen("a", None, Some(BodyId::from_bytes([9; 16]).render()));
        overridden.intent.over = Some(register::Override {
            choice: register::Choice {
                member: program_id(),
                chosen_unix_ms: 1_000,
                chooser: "someone".into(),
            },
            until_unix_ms: 2_000,
        });
        let reader = Reader::default().screen(&overridden);

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
        assert_eq!(screens, vec![overridden.id]);
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
    #[test]
    fn a_second_configuration_of_one_kind_is_refused_and_editing_the_first_is_not() {
        let existing = config("weather");
        let reader = Reader::default().config(&existing);

        let mut second = config("weather");
        second.id = body_id("weather-again", 9);
        assert!(matches!(
            submit_against(
                &reader,
                &config_intent(ConfigIntent::Put { config: second })
            ),
            Err(Rejection::InvalidRequest)
        ));

        let mut edited = existing.clone();
        edited
            .settings
            .insert("units".to_owned(), "imperial".to_owned());
        assert!(
            submit_against(
                &reader,
                &config_intent(ConfigIntent::Put { config: edited })
            )
            .is_ok(),
            "the same document may be rewritten"
        );

        assert!(
            submit_against(
                &reader,
                &config_intent(ConfigIntent::Put {
                    config: config("athan")
                })
            )
            .is_ok(),
            "a different kind is not a conflict"
        );
    }

    /// Two entries of one kind differ by their own settings.
    ///
    /// Medusa kept these on the content row and could hold two YouTube videos.
    /// An earlier shape here named a shared config document instead, which made
    /// every entry of a kind the same entry.
    #[test]
    fn two_entries_of_one_kind_carry_their_own_settings() {
        let mut first = card("a");
        first.source = MediaSource::Kind {
            kind: "youtube".into(),
            settings: BTreeMap::from([("video_id".to_owned(), "aaaaaaaaaaa".to_owned())]),
        };
        let mut second = card("b");
        second.source = MediaSource::Kind {
            kind: "youtube".into(),
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
    fn a_kind_is_configured_when_its_document_is_in_the_list() {
        let reader = Reader::default().config(&config("weather"));
        let ConfigProjection::Configs { configs } = ask(
            &reader,
            contract::config_schema(),
            contract::CONFIG_SCHEMA_VERSION,
            &ConfigQuery::Configs,
        ) else {
            panic!("Configs answers Configs");
        };
        assert_eq!(
            configs
                .iter()
                .map(|row| row.kind.as_str())
                .collect::<Vec<_>>(),
            ["weather"]
        );
    }

    #[test]
    fn deleting_any_of_the_five_tombstones_its_row() {
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
                group_intent(GroupIntent::Delete { group: group_id() }),
                group_id(),
            ),
            (
                config_intent(ConfigIntent::Delete {
                    config: config("weather").id,
                }),
                config("weather").id,
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
            poster: None,
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

    fn config(kind: &str) -> SignageConfig {
        SignageConfig {
            id: body_id(kind, 7),
            kind: kind.into(),
            name: format!("{kind} settings"),
            settings: BTreeMap::from([("units".to_owned(), "metric".to_owned())]),
        }
    }

    fn screen(tag: &str, group: Option<String>, program: Option<String>) -> SignageScreen {
        SignageScreen {
            id: screen_id(tag),
            name: format!("screen {tag}"),
            group,
            intent: register::Slot {
                base: program.map(|member| register::Choice {
                    member,
                    chosen_unix_ms: 1,
                    chooser: "someone".into(),
                }),
                over: None,
            },
            schedule: Vec::new(),
        }
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

    fn group_intent(intent: GroupIntent) -> Intent {
        wire(
            contract::group_schema(),
            contract::GROUP_SCHEMA_VERSION,
            &intent,
        )
    }

    fn config_intent(intent: ConfigIntent) -> Intent {
        wire(
            contract::config_schema(),
            contract::CONFIG_SCHEMA_VERSION,
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

        fn config(self, row: &SignageConfig) -> Self {
            self.put(contract::CONFIG_SCHEMA, &row.id, row)
        }

        fn group(self, row: &SignageGroup) -> Self {
            self.put(contract::GROUP_SCHEMA, &row.id, row)
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
