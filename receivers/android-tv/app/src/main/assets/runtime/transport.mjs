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

export async function boundedJson({ method, url, body = null, headers = {}, maximumBytes, timeoutMs = 30000 }) {
  const serialized = body == null ? null : JSON.stringify(body);
  const requestHeaders = { Accept: "application/json", ...headers };
  if (serialized != null) requestHeaders["Content-Type"] = "application/json; charset=utf-8";
  const response = await xhrRequest({
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
  return xhrRequest({
    method,
    url,
    body: null,
    headers: { Accept: "image/png,image/jpeg,image/webp", ...headers },
    maximumBytes,
    responseType: "arraybuffer",
    timeoutMs,
  });
}
