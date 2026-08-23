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
  validateProgram,
  verifyProgram,
} from "../shared/web/protocol.mjs";
import { DisplayReceiverClient, parseLiveMediaPacket } from "../shared/web/client.mjs";
import {
  coordinatorParent,
  normalizeSiteCode,
  siteOrigin,
  validSiteCode,
  webPkiBootstrap,
} from "../shared/web/provisioning.mjs";

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

test("live-ticket request context is closed and authenticated", async () => {
  const context = {
    ...requestContext(),
    method: "POST",
    route: "live_ticket",
    waitMs: null,
    asset: "a".repeat(64),
    bodySha256: await sha256ForTest('{"transport":"mse"}'),
  };
  assert.equal((await authenticateRequest(fixture.fixture_only_keys.proof_key_hex, context)).length, 64);
  assert.throws(
    () => requestTranscript({ ...context, asset: null }),
    (error) => error instanceof ProtocolError && error.code === "invalid_shape",
  );
});

test("MSE manifests are accepted only with the MSE protocol", () => {
  const program = structuredClone(fixture.program);
  program.items[0].scene = {
    kind: "media",
    manifest: {
      id: "a".repeat(64),
      media_type: "mse_manifest",
      encoded_len: 32,
      sha256: "b".repeat(64),
      width: null,
      height: null,
    },
    protocol: "mse",
    live: true,
  };
  assert.equal(validateProgram(program), program);
  assert.throws(
    () => validateProgram({ ...program, items: [{ ...program.items[0], scene: { ...program.items[0].scene, protocol: "hls" } }] }),
    (error) => error instanceof ProtocolError && error.code === "invalid_shape",
  );
});

test("live WebSocket packets decode their bounded binary envelope", () => {
  const rendition = new TextEncoder().encode("main_h264");
  const payload = Uint8Array.of(1, 2, 3, 4);
  const wire = new ArrayBuffer(36 + rendition.length + payload.length);
  const view = new DataView(wire);
  view.setUint8(0, 2);
  view.setUint16(1, rendition.length, false);
  new Uint8Array(wire, 3, rendition.length).set(rendition);
  const metadata = 3 + rendition.length;
  view.setBigUint64(metadata, 7n, false);
  view.setBigInt64(metadata + 8, -25n, false);
  view.setBigUint64(metadata + 16, 90_000n, false);
  view.setBigUint64(metadata + 24, 180_000n, false);
  view.setUint8(metadata + 32, 1);
  new Uint8Array(wire, metadata + 33).set(payload);

  assert.deepEqual(parseLiveMediaPacket(wire), {
    kind: "fragment",
    rendition: "main_h264",
    sequence: 7n,
    publishedAtMicros: -25n,
    startTimestamp: 90_000n,
    duration: 180_000n,
    discontinuity: true,
    payload,
  });
  assert.throws(
    () => parseLiveMediaPacket(new ArrayBuffer(35)),
    (error) => error instanceof ProtocolError && error.code === "live_packet",
  );
});

async function sha256ForTest(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return Buffer.from(digest).toString("hex");
}

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
  assert.throws(
    () => new DisplayReceiverClient({
      bootstrap: {
        protocol_major: 1,
        trust: { kind: "web_pki_origin", origin: "https://nixiesoftware.com/escaped" },
        certificate_pem: null,
        rendezvous: null,
      },
      capabilities: {},
      ui: {},
    }),
    (error) => error instanceof ProtocolError && error.code === "invalid_origin",
  );
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

function receiverHarness(program = structuredClone(fixture.program)) {
  const events = [];
  const ui = {
    setStaleState: (value) => events.push(["stale", value]),
    setSourceState: (value) => events.push(["source", value]),
    showBlank: (reason) => events.push(["blank", reason]),
    showFrame: (url) => events.push(["frame", url]),
  };
  const receiver = new DisplayReceiverClient({
    bootstrap: {
      protocol_major: 1,
      trust: { kind: "web_pki_origin", origin: "https://nixiesoftware.com" },
      certificate_pem: null,
      rendezvous: null,
    },
    capabilities: {},
    ui,
  });
  receiver.program = program;
  const asset = program.items[program.playback.current_index].scene.asset;
  receiver.staged.set(asset.id, { url: "verified-frame", asset });
  return { receiver, events };
}

test("a no-change response cannot refresh the wrong revision", async () => {
  const { receiver } = receiverHarness();
  await assert.rejects(
    () => receiver.handleProgramResponse({
      kind: "no_change",
      revision: "f".repeat(64),
      playback: structuredClone(receiver.program.playback),
    }),
    (error) => error instanceof ProtocolError && error.code === "invalid_revision",
  );
  assert.equal(receiver.program.revision, fixture.program.revision);
  assert.equal(receiver.lastProgramDeliveryAt, 0);
});

