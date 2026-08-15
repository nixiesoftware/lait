const ALIASES = Object.freeze(["astrolabe_receiver_a", "astrolabe_receiver_b"]);

function isNotFound(error) {
  return error && error.name === "NotFoundError";
}

function keyManager() {
  if (!globalThis.tizen || !tizen.keymanager) {
    throw new Error("Tizen KeyManager is unavailable; receiver credentials cannot be protected");
  }
  return tizen.keymanager;
}

function readSlot(name) {
  try {
    const raw = keyManager().getData({ name });
    const record = JSON.parse(raw);
    if (record.format !== 1
      || !Number.isSafeInteger(record.generation)
      || record.generation < 1
      || !record.state
      || typeof record.state !== "object") {
      throw new Error(`Protected receiver state ${name} is malformed`);
    }
    return { name, generation: record.generation, state: record.state };
  } catch (error) {
    if (isNotFound(error)) return null;
    throw error;
  }
}

function removeSlot(name) {
  try {
    keyManager().removeData({ name });
  } catch (error) {
    if (!isNotFound(error)) throw error;
  }
}

function saveSlot(name, raw) {
  return new Promise((resolve, reject) => {
    keyManager().saveData(name, raw, null, resolve, reject);
  });
}

export class TizenCredentialVault {
  static async open() {
    keyManager();
    return new TizenCredentialVault();
  }

  current() {
    return ALIASES
      .map((name) => readSlot(name))
      .filter((slot) => slot !== null)
      .sort((left, right) => right.generation - left.generation)[0] || null;
  }

  async load() {
    const current = this.current();
    return current ? current.state : null;
  }

  async save(state) {
    const current = this.current();
    const generation = current ? current.generation + 1 : 1;
    const target = current && current.name === ALIASES[0] ? ALIASES[1] : ALIASES[0];

    // The prior generation remains readable until the new KeyManager entry is
    // durably accepted. A restart between commit and cleanup picks the larger
    // generation, so credential rotation never depends on browser storage.
    removeSlot(target);
    await saveSlot(target, JSON.stringify({ format: 1, generation, state }));
    if (current) removeSlot(current.name);
  }

  async clear() {
    for (const name of ALIASES) removeSlot(name);
  }

  close() {}
}
