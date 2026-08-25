use replica::body::{BodyId, SchemaId};
use runtime::world::call::{Access, Call, Code, Context, Failure, Handler, Reply};
use runtime::world::RequestId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use signage::contract;

pub const OPERATION: &str = "signage.control";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum SignageRequest {
    ProgramGet {
        program: String,
    },
    ProgramList,
    ProgramPut {
        program: signage::SignageProgram,
    },
    ProgramDelete {
        program: String,
    },
    MediaGet {
        media: String,
    },
    MediaList,
    MediaPut {
        media: contract::SignageMedia,
    },
    MediaDelete {
        media: String,
    },
    /// Which programs play a library entry. Asked before deleting one, so the
    /// answer has to exist before the deletion is offered.
    MediaUsedBy {
        media: String,
    },
    ScreenGet {
        screen: String,
    },
    ScreenList,
    ScreenPut {
        screen: signage::SignageScreen,
    },
    ScreenDelete {
        screen: String,
    },
    /// Which screens intend a program, answered from the World's own index
    /// rather than by fetching every screen and filtering at the caller.
    ScreenShowing {
        program: String,
    },
    /// What one screen plays, with the group it inherits from, so the caller
    /// can resolve the ladder against its own clock. See
    /// [`contract::ScreenQuery::Plays`] for why the World does not resolve it.
    ScreenPlays {
        screen: String,
    },
    ChannelGet {
        channel: String,
    },
    ChannelList,
    ChannelPut {
        channel: signage::SignageChannel,
    },
    ChannelDelete {
        channel: String,
    },
    AudienceGet {
        audience: String,
    },
    AudienceList,
    AudiencePut {
        audience: signage::SignageAudience,
    },
    AudienceDelete {
        audience: String,
    },
    /// Which screens an audience reaches. Asked before anything is sent.
    AudienceReaches {
        audience: String,
    },
    BroadcastGet {
        broadcast: String,
    },
    BroadcastList,
    BroadcastPut {
        broadcast: signage::SignageBroadcast,
    },
    BroadcastDelete {
        broadcast: String,
    },
    /// What a screen played, as the screen tells it.
    AsRunGet {
        screen: String,
    },
    AsRunRecord {
        asrun: contract::SignageAsRun,
    },
    PresetGet {
        preset: String,
    },
    /// A kind is configured exactly when a config exists for it, so listing is
    /// how a caller learns which are — there is no flag to read.
    PresetList,
    PresetPut {
        preset: contract::SignagePreset,
    },
    PresetDelete {
        preset: String,
    },
}

impl SignageRequest {
    pub fn access(&self) -> Access {
        match self {
            Self::ProgramGet { .. }
            | Self::ProgramList
            | Self::MediaGet { .. }
            | Self::MediaList
            | Self::MediaUsedBy { .. }
            | Self::ScreenGet { .. }
            | Self::ScreenList
            | Self::ScreenShowing { .. }
            | Self::ScreenPlays { .. }
            | Self::ChannelGet { .. }
            | Self::ChannelList
            | Self::AudienceGet { .. }
            | Self::AudienceList
            | Self::AudienceReaches { .. }
            | Self::BroadcastGet { .. }
            | Self::BroadcastList
            | Self::AsRunGet { .. }
            | Self::PresetGet { .. }
            | Self::PresetList => Access::Query,
            Self::ProgramPut { .. }
            | Self::ProgramDelete { .. }
            | Self::MediaPut { .. }
            | Self::MediaDelete { .. }
            | Self::ScreenPut { .. }
            | Self::ScreenDelete { .. }
            | Self::ChannelPut { .. }
            | Self::ChannelDelete { .. }
            | Self::AudiencePut { .. }
            | Self::AudienceDelete { .. }
            | Self::BroadcastPut { .. }
            | Self::BroadcastDelete { .. }
            | Self::AsRunRecord { .. }
            | Self::PresetPut { .. }
            | Self::PresetDelete { .. } => Access::Command,
        }
    }

