/**
 * Document identity, minted where the document is authored. A Body id is 16
 * CSPRNG bytes rendered as lowercase unpadded RFC 4648 base32 (26 chars) —
 * the same rendering `replica::BodyId` parses.
 */

const ALPHABET = 'abcdefghijklmnopqrstuvwxyz234567';

export function mintBodyId(): string {
  const raw = new Uint8Array(16);
  crypto.getRandomValues(raw);
  let bits = 0;
  let value = 0;
  let out = '';
  for (const byte of raw) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      out += ALPHABET[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits > 0) {
    out += ALPHABET[(value << (5 - bits)) & 31];
  }
  return out;
}
