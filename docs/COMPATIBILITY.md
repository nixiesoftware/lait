# Compatibility matrix

Every versioned surface in LAIT, what gates it, and what a bump costs. The point
of collecting them in one table is that they are *not* one version: a store can
be rewritten while the wire holds still, and a wire generation can move without
touching a byte on disk.

There is no legacy fallback anywhere in this table. An unsupported version is
refused, not interpreted.

## 1. How a version is enforced

Three mechanisms, and which one applies decides what a bump breaks.

**A leading version field.** Read first, checked before anything else in the
record is trusted. Applies to stores, manifests, descriptors, and canonical
records. A bump means an older reader refuses the file, and refusing is the
whole point.

**A hash domain.** The version is baked into a domain string
(`lait/manifest/2`), so a signature or content address computed under one
generation simply does not verify under another. There is no version check to
write — a mismatch is a verification failure. A bump therefore invalidates every
signature and every id derived under the old domain, which is why domains carry
the heaviest version numbers and move the least.

**An ALPN.** Negotiated during the QUIC handshake, before either peer speaks. A
bump means peers on different generations share no ALPN and never connect. There
is no half-speaking pair and no in-band fallback, so this is the most expensive
kind of bump and the one that needs feature bits to avoid.

## 2. Durable formats

| Surface | Constant | Value | Gate |
|---|---|---|---|
| Store marker | `replica::marker::STORE_VERSION` | 1 | leading field |
| Store manifest | `journal::STORE_FORMAT_VERSION` | 2 | leading field |
| Replica store meta | `replica::STORE_META_FORMAT_VERSION` | 2 | leading field |
| Manifest root | `replica::manifest::MANIFEST_FORMAT_VERSION` | 2 | leading field + `lait/manifest/2` |
| Content descriptor | `replica::content::CONTENT_FORMAT_VERSION` | 1 | leading field + `lait/content-id/1` |
| Causal artifacts | `fabric::causal::CAUSAL_FORMAT_VERSION` | 1 | leading field |
| Neighbour registry | `runtime::neighbors::REGISTRY_VERSION` | 2 | leading field |
| Custody package | `mechanics::custody::PACKAGE_VERSION` | 1 | leading field |
| Policy compiler | `mechanics::compile::COMPILER_VERSION` | 1 | leading field |
| Ledger semantics | `mechanics::ledger::LEDGER_SEMANTICS_VERSION` | 1 | leading field |

The marker and the store manifest version different things and move
independently: the marker identifies the *store layout* — what files exist and
where — while the manifest version identifies *what a commit records*. Replacing
the paged manifest with an authenticated index changed the second and not the
first.

Index nodes carry no version of their own. They are content-addressed, immutable
journal objects reachable only from a root recorded in a versioned manifest, so
the manifest that names a node is what decides how it is read. Giving a node its
own version would let a node and its root disagree.

## 3. Signed and addressed domains

A domain is versioned prose baked into a hash. These are the ones that carry a
generation above 1, because those are the ones that have already moved:

| Domain | Covers |
|---|---|
| `lait/manifest/2` | the signed Manifest root, plus `…/body-key` and `…/content-key` index keys |
| `lait/body-transaction/2` | the signed Body transaction envelope |
| `lait/coordinates/2` | Space coordinates |
| `lait/space/1/ceremony/2` | ceremony material and authority grants |

Everything else is at `/1`. A domain is never repurposed in place: changing what
a preimage means requires a new domain string, not a new interpretation of an
old one.

## 4. Wire generations

| ALPN | Plane | Status |
|---|---|---|
| `lait/contact/1` | Contact — authority, manifest nodes, Body payloads | implemented |
| `lait/neighbor-presence/1` | liveness probe | implemented |
| `lait/freight/1` | reliable exact-object request and response | shapes frozen, routing pending |
| `lait/session/1` | long-lived realtime session | shapes frozen, routing pending |

Gossip rides iroh's own ALPN inside `crates/comms` and is transport plumbing, not
a LAIT protocol generation.

`PROTOCOL.md` — "Delivery planes" — has the full contract, including the frozen
bounds and which of them are LAIT policy rather than observations of the pinned
transport.

**Feature bits, not generations.** A capability that an older peer can safely
ignore is advertised as a bit in the opening, where an absent field decodes to
zero exactly as an older build would have sent it. The ALPN moves only for a
change an old peer would *misinterpret* — a removed or repurposed field, or
changed semantics for an existing one. Adding a lane, a hint, or an optional
answer is a bit.

## 5. Local surfaces

| Surface | Constant | Value | Note |
|---|---|---|---|
| Local DTOs | `runtime::dto::DTO_PROTOCOL_VERSION` | 1 | loopback control plane and viewer |

DTOs are a local contract between the engine and its own clients. They are
versioned because a stale viewer bundle is a real situation, not because they
cross a trust boundary.

## 6. The pinned dependency

`iroh = 1.0.0-rc.1`. It is a release candidate, so behaviour that LAIT *observes*
rather than *chooses* is recorded separately under "Frozen bounds, and which
are ours" in `PROTOCOL.md`, and measured
by `crates/comms/tests/transport_capabilities.rs`. When the pin moves, those
observations are re-measured; the LAIT policy numbers beside them are unaffected.
