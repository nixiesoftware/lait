import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  authenticatePairingComplete,
  authenticateRequest,
  bytesToHex,
  canonicalProgramRevision,
  confirmationPhrase,
  ProtocolError,
  requestTranscript,
  verifyProgram,
} from "../shared/web/protocol.mjs";

const fixtureUrl = new URL("../../crates/display-protocol/fixtures/v1/conformance.json", import.meta.url);
const fixture = JSON.parse(await readFile(fixtureUrl, "utf8"));

function requestContext() {
  const request = fixture.program_changes_request;
  return {
    protocolMajor: fixture.protocol_major,
    method: request.method,
    route: request.route,
    device: request.device,
    assignment: fixture.program.assignment,
    program: fixture.program.program,
    revision: fixture.program.revision,
    currentItem: fixture.program.items[0].id,
    elapsedMs: request.elapsed_ms,
    waitMs: request.wait_ms,
    asset: null,
    range: null,
    challenge: request.challenge,
    bodySha256: request.body_sha256,
  };
}

test("web adapters reproduce the Rust request transcript byte-for-byte", () => {
  assert.equal(bytesToHex(requestTranscript(requestContext())), fixture.program_changes_request.transcript_hex);
});

test("web adapters reproduce the Rust request HMAC", async () => {
  assert.equal(
    await authenticateRequest(fixture.fixture_only_keys.proof_key_hex, requestContext()),
    fixture.program_changes_request.authentication_tag,
  );
});

test("web adapters independently verify the full program revision", async () => {
  assert.equal(await canonicalProgramRevision(fixture.program), fixture.program.revision);
  assert.equal(await verifyProgram(fixture.program), fixture.program);
});

test("cursor correction does not manufacture a content revision", async () => {
  const corrected = structuredClone(fixture.program);
  corrected.playback.elapsed_ms += 100;
  assert.equal(await canonicalProgramRevision(corrected), fixture.program.revision);
});

test("semantic changes do manufacture a new revision", async () => {
  const changed = structuredClone(fixture.program);
  changed.program_state = { kind: "unavailable" };
  assert.notEqual(await canonicalProgramRevision(changed), fixture.program.revision);
});

test("web adapters reproduce pairing completion proof", async () => {
  const pairing = fixture.pairing_complete;
  assert.equal(
    await authenticatePairingComplete(
      pairing.proof_key_hex,
      pairing.pairing,
      pairing.device,
      pairing.challenge,
    ),
    pairing.authentication_tag,
  );
});

test("web adapters reproduce the human confirmation phrase", async () => {
  const phrase = fixture.confirmation_phrase;
  assert.deepEqual(
    await confirmationPhrase(phrase.fingerprint, phrase.pairing, phrase.receiver_nonce),
    phrase.words,
  );
});

test("unknown receiver-facing fields fail closed", async () => {
  const injected = { ...structuredClone(fixture.program), world: "forbidden" };
  await assert.rejects(() => verifyProgram(injected), (error) => {
    assert.ok(error instanceof ProtocolError);
    assert.equal(error.code, "unknown_field");
    return true;
  });
});

test("forged revisions fail before becoming eligible", async () => {
  const forged = structuredClone(fixture.program);
  forged.revision = "f".repeat(64);
  await assert.rejects(() => verifyProgram(forged), (error) => {
    assert.ok(error instanceof ProtocolError);
    assert.equal(error.code, "integrity");
    return true;
  });
});