test("a no-change cursor must remain inside the current item", () => {
  const { receiver } = receiverHarness();
  const current = receiver.program.items[receiver.program.playback.current_index];
  assert.throws(
    () => receiver.adoptCursor({
      ...receiver.program.playback,
      elapsed_ms: current.duration_ms,
    }, true),
    (error) => error instanceof ProtocolError && error.code === "invalid_cursor",
  );
  assert.equal(receiver.lastProgramDeliveryAt, 0);
});

test("API errors reject unknown fields before changing receiver state", () => {
  const { receiver } = receiverHarness();
  receiver.credential = { mode: "paired", device: "0".repeat(32), proofKey: "1".repeat(64) };
  assert.throws(
    () => receiver.handleApiError({
      protocol_major: 1,
      code: "revoked",
      retry_after_ms: null,
      next_challenge: null,
      world: "forbidden",
    }, 403),
    (error) => error instanceof ProtocolError && error.code === "unknown_field",
  );
  assert.equal(receiver.credential.mode, "paired");
});

test("fresh delivery atomically replaces a stale-sensitive blank", async () => {
  const program = structuredClone(fixture.program);
  program.freshness.on_stale = "blank";
  const { receiver, events } = receiverHarness(program);
  receiver.lastProgramDeliveryAt = performance.now() - program.freshness.stale_after_ms - 1;
  receiver.renderCurrent();
  assert.deepEqual(events.at(-2), ["blank", "host_unavailable"]);

  events.length = 0;
  await receiver.handleProgramResponse({
    kind: "no_change",
    revision: program.revision,
    playback: structuredClone(program.playback),
  });
  assert.ok(events.some(([kind]) => kind === "frame"));
  assert.deepEqual(events.find(([kind]) => kind === "stale"), ["stale", false]);
  clearTimeout(receiver.playbackTimer);
});

// ─── Site provisioning ──────────────────────────────────────────────────────
//
// The doorbell, not the credential. These are the rules that decide which
// coordinator a web receiver will ever speak to, and they run in Node because
// nothing about them needs a television.

test("a site code is one DNS label, and case and padding are the operator's", () => {
  for (const good of ["acme", "acme-lobby", "a", "site-2", "a".repeat(32)]) {
    assert.equal(validSiteCode(good), true, `${good} is a site code`);
  }
  for (const bad of ["", "-acme", "acme-", "acme.lobby", "Acme", "acme lobby", "a".repeat(33), "acme/x"]) {
    assert.equal(validSiteCode(bad), false, `${bad} is not a site code`);
  }
  assert.equal(normalizeSiteCode("  ACME-Lobby \n"), "acme-lobby");
  assert.equal(normalizeSiteCode(null), "");
});

test("a coordinator is a subdomain of whatever served the receiver", () => {
  assert.equal(siteOrigin("acme", "signage.example.pub"), "https://acme.signage.example.pub");
  assert.equal(coordinatorParent("Signage.Example.Pub"), "signage.example.pub");
});

test("a host that cannot name a site refuses rather than inventing one", () => {
  // An IP literal, a bare label or a port has no subdomain to hand a site, and
  // guessing one would produce a coordinator nobody deployed.
  for (const host of ["localhost", "127.0.0.1", "192.168.1.10", "signage.example.pub:8443", "example.pub."]) {
    assert.throws(() => coordinatorParent(host), ProtocolError, `${host} is unprovisionable`);
  }
});

test("an invalid site code never becomes an origin", () => {
  assert.throws(() => siteOrigin("acme.lobby", "signage.example.pub"), ProtocolError);
  assert.throws(() => siteOrigin("", "signage.example.pub"), ProtocolError);
});

test("the provisioned bootstrap is Web-PKI and carries no pinned material", () => {
  const bootstrap = webPkiBootstrap("https://acme.signage.example.pub");
  assert.deepEqual(bootstrap, {
    protocol_major: 1,
    trust: { kind: "web_pki_origin", origin: "https://acme.signage.example.pub" },
    certificate_pem: null,
    rendezvous: null,
  });
  // The client is the authority on bootstrap shape; this is what it accepts.
  assert.doesNotThrow(() => new DisplayReceiverClient({
    bootstrap,
    capabilities: fixture.capabilities ?? {},
    ui: {},
    vaultFactory: async () => ({}),
  }));
});
