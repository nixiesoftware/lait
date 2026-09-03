// HTTPS pinned to exactly one certificate.
//
// The coordinator serves a self-signed certificate whose SAN names the address
// it had when it was minted, not necessarily the one it announces now (on this
// machine: SAN 10.7.23.17, origin 172.20.10.9). `NODE_EXTRA_CA_CERTS` would
// trust the issuer and then refuse the hostname. A native receiver pins the
// fingerprint and ignores the name, and so does this: `checkServerIdentity`
// accepts a leaf whose SHA-256 is the one the bootstrap carries and nothing
// else. No re-exec, no environment, no dependency.
//
// The shared receiver client speaks through `receivers/shared/web/transport.mjs`,
// which prefers a native bridge (`globalThis.AstrolabeNativeTransport`) over
// XMLHttpRequest — the seam the tvOS receiver uses. `installNativeBridge`
// fills that seam with this transport, so `client.mjs` is imported unmodified.

import https from "node:https";
import { X509Certificate } from "node:crypto";

const NEXT_CHALLENGE = "x-astrolabe-next-challenge";

export function pemFromDer(der) {
  const base64 = Buffer.from(der).toString("base64").replace(/(.{64})/g, "$1\n").trimEnd();
  return `-----BEGIN CERTIFICATE-----\n${base64}\n-----END CERTIFICATE-----\n`;
}

export function sha256OfPem(pem) {
  return new X509Certificate(pem).fingerprint256.replace(/:/g, "").toLowerCase();
}

export class PinnedTransport {
  constructor({ pem, sha256 }) {
    this.sha256 = sha256.toLowerCase();
    this.agent = new https.Agent({
      ca: pem,
      keepAlive: true,
      maxSockets: 8,
      rejectUnauthorized: true,
      checkServerIdentity: (_host, cert) => {
        const seen = (cert.fingerprint256 || "").replace(/:/g, "").toLowerCase();
        if (seen !== this.sha256) {
          const error = new Error(`coordinator certificate ${seen} is not the pinned ${this.sha256}`);
          error.code = "ERR_TLS_CERT_PIN";
          return error;
        }
        return undefined;
      },
    });
    this.stats = { requests: 0, refused: 0, failed: 0 };
    this.onResponse = null;
  }

  /**
   * One request; resolves `{status, body: Buffer, contentType, nextChallenge,
   * headers}`. Rejects on network failure, timeout, or a byte bound exceeded.
   */
  request({ method, url, body = null, headers = {}, maximumBytes = 64 * 1024 * 1024, timeoutMs = 30_000 }) {
    this.stats.requests += 1;
    const startedAt = performance.now();
    return new Promise((resolve, reject) => {
      const parsed = new URL(url);
      const request = https.request(
        parsed,
        { method, headers, agent: this.agent, timeout: timeoutMs },
        (response) => {
          const declared = Number(response.headers["content-length"]);
          if (Number.isFinite(declared) && declared > maximumBytes) {
            response.destroy();
            reject(new Error(`response declares ${declared} bytes over the ${maximumBytes} bound`));
            return;
          }
          const chunks = [];
          let received = 0;
          response.on("data", (chunk) => {
            received += chunk.length;
            if (received > maximumBytes) {
              response.destroy(new Error(`response exceeded the ${maximumBytes} byte bound`));
              return;
            }
            chunks.push(chunk);
          });
          response.on("error", (error) => {
            this.stats.failed += 1;
            reject(error);
          });
          response.on("end", () => {
            const result = {
              status: response.statusCode,
              body: Buffer.concat(chunks),
              contentType: response.headers["content-type"] || "",
              nextChallenge: response.headers[NEXT_CHALLENGE] || null,
              headers: response.headers,
              latencyMs: performance.now() - startedAt,
            };
            if (result.status < 200 || result.status >= 300) this.stats.refused += 1;
            if (this.onResponse) this.onResponse({ method, path: parsed.pathname, ...result });
            resolve(result);
          });
        },
      );
      request.on("timeout", () => request.destroy(new Error(`request timed out after ${timeoutMs} ms`)));
      request.on("error", (error) => {
        this.stats.failed += 1;
        reject(error);
      });
      if (body != null) request.write(body);
      request.end();
    });
  }
}

/** Route the shared receiver client's requests through a pinned transport. */
export function installNativeBridge(transport) {
  globalThis.AstrolabeNativeTransport = {
    request(requestId, payload) {
      const options = JSON.parse(payload);
      transport
        .request({
          method: options.method,
          url: options.url,
          body: options.body,
          headers: options.headers,
          maximumBytes: options.maximum_bytes,
          timeoutMs: options.timeout_ms,
        })
        .then((response) => ({
          status: response.status,
          body_base64: response.body.toString("base64"),
          content_type: response.contentType,
          next_challenge: response.nextChallenge,
        }))
        .catch((error) => ({ error: String(error && error.message ? error.message : error) }))
        .then((response) => globalThis.__astrolabeNativeTransportResolve(requestId, JSON.stringify(response)));
    },
  };
}
