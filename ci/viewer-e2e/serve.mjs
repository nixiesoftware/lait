// A tiny same-origin static server for the viewer e2e: serves the built viewer
// bundle plus the two fetched wasms (engine + runner) with correct content
// types. No CSP — this proves FUNCTION (the shipped viewer running on the in-tab
// engine); production CSP is the daemon shell's concern (src/serve/shell.rs),
// not this test server.
//
//   node serve.mjs <port> <webDir> <engineWasm> <runnerWasm>
//
// `/porthole_bg.wasm` and `/lait_issues_runner.wasm` are served from the
// explicit paths (they are gitignored / built elsewhere); everything else is
// read from <webDir>. SPA fallback: an unknown path serves index.html so the
// fragment-routed join URL loads the app.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join, normalize } from "node:path";

const [, , portArg, webDir, engineWasm, runnerWasm] = process.argv;
const port = Number(portArg);

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".wasm": "application/wasm",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
};

const typeFor = (path) => {
  const dot = path.lastIndexOf(".");
  return (dot >= 0 && TYPES[path.slice(dot)]) || "application/octet-stream";
};

const fixed = {
  "/porthole_bg.wasm": engineWasm,
  "/lait_issues_runner.wasm": runnerWasm,
};

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, "http://localhost");
    let file = fixed[url.pathname];
    if (!file) {
      // Contain the path to webDir; SPA-fallback anything without an extension.
      const rel = normalize(url.pathname).replace(/^(\.\.[/\\])+/, "");
      const hasExt = /\.[a-z0-9]+$/i.test(rel);
      file = hasExt ? join(webDir, rel) : join(webDir, "index.html");
    }
    const body = await readFile(file);
    res.writeHead(200, { "content-type": typeFor(file) });
    res.end(body);
  } catch {
    res.writeHead(404, { "content-type": "text/plain" });
    res.end("not found");
  }
});

server.listen(port, "127.0.0.1", () => {
  process.stdout.write(`serving on http://127.0.0.1:${port}\n`);
});
