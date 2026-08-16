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

test("decoded frame remains hidden until dimensions match", async () => {
  const scene = await read("components/AstrolabeScene.brs");
  assert.match(scene, /loadStatus = "ready"/);
  assert.match(scene, /bitmapWidth = m\.expectedWidth/);
  assert.match(scene, /bitmapHeight = m\.expectedHeight/);
  assert.match(scene, /m\.frame\.visible = true/);
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
