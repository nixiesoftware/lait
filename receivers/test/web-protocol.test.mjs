import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  authenticatePairingComplete,
  authenticateRequest,
  bytesToHex,
  canonicalProgramRevision,
  confirmationPhrase,
  groupRendezvousCode,
  normalizeRendezvousCode,
  ProtocolError,
  rendezvousFromCode,
  requestTranscript,
  validateProgram,
  verifyProgram,
} from "../shared/web/protocol.mjs";
import { DisplayReceiverClient, parseLiveMediaPacket } from "../shared/web/client.mjs";
import {
  deploymentRoot,
  normalizeSiteCode,
  parseEntry,
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
    await confirmationPhrase(phrase.profile, phrase.pairing, phrase.receiver_nonce),
    phrase.words,
  );
});

test("web adapters read a rendezvous code the way Rust does, and name the same rendezvous", async () => {
  const code = fixture.rendezvous_code;
  assert.equal(normalizeRendezvousCode(code.entered), code.normalized);
  assert.equal(groupRendezvousCode(code.entered), code.grouped);
  assert.equal(await rendezvousFromCode(code.entered), code.rendezvous);
  for (const bad of ["7K3Q011", "7K3Q01111", "7K3Q-0U11", "", null]) {
    assert.throws(
      () => normalizeRendezvousCode(bad),
      (error) => error instanceof ProtocolError && error.code === "invalid_identifier",
      `${JSON.stringify(bad)} is not a code`,
    );
  }
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

test("an unaligned program keeps its own clock across a no-change answer", async () => {
  // Three ten-second frames, looping, no sync group; the receiver is five
  // seconds into the last one when a poll that opened at the first answers.
  const program = structuredClone(fixture.program);
  program.playback = { current_index: 2, elapsed_ms: 0, cycle: "loop", sync: null };
  program.items = [0, 1, 2].map((index) => ({
    ...structuredClone(fixture.program.items[0]),
    id: String(index).repeat(64),
    duration_ms: 10_000,
  }));
  const { receiver, events } = receiverHarness(program);
  receiver.elapsedBase = 5_000;
  receiver.itemStartedAt = performance.now();
  await receiver.handleProgramResponse({
    kind: "no_change",
    revision: program.revision,
    playback: { current_index: 0, elapsed_ms: 0, cycle: "loop", sync: null },
  });
  assert.equal(receiver.program.playback.current_index, 2, "still on the item it was showing");
  assert.ok(receiver.currentPlayback().elapsedMs >= 5_000, "and not rewound within it");
  assert.ok(receiver.lastProgramDeliveryAt > 0, "the delivery still counts as fresh");
  assert.equal(events.length, 0, "nothing is redrawn for an answer that changes nothing");
  // A malformed cursor is still refused, even one that would not be adopted.
  await assert.rejects(
    () => receiver.handleProgramResponse({
      kind: "no_change",
      revision: program.revision,
      playback: { current_index: 7, elapsed_ms: 0, cycle: "loop", sync: null },
    }),
    (error) => error instanceof ProtocolError && error.code === "invalid_cursor",
  );
  clearTimeout(receiver.playbackTimer);
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

test("a coordinator is a sibling of the app that served the receiver", () => {
  // The app is itself one identity's subdomain of the deployment root, so the
  // root is the serving host minus the app's own label — never the host
  // itself, which would nest every site under the app's identity.
  assert.equal(deploymentRoot("astrolabe.foundation.pub"), "foundation.pub");
  assert.equal(deploymentRoot("Display.Internal.Example.Lan"), "internal.example.lan");
  assert.equal(siteOrigin("acme", deploymentRoot("astrolabe.foundation.pub")), "https://acme.foundation.pub");
});

test("a host that cannot name a site refuses rather than inventing one", () => {
  // An IP literal, a bare label, a port, or an apex has nothing to hand a
  // site, and guessing would produce a coordinator nobody deployed.
  for (const host of ["localhost", "127.0.0.1", "192.168.1.10", "astrolabe.foundation.pub:8443", "example.pub.", "foundation.pub"]) {
    assert.throws(() => deploymentRoot(host), ProtocolError, `${host} is unprovisionable`);
  }
});

test("an invalid site code never becomes an origin", () => {
  assert.throws(() => siteOrigin("acme.lobby", "foundation.pub"), ProtocolError);
  assert.throws(() => siteOrigin("", "foundation.pub"), ProtocolError);
});

test("what a person types is a site, and a code if Astrolabe gave one", () => {
  // Case, spacing and the grouping hyphens are theirs; the code is always
  // the last eight symbols, so a hyphenated site survives in front of it.
  assert.deepEqual(parseEntry(" Acme-Lobby-7k3q-oi1l "), { site: "acme-lobby", code: "7K3Q0111" });
  assert.deepEqual(parseEntry("acme 7K3Q 0111"), { site: "acme", code: "7K3Q0111" });
  assert.deepEqual(parseEntry("acme-7k3q0111"), { site: "acme", code: "7K3Q0111" });
  // A site alone is the long way, not a mistake.
  assert.deepEqual(parseEntry("acme-lobby"), { site: "acme-lobby", code: null });
  assert.deepEqual(parseEntry("acme-lobby-abcd"), { site: "acme-lobby-abcd", code: null });
  // A tail that is not a code — U is not in the alphabet — is part of the site.
  assert.deepEqual(parseEntry("acme-7k3q-0u11"), { site: "acme-7k3q-0u11", code: null });
  assert.deepEqual(parseEntry(""), { site: "", code: null });
});

test("the provisioned bootstrap is Web-PKI and carries no pinned material", () => {
  const bootstrap = webPkiBootstrap("https://acme.foundation.pub");
  assert.deepEqual(bootstrap, {
    protocol_major: 1,
    trust: { kind: "web_pki_origin", origin: "https://acme.foundation.pub" },
    certificate_pem: null,
    rendezvous: null,
  });
  // A code rides in the slot the protocol reserved for it, as the wire id.
  assert.equal(webPkiBootstrap("https://acme.foundation.pub", "ab".repeat(16)).rendezvous, "ab".repeat(16));
  // The client is the authority on bootstrap shape; this is what it accepts.
  assert.doesNotThrow(() => new DisplayReceiverClient({
    bootstrap,
    capabilities: fixture.capabilities ?? {},
    ui: {},
    vaultFactory: async () => ({}),
  }));
});

// ─── Pairing against a real offer ───────────────────────────────────────────
//
// The fixture proves the phrase function; nothing above drove `startPairing`
// itself, which is how the client kept accepting a five-field offer from a
// coordinator that had been sending six. These run the whole first exchange
// — instance, then offer — over the native-transport seam the Android bridge
// uses, so no XHR and no television are needed.

function fakeCoordinator(routes) {
  const encoder = new TextEncoder();
  const requests = [];
  globalThis.AstrolabeNativeTransport = {
    request(requestId, payload) {
      const request = JSON.parse(payload);
      requests.push(request);
      const path = new URL(request.url).pathname;
      const handler = routes[`${request.method} ${path}`]
        ?? (() => { throw new Error(`no fake route for ${request.method} ${path}`); });
      Promise.resolve()
        .then(() => handler(request.body === null ? null : JSON.parse(request.body)))
        .then(
          (reply) => ({
            status: reply.status ?? 200,
            body_base64: Buffer.from(encoder.encode(JSON.stringify(reply.body))).toString("base64"),
            content_type: "application/json",
            next_challenge: reply.nextChallenge ?? "",
          }),
          (error) => ({ error: String(error) }),
        )
        .then((response) => globalThis.__astrolabeNativeTransportResolve(requestId, JSON.stringify(response)));
    },
  };
  return { requests, dispose: () => { delete globalThis.AstrolabeNativeTransport; } };
}

function recordingUi() {
  const events = [];
  return {
    events,
    showBooting: () => events.push(["booting"]),
    showPairing: (pairing) => events.push(["pairing", pairing]),
    showPairingWaiting: () => events.push(["waiting"]),
    showPairingNetworkError: () => events.push(["network"]),
    showPairingRejected: (kind, reason) => events.push(["rejected", kind, reason]),
    showFailure: (code, detail) => events.push(["failure", code, detail]),
    showConnecting: () => events.push(["connecting"]),
    showUnassigned: (device) => events.push(["unassigned", device]),
    showRecovering: (code) => events.push(["recovering", code]),
    setTransportState: () => {},
    setStaleState: () => {},
    setSourceState: () => {},
  };
}

async function waitFor(condition, what) {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(`timed out waiting for ${what}`);
}

function memoryVault() {
  const saved = [];
  return {
    saved,
    factory: async () => ({
      load: async () => null,
      save: async (state) => { saved.push(structuredClone(state)); },
      clear: async () => {},
      close: () => {},
    }),
  };
}

const ORIGIN = "https://acme.foundation.pub";
const PROFILE = fixture.confirmation_phrase.profile;
const OTHER_PROFILE = "prf_00000000000000000000000000";
const FINGERPRINT = "c".repeat(64);
const PAIRING = "d".repeat(32);
const RENDEZVOUS = await rendezvousFromCode("7K3Q-0111");

/**
 * What a real daemon answers: it describes the placement that listens — a
 * pinned certificate at a LAN origin — and names the identity it answers
 * for. A receiver that arrived by site through a route sees exactly this,
 * and holds the coordinator to the profile, not the placement.
 */
function instanceRoute() {
  return {
    body: {
      protocol_major: 1,
      instance: "e".repeat(32),
      label: "Home Astrolabe",
      profile: PROFILE,
      trust: { kind: "pinned_certificate", origin: "https://192.168.1.20:7443", sha256: FINGERPRINT },
    },
  };
}

function refusalRoute(status, code) {
  return { status, body: { protocol_major: 1, code, retry_after_ms: null, next_challenge: null } };
}

/** What the coordinator sends since it anchored on its identity. */
async function identityOffer(request, { profile = PROFILE, phraseProfile = profile, shape = null } = {}) {
  const words = await confirmationPhrase(phraseProfile, PAIRING, request.receiver_nonce);
  const offer = {
    protocol_major: 1,
    pairing: PAIRING,
    expires_in_ms: 600_000,
    confirmation_phrase: words,
    coordinator_fingerprint: FINGERPRINT,
    coordinator_profile: profile,
  };
  return { body: shape ? shape(offer) : offer };
}

async function pair(t, offerOptions, { rendezvous = null, holds = null } = {}) {
  const ui = recordingUi();
  const vault = memoryVault();
  const coordinator = fakeCoordinator({
    "GET /head/v1/instance": () => instanceRoute(),
    "POST /head/v1/pairings": (request) => {
      if (request.rendezvous !== null && request.rendezvous !== holds) {
        return refusalRoute(403, "authentication_failed");
      }
      return identityOffer(request, offerOptions);
    },
    "POST /head/v1/pairings/status": () => ({ body: { kind: "pending", retry_after_ms: 1000 } }),
  });
  t.after(coordinator.dispose);
  const receiver = new DisplayReceiverClient({
    bootstrap: webPkiBootstrap(ORIGIN, rendezvous),
    capabilities: { protocol_major: 1, platform: "webos", build: "test/0" },
    ui,
    vaultFactory: vault.factory,
  });
  t.after(() => receiver.stop());
  await receiver.start();
  return { receiver, ui, vault, requests: coordinator.requests };
}

test("the receiver pairs with a coordinator that anchors on its identity", async (t) => {
  const { ui, vault, requests } = await pair(t);

  const failure = ui.events.find(([kind]) => kind === "failure");
  assert.equal(failure, undefined, `pairing refused: ${JSON.stringify(failure)}`);
  const shown = ui.events.find(([kind]) => kind === "pairing")?.[1];
  assert.ok(shown, "the pairing screen was shown");

  // The words the television shows are the words the coordinator derived
  // from its identity and this receiver's nonce — the ones Astrolabe shows.
  const start = requests.find((request) => request.url.endsWith("/head/v1/pairings"));
  const started = JSON.parse(start.body);
  assert.deepEqual(shown.phrase, await confirmationPhrase(PROFILE, PAIRING, started.receiver_nonce));
  assert.equal(shown.fingerprint, FINGERPRINT);
  assert.equal(shown.confirmed, false);

  // And the credential written before anything is proven records which
  // identity those words belong to, beside the certificate that carried them.
  assert.equal(vault.saved.length, 1);
  assert.equal(vault.saved[0].mode, "pairing");
  assert.equal(vault.saved[0].profile, PROFILE);
  assert.equal(vault.saved[0].fingerprint, FINGERPRINT);
  assert.equal(vault.saved[0].receiverNonce, started.receiver_nonce);
  assert.equal(vault.saved[0].pollKey, started.poll_key);
});

test("an offer that does not name the coordinator's identity is refused", async (t) => {
  // The pre-identity shape: five fields, phrase from the certificate. A
  // receiver that accepted it would show words no current Astrolabe shows.
  const { ui, vault } = await pair(t, {
    shape: ({ coordinator_profile: _dropped, ...rest }) => rest,
  });
  assert.deepEqual(ui.events.at(-1).slice(0, 2), ["failure", "unknown_field"]);
  assert.equal(vault.saved.length, 0, "nothing is written for a refused offer");
});

test("words derived from a different identity than the one named are refused", async (t) => {
  const { ui, vault } = await pair(t, { phraseProfile: OTHER_PROFILE });
  assert.deepEqual(ui.events.at(-1).slice(0, 2), ["failure", "pairing_integrity"]);
  assert.equal(vault.saved.length, 0);
});

test("a coordinator profile that is not a profile id is refused before the phrase is checked", async (t) => {
  const { ui, vault } = await pair(t, { profile: FINGERPRINT, phraseProfile: PROFILE });
  assert.deepEqual(ui.events.at(-1).slice(0, 2), ["failure", "invalid_pairing"]);
  assert.equal(vault.saved.length, 0);
});

test("an offer from an identity other than the one the instance named is refused", async (t) => {
  // Self-consistent words for the wrong identity: the route answered as one
  // coordinator and the offer came from another.
  const { ui, vault } = await pair(t, { profile: OTHER_PROFILE, phraseProfile: OTHER_PROFILE });
  assert.deepEqual(ui.events.at(-1).slice(0, 2), ["failure", "pairing_integrity"]);
  assert.equal(vault.saved.length, 0);
});

test("a code from the controller rides the pairing start and needs no press", async (t) => {
  const { receiver, ui, vault, requests } = await pair(t, {}, { rendezvous: RENDEZVOUS, holds: RENDEZVOUS });

  const start = JSON.parse(requests.find((request) => request.url.endsWith("/head/v1/pairings")).body);
  assert.equal(start.rendezvous, RENDEZVOUS, "the code names its rendezvous on the wire");

  // Nobody compares words and nobody presses OK: the code was the
  // confirmation, made at the controller. The screen says so, and the
  // receiver is already asking whether it was approved.
  const shown = ui.events.filter(([kind]) => kind === "pairing").map(([, pairing]) => pairing);
  assert.ok(shown.every((pairing) => pairing.viaCode === true), JSON.stringify(shown));
  assert.equal(shown.at(-1).confirmed, true);
  assert.equal(vault.saved.at(-1).userConfirmed, true);
  await waitFor(
    () => requests.some((request) => request.url.endsWith("/head/v1/pairings/status")),
    "the receiver to poll for approval on its own",
  );
  assert.equal(receiver.rendezvous, null, "a code is spent by the start that carried it");
  assert.equal(ui.events.find(([kind]) => kind === "failure"), undefined);
});

test("a code the coordinator does not hold is refused as a code, and nothing is written", async (t) => {
  const other = await rendezvousFromCode("ABCD-EFGH");
  const { ui, vault } = await pair(t, {}, { rendezvous: RENDEZVOUS, holds: other });
  assert.deepEqual(ui.events.at(-1).slice(0, 2), ["failure", "rendezvous_refused"]);
  assert.equal(vault.saved.length, 0);
});
