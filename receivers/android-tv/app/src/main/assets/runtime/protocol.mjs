const encoder = new TextEncoder();

export const PROTOCOL_MAJOR = 1;

export const BOUNDS = Object.freeze({
  maxAssetBytes: 16 * 1024 * 1024,
  maxStagedBytes: 48 * 1024 * 1024,
  maxFrameWidth: 4096,
  maxFrameHeight: 2160,
  maxFramePixels: 8847360,
  maxProgramItems: 16,
  maxStagingHorizonMs: 86400000,
  minItemDurationMs: 250,
  maxItemDurationMs: 86400000,
  minStaleAfterMs: 30000,
  maxStaleAfterMs: 86400000,
  maxLongPollWaitMs: 25000,
  longPollStaleMarginMs: 5000,
  maxSummaryBytes: 1024,
  maxSyncGroupBytes: 64,
});

const CONFIRMATION_WORDS = Object.freeze([
  "amber", "anchor", "apple", "beacon", "birch", "cedar", "comet", "coral",
  "delta", "ember", "falcon", "fjord", "garden", "harbor", "hazel", "indigo",
  "juniper", "lantern", "maple", "meadow", "meteor", "olive", "orbit", "pebble",
  "quartz", "river", "saffron", "signal", "spruce", "violet", "willow", "zephyr",
]);

export class ProtocolError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "ProtocolError";
    this.code = code;
  }
}

function refuse(code, message) {
  throw new ProtocolError(code, message);
}

function requireObject(value, name) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    refuse("invalid_shape", `${name} must be an object`);
  }
  return value;
}

function requireFields(value, required, name) {
  requireObject(value, name);
  const actual = Object.keys(value).sort();
  const expected = [...required].sort();
  if (actual.length !== expected.length || actual.some((field, index) => field !== expected[index])) {
    refuse("unknown_field", `${name} fields do not match protocol major 1`);
  }
}

function requireInteger(value, minimum, maximum, name) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    refuse("bound_exceeded", `${name} is outside its bound`);
  }
}

function requireText(value, maximumBytes, name) {
  if (typeof value !== "string" || value.length === 0 || encoder.encode(value).length > maximumBytes) {
    refuse("bound_exceeded", `${name} is outside its bound`);
  }
  for (const character of value) {
    if (/\p{Cc}/u.test(character)) {
      refuse("invalid_shape", `${name} contains a control character`);
    }
  }
}

export function isLowerHex(value, characters) {
  return typeof value === "string"
    && value.length === characters
    && /^[0-9a-f]+$/.test(value);
}

export function requireId(value, characters, name) {
  if (!isLowerHex(value, characters)) {
    refuse("invalid_identifier", `${name} is not canonical lowercase hex`);
  }
  return value;
}

export function hexToBytes(value, expectedBytes) {
  requireId(value, expectedBytes * 2, "hex value");
  const output = new Uint8Array(expectedBytes);
  for (let index = 0; index < output.length; index += 1) {
    output[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16);
  }
  return output;
}

export function bytesToHex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function concat(parts) {
  const length = parts.reduce((total, part) => total + part.length, 0);
  const output = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.length;
  }
  return output;
}

function numberBytes(value, bytes) {
  const output = new Uint8Array(bytes);
  let remaining = BigInt(value);
  for (let index = bytes - 1; index >= 0; index -= 1) {
    output[index] = Number(remaining & 255n);
    remaining >>= 8n;
  }
  return output;
}

function field(value) {
  const bytes = typeof value === "string" ? encoder.encode(value) : value;
  return concat([numberBytes(bytes.length, 4), bytes]);
}

class Transcript {
  constructor(domain) {
    this.parts = [field(domain)];
  }

  text(value) {
    this.parts.push(field(value));
  }

  optionalText(value) {
    this.parts.push(field(value == null ? new Uint8Array() : value));
  }

  u32(value) {
    this.parts.push(field(numberBytes(value, 4)));
  }

  optionalU32(value) {
    this.parts.push(field(value == null ? new Uint8Array() : numberBytes(value, 4)));
  }

  optionalU64(value) {
    this.parts.push(field(value == null ? new Uint8Array() : numberBytes(value, 8)));
  }

