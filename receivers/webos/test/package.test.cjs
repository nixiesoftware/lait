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

  assert.match(html, /https:\/\/nixiesoftware\.com\/astrolabe\/display\//);
  assert.doesNotMatch(html, /http:\/\//);
});

test('the hosted receiver contains production pairing and assigned-frame states', () => {
  const html = read('hosted/index.html');

  assert.match(html, /Compare these words in Astrolabe/);
  assert.match(html, /Ready for an assignment/);
  assert.match(html, /program-frame/);
  assert.match(html, /type="module" src="app\.mjs"/);
  assert.doesNotMatch(html, /ASTR-DEMO|Demo program|Preview only/);
  assert.equal(fs.existsSync(hostedDir), true);
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
  for (const name of ['client.mjs', 'protocol.mjs', 'transport.mjs', 'vault.mjs']) {
    assert.deepEqual(
      fs.readFileSync(path.join(hostedDir, 'runtime', name)),
      fs.readFileSync(path.join(sharedRuntimeDir, name)),
      `${name} is synchronized`
    );
  }
});
