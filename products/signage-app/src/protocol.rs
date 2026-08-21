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
        screen: contract::SignageScreen,
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
    GroupGet {
        group: String,
    },
    GroupList,
    GroupPut {
        group: contract::SignageGroup,
    },
    GroupDelete {
        group: String,
    },
    ConfigGet {
        config: String,
    },
    /// A kind is configured exactly when a config exists for it, so listing is
    /// how a caller learns which are — there is no flag to read.
    ConfigList,
    ConfigPut {
        config: contract::SignageConfig,
    },
    ConfigDelete {
        config: String,
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
            | Self::GroupGet { .. }
            | Self::GroupList
            | Self::ConfigGet { .. }
            | Self::ConfigList => Access::Query,
            Self::ProgramPut { .. }
            | Self::ProgramDelete { .. }
            | Self::MediaPut { .. }
            | Self::MediaDelete { .. }
            | Self::ScreenPut { .. }
            | Self::ScreenDelete { .. }
            | Self::GroupPut { .. }
            | Self::GroupDelete { .. }
            | Self::ConfigPut { .. }
            | Self::ConfigDelete { .. } => Access::Command,
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
            Self::GroupDelete { group } => ("group", group),
            Self::ConfigDelete { config } => ("config", config),
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
            | Self::GroupGet { .. }
            | Self::GroupList
            | Self::GroupPut { .. }
            | Self::ConfigGet { .. }
            | Self::ConfigList
            | Self::ConfigPut { .. } => return None,
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
    Program {
        program: Option<signage::SignageProgram>,
        media: Vec<contract::SignageMedia>,
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
        screen: Option<contract::SignageScreen>,
    },
    Screens {
        screens: Vec<contract::SignageScreen>,
    },
    Showing {
        screens: Vec<String>,
    },
    /// The inputs to the ladder, never its answer — the caller brings the clock
    /// and calls [`contract::ScreenProjection::playback`].
    Plays {
        screen: Option<contract::SignageScreen>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<contract::SignageGroup>,
    },
    ScreenSaved {
        screen: String,
    },
    ScreenDeleted {
        screen: String,
    },
    Group {
        group: Option<contract::SignageGroup>,
    },
    Groups {
        groups: Vec<contract::SignageGroup>,
    },
    GroupSaved {
        group: String,
    },
    GroupDeleted {
        group: String,
    },
    Config {
        config: Option<contract::SignageConfig>,
    },
    Configs {
        configs: Vec<contract::SignageConfig>,
    },
    ConfigSaved {
        config: String,
    },
    ConfigDeleted {
        config: String,
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
struct Group;
struct Config;

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

impl Document for Group {
    type Query = contract::GroupQuery;
    type Intent = contract::GroupIntent;
    type Projection = contract::GroupProjection;
    const VERSION: u32 = contract::GROUP_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::group_schema()
    }
}

impl Document for Config {
    type Query = contract::ConfigQuery;
    type Intent = contract::ConfigIntent;
    type Projection = contract::ConfigProjection;
    const VERSION: u32 = contract::CONFIG_SCHEMA_VERSION;
    fn schema() -> SchemaId {
        contract::config_schema()
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
            SignageRequest::GroupGet { group } => {
                Self::group_query(contract::GroupQuery::Group { group }, context)
            }
            SignageRequest::GroupList => Self::group_query(contract::GroupQuery::Groups, context),
            SignageRequest::GroupPut { group } => {
                if !group.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage group".into(),
                    };
                }
                let id = group.id.clone();
                match Self::submit::<Group>(contract::GroupIntent::Put { group }, context) {
                    Ok(()) => SignageResponse::GroupSaved { group: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::GroupDelete { group } => {
                if BodyId::parse(&group).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage group id".into(),
                    };
                }
                match Self::submit::<Group>(
                    contract::GroupIntent::Delete {
                        group: group.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::GroupDeleted { group },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::ConfigGet { config } => {
                Self::config_query(contract::ConfigQuery::Config { config }, context)
            }
            SignageRequest::ConfigList => {
                Self::config_query(contract::ConfigQuery::Configs, context)
            }
            SignageRequest::ConfigPut { config } => {
                if !config.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage config".into(),
                    };
                }
                let id = config.id.clone();
                match Self::submit::<Config>(contract::ConfigIntent::Put { config }, context) {
                    Ok(()) => SignageResponse::ConfigSaved { config: id },
                    Err(message) => SignageResponse::Error { message },
                }
            }
            SignageRequest::ConfigDelete { config } => {
                if BodyId::parse(&config).is_none() {
                    return SignageResponse::Error {
                        message: "invalid signage config id".into(),
                    };
                }
                match Self::submit::<Config>(
                    contract::ConfigIntent::Delete {
                        config: config.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::ConfigDeleted { config },
                    Err(message) => SignageResponse::Error { message },
                }
            }
        }
    }

    fn program_query(query: signage::SignageQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Program>(query, context) {
            Ok(signage::SignageProjection::Program { program, media }) => {
                SignageResponse::Program { program, media }
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
            Ok(contract::ScreenProjection::Plays { screen, group }) => {
                SignageResponse::Plays { screen, group }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn group_query(query: contract::GroupQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Group>(query, context) {
            Ok(contract::GroupProjection::Group { group }) => SignageResponse::Group { group },
            Ok(contract::GroupProjection::Groups { groups }) => SignageResponse::Groups { groups },
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn config_query(query: contract::ConfigQuery, context: &Context<'_>) -> SignageResponse {
        match Self::ask::<Config>(query, context) {
            Ok(contract::ConfigProjection::Config { config }) => SignageResponse::Config { config },
            Ok(contract::ConfigProjection::Configs { configs }) => {
                SignageResponse::Configs { configs }
            }
            Err(message) => SignageResponse::Error { message },
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
            SignageRequest::GroupGet {
                group: tests::group().id,
            },
            Access::Query,
        ),
        (SignageRequest::GroupList, Access::Query),
        (
            SignageRequest::GroupPut {
                group: tests::group(),
            },
            Access::Command,
        ),
        (
            SignageRequest::GroupDelete {
                group: tests::group().id,
            },
            Access::Command,
        ),
        (
            SignageRequest::ConfigGet {
                config: tests::config().id,
            },
            Access::Query,
        ),
        (SignageRequest::ConfigList, Access::Query),
        (
            SignageRequest::ConfigPut {
                config: tests::config(),
            },
            Access::Command,
        ),
        (
            SignageRequest::ConfigDelete {
                config: tests::config().id,
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

    pub(super) fn screen() -> contract::SignageScreen {
        contract::SignageScreen {
            id: id(3),
            name: "Front door".into(),
            group: None,
            intent: Default::default(),
            schedule: Vec::new(),
        }
    }

    pub(super) fn group() -> contract::SignageGroup {
        contract::SignageGroup {
            id: id(4),
            name: "Lobby screens".into(),
            intent: Default::default(),
            screens: vec![screen().id],
        }
    }

    pub(super) fn config() -> contract::SignageConfig {
        contract::SignageConfig {
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
        })
        .unwrap();
        assert_eq!(answer["kind"], "program");
        assert_eq!(answer["program"]["items"][0]["media"], media().id);
        assert_eq!(answer["media"][0]["id"], media().id);
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
        assert_eq!(Group::schema(), contract::group_schema());
        assert_eq!(Group::VERSION, contract::GROUP_SCHEMA_VERSION);
        assert_eq!(Config::schema(), contract::config_schema());
        assert_eq!(Config::VERSION, contract::CONFIG_SCHEMA_VERSION);
        let distinct: std::collections::BTreeSet<_> = [
            Program::schema(),
            Media::schema(),
            Screen::schema(),
            Group::schema(),
            Config::schema(),
        ]
        .into_iter()
        .collect();
        assert_eq!(distinct.len(), 5);
    }
}
