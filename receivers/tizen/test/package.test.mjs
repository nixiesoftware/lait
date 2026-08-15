import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const shared = path.resolve(root, "..", "shared", "web");
const read = (relative) => readFile(path.join(root, relative), "utf8");

test("manifest is a closed Samsung TV package", async () => {
  const manifest = await read("app/config.xml");
  assert.match(manifest, /id="https:\/\/nixiesoftware\.com\/astrolabe"/);
  assert.match(manifest, /required_version="5\.5"/);
  assert.match(manifest, /<tizen:profile name="tv-samsung"\/>/);
  assert.match(manifest, /<access origin="https:\/\/nixiesoftware\.com" subdomains="false"\/>/);
  assert.doesNotMatch(manifest, /<tizen:privilege/);
});

test("receiver uses Tizen protected storage and the production protocol", async () => {
  const source = `${await read("app/app.mjs")}\n${await read("app/tizen-vault.mjs")}`;
  assert.match(source, /platform: "tizen"/);
  assert.match(source, /tizen\.keymanager/);
  assert.match(source, /saveData/);
  assert.match(source, /getData/);
  assert.match(source, /https:\/\/nixiesoftware\.com/);
  assert.doesNotMatch(source, /localStorage|demo/i);
});

test("packaged runtime equals the conformance-tested shared runtime", async () => {
  for (const name of ["client.mjs", "protocol.mjs", "transport.mjs", "vault.mjs"]) {
    assert.deepEqual(
      await readFile(path.join(root, "app", "runtime", name)),
      await readFile(path.join(shared, name)),
      name,
    );
  }
});

test("receiver UI exposes real pairing, assignment, and refusal states", async () => {
  const html = await read("app/index.html");
  assert.match(html, /Compare these words in Astrolabe/);
  assert.match(html, /Ready for an assignment/);
  assert.match(html, /program-frame/);
  assert.doesNotMatch(html, /ASTR-DEMO|Demo program|Preview only/);
});
