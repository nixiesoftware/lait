import { PROTOCOL_MAJOR, ProtocolError } from "./protocol.mjs";

// Which coordinator this receiver was pointed at, and nothing about whether it
// may talk to one.
//
// PROTOCOL.md calls discovery, deep links, QR data and manual entry untrusted
// doorbells that carry at most an origin. This is that doorbell, which is why
// it is kept out of the credential vault rather than beside the proof key: the
// vault's contents are proven material, and an origin somebody typed is not.
// Nothing here grants anything. The confirmation ceremony is still what
// enrolls a receiver, and a wrong site code produces a coordinator that
// refuses rather than one that serves the wrong pixels.

const DATABASE = "astrolabe-display-provisioning-v1";
const STORE = "provisioning";
const SITE = "site";

// One DNS label: what a person can read off a card and enter with a remote.
const SITE_CODE = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/;

function requestResult(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error("IndexedDB request failed"));
  });
}

function transactionComplete(transaction) {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () => reject(transaction.error || new Error("IndexedDB transaction aborted"));
    transaction.onerror = () => reject(transaction.error || new Error("IndexedDB transaction failed"));
  });
}

export function normalizeSiteCode(entered) {
  return String(entered === null || entered === undefined ? "" : entered).trim().toLowerCase();
}

export function validSiteCode(code) {
  return typeof code === "string" && SITE_CODE.test(code);
}

function validParent(value) {
  const parent = String(value === null || value === undefined ? "" : value).trim().toLowerCase();
  if (!parent.includes(".") || parent.endsWith(".") || /^[0-9.]+$/.test(parent) || parent.includes(":")) {
    throw new ProtocolError(
      "unprovisionable_host",
      "This receiver must be served from a named domain to resolve a site",
    );
  }
  return parent;
}

/// The domain the coordinators of this deployment live under.
///
/// Derived from the host that served this receiver rather than compiled in,
/// minus the app's own label: the receiver app is itself one identity's
/// subdomain of the deployment root, and coordinators are its *siblings*,
/// never its children. `astrolabe.foundation.pub` serving the app means
/// `acme` resolves to `acme.foundation.pub`. That keeps a private or
/// self-hosted install working with no build of its own — the same freedom
/// `receiver-bootstrap.json` gives the native receivers — and it cannot
/// disagree with the Content-Security-Policy, which is the thing that
/// actually enforces it.
export function deploymentRoot(served) {
  const host = validParent(served);
  const labels = host.split(".");
  if (labels.length < 3) {
    throw new ProtocolError(
      "unprovisionable_host",
      "This receiver must be served one label below its deployment root",
    );
  }
  return validParent(labels.slice(1).join("."));
}

export function siteOrigin(code, root) {
  if (!validSiteCode(code)) {
    throw new ProtocolError("invalid_site", "A site code is up to 32 letters, digits and hyphens");
  }
  return `https://${code}.${validParent(root)}`;
}

export function webPkiBootstrap(origin) {
  return {
    protocol_major: PROTOCOL_MAJOR,
    trust: { kind: "web_pki_origin", origin },
    certificate_pem: null,
    rendezvous: null,
  };
}

async function openDatabase() {
  if (!globalThis.indexedDB) {
    throw new ProtocolError("secure_storage_unavailable", "IndexedDB is required");
  }
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(STORE)) database.createObjectStore(STORE);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error("Receiver provisioning could not open"));
    request.onblocked = () => reject(new Error("Receiver provisioning upgrade is blocked"));
  });
}

export class ProvisioningStore {
  constructor(database) {
    this.database = database;
  }

  static async open() {
    return new ProvisioningStore(await openDatabase());
  }

  /// The stored site, or `null` when this receiver has never been pointed at
  /// one. A record that does not re-derive to the same origin under the host
  /// serving this app is discarded rather than trusted: the deployment moved,
  /// and a stale origin would be refused by CSP anyway, at a layer with no
  /// state to show for it.
  async read(parent) {
    const transaction = this.database.transaction(STORE, "readonly");
    const stored = await requestResult(transaction.objectStore(STORE).get(SITE));
    await transactionComplete(transaction);
    if (!stored || stored.version !== 1 || !validSiteCode(stored.code)) return null;
    let origin;
    try {
      origin = siteOrigin(stored.code, parent);
    } catch {
      return null;
    }
    return origin === stored.origin ? { code: stored.code, origin } : null;
  }

  async save(code, parent) {
    const origin = siteOrigin(code, parent);
    const transaction = this.database.transaction(STORE, "readwrite");
    transaction.objectStore(STORE).put({ version: 1, code, origin }, SITE);
    await transactionComplete(transaction);
    return { code, origin };
  }

  async clear() {
    const transaction = this.database.transaction(STORE, "readwrite");
    transaction.objectStore(STORE).delete(SITE);
    await transactionComplete(transaction);
  }

  close() {
    this.database.close();
  }
}
