import { bytesToHex, hexToBytes, ProtocolError } from "./protocol.mjs";

const DATABASE = "astrolabe-display-receiver-v1";
const STORE = "receiver";
const KEY_NAME = "wrapping-key";
const STATE_NAME = "encrypted-state";
const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

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

async function openDatabase() {
  if (!globalThis.indexedDB || !globalThis.crypto || !globalThis.crypto.subtle || !globalThis.crypto.getRandomValues) {
    throw new ProtocolError("secure_storage_unavailable", "IndexedDB and Web Crypto are required");
  }
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(STORE)) database.createObjectStore(STORE);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error("Receiver vault could not open"));
    request.onblocked = () => reject(new Error("Receiver vault upgrade is blocked"));
  });
}

async function createWrappingKey() {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

export class CredentialVault {
  constructor(database) {
    this.database = database;
  }

  static async open() {
    return new CredentialVault(await openDatabase());
  }

  async readPair() {
    const transaction = this.database.transaction(STORE, "readonly");
    const store = transaction.objectStore(STORE);
    const keyRequest = store.get(KEY_NAME);
    const stateRequest = store.get(STATE_NAME);
    const [key, state] = await Promise.all([requestResult(keyRequest), requestResult(stateRequest)]);
    await transactionComplete(transaction);
    return { key, state };
  }

  async load() {
    const { key, state } = await this.readPair();
    if (!key && !state) return null;
    if (!key || !state || state.version !== 1 || !state.iv || !state.ciphertext) {
      throw new ProtocolError("credential_corrupt", "Receiver credential record is incomplete");
    }
    try {
      const plaintext = await crypto.subtle.decrypt(
        { name: "AES-GCM", iv: hexToBytes(state.iv, 12) },
        key,
        state.ciphertext,
      );
      const decoded = JSON.parse(decoder.decode(plaintext));
      if (!decoded || decoded.version !== 1 || typeof decoded.mode !== "string") {
        throw new Error("unknown receiver credential version");
      }
      return decoded;
    } catch (error) {
      throw new ProtocolError("credential_corrupt", `Receiver credential could not be opened: ${error}`);
    }
  }

  async save(state) {
    const previous = await this.readPair();
    const key = previous.key || await createWrappingKey();
    const iv = new Uint8Array(12);
    crypto.getRandomValues(iv);
    const plaintext = encoder.encode(JSON.stringify({ ...state, version: 1 }));
    const ciphertext = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, plaintext);
    const transaction = this.database.transaction(STORE, "readwrite");
    const store = transaction.objectStore(STORE);
    store.put(key, KEY_NAME);
    store.put({ version: 1, iv: bytesToHex(iv), ciphertext }, STATE_NAME);
    await transactionComplete(transaction);
  }

  async clear() {
    const transaction = this.database.transaction(STORE, "readwrite");
    const store = transaction.objectStore(STORE);
    store.delete(KEY_NAME);
    store.delete(STATE_NAME);
    await transactionComplete(transaction);
  }

  close() {
    this.database.close();
  }
}
