import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (relative) => readFile(path.join(root, relative), "utf8");

test("project is a native tvOS application with a production bundle ID", async () => {
  const project = await read("project.yml");
  assert.match(project, /platform: tvOS/);
  assert.match(project, /com\.nixiesoftware\.astrolabe/);
  assert.match(project, /deploymentTarget:[\s\S]+tvOS: "17\.0"/);
});

test("receiver uses Apple security boundaries and bounded transport", async () => {
  const protocol = await read("AstrolabeReceiver/DisplayProtocol.swift");
  const vault = await read("AstrolabeReceiver/KeychainVault.swift");
  const transport = await read("AstrolabeReceiver/BoundedTransport.swift");
  assert.match(protocol, /SecRandomCopyBytes/);
  assert.match(protocol, /HMAC<SHA256>/);
  assert.match(vault, /kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly/);
  assert.match(transport, /URLSessionConfiguration\.ephemeral/);
  assert.match(transport, /for try await byte in stream/);
  assert.match(transport, /completionHandler\(nil\)/);
});

test("receiver speaks only the closed authenticated Display protocol", async () => {
  const source = await read("AstrolabeReceiver/ReceiverCoordinator.swift");
  assert.match(source, /https:\/\/nixiesoftware\.com/);
  assert.match(source, /X-Astrolabe-Next-Challenge/);
  assert.match(source, /\/head\/v1\/program\/changes/);
  assert.match(source, /StrictJSON\.program/);
  assert.match(source, /DisplayProtocolV1\.verifyProgram/);
  assert.doesNotMatch(source, /WKWebView|\/world|\/space|demo/i);
});

test("XCTest suite independently consumes the frozen fixture", async () => {
  const tests = await read("AstrolabeReceiverTests/DisplayProtocolTests.swift");
  assert.match(tests, /confirmationPhrase/);
  assert.match(tests, /pairingCompleteTag/);
  assert.match(tests, /requestTag/);
  assert.match(tests, /verifyProgram/);
  assert.match(tests, /UnknownProgramFieldIsRefused/);
});
