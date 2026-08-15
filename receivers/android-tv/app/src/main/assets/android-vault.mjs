function bridge() {
  if (!globalThis.AstrolabeSecureStore) {
    throw new Error("Android Keystore bridge is unavailable; receiver credentials cannot be protected");
  }
  return globalThis.AstrolabeSecureStore;
}

export class AndroidCredentialVault {
  static async open() {
    bridge();
    return new AndroidCredentialVault();
  }

  async load() {
    const raw = bridge().load();
    return raw === "" ? null : JSON.parse(raw);
  }

  async save(state) {
    bridge().save(JSON.stringify(state));
  }

  async clear() {
    bridge().clear();
  }

  close() {}
}
