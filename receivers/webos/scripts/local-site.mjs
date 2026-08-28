// A deployment on one machine, for driving the hosted receiver against a
// coordinator running here — in a browser, or in the LG simulator.
//
// The production shape is: the receiver app is served at
// `astrolabe.<root>`, a site label resolves to `<label>.<root>`, and a
// router splices that to the identity's coordinator. This is that shape,
// small: one HTTPS listener that serves `hosted/` for the app's own host
// and proxies every other label to one coordinator's LAN listener. The
// certificate comes from mkcert, which is what makes a browser on this
// machine trust the deployment; nothing here relaxes what the receiver
// checks, because the receiver never knew the deployment was real.
//
//   node scripts/local-site.mjs \
//     [--root localtest.me]                 # resolves to 127.0.0.1 publicly
//     [--port 443]                          # the site origin has no port
//     [--coordinator https://127.0.0.1:7443]
//     [--certs <dir with cert.pem + key.pem>]
//
// The receiver app is then at https://astrolabe.<root>/display/ and any site
// label — `lait`, `acme` — reaches the coordinator.

import { readFile, stat } from "node:fs/promises";
import https from "node:https";
import net from "node:net";
import path from "node:path";
import tls from "node:tls";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const hosted = path.resolve(here, "..", "hosted");

const options = {
  root: "localtest.me",
  port: 443,
  coordinator: "https://127.0.0.1:7443",
  certs: path.resolve(here, "..", "dist", "local-site"),
};
const argv = process.argv.slice(2);
for (let index = 0; index < argv.length; index += 2) {
  const key = argv[index].replace(/^--/, "");
  if (!(key in options)) {
    console.error(`unknown option --${key}`);
    process.exit(2);
  }
  options[key] = key === "port" ? Number(argv[index + 1]) : argv[index + 1];
}

const appHost = `astrolabe.${options.root}`;
const upstream = new URL(options.coordinator);
const types = {
  ".html": "text/html; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".png": "image/png",
  ".json": "application/json",
};

function hostOf(request) {
  return String(request.headers.host || "").split(":")[0].toLowerCase();
}

/// The shipped page, with the deployment root the policy names swapped
/// for this one. That is the one thing a deployment legitimately differs
/// in; everything else is served byte for byte.
async function servePage(response) {
  const page = await readFile(path.join(hosted, "index.html"), "utf8");
  const body = page.replaceAll("foundation.pub", options.root);
  response.writeHead(200, { "content-type": types[".html"], "cache-control": "no-store" });
  response.end(body);
}

async function serveHosted(request, response) {
  const url = new URL(request.url, `https://${appHost}`);
  if (!url.pathname.startsWith("/display/")) {
    response.writeHead(302, { location: "/display/" });
    response.end();
    return;
  }
  const relative = url.pathname.slice("/display/".length) || "index.html";
  if (relative === "index.html") return servePage(response);
  const file = path.resolve(hosted, relative);
  if (!file.startsWith(hosted + path.sep)) {
    response.writeHead(403);
    response.end();
    return;
  }
  try {
    await stat(file);
  } catch {
    response.writeHead(404);
    response.end();
    return;
  }
  response.writeHead(200, {
    "content-type": types[path.extname(file)] || "application/octet-stream",
    "cache-control": "no-store",
  });
  response.end(await readFile(file));
}

/// The splice to the coordinator. The daemon's certificate is self-signed
/// and pinned by native receivers; a browser receiver never sees it, so it
/// is not verified here either — this hop is the router's, on one machine.
function proxy(request, response) {
  const forwarded = https.request({
    host: upstream.hostname,
    port: upstream.port || 443,
    method: request.method,
    path: request.url,
    headers: { ...request.headers, host: upstream.host },
    rejectUnauthorized: false,
  }, (answer) => {
    response.writeHead(answer.statusCode, answer.headers);
    answer.pipe(response);
  });
  forwarded.on("error", (error) => {
    console.error(`coordinator unreachable: ${error.message}`);
    if (!response.headersSent) response.writeHead(502, { "content-type": "text/plain" });
    response.end("coordinator unreachable");
  });
  request.pipe(forwarded);
}

/// WebSocket upgrades — the live media session — spliced byte for byte
/// after the handshake, the way the router does it.
function proxyUpgrade(request, socket, head) {
  const target = tls.connect({
    host: upstream.hostname,
    port: Number(upstream.port || 443),
    rejectUnauthorized: false,
    servername: upstream.hostname,
  }, () => {
    const lines = [`${request.method} ${request.url} HTTP/1.1`];
    for (const [name, value] of Object.entries({ ...request.headers, host: upstream.host })) {
      lines.push(`${name}: ${value}`);
    }
    target.write(`${lines.join("\r\n")}\r\n\r\n`);
    if (head.length) target.write(head);
    socket.pipe(target).pipe(socket);
  });
  target.on("error", () => socket.destroy());
  socket.on("error", () => target.destroy());
}

async function main() {
  const [cert, key] = await Promise.all([
    readFile(path.join(options.certs, "cert.pem")),
    readFile(path.join(options.certs, "key.pem")),
  ]).catch(() => {
    console.error(`no certificate under ${options.certs}\n  mkcert -cert-file ${path.join(options.certs, "cert.pem")} -key-file ${path.join(options.certs, "key.pem")} "*.${options.root}" ${appHost}`);
    process.exit(2);
  });
  const server = https.createServer({ cert, key }, (request, response) => {
    if (hostOf(request) === appHost) {
      serveHosted(request, response).catch((error) => {
        console.error(error);
        if (!response.headersSent) response.writeHead(500);
        response.end();
      });
      return;
    }
    proxy(request, response);
  });
  server.on("upgrade", (request, socket, head) => {
    if (hostOf(request) === appHost) {
      socket.destroy();
      return;
    }
    proxyUpgrade(request, socket, head);
  });
  server.on("error", (error) => {
    console.error(`could not listen on ${options.port}: ${error.message}`);
    process.exit(1);
  });
  // Dual-stack: the root resolves to both loopbacks, and a browser may try
  // `::1` first.
  await new Promise((resolve) => server.listen(options.port, "::", resolve));
  const suffix = options.port === 443 ? "" : `:${options.port}`;
  console.log(`receiver app   https://${appHost}${suffix}/display/`);
  console.log(`any site       https://<site>.${options.root}${suffix}  ->  ${options.coordinator}`);
  if (options.port !== 443) {
    console.log("note: the receiver resolves a site to port 443; this listener is not on it");
  }
}

// Sanity: the root must resolve here, or nothing below it will.
net.setDefaultAutoSelectFamily?.(false);
main().catch((error) => {
  console.error(error);
  process.exit(1);
});