    /// The question a client must ask before running this, or `None` when the
    /// request is unremarkable.
    ///
    /// Only deletion asks. A delete tombstones a replicated Body, so it is not
    /// a local erasure that the author can reconsider — it converges to every
    /// replica of the Space, and the Body plane carries no undo. A put is
    /// ordinary authoring and a query changes nothing.
    ///
    /// The silent arm is spelled out rather than wildcarded, so a delete added
    /// later cannot arrive unasked.
    pub fn destructive_question(&self) -> Option<String> {
        let (noun, id) = match self {
            Self::ProgramDelete { program } => ("program", program),
            Self::MediaDelete { media } => ("media", media),
            Self::ScreenDelete { screen } => ("screen", screen),
            Self::ChannelDelete { channel } => ("channel", channel),
            Self::AudienceDelete { audience } => ("audience", audience),
            Self::BroadcastDelete { broadcast } => ("broadcast", broadcast),
            Self::PresetDelete { preset } => ("preset", preset),
            Self::ProgramGet { .. }
            | Self::ProgramList
            | Self::ProgramPut { .. }
            | Self::MediaGet { .. }
            | Self::MediaList
            | Self::MediaPut { .. }
            | Self::MediaUsedBy { .. }
            | Self::ScreenGet { .. }
            | Self::ScreenList
            | Self::ScreenPut { .. }
            | Self::ScreenShowing { .. }
            | Self::ScreenPlays { .. }
            | Self::ChannelGet { .. }
            | Self::ChannelList
            | Self::ChannelPut { .. }
            | Self::AudienceGet { .. }
            | Self::AudienceList
            | Self::AudienceReaches { .. }
            | Self::AudiencePut { .. }
            | Self::BroadcastGet { .. }
            | Self::BroadcastList
            | Self::BroadcastPut { .. }
            | Self::AsRunGet { .. }
            | Self::AsRunRecord { .. }
            | Self::PresetGet { .. }
            | Self::PresetList
            | Self::PresetPut { .. } => return None,
        };
        Some(format!(
            "Delete signage {noun} {id}? This removes it for everyone in the Space."
        ))
    }
}

/// One variant per projection the World returns, plus one acknowledgement per
/// write.
///
/// `saved` and `deleted` are the program's and stay unprefixed: they are the
/// wire form this operation shipped with, and readers already match them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignageResponse {
    /// The program and the library entries its items name, together, because
    /// fetching the rows one by one is the round trip this surface removes.
    ///
    /// `presets` is every kind presentation, joined here so a display prepare
    /// that is one World Query can resolve an entry's preset without a second
    /// ClientInvocation. Absent on older answers; default empty.
    Program {
        program: Option<signage::SignageProgram>,
        media: Vec<contract::SignageMedia>,
        #[serde(default)]
        presets: Vec<contract::SignagePreset>,
    },
    Programs {
        programs: Vec<signage::SignageProgram>,
    },
    Saved {
        program: String,
    },
    Deleted {
        program: String,
    },
    Media {
        media: Option<contract::SignageMedia>,
    },
    Library {
        media: Vec<contract::SignageMedia>,
    },
    MediaSaved {
        media: String,
    },
    MediaDeleted {
        media: String,
    },
    UsedBy {
        programs: Vec<String>,
    },
    Screen {
        screen: Option<signage::SignageScreen>,
    },
    Screens {
        screens: Vec<signage::SignageScreen>,
    },
    Showing {
        screens: Vec<String>,
    },
    /// The inputs to the ladder, never its answer — the caller brings the clock
    /// and calls [`contract::ScreenProjection::playback`].
    Plays {
        screen: Option<signage::SignageScreen>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        channels: Vec<signage::SignageChannel>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        broadcasts: Vec<signage::SignageBroadcast>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        audiences: Vec<signage::SignageAudience>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        programs: Vec<signage::SignageProgram>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media: Vec<contract::SignageMedia>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        presets: Vec<contract::SignagePreset>,
    },
    ScreenSaved {
        screen: String,
    },
    ScreenDeleted {
        screen: String,
    },
    Channel {
        channel: Option<signage::SignageChannel>,
    },
    Channels {
        channels: Vec<signage::SignageChannel>,
    },
    ChannelSaved {
        channel: String,
    },
    ChannelDeleted {
        channel: String,
    },
    Audience {
        audience: Option<signage::SignageAudience>,
    },
    Audiences {
        audiences: Vec<signage::SignageAudience>,
    },
    AudienceSaved {
        audience: String,
    },
    AudienceDeleted {
        audience: String,
    },
    /// The blast radius, by screen id.
    Reaches {
        screens: Vec<String>,
    },
    Broadcast {
        broadcast: Option<signage::SignageBroadcast>,
    },
    Broadcasts {
        broadcasts: Vec<signage::SignageBroadcast>,
    },
    BroadcastSaved {
        broadcast: String,
    },
    BroadcastDeleted {
        broadcast: String,
    },
    AsRun {
        asrun: Option<contract::SignageAsRun>,
    },
    AsRunRecorded {
        screen: String,
    },
    Preset {
        preset: Option<contract::SignagePreset>,
    },
    Presets {
        presets: Vec<contract::SignagePreset>,
    },
    PresetSaved {
        preset: String,
    },
    PresetDeleted {
        preset: String,
    },
    Error {
        message: String,
    },
}

