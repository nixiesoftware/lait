# Adopt HPKE (RFC 9180) under `crypto::seal_to`

Status: **scoping**. Kernel change, independent of the correspondence plane.
Shippable well before any of CORR.

Local working note.

---

## 1. What is there now

`crates/mechanics/src/crypto.rs:208` is a libsodium-style anonymous sealed box:

```
seal_to(recipient: &DeviceId, msg) -> eph_x_pub(32) || nonce(12) || ciphertext
```

- ed25519 `DeviceId` → X25519 via Edwards→Montgomery (`ed_pk_to_x`), secret via
  SHA-512 clamp (`ed_seed_to_x`, libsodium `sk_to_curve25519`)
- ephemeral X25519 DH, key = `box_key(shared, eph_pub, recip_x)`
- ChaCha20-Poly1305 with a random 12-byte nonce

The construction is well-trodden and there is no reason to think it is broken.
This is not a break-fix.

## 2. Why change it

**The win is AAD, not standards hygiene.**

Today `box_key` binds the ephemeral key and the recipient into the KDF. It binds
**nothing else** — not the Space, not the epoch, not what the sealed blob is
*for*. A sealed envelope is contextless ciphertext whose meaning comes entirely
from where it happens to be filed.

Every current call site seals something whose context matters:

| Call site | Seals | Context that is currently unbound |
|---|---|---|
| `src/orbital/mechanics.rs:226` | the Space key to a device | space id, epoch |
| `crates/mechanics/src/custody.rs:302` | a DEK to a recipient | space, authority, ceremony, leaf |
| `crates/mechanics/src/ceremony.rs:2512` | a DKG package | ceremony, round |
| `crates/mechanics/src/ceremony.rs:2813` | a reshare subshare | ceremony, generation |
| (planned) mailbox DEK | mail to an actor's device | actor, mailbox |

RFC 9180 gives two binding points for free: `info` at context setup and `aad`
per seal. Bind `(domain, space, epoch/generation, recipient)` and misfiling a
sealed blob stops being a policy question and becomes a **decrypt failure**.

`custody.rs:22` already argues this exact principle for its own package format —
"The package binds itself to its context — space, authority, ceremony, principal
and leaf — so a restored share cannot be silently reopened against the wrong
space." The sealing primitive underneath it does not do the same, so custody
re-implements binding at a higher layer while the layer below stays context-free.
HPKE makes that binding uniform and cheap.

Secondary: HPKE is what MLS uses internally, so this pre-aligns the kernel with
whatever the correspondence boundary adopts (see `correspondence-specs.md`).

## 3. Proposed construction

RFC 9180, `mode_base` (anonymous sender — matches today's semantics exactly):

- KEM: DHKEM(X25519, HKDF-SHA256) — `0x0020`
- KDF: HKDF-SHA256 — `0x0001`
- AEAD: ChaCha20-Poly1305 — `0x0003`

Wire: `version(1) || enc(32) || ciphertext`

`info` carries the purpose domain per call site (`lait/seal/space-key/1`,
`lait/seal/custody-dek/1`, `lait/seal/ceremony-pkg/1`, …). `aad` carries the
caller's binding tuple.

**Keep the ed25519→X25519 conversion as-is.** It is orthogonal to this change
and is pinned by a real interop test —
`crates/comms/tests/it/identity_interop.rs:40` seals to a key derived from
iroh's encoding. That test must pass unchanged.

Candidate crates to evaluate: `hpke-rs` (Cryspen — the one OpenMLS builds on,
so it aligns with a future MLS decision) and `hpke` (rozbb). Both pull HKDF;
x25519-dalek and chacha20poly1305 are already direct dependencies.

## 4. The migration trap — read before writing code

**The sealed envelope is a durable format.** `ledger.rs:958`
`sealed_for(epoch, device)` returns stored bytes; sealed epoch keys persist for
the life of the Space. This falls under `docs/COMPATIBILITY.md` §2 "Durable
formats" and §4 "Wire generations", and needs the same treatment the attachment
migration window got (§143).

**And v2 cannot be self-describing in-band.** The obvious move — prefix a
version octet and sniff it on open — does not work here:

- v1 begins with `eph_x_pub[0]`, a **uniformly random** byte.
- Any version tag chosen for v2 therefore collides with ~1/256 of v1 envelopes.
- Length disambiguation fails too: v1 is `44 + |ct|`, v2 is `33 + |ct|`, and
  `|ct|` is caller-controlled, so the ranges overlap.

So **the version must ride the record, not the blob**: the ledger's sealed map
entry, the custody `KeySlot`, the ceremony package. That is a wider edit than
the crypto module and is the real cost of this change — budget for it up front
rather than discovering it at the first `open_sealed` that guesses wrong.

Migration shape:

1. Add an explicit version discriminant to each durable holder of a sealed blob.
2. Existing entries are v1 by absence.
3. `open_sealed` dispatches on the record's version; both paths stay live.
4. New seals write v2 only.
5. No rewrite of existing envelopes — v1 stays readable indefinitely, exactly as
   the attachment window did.

## 5. Scope

Six production call sites (`ceremony.rs` ×2, `custody.rs`, `authorization.rs`
re-export, `orbital/mechanics.rs`), plus the durable holders in §4.

Explicitly **not** in scope: signing (stays ed25519 + FROST), the actor plane,
the ACL, and MLS itself.

## 6. Tests

Per `docs/THREAT-MODEL.md:402`, every property needs a test at the enforcing
boundary:

- **v1 golden fixtures still open.** Freeze real v1 envelopes now, before any
  code moves.
- **RFC 9180 test vectors** for the primitive, so the ciphersuite is verified
  against the spec rather than against ourselves.
- **AAD mismatch fails to open** — the whole point of the change. Seal a Space
  key for `(space A, epoch 3)`, attempt to open it as `(space A, epoch 4)` and
  as `(space B, epoch 3)`; both must fail.
- **`identity_interop.rs` unchanged and passing** — the ed25519→X25519 edge is
  untouched.
- Round-trip and wrong-recipient tests already at `crypto.rs:473-503`, extended
  to v2.

## 7. Sequencing

1. Freeze v1 fixtures.
2. Add version discriminants to the durable holders (largest edit, no crypto).
3. Introduce v2 seal/open behind the existing signatures, dispatching on record
   version.
4. Thread `info`/`aad` through each call site with its binding tuple.
5. Flip new seals to v2.

Steps 1–2 are pure refactor and land safely on their own.
