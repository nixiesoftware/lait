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
//! no edit here beyond [`COMMANDS`], which is the property worth having: two
//! hand-written translations of one schema is how the head and the daemon come
//! to disagree about what a request means.

use runtime::world::call::Access;
use serde_json::Value;
use world_interface::{ClientAccess, ClientInvocation, Failure};

use crate::protocol::SignageRequest;

/// The `cmd` values this build serves, named in a refusal so a caller learns
/// what was available rather than only that it was wrong.
///
/// Pinned against the request enum by test, because a list that lags the enum
/// misleads exactly the caller who is already lost.
const COMMANDS: &str = "program_get, program_list, program_put, program_delete, \
     media_get, media_list, media_put, media_delete, media_used_by, \
     screen_get, screen_list, screen_put, screen_delete, screen_showing, screen_plays, \
     group_get, group_list, group_put, group_delete, \
     config_get, config_list, config_put, config_delete";

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
    use crate::protocol::every_verb;
    use replica::body::BodyId;
    use serde_json::json;

    fn program_id() -> String {
        BodyId::from_bytes([7; 16]).render()
    }

    /// The wire form of a request, which is what a browser actually posts.
    fn wire(request: &SignageRequest) -> Value {
        serde_json::to_value(request).unwrap()
    }

    fn command_of(request: &SignageRequest) -> String {
        wire(request)
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned()
    }

    #[test]
    fn a_query_parses_and_stays_a_query() {
        let invocation = parse_web(json!({ "cmd": "program_list" })).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Query);
        assert_eq!(invocation.world_id(), &signage::contract::world_id());
        assert!(invocation.confirmation_question().is_none());
    }

    /// Every verb survives the round trip through the browser's wire form with
    /// the class and the question the protocol assigned it.
    #[test]
    fn every_verb_parses_back_with_the_class_the_protocol_assigns() {
        for (request, access) in every_verb() {
            let command = command_of(&request);
            let expected = match access {
                Access::Query => ClientAccess::Query,
                Access::Command => ClientAccess::Command,
            };
            let asks = request.destructive_question().is_some();
            let invocation = parse_web(wire(&request)).unwrap_or_else(|error| {
                panic!("{command} did not parse: {error}");
            });
            assert_eq!(invocation.access(), expected, "{command}");
            assert_eq!(
                invocation.confirmation_question().is_some(),
                asks,
                "{command}"
            );
        }
    }

    #[test]
    fn every_delete_is_a_command_and_asks_by_name() {
        let mut deletes = 0;
        for (request, _) in every_verb() {
            let Some(question) = request.destructive_question() else {
                continue;
            };
            deletes += 1;
            let wire = wire(&request);
            let target = wire
                .as_object()
                .unwrap()
                .iter()
                .find(|(field, _)| field.as_str() != "cmd")
                .and_then(|(_, value)| value.as_str())
                .unwrap()
                .to_owned();
            let invocation = parse_web(wire).unwrap();
            assert_eq!(invocation.access(), ClientAccess::Command);
            assert_eq!(
                invocation.confirmation_question(),
                Some(question.as_str()),
                "the head asks the protocol's question, not its own"
            );
            assert!(question.contains(&target), "got: {question}");
        }
        assert_eq!(deletes, 5, "one delete per document type");
    }

    #[test]
    fn a_media_put_is_a_command_that_does_not_ask() {
        let (request, _) = every_verb()
            .into_iter()
            .find(|(request, _)| matches!(request, SignageRequest::MediaPut { .. }))
            .expect("the verb table serves a media put");
        let invocation = parse_web(wire(&request)).unwrap();
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
        assert!(message.contains("media_put"), "got: {message}");
        assert!(message.contains("screen_showing"), "got: {message}");
    }

    /// The listed commands are the served commands, both ways.
    ///
    /// A name here that no verb answers sends a caller to write a request that
    /// cannot work, and a verb missing from it is unreachable by anyone reading
    /// the refusal.
    #[test]
    fn the_refusal_lists_exactly_the_commands_this_build_serves() {
        let listed: std::collections::BTreeSet<&str> = COMMANDS.split(',').map(str::trim).collect();
        let served: std::collections::BTreeSet<String> = every_verb()
            .iter()
            .map(|(request, _)| command_of(request))
            .collect();
        let served: std::collections::BTreeSet<&str> = served.iter().map(String::as_str).collect();
        assert_eq!(listed, served);
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
