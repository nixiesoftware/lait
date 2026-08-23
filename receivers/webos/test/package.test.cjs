'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const packageDir = path.join(root, 'package');
const hostedDir = path.join(root, 'hosted');
const sharedRuntimeDir = path.resolve(root, '..', 'shared', 'web');

function read(relativePath) {
  return fs.readFileSync(path.join(root, relativePath), 'utf8');
}

function pngSize(relativePath) {
  const bytes = fs.readFileSync(path.join(root, relativePath));
  const signature = bytes.subarray(0, 8).toString('hex');
  assert.equal(signature, '89504e470d0a1a0a', `${relativePath} is a PNG`);
  return {
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20)
  };
}

test('manifest commits the Nixie and Astrolabe identities', () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(packageDir, 'appinfo.json'), 'utf8'));

  assert.equal(manifest.id, 'com.nixiesoftware.app.astrolabe');
  assert.equal(manifest.vendor, 'Nixie Solutions LLC');
  assert.equal(manifest.title, 'Astrolabe');
  assert.match(manifest.version, /^\d+\.\d+\.\d+$/);
  assert.equal(manifest.type, 'web');
  assert.equal(manifest.main, 'index.html');
  assert.equal(manifest.disableBackHistoryAPI, false);

  for (const asset of [manifest.main, manifest.icon, manifest.largeIcon, manifest.splashBackground]) {
    assert.equal(fs.existsSync(path.join(packageDir, asset)), true, `${asset} exists`);
  }
});

test('mandatory app art has the exact webOS dimensions', () => {
  assert.deepEqual(pngSize('package/icon.png'), { width: 80, height: 80 });
  assert.deepEqual(pngSize('package/largeIcon.png'), { width: 130, height: 130 });
  assert.deepEqual(pngSize('package/splashBackground.png'), { width: 1920, height: 1080 });
});

test('the store stub redirects only to the named HTTPS host', () => {
  const html = read('package/index.html');

  assert.match(html, /https:\/\/astrolabe\.foundation\.pub\/display\//);
  assert.doesNotMatch(html, /http:\/\//);
});

test('the hosted receiver contains production pairing and assigned-frame states', () => {
  const html = read('hosted/index.html');

  assert.match(html, /Compare these words in Astrolabe/);
  assert.match(html, /Ready for an assignment/);
  assert.match(html, /program-frame/);
  assert.match(html, /program-media/);
  assert.match(html, /type="module" src="app\.mjs"/);
  assert.doesNotMatch(html, /ASTR-DEMO|Demo program|Preview only/);
  assert.equal(fs.existsSync(hostedDir), true);
});

test('the hosted receiver uses granted MSE live media', () => {
  const source = `${read('hosted/index.html')}\n${read('hosted/app.mjs')}\n${read('hosted/runtime/client.mjs')}`;
  assert.match(source, /tier: mseCapable \? "mse_live"/);
  assert.match(source, /\/head\/v1\/live\/tickets/);
  assert.match(source, /connect-src https:\/\/\*\.foundation\.pub wss:\/\/\*\.foundation\.pub/);
  assert.match(source, /new MediaSource\(\)/);
  assert.match(source, /new WebSocket\(/);
});

test('the receiver implements the closed authenticated protocol without a product route', () => {
  const source = [
    read('hosted/index.html'),
    read('hosted/app.mjs'),
    read('hosted/runtime/client.mjs'),
    read('hosted/runtime/protocol.mjs'),
    read('hosted/runtime/transport.mjs'),
    read('hosted/runtime/vault.mjs')
  ].join('\n');

  assert.match(source, /astrolabe-display\/request\/v1/);
  assert.match(source, /HMAC/);
  assert.match(source, /AES-GCM/);
  assert.match(source, /indexedDB/);
  assert.match(source, /\/head\/v1\/program\/changes/);
  assert.match(source, /asset_digest/);
  assert.doesNotMatch(source, /\/world|\/space|generic[^\n]+rpc|ASTR-DEMO/i);
  assert.doesNotMatch(source, /<script[^>]+src=["']https?:/i);
});

test('the hosted copy exactly matches the shared conformance-tested runtime', () => {
  // Enumerated rather than listed: a module added to the shared runtime must
  // not be able to escape the sync guard by not being named here.
  const modules = (directory) =>
    fs.readdirSync(directory).filter((name) => name.endsWith('.mjs')).sort();
  const shared = modules(sharedRuntimeDir);

  assert.ok(shared.length >= 5, 'the shared runtime has modules to synchronize');
  assert.deepEqual(
    modules(path.join(hostedDir, 'runtime')),
    shared,
    'every shared module is copied, and no stale copy remains'
  );
  for (const name of shared) {
    assert.deepEqual(
      fs.readFileSync(path.join(hostedDir, 'runtime', name)),
      fs.readFileSync(path.join(sharedRuntimeDir, name)),
      `${name} is synchronized`
    );
  }
});

test('no coordinator origin is compiled into the receiver', () => {
  // One package serves every site. The origin is a fact about where the
  // television is standing, resolved at runtime from the host that served the
  // app, so a new location never needs a new build.
  assert.doesNotMatch(read('hosted/app.mjs'), /https:\/\/[a-z0-9]/i);
  assert.doesNotMatch(read('hosted/runtime/provisioning.mjs'), /https:\/\/[a-z0-9]/i);
  assert.match(read('hosted/app.mjs'), /deploymentRoot\(window\.location\.hostname\)/);
  assert.match(read('hosted/index.html'), /id="site-entry"/);
});

test('a mistyped site is recoverable only before anything is enrolled', () => {
  // After enrollment the site is not a typo to correct but a credential to
  // revoke, and that belongs to Astrolabe rather than to whoever holds the
  // remote. Before it, an unreachable coordinator must not be a dead display.
  const source = read('hosted/app.mjs');

  assert.match(read('hosted/index.html'), /id="change-site-action"/);
  assert.match(source, /allowChangeSite\(!enrolled, \(\) => store\.clear\(\)\)/);
  assert.match(source, /this\.canChangeSite = unenrolled;/);
  assert.match(source, /if \(!unenrolled\) return;/);
});

test('the content policy admits the media this receiver actually plays', () => {
  // MSE attaches `URL.createObjectURL(mediaSource)` to the video element, so a
  // policy without `media-src blob:` refuses the receiver's own live playback.
  // Node conformance runs without a CSP, which is why this is asserted here.
  const html = read('hosted/index.html');
  assert.match(html, /img-src 'self' blob:/);
  assert.match(html, /media-src blob:/);
});
