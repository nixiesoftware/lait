import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (relative) => readFile(path.join(root, relative), "utf8");

test("manifest is TV-only, network-only, and non-backup", async () => {
  const manifest = await read("app/src/main/AndroidManifest.xml");
  assert.match(manifest, /android\.software\.leanback/);
  assert.match(manifest, /LEANBACK_LAUNCHER/);
  assert.match(manifest, /android\.permission\.INTERNET/);
  assert.match(manifest, /android:allowBackup="false"/);
  assert.match(manifest, /android:usesCleartextTraffic="false"/);
  assert.doesNotMatch(manifest, /BROWSABLE|WRITE_EXTERNAL_STORAGE/);
});

test("credential bridge is Android Keystore AES-GCM and bounded", async () => {
  const source = await read("app/src/main/java/com/nixiesoftware/astrolabe/SecureStoreBridge.java");
  assert.match(source, /AndroidKeyStore/);
  assert.match(source, /AES\/GCM\/NoPadding/);
  assert.match(source, /setRandomizedEncryptionRequired\(true\)/);
  assert.match(source, /16 \* 1024/);
  assert.match(source, /\.commit\(\)/);
});

test("web surface is bundled and closes navigation", async () => {
  const source = await read("app/src/main/java/com/nixiesoftware/astrolabe/ReceiverActivity.java");
  const transport = await read("app/src/main/java/com/nixiesoftware/astrolabe/NativeTransportBridge.java");
  assert.match(source, /WebViewAssetLoader/);
  assert.match(source, /MIXED_CONTENT_NEVER_ALLOW/);
  assert.match(source, /setAllowFileAccess\(false\)/);
  assert.match(source, /shouldOverrideUrlLoading/);
  assert.doesNotMatch(source, /setAllowUniversalAccessFromFileURLs\(true\)/);
  assert.match(source, /AstrolabeNativeTransport/);
  assert.match(transport, /MessageDigest\.isEqual/);
  assert.match(transport, /setInstanceFollowRedirects\(false\)/);
  assert.match(transport, /getDefaultHostnameVerifier/);
  assert.match(transport, /Executors\.newSingleThreadExecutor/);
  assert.match(transport, /webView\.post/);
  assert.doesNotMatch(`${source}\n${transport}`, /onReceivedSslError|SslErrorHandler\.proceed/);
});

test("application runtime declares the production Android TV capability", async () => {
  const source = await read("app/src/main/assets/app.mjs");
  assert.match(source, /platform: "android_tv"/);
  assert.match(source, /vaultFactory: AndroidCredentialVault\.open/);
  assert.match(source, /AstrolabeNativeTransport\.bootstrap/);
  const bootstrap = JSON.parse(await read("app/src/main/assets/receiver-bootstrap.json"));
  assert.deepEqual(bootstrap.trust, { kind: "web_pki_origin", origin: "https://nixiesoftware.com" });
  assert.equal(bootstrap.certificate_pem, null);
  assert.doesNotMatch(source, /ASTR-DEMO|Demo program|Preview only/);
});

test("Android TV uses the granted MSE decoder path", async () => {
  const app = await read("app/src/main/assets/app.mjs");
  const runtime = await read("app/src/main/assets/runtime/client.mjs");
  const transport = await read("app/src/main/java/com/nixiesoftware/astrolabe/NativeTransportBridge.java");
  const html = await read("app/src/main/assets/index.html");
  assert.match(app, /tier: mseCapable \? "mse_live"/);
  assert.match(app, /bootstrap\.trust\?\.kind === "web_pki_origin"/);
  assert.match(runtime, /\/head\/v1\/live\/tickets/);
  assert.match(runtime, /new MediaSource\(\)/);
  assert.match(transport, /"\/head\/v1\/live\/tickets"\.equals\(path\)/);
  assert.match(html, /connect-src wss:\/\/nixiesoftware\.com/);
  assert.match(html, /program-media/);
});
