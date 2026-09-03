import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (relative) => readFile(path.join(root, relative), "utf8");

test("channel declares Astrolabe identity and a current SceneGraph floor", async () => {
  const manifest = await read("manifest");
  assert.match(manifest, /^title=Astrolabe$/m);
  assert.match(manifest, /^requires_roku_api=12\.5$/m);
  assert.match(manifest, /^supports_audio_guide=1$/m);
  assert.doesNotMatch(manifest, /demo|screensaver/i);
});

test("protocol implementation uses Roku production cryptography", async () => {
  const protocol = await read("source/Protocol.brs");
  const store = await read("source/Store.brs");
  assert.match(protocol, /astrolabe-display\/request\/v1/);
  assert.match(protocol, /roHMAC/);
  assert.match(protocol, /roEVPDigest/);
  assert.match(protocol, /GetRandomUUID/);
  assert.match(protocol, /130ed97e77f7751b21fe524e1d48f49f40129342cdfcce26ef3c12ce56a7ff0d/);
  assert.match(store, /roDeviceCrypto/);
  assert.match(store, /\.Flush\(\)/);
  assert.doesNotMatch(`${protocol}\n${store}`, /Rnd\(/);
});

test("receiver uses only closed authenticated coordinator routes", async () => {
  const task = await read("components/ReceiverTask.brs");
  const protocol = await read("source/Protocol.brs");
  const bootstrap = JSON.parse(await read("receiver-bootstrap.json"));
  assert.match(task, /SetCertificatesFile\(m\.certificates\)/);
  assert.match(protocol, /AstrolabeSha256\(certificate\) <> trust\.sha256/);
  assert.match(protocol, /tmp:\/astrolabe-coordinator-ca\.pem/);
  assert.deepEqual(bootstrap.trust, { kind: "web_pki_origin", origin: "https://nixiesoftware.com" });
  assert.match(task, /X-Astrolabe-Next-Challenge/);
  assert.match(task, /\/head\/v1\/program\/changes/);
  assert.match(task, /Range/);
  assert.match(task, /AstrolabeVerifyProgram/);
  assert.doesNotMatch(task, /\/world|\/space|generic[^\n]+rpc|demo/i);
});

test("decoded frame remains hidden until dimensions match, then is revealed", async () => {
  const scene = await read("components/AstrolabeScene.brs");
  assert.match(scene, /poster\.loadStatus = "ready"/);
  assert.match(scene, /bitmapWidth = expectedWidth/);
  assert.match(scene, /bitmapHeight = expectedHeight/);
  // A verified still is brought to glass only through the reveal, which makes
  // exactly one poster visible; a mismatch takes the refusal branch instead.
  assert.match(scene, /AstrolabeRevealFrame\(index\)/);
  assert.match(scene, /m\.posters\[index\]\.visible = true/);
});

test("Roku stages assignment-bound HLS and hands it to Video", async () => {
  const task = await read("components/ReceiverTask.brs");
  const scene = await read("components/AstrolabeScene.brs");
  const xml = await read("components/AstrolabeScene.xml");
  assert.match(task, /tier: "native_hls"/);
  assert.match(task, /AstrolabeAuthorizedJson\("live_ticket"/);
  assert.match(task, /\/head\/v1\/live\/tickets/);
  assert.match(xml, /<Video id="programMedia"/);
  assert.match(scene, /content\.streamFormat = "hls"/);
  assert.match(scene, /m\.media\.control = "play"/);
});

test("a player error asks the stream before it asks for a ticket", async () => {
  const task = await read("components/ReceiverTask.brs");
  const scene = await read("components/AstrolabeScene.brs");
  const refresh = task.slice(task.indexOf("function AstrolabeRefreshLive"));
  const probe = refresh.indexOf("AstrolabeProbeLive(current)");
  const mint = refresh.indexOf("AstrolabeAuthorizedLive(item, m.program)");
  assert.ok(probe >= 0 && mint > probe, "the live playlist is probed before a ticket is minted");
  assert.match(task, /verdict = "alive"[\s\S]*?AstrolabeRenderCurrent\(\)/);
  assert.match(task, /verdict = "refused" or waited >= AstrolabeLiveRetryLimitMs\(\)/);
  // The scene no longer blanks the glass on the player's word alone.
  const onError = scene.slice(scene.indexOf('if m.media.state = "error"'));
  assert.doesNotMatch(onError.slice(0, onError.indexOf("end if")), /AstrolabeMessage\(/);
});