pub fn encode_call(request: &SignageRequest) -> Result<Call, Failure> {
    let payload = serde_json::to_vec(request).map_err(|error| {
        Failure::new(
            Code::InvalidCall,
            format!("encode Signage request: {error}"),
        )
    })?;
    Call::new(signage::contract::world_id(), OPERATION, VERSION, payload)
}

pub fn decode_call(call: &Call) -> Result<SignageRequest, Failure> {
    validate_contract(call)?;
    serde_json::from_slice(call.payload()).map_err(|error| {
        Failure::new(
            Code::InvalidCall,
            format!("decode Signage request: {error}"),
        )
    })
}

pub fn decode_reply(call: &Call, reply: Reply) -> Result<Value, Failure> {
    reply.validate_for(call)?;
    serde_json::from_slice(&reply.into_result()?)
        .map_err(|error| Failure::new(Code::Internal, format!("decode Signage response: {error}")))
}

fn validate_contract(call: &Call) -> Result<(), Failure> {
    if call.world() != &signage::contract::world_id() {
        return Err(Failure::new(Code::InvalidCall, "wrong Signage World"));
    }
    if call.operation() != OPERATION {
        return Err(Failure::new(
            Code::UnsupportedOperation,
            "unsupported Signage operation",
        ));
    }
    if call.version() != VERSION {
        return Err(Failure::new(
            Code::UnsupportedVersion,
            "unsupported Signage protocol version",
        ));
    }
    Ok(())
}

/// One document type of the Signage World, bound to the schema pair that
/// speaks it.
///
/// Each type has its own schema and version, and a payload carried under
/// another one is refused as `UnsupportedSchema` — a refusal that names neither
/// the document nor the pair it should have carried. Binding the pair to the
/// query and intent types makes the mismatch unwritable instead.
trait Document {
    type Query: Serialize;
    type Intent: Serialize;
    type Projection: DeserializeOwned;
    const VERSION: u32;
    fn schema() -> SchemaId;
}

struct Program;
struct Media;
struct Screen;
struct Channel;
struct Audience;
struct Broadcast;
struct Preset;
struct AsRun;

impl Document for Program {
    type Query = signage::SignageQuery;
    type Intent = signage::SignageIntent;
    type Projection = signage::SignageProjection;
    const VERSION: u32 = contract::PROGRAM_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::program_schema()
    }
}

impl Document for Media {
    type Query = contract::MediaQuery;
    type Intent = contract::MediaIntent;
    type Projection = contract::MediaProjection;
    const VERSION: u32 = contract::MEDIA_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::media_schema()
    }
}

impl Document for Screen {
    type Query = contract::ScreenQuery;
    type Intent = contract::ScreenIntent;
    type Projection = contract::ScreenProjection;
    const VERSION: u32 = contract::SCREEN_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::screen_schema()
    }
}

impl Document for Channel {
    type Query = contract::ChannelQuery;
    type Intent = contract::ChannelIntent;
    type Projection = contract::ChannelProjection;
    const VERSION: u32 = contract::CHANNEL_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::channel_schema()
    }
}

impl Document for Audience {
    type Query = contract::AudienceQuery;
    type Intent = contract::AudienceIntent;
    type Projection = contract::AudienceProjection;
    const VERSION: u32 = contract::AUDIENCE_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::audience_schema()
    }
}

impl Document for Broadcast {
    type Query = contract::BroadcastQuery;
    type Intent = contract::BroadcastIntent;
    type Projection = contract::BroadcastProjection;
    const VERSION: u32 = contract::BROADCAST_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::broadcast_schema()
    }
}

impl Document for AsRun {
    type Query = contract::AsRunQuery;
    type Intent = contract::AsRunIntent;
    type Projection = contract::AsRunProjection;
    const VERSION: u32 = contract::ASRUN_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::asrun_schema()
    }
}

impl Document for Preset {
    type Query = contract::PresetQuery;
    type Intent = contract::PresetIntent;
    type Projection = contract::PresetProjection;
    const VERSION: u32 = contract::PRESET_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::preset_schema()
    }
}

#[derive(Debug, Default)]
pub struct SignageCallHandler;

