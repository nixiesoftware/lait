//! The browser-facing seam for Signage.
//!
//! Signage shipped able to be *rendered* — a display surface a receiver
//! consumes — and unable to be *driven*: the package declared no web parser,
//! so `POST /api/spaces/{id}/worlds/signage/rpc` answered every request with
//! "World 'signage' does not expose a web client interface". A World that only
//! a screen can reach has no authoring surface at all.
//!
//! There is very little here on purpose. [`SignageRequest`] is already the
//! product's own strict schema, tagged `cmd` and rendered in snake case, so
//! the browser's wire shape *is* that enum and this module is a decoder rather
//! than a second protocol. Adding a request variant reaches the browser with
//! no edit here, which is the property worth having: two hand-written
//! translations of one schema is how the head and the daemon come to disagree
//! about what a request means.

use runtime::world::call::Access;
use serde_json::Value;
use world_interface::{ClientAccess, ClientInvocation, Failure};

use crate::protocol::SignageRequest;

/// The `cmd` values this build serves, named in a refusal so a caller learns
/// what was available rather than only that it was wrong.
const COMMANDS: &str = "program_get, program_list, program_put, program_delete";

/// Construct one Signage World invocation with package-owned client policy.
///
/// The access class is read off the same [`SignageRequest::access`] that
/// `SignageCallHandler::access` runs on the daemon's side, so the head's copy
/// and the daemon's cannot describe the same bytes differently. This one is
/// *head* policy — what a client may draw and whether it must confirm; the
/// daemon derives its own classification after the call arrives and that is
/// what authorization consults.
pub fn world_invocation(request: SignageRequest) -> Result<ClientInvocation, Failure> {
    let confirmation = request.destructive_question();
    let access = match request.access() {
        Access::Query => ClientAccess::Query,
        Access::Command => ClientAccess::Command,
    };
    let call = crate::encode_call(&request).map_err(|error| Failure::new(error.to_string()))?;
    Ok(ClientInvocation::world(call, access, confirmation))
}

/// Decode the Signage browser protocol behind its explicit World route.
///
/// The two failures are kept apart because they are different mistakes: a body
/// with no `cmd` is not a Signage request at all, while a named command that
/// will not decode is a Signage request whose payload is wrong. Answering both
/// with one message would make a typo and a schema change look alike.
pub fn parse_web(input: Value) -> Result<ClientInvocation, Failure> {
    let command = input
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("Signage request is missing string field 'cmd'"))?
        .to_owned();
    match serde_json::from_value::<SignageRequest>(input) {
        Ok(request) => world_invocation(request),
        Err(error) => Err(Failure::new(format!(
            "Signage request '{command}' could not be read ({COMMANDS}): {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::body::BodyId;
    use serde_json::json;

    fn program_id() -> String {
        BodyId::from_bytes([7; 16]).render()
    }

    #[test]
    fn a_query_parses_and_stays_a_query() {
        let invocation = parse_web(json!({ "cmd": "program_list" })).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Query);
        assert_eq!(invocation.world_id(), &signage::contract::world_id());
        assert!(invocation.confirmation_question().is_none());
    }

    #[test]
    fn a_delete_is_a_command_and_asks_first() {
        let id = program_id();
        let invocation = parse_web(json!({ "cmd": "program_delete", "program": id })).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Command);
        let question = invocation
            .confirmation_question()
            .expect("deleting a replicated Body asks before it converges");
        assert!(question.contains(&id), "the question names what it deletes");
    }

    #[test]
    fn a_put_is_a_command_that_does_not_ask() {
        let program = signage::SignageProgram {
            id: program_id(),
            name: "Lobby".into(),
            cycle: signage::ProgramCycle::Loop,
            items: vec![signage::SignageItem {
                id: "a".into(),
                title: "Welcome".into(),
                body: String::new(),
                background: "101010".into(),
                foreground: "fafafa".into(),
                live_resource: None,
                duration_ms: Some(5_000),
            }],
            windows: Vec::new(),
        };
        let input = serde_json::to_value(SignageRequest::ProgramPut { program }).unwrap();
        let invocation = parse_web(input).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Command);
        assert!(invocation.confirmation_question().is_none());
    }

    /// The repairable detail, which is what a caller fixing its request needs.
    ///
    /// `Display` renders only the stable classification on purpose, so a test
    /// that reads it back cannot tell any two malformed requests apart.
    fn diagnostic(error: &Failure) -> String {
        error
            .diagnostic()
            .expect("an adapter refusal carries the detail that repairs it")
            .to_owned()
    }

    #[test]
    fn a_body_without_a_command_is_not_a_signage_request() {
        let error = parse_web(json!({ "program": program_id() })).unwrap_err();
        let message = diagnostic(&error);
        assert!(
            message.contains("missing string field 'cmd'"),
            "got: {message}"
        );
    }

    #[test]
    fn an_unknown_command_is_refused_by_name_and_lists_what_exists() {
        let error = parse_web(json!({ "cmd": "program_publish" })).unwrap_err();
        let message = diagnostic(&error);
        assert!(message.contains("program_publish"), "got: {message}");
        assert!(message.contains("program_list"), "got: {message}");
    }

    #[test]
    fn a_known_command_with_a_wrong_payload_is_refused_as_a_payload_problem() {
        let error = parse_web(json!({ "cmd": "program_get" })).unwrap_err();
        let message = diagnostic(&error);
        assert!(message.contains("program_get"), "got: {message}");
        assert!(
            !message.contains("missing string field 'cmd'"),
            "a payload fault must not read as a missing command: {message}"
        );
    }

    #[test]
    fn the_package_now_exposes_a_web_client_interface() {
        let package = crate::package().unwrap();
        let invocation = package.parse_web(json!({ "cmd": "program_list" })).unwrap();
        assert_eq!(invocation.world_id(), &signage::contract::world_id());
    }
}
