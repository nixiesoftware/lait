use replica::body::BodyId;
use runtime::world::call::{Access, Call, Code, Context, Failure, Handler, Reply};
use runtime::world::{Intent, Query, RequestId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const OPERATION: &str = "signage.control";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum SignageRequest {
    ProgramGet { program: String },
    ProgramList,
    ProgramPut { program: signage::SignageProgram },
    ProgramDelete { program: String },
}

impl SignageRequest {
    pub fn access(&self) -> Access {
        match self {
            Self::ProgramGet { .. } | Self::ProgramList => Access::Query,
            Self::ProgramPut { .. } | Self::ProgramDelete { .. } => Access::Command,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignageResponse {
    Program {
        program: Option<signage::SignageProgram>,
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

#[derive(Debug, Default)]
pub struct SignageCallHandler;

impl SignageCallHandler {
    fn route(request: SignageRequest, context: &Context<'_>) -> SignageResponse {
        match request {
            SignageRequest::ProgramGet { program } => {
                Self::query(signage::SignageQuery::Program { program }, context)
            }
            SignageRequest::ProgramList => Self::query(signage::SignageQuery::Programs, context),
            SignageRequest::ProgramPut { program } => {
                if !program.validate() {
                    return SignageResponse::Error {
                        message: "invalid signage program".into(),
                    };
                }
                let id = program.id.clone();
                match Self::submit(signage::SignageIntent::Put { program }, context) {
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
                match Self::submit(
                    signage::SignageIntent::Delete {
                        program: program.clone(),
                    },
                    context,
                ) {
                    Ok(()) => SignageResponse::Deleted { program },
                    Err(message) => SignageResponse::Error { message },
                }
            }
        }
    }

    fn query(query: signage::SignageQuery, context: &Context<'_>) -> SignageResponse {
        let payload = match serde_json::to_vec(&query) {
            Ok(payload) => payload,
            Err(error) => {
                return SignageResponse::Error {
                    message: format!("encode Signage query: {error}"),
                }
            }
        };
        let answer = context.session.query(Query {
            schema: signage::contract::program_schema(),
            schema_version: signage::contract::PROGRAM_SCHEMA_VERSION,
            payload,
            publication: None,
        });
        let projection = answer
            .map_err(|error| error.to_string())
            .and_then(|projection| {
                serde_json::from_slice::<signage::SignageProjection>(&projection.bytes)
                    .map_err(|error| error.to_string())
            });
        match projection {
            Ok(signage::SignageProjection::Program { program }) => {
                SignageResponse::Program { program }
            }
            Ok(signage::SignageProjection::Programs { programs }) => {
                SignageResponse::Programs { programs }
            }
            Err(message) => SignageResponse::Error { message },
        }
    }

    fn submit(intent: signage::SignageIntent, context: &Context<'_>) -> Result<(), String> {
        let payload = serde_json::to_vec(&intent).map_err(|error| error.to_string())?;
        let action = context
            .identity
            .sign_action(
                context.session,
                RequestId::mint(),
                Intent {
                    schema: signage::contract::program_schema(),
                    schema_version: signage::contract::PROGRAM_SCHEMA_VERSION,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_and_command_classification_is_product_owned() {
        let read = encode_call(&SignageRequest::ProgramList).unwrap();
        assert_eq!(SignageCallHandler.access(&read).unwrap(), Access::Query);
        let write = encode_call(&SignageRequest::ProgramDelete {
            program: BodyId::from_bytes([4; 16]).render(),
        })
        .unwrap();
        assert_eq!(SignageCallHandler.access(&write).unwrap(), Access::Command);
    }
}