impl SignageCallHandler {
    fn route(request: SignageRequest, context: &Context<'_>) -> SignageResponse {
        match request {
            SignageRequest::ProgramGet { program } => {
                Self::program_query(signage::SignageQuery::Program { program }, context)
            }
            SignageRequest::ProgramList => {
                Self::program_query(signage::SignageQuery::Programs, context)
            }
            SignageRequest::ProgramPut { program } => {
                if !program.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage program".into(),
                    };
                }
                let id = program.id.clone();
                match Self::submit::<Program>(signage::SignageIntent::Put { program }, context) {
                    Ok(()) => SignageResponse::Saved { program: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::ProgramDelete { program } => {
                if BodyId::parse(&program).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage program id".into(),
                    };
                }
                match Self::submit::<Program>(
                    signage::SignageIntent::Delete {
                        program: program.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::Deleted { program },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::MediaGet { media } => {
                Self::media_query(contract::MediaQuery::Media { media }, context)
            }
            SignageRequest::MediaList => Self::media_query(contract::MediaQuery::Library, context),
            SignageRequest::MediaUsedBy { media } => {
                Self::media_query(contract::MediaQuery::UsedBy { media }, context)
            }
            SignageRequest::MediaPut { media } => {
                if !media.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage media".into(),
                    };
                }
                let id = media.id.clone();
                match Self::submit::<Media>(contract::MediaIntent::Put { media }, context) {
                    Ok(()) => SignageResponse::MediaSaved { media: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::MediaDelete { media } => {
                if BodyId::parse(&media).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage media id".into(),
                    };
                }
                match Self::submit::<Media>(
                    contract::MediaIntent::Delete {
                        media: media.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::MediaDeleted { media },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::ScreenGet { screen } => {
                Self::screen_query(contract::ScreenQuery::Screen { screen }, context)
            }
            SignageRequest::ScreenList => {
                Self::screen_query(contract::ScreenQuery::Screens, context)
            }
            SignageRequest::ScreenShowing { program } => {
                Self::screen_query(contract::ScreenQuery::Showing { program }, context)
            }
            SignageRequest::ScreenPlays { screen } => {
                Self::screen_query(contract::ScreenQuery::Plays { screen }, context)
            }
            SignageRequest::ScreenPut { screen } => {
                if !screen.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage screen".into(),
                    };
                }
                let id = screen.id.clone();
                match Self::submit::<Screen>(contract::ScreenIntent::Put { screen }, context) {
                    Ok(()) => SignageResponse::ScreenSaved { screen: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::ScreenDelete { screen } => {
                if BodyId::parse(&screen).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage screen id".into(),
                    };
                }
                match Self::submit::<Screen>(
                    contract::ScreenIntent::Delete {
                        screen: screen.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::ScreenDeleted { screen },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::ChannelGet { channel } => {
                Self::channel_query(contract::ChannelQuery::Channel { channel }, context)
            }
            SignageRequest::ChannelList => {
                Self::channel_query(contract::ChannelQuery::Channels, context)
            }
            SignageRequest::ChannelPut { channel } => {
                if !channel.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage channel".into(),
                    };
                }
                let id = channel.id.clone();
                match Self::submit::<Channel>(contract::ChannelIntent::Put { channel }, context) {
                    Ok(()) => SignageResponse::ChannelSaved { channel: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::ChannelDelete { channel } => {
                if BodyId::parse(&channel).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage channel id".into(),
                    };
                }
                match Self::submit::<Channel>(
                    contract::ChannelIntent::Delete {
                        channel: channel.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::ChannelDeleted { channel },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::AudienceGet { audience } => {
                Self::audience_query(contract::AudienceQuery::Audience { audience }, context)
            }
            SignageRequest::AudienceList => {
                Self::audience_query(contract::AudienceQuery::Audiences, context)
            }
            SignageRequest::AudienceReaches { audience } => {
                match Self::ask::<Screen>(contract::ScreenQuery::Reaches { audience }, context) {
                    Ok(contract::ScreenProjection::Reaches { screens }) => {
                        SignageResponse::Reaches { screens }
                    }
                    Ok(_) => SignageResponse::Error {
                        message: "Reaches answered something else".into(),
                    },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::AudiencePut { audience } => {
                if !audience.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage audience".into(),
                    };
                }
                let id = audience.id.clone();
                match Self::submit::<Audience>(contract::AudienceIntent::Put { audience }, context)
                {
                    Ok(()) => SignageResponse::AudienceSaved { audience: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::AudienceDelete { audience } => {
                if BodyId::parse(&audience).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage audience id".into(),
                    };
                }
                match Self::submit::<Audience>(
                    contract::AudienceIntent::Delete {
                        audience: audience.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::AudienceDeleted { audience },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::BroadcastGet { broadcast } => {
                Self::broadcast_query(contract::BroadcastQuery::Broadcast { broadcast }, context)
            }
            SignageRequest::BroadcastList => {
                Self::broadcast_query(contract::BroadcastQuery::Broadcasts, context)
            }
            SignageRequest::BroadcastPut { broadcast } => {
                if !broadcast.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage broadcast".into(),
                    };
                }
                let id = broadcast.id.clone();
                match Self::submit::<Broadcast>(
                    contract::BroadcastIntent::Put { broadcast },
                    context,
                ) {
                    Ok(()) => SignageResponse::BroadcastSaved { broadcast: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::BroadcastDelete { broadcast } => {
                if BodyId::parse(&broadcast).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage broadcast id".into(),
                    };
                }
                match Self::submit::<Broadcast>(
                    contract::BroadcastIntent::Delete {
                        broadcast: broadcast.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::BroadcastDeleted { broadcast },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::AsRunGet { screen } => {
                match Self::ask::<AsRun>(contract::AsRunQuery::AsRun { screen }, context) {
                    Ok(contract::AsRunProjection::AsRun { asrun }) => {
                        SignageResponse::AsRun { asrun }
                    }
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::AsRunRecord { asrun } => {
                if !asrun.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage as-run record".into(),
                    };
                }
                let screen = asrun.screen.clone();
                match Self::submit::<AsRun>(contract::AsRunIntent::Record { asrun }, context) {
                    Ok(()) => SignageResponse::AsRunRecorded { screen },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::PresetGet { preset } => {
                Self::preset_query(contract::PresetQuery::Preset { preset }, context)
            }
            SignageRequest::PresetList => {
                Self::preset_query(contract::PresetQuery::Presets, context)
            }
            SignageRequest::PresetPut { preset } => {
                if !preset.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage preset".into(),
                    };
                }
                let id = preset.id.clone();
                match Self::submit::<Preset>(contract::PresetIntent::Put { preset }, context) {
                    Ok(()) => SignageResponse::PresetSaved { preset: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::PresetDelete { preset } => {
                if BodyId::parse(&preset).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage preset id".into(),
                    };
                }
                match Self::submit::<Preset>(
                    contract::PresetIntent::Delete {
                        preset: preset.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::PresetDeleted { preset },
                    Err(message) => SignageResponse::Error { message },
                }
            }
        }
    }

    fn program_query(query: signage::SignageQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Program>(query, context) {
            Ok(signage::SignageProjection::Program { program, media }) => {
                SignageResponse::Program {
                    program,
                    media,
                    presets: Self::presets_or_empty(context),
                }
            }
            Ok(signage::SignageProjection::Programs { programs }) => {
                SignageResponse::Programs { programs }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn media_query(query: contract::MediaQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Media>(query, context) {
            Ok(contract::MediaProjection::Media { media }) => SignageResponse::Media { media },
            Ok(contract::MediaProjection::Library { media }) => SignageResponse::Library { media },
            Ok(contract::MediaProjection::UsedBy { programs }) => {
                SignageResponse::UsedBy { programs }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn screen_query(query: contract::ScreenQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Screen>(query, context) {
            Ok(contract::ScreenProjection::Screen { screen }) => SignageResponse::Screen { screen },
            Ok(contract::ScreenProjection::Screens { screens }) => {
                SignageResponse::Screens { screens }
            }
            Ok(contract::ScreenProjection::Showing { screens }) => {
                SignageResponse::Showing { screens }
            }
            Ok(contract::ScreenProjection::Reaches { screens }) => {
                SignageResponse::Reaches { screens }
            }
            Ok(contract::ScreenProjection::Plays {
                screen,
                channels,
                broadcasts,
                audiences,
                programs,
                media,
                presets,
            }) => SignageResponse::Plays {
                screen,
                channels,
                broadcasts,
                audiences,
                programs,
                media,
                presets,
            },
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn channel_query(query: contract::ChannelQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Channel>(query, context) {
            Ok(contract::ChannelProjection::Channel { channel }) => {
                SignageResponse::Channel { channel }
            }
            Ok(contract::ChannelProjection::Channels { channels }) => {
                SignageResponse::Channels { channels }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn audience_query(query: contract::AudienceQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Audience>(query, context) {
            Ok(contract::AudienceProjection::Audience { audience }) => {
                SignageResponse::Audience { audience }
            }
            Ok(contract::AudienceProjection::Audiences { audiences }) => {
                SignageResponse::Audiences { audiences }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn broadcast_query(query: contract::BroadcastQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Broadcast>(query, context) {
            Ok(contract::BroadcastProjection::Broadcast { broadcast }) => {
                SignageResponse::Broadcast { broadcast }
            }
            Ok(contract::BroadcastProjection::Broadcasts { broadcasts }) => {
                SignageResponse::Broadcasts { broadcasts }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn preset_query(query: contract::PresetQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Preset>(query, context) {
            Ok(contract::PresetProjection::Preset { preset }) => SignageResponse::Preset { preset },
            Ok(contract::PresetProjection::Presets { presets }) => {
                SignageResponse::Presets { presets }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    /// Every kind presentation, or none. A failed preset query must not fail
    /// the program the screen is trying to draw.
    fn presets_or_empty(context: &Context<'_>) -> Vec<contract::SignagePreset> {
        match Self::ask::<Preset>(contract::PresetQuery::Presets, context) {
            Ok(contract::PresetProjection::Presets { presets }) => presets,
            _ => Vec::new(),
        }
    }

    fn ask<D: Document>(query: D::Query, context: &Context<'_>) -> Result<D::Projection, String> {
        let payload =
            serde_json::to_vec(&query).map_err(|error| format!("encode Signage query: {error}"))?;
        let projection = context
            .session
            .query(runtime::world::Query {
                schema: D::schema(),
                schema_version: D::VERSION,
                payload,
                publication: None,
            })
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&projection.bytes).map_err(|error| error.to_string())
    }

    fn submit<D: Document>(intent: D::Intent, context: &Context<'_>) -> Result<(), String> {
        let payload = serde_json::to_vec(&intent).map_err(|error| error.to_string())?;
        let action = context
            .identity
            .sign_action(
                context.session,
                RequestId::mint(),
                runtime::world::Intent {
                    schema: D::schema(),
                    schema_version: D::VERSION,
                    payload,
                },
            )
            .map_err(|error| error.to_string())?;
        context
            .session
            .submit(action)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

impl Handler for SignageCallHandler {
    fn access(&self, call: &Call) -> Result<Access, Failure> {
        Ok(decode_call(call)?.access())
    }

    fn call(&self, call: &Call, context: &Context<'_>) -> Reply {
        let request = match decode_call(call) {
            Ok(request) => request,
            Err(error) => return Reply::error(call, error.code, error.message()),
        };
        match serde_json::to_vec(&Self::route(request, context)) {
            Ok(payload) => Reply::ok(call, payload),
            Err(error) => Reply::error(
                call,
                Code::Internal,
                format!("encode Signage response: {error}"),
            ),
        }
    }
}

/// Every verb this build serves, with the class it must be read as.
///
/// One table, shared with the host tests, so the command list a refusal names
/// cannot drift from the commands that exist.
#[cfg(test)]
pub(crate) fn every_verb() -> Vec<(SignageRequest, Access)> {
    let program = tests::program();
    vec![
        (
            SignageRequest::ProgramGet {
                program: program.id.clone(),
            },
            Access::Query,
        ),
        (SignageRequest::ProgramList, Access::Query),
        (
            SignageRequest::ProgramPut {
                program: program.clone(),
            },
            Access::Command,
        ),
        (
            SignageRequest::ProgramDelete {
                program: program.id.clone(),
            },
            Access::Command,
        ),
        (
            SignageRequest::MediaGet {
                media: tests::media().id,
            },
            Access::Query,
        ),
        (SignageRequest::MediaList, Access::Query),
        (
            SignageRequest::MediaPut {
                media: tests::media(),
            },
            Access::Command,
        ),
        (
            SignageRequest::MediaDelete {
                media: tests::media().id,
            },
            Access::Command,
        ),
        (
            SignageRequest::MediaUsedBy {
                media: tests::media().id,
            },
            Access::Query,
        ),
        (
            SignageRequest::ScreenGet {
                screen: tests::screen().id,
            },
            Access::Query,
        ),
        (SignageRequest::ScreenList, Access::Query),
        (
            SignageRequest::ScreenShowing {
                program: program.id,
            },
            Access::Query,
        ),
        (
            SignageRequest::ScreenPlays {
                screen: tests::screen().id,
            },
            Access::Query,
        ),
        (
            SignageRequest::ScreenPut {
                screen: tests::screen(),
            },
            Access::Command,
        ),
        (
            SignageRequest::ScreenDelete {
                screen: tests::screen().id,
            },
            Access::Command,
        ),
        (
            SignageRequest::ChannelGet {
                channel: tests::channel().id,
            },
            Access::Query,
        ),
        (SignageRequest::ChannelList, Access::Query),
        (
            SignageRequest::ChannelPut {
                channel: tests::channel(),
            },
            Access::Command,
        ),
        (
            SignageRequest::ChannelDelete {
                channel: tests::channel().id,
            },
            Access::Command,
        ),
        (
            SignageRequest::AudienceGet {
                audience: tests::audience().id,
            },
            Access::Query,
        ),
        (SignageRequest::AudienceList, Access::Query),
        (
            SignageRequest::AudienceReaches {
                audience: tests::audience().id,
            },
            Access::Query,
        ),
        (
            SignageRequest::AudiencePut {
                audience: tests::audience(),
            },
            Access::Command,
        ),
        (
            SignageRequest::AudienceDelete {
                audience: tests::audience().id,
            },
            Access::Command,
        ),
        (
            SignageRequest::BroadcastGet {
                broadcast: tests::broadcast().id,
            },
            Access::Query,
        ),
        (SignageRequest::BroadcastList, Access::Query),
        (
            SignageRequest::BroadcastPut {
                broadcast: tests::broadcast(),
            },
            Access::Command,
        ),
        (
            SignageRequest::BroadcastDelete {
                broadcast: tests::broadcast().id,
            },
            Access::Command,
        ),
        (
            SignageRequest::AsRunGet {
                screen: tests::screen().id,
            },
            Access::Query,
        ),
        (
            SignageRequest::AsRunRecord {
                asrun: tests::asrun(),
            },
            Access::Command,
        ),
        (
            SignageRequest::PresetGet {
                preset: tests::preset().id,
            },
            Access::Query,
        ),
        (SignageRequest::PresetList, Access::Query),
        (
            SignageRequest::PresetPut {
                preset: tests::preset(),
            },
            Access::Command,
        ),
        (
            SignageRequest::PresetDelete {
                preset: tests::preset().id,
            },
            Access::Command,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> String {
        BodyId::from_bytes([byte; 16]).render()
    }

    pub(super) fn program() -> signage::SignageProgram {
        signage::SignageProgram {
            id: id(1),
            name: "Lobby".into(),
            cycle: signage::ProgramCycle::Loop,
            items: vec![signage::SignageItem {
                id: "welcome".into(),
                media: media().id,
                duration_ms: Some(5_000),
            }],
            windows: Vec::new(),
        }
    }

    pub(super) fn media() -> contract::SignageMedia {
        contract::SignageMedia {
            id: id(2),
            name: "Lobby loop".into(),
            source: contract::MediaSource::Stored {
                // A content id in the only shape the upload route writes.
                content: "ab".repeat(32),
                size: 4_096,
                mime: "video/mp4".into(),
            },
            duration_ms: Some(10_000),
            width: Some(1_920),
            height: Some(1_080),
            catalog: None,
        }
    }

    pub(super) fn screen() -> signage::SignageScreen {
        signage::SignageScreen {
            id: id(3),
            name: "Front door".into(),
            place: None,
            facts: Default::default(),
            sync: None,
            labels: vec!["role:lobby".into()],
            tuned: None,
        }
    }

    pub(super) fn channel() -> signage::SignageChannel {
        signage::SignageChannel {
            id: id(4),
            name: "Lobby loop".into(),
            base: Some(program().id),
            schedule: Vec::new(),
        }
    }

    pub(super) fn audience() -> signage::SignageAudience {
        signage::SignageAudience {
            id: id(6),
            name: "Every screen".into(),
            rule: signage::Match::All,
        }
    }

    pub(super) fn broadcast() -> signage::SignageBroadcast {
        signage::SignageBroadcast {
            id: id(7),
            name: "Evacuate".into(),
            audience: audience().id,
            action: signage::Action::Play {
                program: program().id,
            },
            timing: signage::Timing::When {
                of: signage::Match::All,
                priority: 90,
            },
            supersedes: Vec::new(),
            cancelled_at_unix_ms: None,
        }
    }

    pub(super) fn asrun() -> contract::SignageAsRun {
        contract::SignageAsRun {
            id: id(8),
            screen: screen().id,
            entries: Vec::new(),
            observations: Default::default(),
        }
    }

    pub(super) fn preset() -> contract::SignagePreset {
        contract::SignagePreset {
            id: id(5),
            kind: "weather".into(),
            name: "Weather".into(),
            settings: std::collections::BTreeMap::from([("city".to_owned(), "Chicago".to_owned())]),
        }
    }

    fn wire(request: &SignageRequest) -> serde_json::Map<String, Value> {
        serde_json::to_value(request)
            .unwrap()
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn query_and_command_classification_is_product_owned() {
        for (request, expected) in every_verb() {
            let call = encode_call(&request).unwrap();
            assert_eq!(
                SignageCallHandler.access(&call).unwrap(),
                expected,
                "{request:?}"
            );
        }
    }

    #[test]
    fn every_delete_asks_and_names_what_it_deletes() {
        for (request, _) in every_verb() {
            let wire = wire(&request);
            let command = wire.get("cmd").and_then(Value::as_str).unwrap().to_owned();
            match request.destructive_question() {
                Some(question) => {
                    assert!(
                        command.ends_with("_delete"),
                        "only a delete asks: {command}"
                    );
                    let target = wire
                        .iter()
                        .find(|(field, _)| field.as_str() != "cmd")
                        .and_then(|(_, value)| value.as_str())
                        .unwrap();
                    assert!(question.contains(target), "got: {question}");
                }
                None => assert!(
                    !command.ends_with("_delete"),
                    "a delete that converges without asking: {command}"
                ),
            }
        }
    }

    #[test]
    fn a_media_put_round_trips_through_the_call() {
        let call = encode_call(&SignageRequest::MediaPut { media: media() }).unwrap();
        let SignageRequest::MediaPut { media: decoded } = decode_call(&call).unwrap() else {
            panic!("a media put decodes as the media put it was");
        };
        assert_eq!(decoded, media());
    }

    /// An item names an entry by id, and the entry travels in the same answer.
    ///
    /// The join the browser would otherwise make one request at a time, made
    /// once by the World and carried whole.
    #[test]
    fn a_program_answer_carries_the_entries_its_items_name() {
        let answer = serde_json::to_value(SignageResponse::Program {
            program: Some(program()),
            media: vec![media()],
            presets: vec![preset()],
        })
        .unwrap();
        assert_eq!(answer["kind"], "program");
        assert_eq!(answer["program"]["items"][0]["media"], media().id);
        assert_eq!(answer["media"][0]["id"], media().id);
        assert_eq!(answer["presets"][0]["kind"], "weather");
    }

    #[test]
    fn a_program_answer_without_presets_still_reads() {
        let raw = serde_json::json!({
            "kind": "program",
            "program": program(),
            "media": [media()],
        });
        let SignageResponse::Program { presets, .. } = serde_json::from_value(raw).unwrap() else {
            panic!("a program answer deserializes as a program");
        };
        assert!(presets.is_empty());
    }

    /// One schema pair per document, and no two alike.
    ///
    /// A payload routed under another document's schema is refused as
    /// `UnsupportedSchema`, which names nothing — so the pairing is pinned here
    /// rather than left to be discovered from a silent refusal.
    #[test]
    fn each_document_carries_its_own_schema_pair() {
        assert_eq!(Program::schema(), contract::program_schema());
        assert_eq!(Program::VERSION, contract::PROGRAM_SCHEMA_VERSION);
        assert_eq!(Media::schema(), contract::media_schema());
        assert_eq!(Media::VERSION, contract::MEDIA_SCHEMA_VERSION);
        assert_eq!(Screen::schema(), contract::screen_schema());
        assert_eq!(Screen::VERSION, contract::SCREEN_SCHEMA_VERSION);
        assert_eq!(Channel::schema(), contract::channel_schema());
        assert_eq!(Channel::VERSION, contract::CHANNEL_SCHEMA_VERSION);
        assert_eq!(Audience::schema(), contract::audience_schema());
        assert_eq!(Audience::VERSION, contract::AUDIENCE_SCHEMA_VERSION);
        assert_eq!(Broadcast::schema(), contract::broadcast_schema());
        assert_eq!(Broadcast::VERSION, contract::BROADCAST_SCHEMA_VERSION);
        assert_eq!(Preset::schema(), contract::preset_schema());
        assert_eq!(Preset::VERSION, contract::PRESET_SCHEMA_VERSION);
        assert_eq!(AsRun::schema(), contract::asrun_schema());
        assert_eq!(AsRun::VERSION, contract::ASRUN_SCHEMA_VERSION);
        let distinct: std::collections::BTreeSet<_> = [
            Program::schema(),
            Media::schema(),
            Screen::schema(),
            Channel::schema(),
            Preset::schema(),
        ]
        .into_iter()
        .collect();
        assert_eq!(distinct.len(), 5);
    }
}