  boolean(value) {
    this.parts.push(field(Uint8Array.of(value ? 1 : 0)));
  }

  finish() {
    return concat(this.parts);
  }
}

async function subtleCrypto() {
  const candidate = globalThis.crypto && globalThis.crypto.subtle;
  if (!candidate) {
    refuse("unsupported_crypto", "Web Crypto is required for a production receiver");
  }
  return candidate;
}

export async function sha256(bytes) {
  const subtle = await subtleCrypto();
  return bytesToHex(new Uint8Array(await subtle.digest("SHA-256", bytes)));
}

export async function hmacSha256(keyHex, bytes) {
  const subtle = await subtleCrypto();
  const key = await subtle.importKey(
    "raw",
    hexToBytes(keyHex, 32),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return bytesToHex(new Uint8Array(await subtle.sign("HMAC", key, bytes)));
}

const METHODS = Object.freeze({ GET: true, POST: true });
const ROUTES = Object.freeze({
  capabilities: true,
  program_snapshot: true,
  program_changes: true,
  asset: true,
  health: true,
});

function validateRequestContext(context) {
  requireObject(context, "request context");
  if (context.protocolMajor !== PROTOCOL_MAJOR) {
    refuse("unsupported", "unsupported protocol major");
  }
  if (!METHODS[context.method] || !ROUTES[context.route]) {
    refuse("invalid_shape", "unknown request method or route");
  }
  requireId(context.device, 32, "device id");
  requireId(context.challenge, 64, "challenge");
  requireId(context.bodySha256, 64, "body SHA-256");
  if ((context.assignment == null) !== (context.program == null)) {
    refuse("invalid_shape", "assignment and program must appear together");
  }
  if (context.assignment != null) requireId(context.assignment, 32, "assignment id");
  if (context.program != null) requireId(context.program, 32, "program id");
  if (context.revision != null) requireId(context.revision, 64, "program revision");
  if (context.currentItem != null) requireId(context.currentItem, 64, "current item id");
  if (context.asset != null) requireId(context.asset, 64, "asset id");
  if (context.elapsedMs != null) requireInteger(context.elapsedMs, 0, 0xffffffff, "elapsed milliseconds");
  if (context.waitMs != null) requireInteger(context.waitMs, 1, BOUNDS.maxLongPollWaitMs, "long-poll wait");
  if (context.range != null) {
    requireFields(context.range, ["start", "length"], "asset range");
    requireInteger(context.range.start, 0, Number.MAX_SAFE_INTEGER, "asset range start");
    requireInteger(context.range.length, 1, 0xffffffff, "asset range length");
  }

  const noCursorOrAsset = context.currentItem == null
    && context.elapsedMs == null
    && context.waitMs == null
    && context.asset == null
    && context.range == null;
  let valid = false;
  switch (context.route) {
    case "capabilities":
      valid = context.method === "POST" && noCursorOrAsset;
      break;
    case "program_snapshot":
      valid = context.method === "GET" && context.revision == null && noCursorOrAsset;
      break;
    case "program_changes":
      valid = context.method === "GET"
        && context.assignment != null
        && context.revision != null
        && context.currentItem != null
        && context.elapsedMs != null
        && context.waitMs != null
        && context.asset == null
        && context.range == null;
      break;
    case "asset":
      valid = context.method === "GET"
        && context.assignment != null
        && context.revision != null
        && context.currentItem == null
        && context.elapsedMs == null
        && context.waitMs == null
        && context.asset != null;
      break;
    case "health":
      valid = context.method === "POST"
        && context.assignment != null
        && context.revision != null
        && context.currentItem != null
        && context.elapsedMs != null
        && context.waitMs == null
        && context.asset == null
        && context.range == null;
      break;
    default:
      valid = false;
  }
  if (!valid) refuse("invalid_shape", "request context does not match its route");
}

export function requestTranscript(context) {
  validateRequestContext(context);
  const transcript = new Transcript("astrolabe-display/request/v1");
  transcript.u32(context.protocolMajor);
  transcript.text(context.method);
  transcript.text(context.route);
  transcript.text(context.device);
  transcript.optionalText(context.assignment);
  transcript.optionalText(context.program);
  transcript.optionalText(context.revision);
  transcript.optionalText(context.currentItem);
  transcript.optionalU32(context.elapsedMs);
  transcript.optionalU32(context.waitMs);
  transcript.optionalText(context.asset);
  transcript.optionalU64(context.range ? context.range.start : null);
  transcript.optionalU32(context.range ? context.range.length : null);
  transcript.text(context.challenge);
  transcript.text(context.bodySha256);
  return transcript.finish();
}

export async function authenticateRequest(proofKey, context) {
  requireId(proofKey, 64, "proof key");
  return hmacSha256(proofKey, requestTranscript(context));
}

function requireSortedUnique(values, maximum, name) {
  if (!Array.isArray(values) || values.length === 0 || values.length > maximum) {
    refuse("bound_exceeded", `${name} count is outside its bound`);
  }
  for (let index = 1; index < values.length; index += 1) {
    if (values[index - 1] >= values[index]) {
      refuse("invalid_shape", `${name} must be sorted and unique`);
    }
  }
}

const PARTIAL_REASONS = Object.freeze([
  "corrupt_records", "degraded_source", "incomplete_projection", "provisional_data",
]);

function validateSourceState(state) {
  requireObject(state, "source state");
  if (state.kind === "current" || state.kind === "unavailable") {
    requireFields(state, ["kind"], "source state");
    return;
  }
  if (state.kind !== "partial") refuse("invalid_shape", "unknown source state");
  requireFields(state, ["kind", "reasons"], "partial source state");
  requireSortedUnique(state.reasons, 4, "partial reasons");
  if (state.reasons.some((reason) => !PARTIAL_REASONS.includes(reason))) {
    refuse("invalid_shape", "unknown partial reason");
  }
}

function encodeSourceState(transcript, state) {
  validateSourceState(state);
  transcript.text(state.kind);
  if (state.kind === "partial") {
    transcript.u32(state.reasons.length);
    for (const reason of state.reasons) transcript.text(reason);
  }
}

const IMAGE_TYPES = Object.freeze(["image_jpeg", "image_png", "image_webp"]);
const MANIFEST_TYPES = Object.freeze(["dash_manifest", "hls_manifest"]);

function validateAsset(asset) {
  requireFields(asset, ["id", "media_type", "encoded_len", "sha256", "width", "height"], "asset");
  requireId(asset.id, 64, "asset id");
  requireId(asset.sha256, 64, "asset SHA-256");
  requireInteger(asset.encoded_len, 1, BOUNDS.maxAssetBytes, "asset encoded length");
  if (IMAGE_TYPES.includes(asset.media_type)) {
    requireInteger(asset.width, 1, BOUNDS.maxFrameWidth, "image width");
    requireInteger(asset.height, 1, BOUNDS.maxFrameHeight, "image height");
    if (asset.width * asset.height > BOUNDS.maxFramePixels) {
      refuse("bound_exceeded", "decoded image pixels exceed the bound");
    }
  } else if (MANIFEST_TYPES.includes(asset.media_type)) {
    if (asset.width !== null || asset.height !== null) {
      refuse("invalid_shape", "manifest dimensions must be null");
    }
  } else {
    refuse("unsupported", "unknown asset media type");
  }
}

function encodeAsset(transcript, asset) {
  validateAsset(asset);
  transcript.text(asset.media_type);
  transcript.u32(asset.encoded_len);
  transcript.text(asset.sha256);
  transcript.optionalU32(asset.width);
  transcript.optionalU32(asset.height);
}

const CYCLES = Object.freeze(["blank_at_end", "hold_last", "loop", "poll_at_end"]);
const SYNC_MODES = Object.freeze(["stay_in_sync", "positional"]);
const STALE_ACTIONS = Object.freeze(["blank", "keep_with_native_banner"]);
const BLANK_REASONS = Object.freeze([
  "host_unavailable", "program_ended", "revoked", "source_unavailable", "unassigned", "unsupported",
]);

function validateScene(scene) {
  requireObject(scene, "scene");
  switch (scene.kind) {
    case "frame":
      requireFields(scene, ["kind", "asset"], "frame scene");
      validateAsset(scene.asset);
      if (!IMAGE_TYPES.includes(scene.asset.media_type)) refuse("invalid_shape", "frame requires an image");
      break;
    case "media":
      requireFields(scene, ["kind", "manifest", "protocol", "live"], "media scene");
      validateAsset(scene.manifest);
      if ((scene.protocol === "hls" && scene.manifest.media_type !== "hls_manifest")
        || (scene.protocol === "dash" && scene.manifest.media_type !== "dash_manifest")
        || !["hls", "dash"].includes(scene.protocol)
        || typeof scene.live !== "boolean") {
        refuse("invalid_shape", "media manifest does not match its protocol");
      }
      break;
    case "blank":
      requireFields(scene, ["kind", "reason"], "blank scene");
      if (!BLANK_REASONS.includes(scene.reason)) refuse("unsupported", "unknown blank reason");
      break;
    default:
      refuse("unsupported", "unknown scene kind");
  }
}

export function validateProgram(program, verifyRevision = false) {
  requireFields(
    program,
    ["protocol_major", "assignment", "program", "revision", "program_state", "freshness", "playback", "items"],
    "display program",
  );
  if (program.protocol_major !== PROTOCOL_MAJOR) refuse("unsupported", "unsupported protocol major");
  requireId(program.assignment, 32, "assignment id");
  requireId(program.program, 32, "program id");
  requireId(program.revision, 64, "program revision");
  validateSourceState(program.program_state);
  requireFields(program.freshness, ["stale_after_ms", "on_stale"], "freshness policy");
  requireInteger(
    program.freshness.stale_after_ms,
    BOUNDS.minStaleAfterMs,
    BOUNDS.maxStaleAfterMs,
    "stale interval",
  );
  if (program.freshness.stale_after_ms <= BOUNDS.maxLongPollWaitMs + BOUNDS.longPollStaleMarginMs) {
    refuse("invalid_shape", "stale interval has no long-poll margin");
  }
  if (!STALE_ACTIONS.includes(program.freshness.on_stale)) refuse("unsupported", "unknown stale action");
  requireFields(program.playback, ["current_index", "elapsed_ms", "cycle", "sync"], "playback cursor");
  if (!CYCLES.includes(program.playback.cycle)) refuse("unsupported", "unknown program cycle");
  if (program.playback.sync !== null) {
    requireFields(program.playback.sync, ["group", "mode", "sampled_at_unix_ms"], "sync target");
    requireText(program.playback.sync.group, BOUNDS.maxSyncGroupBytes, "sync group");
    if (!/^[a-z0-9_-]+$/.test(program.playback.sync.group)) {
      refuse("invalid_identifier", "sync group is not canonical");
    }
    if (!SYNC_MODES.includes(program.playback.sync.mode)) refuse("unsupported", "unknown sync mode");
    requireInteger(
      program.playback.sync.sampled_at_unix_ms,
      1,
      Number.MAX_SAFE_INTEGER,
      "sync target sample time",
    );
  }
  if (!Array.isArray(program.items) || program.items.length === 0 || program.items.length > BOUNDS.maxProgramItems) {
    refuse("bound_exceeded", "program item count is outside its bound");
  }
  requireInteger(program.playback.current_index, 0, program.items.length - 1, "current item index");
  requireInteger(program.playback.elapsed_ms, 0, 0xffffffff, "elapsed milliseconds");

  const itemIds = new Set();
  let horizon = 0;
  for (let index = 0; index < program.items.length; index += 1) {
    const item = program.items[index];
    requireFields(item, ["id", "duration_ms", "source_state", "scene", "spoken_summary"], "program item");
    requireId(item.id, 64, "program item id");
    if (itemIds.has(item.id)) refuse("invalid_shape", "duplicate program item id");
    itemIds.add(item.id);
    validateSourceState(item.source_state);
    validateScene(item.scene);
    if (item.spoken_summary !== null) requireText(item.spoken_summary, BOUNDS.maxSummaryBytes, "spoken summary");
    if (item.duration_ms === null) {
      if (index !== program.items.length - 1 || program.playback.cycle !== "hold_last") {
        refuse("invalid_shape", "only the final hold-last item can be open-ended");
      }
    } else {
      requireInteger(item.duration_ms, BOUNDS.minItemDurationMs, BOUNDS.maxItemDurationMs, "item duration");
      horizon += item.duration_ms;
      if (horizon > BOUNDS.maxStagingHorizonMs) refuse("bound_exceeded", "staging horizon exceeds bound");
    }
  }
  const current = program.items[program.playback.current_index];
  if (current.duration_ms !== null && program.playback.elapsed_ms >= current.duration_ms) {
    refuse("invalid_shape", "elapsed position is outside the current item");
  }
  if (verifyRevision) return canonicalProgramRevision(program).then((revision) => {
    if (revision !== program.revision) refuse("integrity", "program revision mismatch");
    return program;
  });
  return program;
}

export function programSemanticsTranscript(program) {
  validateProgram(program);
  const transcript = new Transcript("astrolabe-display/program-semantics/v2");
  transcript.u32(program.protocol_major);
  transcript.text(program.assignment);
  transcript.text(program.program);
  encodeSourceState(transcript, program.program_state);
  transcript.u32(program.freshness.stale_after_ms);
  transcript.text(program.freshness.on_stale);
  transcript.text(program.playback.cycle);
  transcript.boolean(program.playback.sync !== null);
  if (program.playback.sync !== null) {
    transcript.text(program.playback.sync.group);
    transcript.text(program.playback.sync.mode);
  }
  transcript.u32(program.items.length);
  for (const item of program.items) {
    transcript.text(item.id);
    transcript.optionalU32(item.duration_ms);
    encodeSourceState(transcript, item.source_state);
    transcript.text(item.scene.kind);
    if (item.scene.kind === "frame") {
      encodeAsset(transcript, item.scene.asset);
    } else if (item.scene.kind === "media") {
      encodeAsset(transcript, item.scene.manifest);
      transcript.text(item.scene.protocol);
      transcript.boolean(item.scene.live);
    } else {
      transcript.text(item.scene.reason);
    }
    transcript.optionalText(item.spoken_summary);
  }
  return transcript.finish();
}

export async function canonicalProgramRevision(program) {
  return sha256(programSemanticsTranscript(program));
}

export async function verifyProgram(program) {
  return validateProgram(program, true);
}

export function pairingStatusTranscript(pairing) {
  requireId(pairing, 32, "pairing id");
  const transcript = new Transcript("astrolabe-display/pairing-status/v1");
  transcript.u32(PROTOCOL_MAJOR);
  transcript.text(pairing);
  return transcript.finish();
}

export async function authenticatePairingStatus(pollKey, pairing) {
  requireId(pollKey, 64, "pairing poll key");
  return hmacSha256(pollKey, pairingStatusTranscript(pairing));
}

export function pairingCompleteTranscript(pairing, device, challenge) {
  requireId(pairing, 32, "pairing id");
  requireId(device, 32, "device id");
  requireId(challenge, 64, "enrollment challenge");
  const transcript = new Transcript("astrolabe-display/pairing-complete/v1");
  transcript.u32(PROTOCOL_MAJOR);
  transcript.text(pairing);
  transcript.text(device);
  transcript.text(challenge);
  return transcript.finish();
}

export async function authenticatePairingComplete(proofKey, pairing, device, challenge) {
  requireId(proofKey, 64, "proof key");
  return hmacSha256(proofKey, pairingCompleteTranscript(pairing, device, challenge));
}

export async function confirmationPhrase(fingerprint, pairing, receiverNonce) {
  requireId(fingerprint, 64, "coordinator fingerprint");
  requireId(pairing, 32, "pairing id");
  requireId(receiverNonce, 64, "receiver nonce");
  const transcript = new Transcript("astrolabe-display/confirmation-phrase/v1");
  transcript.u32(PROTOCOL_MAJOR);
  transcript.text(fingerprint);
  transcript.text(pairing);
  transcript.text(receiverNonce);
  const digest = hexToBytes(await sha256(transcript.finish()), 32);
  return Array.from(digest.slice(0, 6), (byte) => CONFIRMATION_WORDS[byte & 0x1f]);
}

export function randomHex(byteLength) {
  if (!globalThis.crypto || !globalThis.crypto.getRandomValues) {
    refuse("unsupported_crypto", "secure random generation is required");
  }
  const bytes = new Uint8Array(byteLength);
  globalThis.crypto.getRandomValues(bytes);
  return bytesToHex(bytes);
}
