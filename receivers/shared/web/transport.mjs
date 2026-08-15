import { ProtocolError } from "./protocol.mjs";

function normalizedUrl(url) {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:" || parsed.username || parsed.password || parsed.hash) {
    throw new ProtocolError("invalid_origin", "Receiver transport requires a credential-free HTTPS URL");
  }
  return parsed.href;
}

function contentLength(xhr) {
  const value = xhr.getResponseHeader("Content-Length");
  if (value == null || value === "") return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : null;
}

function xhrRequest({ method, url, body, headers, maximumBytes, responseType, timeoutMs }) {
  const requestedUrl = normalizedUrl(url);
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    let boundFailure = null;
    xhr.open(method, requestedUrl, true);
    xhr.responseType = responseType;
    xhr.timeout = timeoutMs;
    xhr.withCredentials = false;
    for (const [name, value] of Object.entries(headers || {})) xhr.setRequestHeader(name, value);
    xhr.onprogress = (event) => {
      if (event.loaded > maximumBytes) {
        boundFailure = new ProtocolError("bound_exceeded", "Coordinator response exceeded its byte bound");
        xhr.abort();
      }
    };
    xhr.onload = () => {
      const declared = contentLength(xhr);
      if (declared != null && declared > maximumBytes) {
        reject(new ProtocolError("bound_exceeded", "Coordinator Content-Length exceeded its byte bound"));
        return;
      }
      if (xhr.responseURL && normalizedUrl(xhr.responseURL) !== requestedUrl) {
        reject(new ProtocolError("redirect_refused", "Receiver routes do not follow redirects"));
        return;
      }
      const received = responseType === "arraybuffer"
        ? (xhr.response ? xhr.response.byteLength : 0)
        : new TextEncoder().encode(xhr.responseText || "").length;
      if (received > maximumBytes) {
        reject(new ProtocolError("bound_exceeded", "Coordinator response exceeded its byte bound"));
        return;
      }
      resolve({
        status: xhr.status,
        body: responseType === "arraybuffer" ? xhr.response : xhr.responseText,
        contentType: xhr.getResponseHeader("Content-Type") || "",
        nextChallenge: xhr.getResponseHeader("X-Astrolabe-Next-Challenge"),
      });
    };
    xhr.onerror = () => reject(new ProtocolError("network", "Coordinator request failed"));
    xhr.ontimeout = () => reject(new ProtocolError("timeout", "Coordinator request timed out"));
    xhr.onabort = () => reject(boundFailure || new ProtocolError("aborted", "Coordinator request aborted"));
    xhr.send(body == null ? null : body);
  });
}

function decodeBase64(value) {
  const decoded = atob(value);
  const bytes = new Uint8Array(decoded.length);
  for (let index = 0; index < decoded.length; index += 1) bytes[index] = decoded.charCodeAt(index);
  return bytes;
}

let nativeRequestSequence = 0;
const nativeRequests = new Map();
globalThis.__astrolabeNativeTransportResolve = (requestId, response) => {
  const pending = nativeRequests.get(requestId);
  if (!pending) return;
  nativeRequests.delete(requestId);
  pending(response);
};

function nativeResponse(bridge, payload) {
  nativeRequestSequence += 1;
  const requestId = String(nativeRequestSequence);
  return new Promise((resolve, reject) => {
    nativeRequests.set(requestId, resolve);
    try {
      bridge.request(requestId, payload);
    } catch (error) {
      nativeRequests.delete(requestId);
      reject(error);
    }
  });
}

function nativeRequest({ method, url, body, headers, maximumBytes, responseType, timeoutMs }) {
  const bridge = globalThis.AstrolabeNativeTransport;
  if (!bridge || typeof bridge.request !== "function") return null;
  const payload = JSON.stringify({
    method,
    url: normalizedUrl(url),
    body,
    headers: headers || {},
    maximum_bytes: maximumBytes,
    timeout_ms: timeoutMs,
  });
  return nativeResponse(bridge, payload).then((rawResponse) => {
    let response;
    try {
      response = JSON.parse(rawResponse);
    } catch {
      throw new ProtocolError("native_transport", "Native receiver transport returned an invalid response");
    }
    if (!response || typeof response !== "object" || typeof response.error !== "undefined") {
      throw new ProtocolError("network", response?.error || "Native receiver transport failed");
    }
    const bytes = decodeBase64(response.body_base64);
    if (bytes.byteLength > maximumBytes) {
      throw new ProtocolError("bound_exceeded", "Native coordinator response exceeded its byte bound");
    }
    return {
      status: response.status,
      body: responseType === "arraybuffer" ? bytes.buffer : new TextDecoder().decode(bytes),
      contentType: response.content_type || "",
      nextChallenge: response.next_challenge || null,
    };
  });
}

function request(options) {
  return nativeRequest(options) || xhrRequest(options);
}

export async function boundedJson({ method, url, body = null, headers = {}, maximumBytes, timeoutMs = 30000 }) {
  const serialized = body == null ? null : JSON.stringify(body);
  const requestHeaders = { Accept: "application/json", ...headers };
  if (serialized != null) requestHeaders["Content-Type"] = "application/json; charset=utf-8";
  const response = await request({
    method,
    url,
    body: serialized,
    headers: requestHeaders,
    maximumBytes,
    responseType: "text",
    timeoutMs,
  });
  if (!/^(application\/json|application\/problem\+json)(;|$)/i.test(response.contentType)) {
    throw new ProtocolError("invalid_content_type", "Coordinator JSON route returned a different media type");
  }
  try {
    return { ...response, body: JSON.parse(response.body) };
  } catch {
    throw new ProtocolError("invalid_json", "Coordinator response was not valid JSON");
  }
}

export function boundedBytes({ method, url, headers = {}, maximumBytes, timeoutMs = 30000 }) {
  return request({
    method,
    url,
    body: null,
    headers: { Accept: "image/png,image/jpeg,image/webp", ...headers },
    maximumBytes,
    responseType: "arraybuffer",
    timeoutMs,
  });
}
