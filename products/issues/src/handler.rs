//! Local `issue.verify/v1` handler.
//!
//! The first backend is trusted Rust running inside the supervised Issues
//! runner. It does not compile a repository. It records that the pinned source
//! was present at Start and emits a self-describing report Runtime ingests as
//! ordinary content.

use std::sync::Arc;

use replica::content::ContentRef;
use runtime::exec::{Candidate, Failure, Handler, HandlerBinding};

use crate::contract::{self, VerifyInput, VerifyOutput};

pub struct VerifyHandler {
    binding: HandlerBinding,
}

impl VerifyHandler {
    pub fn new(build: &runtime::exec::Build) -> Self {
        Self {
            binding: HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: build.handler,
                role: None,
                links: Vec::new(),
            },
        }
    }
}

impl Handler for VerifyHandler {
    fn binding(&self) -> &HandlerBinding {
        &self.binding
    }

    fn handle(
        &self,
        context: &mut dyn runtime::exec::HandlerContext,
    ) -> Result<Candidate, Failure> {
        let input =
            VerifyInput::from_json(context.input_inline()).ok_or(Failure::InvalidOutcome)?;
        let source = context
            .input_content()
            .first()
            .copied()
            .ok_or(Failure::InvalidOutcome)?;
        let expected = parse_content_hex(&input.source).ok_or(Failure::InvalidOutcome)?;
        if expected != source {
            return Err(Failure::InvalidOutcome);
        }
        let report = serde_json::to_vec(&VerifyReport {
            spec: contract::VERIFY_SPEC,
            version: contract::VERIFY_SPEC_VERSION,
            doc: input.doc,
            source: input.source,
            build: hex(&context.build().as_bytes()),
            run: hex(&context.run().as_bytes()),
            attempt: hex(&context.attempt().as_bytes()),
            note: "runner-local verifier bound the pinned source",
        })
        .map_err(|_| Failure::InvalidOutcome)?;
        context.stage_output(report)?;
        let inline = serde_json::to_vec(&VerifyOutput {
            verdict: "pass".to_owned(),
        })
        .map_err(|_| Failure::InvalidOutcome)?;
        Ok(Candidate {
            output: contract::verify_output_ref(),
            inline,
            content: Vec::new(),
            content_bytes: 0,
            terminal: runtime::exec::TerminalClass::Succeeded,
            usage: Vec::new(),
            evidence: Vec::new(),
        })
    }
}

#[derive(serde::Serialize)]
struct VerifyReport {
    spec: &'static str,
    version: u32,
    doc: String,
    source: String,
    build: String,
    run: String,
    attempt: String,
    note: &'static str,
}

fn parse_content_hex(value: &str) -> Option<ContentRef> {
    let bytes = data_encoding::HEXLOWER.decode(value.as_bytes()).ok()?;
    let content_id: [u8; 32] = bytes.try_into().ok()?;
    Some(ContentRef { content_id })
}

fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

pub fn verify_handler(build: &runtime::exec::Build) -> Arc<dyn Handler> {
    Arc::new(VerifyHandler::new(build))
}
