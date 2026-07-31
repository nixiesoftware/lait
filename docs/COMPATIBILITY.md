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
| World implementation descriptor | `runtime::implementation::DESCRIPTOR_VERSION_SECTIONED` | 2 | leading field |

The marker and the store manifest version different things and move
independently: the marker identifies the *store layout* — what files exist and
where — while the manifest version identifies *what a commit records*. Replacing
the paged manifest with an authenticated index changed the second and not the
first.

The descriptor is the only row whose version is chosen by the record's content
rather than by the build that wrote it. A descriptor emits 1 when it declares no
sections and 2 when it declares any, so the set of implementation ids this bump
moves is the set of Worlds that declare a section — today, none. That is the
whole reason the section table exists: adding a section kind must not move the
id of a World that declares nothing of that kind, which two more fields in a
fixed-order tuple would have done to every id in the system.

The hash domain deliberately did not move with it. Ids stay derived under
`lait.world-implementation.v1` even at encoding version 2, because the domain is
what every shipped activation record was derived under and moving it invalidates
all of them at once — the cost §1 records against the "hash domain" mechanism.
An encoding version and a domain generation are separate numbers here precisely
so that the cheap change can be made without paying for the expensive one.

Within the table, an unknown section tag is refused rather than skipped. A
skipped section would make the implementation id a digest over bytes the build
did not interpret, which is the one thing a reviewed trust identity may not be.
Forward tolerance is not what the tag buys; not moving unrelated ids is.

Every count in the record is a `u16`, so 65535 schemas, and 65535 entries per
section, is a limit of the format rather than a policy anyone tunes
(`runtime::implementation::MAX_ENCODABLE_ENTRIES`). A longer list has no
encoding, and `encode` refuses it instead of writing a count that describes
fewer entries than follow it — which would derive an id over bytes `decode`
rejects, an encoding that does not decode.

A Manifest root's acceptance rule tightened without its version moving, which
is worth stating plainly because the two usually move together. A root whose
Body entries declare `ContentRef`s is now refused unless every one of them
resolves — from the root's own content index, or from a descriptor the receiver
already holds. The encoding did not change: `content_index_root` and
`content_count` have been in the record since `lait/manifest/2`, and a root that
declares no content is accepted exactly as before. What changed is that
advertising a declaration you cannot back is no longer a valid advertisement.

No peer in the field is affected. Nothing produces a content declaration yet —
the Issues attachment cutover is what will — so the set of roots this newly
refuses is empty today, and that is precisely why the rule lands now rather than
after there is state to break.

Index nodes carry no version of their own. They are content-addressed, immutable
journal objects reachable only from a root recorded in a versioned manifest, so
the manifest that names a node is what decides how it is read. Giving a node its
own version would let a node and its root disagree.

The content cache is the other unversioned thing on disk, and it is unversioned
for a different reason. `content-cache/` appeared beside the journal when the
content plane opened at activation, and it moved neither the marker nor the
manifest version — correctly, because the marker identifies the layout the
*record* is read from and the cache holds nothing the record names
(`DATA-CONTRACT.md` §13 says what does live there). Every entry is filed under
its own content address and re-checked on read, so an entry this build cannot
make sense of is dropped and refetched rather than interpreted. A format that can
always be discarded needs no version to refuse an old one, and giving it one
would imply a store could be made unreadable by its cache.

### The attachment migration window

Two record shapes coexist in the `attachments` map of an issue Body, and the
window has no end date because it needs none.

| Shape | Written by | Read by |
|---|---|---|
| `{…, data_b64}` | builds before the content cutover | every build, permanently |
| `{…, content}` | this build onward | this build onward |

`AttachmentMeta` carries no `deny_unknown_fields` and defaults every field but
`id` and `name`, which is why both shapes decode through one type and why the
new field could be added without a version moving. The *write* path is a clean
break — nothing emits `data_b64` any more, and the encoder that produced it is
deleted in both the engine and the viewer.

The read path is not a deprecation. It is the permanent cost of having once
written files into Bodies, and it is cheap: the old shape was bounded at 256 KiB
by 8 attachments, so the worst legacy Body is 2 MiB and needs no streaming
reader. `MAX_LEGACY_ATTACHMENT_BYTES` records that bound as what it now is — the
shape of what was already written, not a policy anyone tunes.

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
| `lait/freight/1` | Freight — reliable exact-object request and response | implemented and **mounted** |
| `lait/session/1` | Live — transient collaboration and reliable signals | implemented and **mounted**; the transient core and the signal wire are in, the session's own dial-out is not |

**What "implemented" means in this column.** It means a dial on that ALPN
reaches a handler that reads the opening, judges it, and answers — not that
every message the generation defines has a sender. `lait/freight/1` routes now:
an admitted peer's availability question and ranged chunk request are served
from committed descriptors and validated proof sidecars, behind one coarse
refusal. The fetching half is the same generation seen from the other end and
needs no wire change to arrive, which is why the status is recorded against the
generation rather than against a feature — the frames, bounds, and refusal
vocabulary a `1` peer must implement are already fixed by what serves them.

Advertising an ALPN is not the same as serving it, and until this branch both of
these were advertised and unserved: the endpoint registered them, a peer that
dialled completed a handshake, and the hub turned the opening away because no
driver owned the plane. `Orbit::activate` mounts both now.

This column tracks the service and not the registration. A successful handshake
was never evidence a plane was live, and for two releases it was evidence of the
opposite.

