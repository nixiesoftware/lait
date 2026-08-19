# LG self-checklist transfer notes

- Product: **Astrolabe**, Nixie Solutions LLC.
- Hosted runtime: `https://nixiesoftware.com/astrolabe/display/`; fixed
  coordinator origin: `https://nixiesoftware.com`.
- Minimum engine: webOS 6.0 / Chromium 79. Web Crypto, IndexedDB CryptoKey
  cloning, fetch/XHR byte-bound behavior, image decode, process death, and
  storage-pressure recovery require physical-device evidence.
- No account login, fake code, generic browser, external script, arbitrary URL,
  Space/World catalog, or command path exists on the TV.
- Pairing requires matching words/fingerprint in Astrolabe and on-screen OK.
- Credential state is AES-GCM encrypted under a non-extractable IndexedDB key;
  unsupported secure primitives fail closed.
- Back, OK, and Info follow current LG conventions. Package-managed history is
  enabled by `disableBackHistoryAPI: false`.
- Native status remains visible above assigned pixels and includes transport,
  source truth, and delivery staleness.
- Every assigned image is verified by type, exact encoded length, SHA-256, and
  decoded dimensions before display. Staging is app-memory-only and bounded.
- Eligible live assignments use only the assignment-bound CMAF ticket and MSE
  stream issued by the coordinator; the package exposes no generic media URL.
- Network loss uses bounded retry. Verified content follows the assignment's
  stale action; no content is silently skipped or replaced.
- Revocation blanks on the next authenticated response; already observed or
  photographed pixels cannot be clawed back and are not claimed otherwise.