Each plane owns its own inbound queue. One queue for both would hand two drivers
strictly alternating connections, each refusing half of what it was given as a
foreign ALPN — so the split is what makes a second plane possible at all, rather
than a tidiness.

One detail reads like a contradiction and is not. The opening still carries
`protocol_version` and the refusal vocabulary still has `UnsupportedVersion`,
even though the ALPN already stops a mismatched pair from connecting at all. It
is belt and braces, and it costs a comparison. It is also the *only* refusal
that names its reason — every other one is deliberately coarse — so a peer
reading a bare refusal must not read it as a version problem.

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

**How a bit is negotiated, now that bits are negotiated.** Admission intersects
what the peer offered with what this build implements, and that intersection —
not the peer's offer — is what the accept carries. Three consequences, none of
them an error path:

- an absent or zero `features` is a peer that offers nothing, not a peer that is
  malformed. Zero is exactly what an older build sends, and being able to decode
  to it is what makes the field additive at all;
- a bit this build does not know is dropped from the intersection and never
  echoed, so a peer cannot discover what a later generation calls something by
  watching what we reflect back;
- a capability the peer did not offer is never used against it. A bit is
  permission to speak, not a hint, and the intersection is computed once at
  admission so no later decision re-guesses it.

Requested lanes are intersected the same way and for the same reason: a lane is
granted only when this build implements it *and* the peer asked for it. A lane
this build does not implement is dropped from the intersection, not treated as a
malformed opening — refusing the whole connection over one unknown lane byte
would make lanes an ALPN-level change, which is exactly what the additive rule
exists to avoid. An opening that asks for lanes and would be granted none is
refused rather than admitted lane-less, because a connection neither side can
use is a slot held open for nothing.

## 5. Local surfaces

| Surface | Constant | Value | Note |
|---|---|---|---|
| Local DTOs | `runtime::dto::DTO_PROTOCOL_VERSION` | 1 | loopback control plane and viewer |

| Local control channel | `control::CONTROL_PROTOCOL_VERSION` | 7 | daemon socket; `MIN_SUPPORTED_CONTROL_PROTOCOL` is also 7, so the mixed-version window is currently empty |

DTOs are a local contract between the engine and its own clients. They are
versioned because a stale viewer bundle is a real situation, not because they
cross a trust boundary.

v7 moved the minimum with the version rather than leaving a window, and that is
not the usual caution. A v6 process would accept a content request's header line
and then read the raw body as a second request — so an attached SpaceBridge on
v6 does not fail the call, it desynchronises the channel the first time anyone
uploads, and every later request on that connection reads someone else's bytes.
A window whose only content is that outcome is not a window worth having.

## 6. The pinned dependency

`iroh = 1.0.0-rc.1`. It is a release candidate, so behaviour that LAIT *observes*
rather than *chooses* is recorded separately under "Frozen bounds, and which
are ours" in `PROTOCOL.md`, and measured
by `crates/comms/tests/transport_capabilities.rs`. When the pin moves, those
observations are re-measured; the LAIT policy numbers beside them are unaffected.

Observed and chosen are not the only two answers a frozen number can have. §7
adds the third.

## 7. Upgrade posture — whose number is it?

A frozen number stays re-examinable only if the reason it holds that value sits
beside it. There are three reasons, and they age differently.

**lait policy.** Chosen by us and moved when we decide it should move:
`MAX_OPENING_BYTES`, `MAX_FLOW_READ_BYTES`, `MAX_LANES`, the refusal
vocabulary, the ALPN generations. A dependency bump does not touch these.

**Observed.** Measured from the pinned transport rather than chosen:
`max_datagram_size`, send-buffer space, reset semantics. §6 governs them — when
the pin moves they are re-measured, and `MAX_DATAGRAM_BYTES` is advisory rather
than authoritative because a measurement already came in under it.

**Host-derived.** Calibrated to the machine lait was built on — one developer
laptop, with a laptop's disk, memory, and uplink — rather than to anything the
protocol requires. This is the category that needed adding, because a laptop
default frozen into a shipped constant becomes, a year later, indistinguishable
from a ceiling a peer is entitled to rely on. Nothing on the wire reads any of
them: a peer meets one only as a refusal it already has to handle, which is
precisely why an operator may raise or lower it without speaking a different
protocol. `PROTOCOL.md` §12.2 lists the same ceilings from the peer's side.

| Constant | Value | Operator-configurable today |
|---|---|---|
| `runtime::budget::slots::MAX_ENDPOINT_CONNECTIONS` | 128 inbound | no — compiled in |
| `runtime::budget::slots::MAX_SPACE_CONNECTIONS` | 64 inbound | no — compiled in |
| `runtime::budget::slots::MAX_STAGED_BYTES` | 64 MiB per Space | no — compiled in |
| `runtime::lifecycle::ContentOptions::cache_quota_bytes` | 4 GiB | yes, per activation |

The cache quota is the shape the other three are headed for: a default on an
options struct the composition root fills in, not a constant. Until they get
there, this table is the whole defence — "128 inbound connections" is otherwise
a sentence somebody quotes back as a rule of the protocol.

`ContentOptions::max_content_len` (256 MiB) sits beside the quota and is *not*
host-derived. It is an operator lowering plan 13's maximum, so it may only ever
move down, and a peer that meets it has met this Station's policy rather than a
limit of the format.

Ceilings the S2–S5 blueprint names as host-derived but this build has not yet
minted — `MAX_ENDPOINT_MEMORY_INFLIGHT`, `SPACE_MEMORY_INFLIGHT`, and
`CONN_MEMORY_INFLIGHT` — join this table when they land.
