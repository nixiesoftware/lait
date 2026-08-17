# Correspondence workstream — the mailbox primitive and its Body schemas

Status: **blueprint**. No production code. Every claim below cites the file and
line it was checked against, at the tree state of this branch.

Scope: sealing, the four Body schemas and their Op sequences, attachment
referencing, the sync-cursor trap, the retention-class declaration, and where
the thing registers. **Out of scope** (other workstreams): addressbook/naming,
the sponsorship relation, the correspondence crate's contractor trait and mail
protocols, credentials/passports.

Local working note. Tracked documentation remains the product truth.

---

## 0. The three findings that change the shape

Before the specification, the three things the code says that the scoping note
did not:

1. **The envelope already carries a key selector.** A protected Body envelope is
   `epoch_id[16] || nonce[12] || ct` (`crates/replica/src/protected.rs:9`), and
   `BodyKeySource::opening_key(epoch)` dispatches on that prefix
   (`crates/replica/src/protected.rs:163`). A mailbox DEK therefore does not need
   a new envelope, new cryptography, or a `Material` change — it needs to *be* a
   key epoch whose id is derived from the mailbox instead of minted by the Space.
   The only production line that must move is
   `crates/replica/src/replica.rs:1887`.

2. **Opaque retention is already the exact behaviour a foreign mailbox needs.**
   `crates/replica/src/replica.rs:796-798` — "A Body without local key material
   is retained opaquely." `docs/DATA-CONTRACT.md:181-188` makes it contract:
   retained byte-for-byte, counted against quotas, included in Manifest
   completeness, unavailable to Engine and World callbacks, forwardable. A
   member who is not the mailbox owner gets that for free, with no new plane.

3. **Attachments would leak even when the mailbox does not.** Chunk AAD binds
   `(space, content_nonce, chunk_index)` and nothing else
   (`crates/mechanics/src/crypto.rs:350-368`), and `StationContentKeys` forwards
   straight to the Space epoch key
   (`crates/runtime/src/content_host.rs:176-191`). Sealing the mailbox Body
   under a mailbox DEK while its attachments stay under the Space epoch is a
   confidentiality hole with no error attached to it. §3.4 closes it at a seam
   that already exists, with zero changes to `content_host.rs`.

---

## 1. Sealing

### 1.1 What is there now

`crates/replica/src/replica.rs:1884-1891` selects **one** sealing capability per
transaction, before the per-Body loop:

```rust
let sealing = match self.keys.as_ref().and_then(|k| k.sealing_key()) {
    Some(key) => Some(key),
    None if self.durable.is_some() => return Err(Failure::BodyKeyUnavailable),
    None => None,
};
```

and applies it to every touched Body at `replica.rs:1923-1934`. The trait it
calls has no selector at all (`crates/replica/src/protected.rs:154-164`):

```rust
pub trait BodyKeySource: Send + Sync {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey>;
    fn opening_key(&self, epoch: &[u8; BODY_EPOCH_ID_LEN]) -> Option<AuthorizedBodyKey>;
}
```

`AuthorizedBodyKey` is opaque, non-serializable, and carries `(epoch_id[16],
SpaceKey)` (`crates/mechanics/src/crypto.rs:272-298`). Nothing in it says the
epoch has to be a *Space* epoch — only the composition root's implementation
does.

### 1.2 The change, in four edits

**Edit 1 — `crates/replica/src/protected.rs:158`.** Widen the sealing selector,
with a defaulted method so no existing implementor breaks:

```rust
pub trait BodyKeySource: Send + Sync {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey>;

    /// The capability for sealing new material **of one Body**. Defaults to the
    /// Space-wide answer, which is correct for every schema whose readership is
    /// the Space. A schema whose readership is narrower — a mailbox — overrides.
    fn sealing_key_for(&self, _key: &BodyKey) -> Option<AuthorizedBodyKey> {
        self.sealing_key()
    }

    fn opening_key(&self, epoch: &[u8; BODY_EPOCH_ID_LEN]) -> Option<AuthorizedBodyKey>;
}
```

`opening_key` needs **no change**: it already takes the epoch read off the
envelope, and returning `None` for an epoch this device does not hold is already
the documented "opaque branch" (`protected.rs:163`).

Blast radius of the default: nine implementors compile unchanged —
`StaticBodyKeys` (`protected.rs:177`), `NoBodyKeys`
(`crates/runtime/src/lifecycle.rs:70`), and the seven test sources at
`crates/runtime/src/internal_tests/freight_two_node.rs:39`,
`freight_wire.rs:34`, `crates/replica/src/canonical_store_tests.rs:621`,
`crates/replica/src/replica/generation.rs:376`, and the four
`crates/runtime/tests/it/*` constructions.

**Edit 2 — `crates/replica/src/replica.rs:1884-1934`.** Move the selection into
the per-Body loop. The `durable && None => BodyKeyUnavailable` guard
(`replica.rs:1889`) moves with it, so a Body with no key still fails a durable
commit — it fails per Body rather than per transaction, which is strictly more
precise. This is the only production line-change in Replica.

**Edit 3 — the mailbox epoch id.** Deterministic, so any device that unwraps the
DEK can also name the epoch without a lookup table:

```rust
/// 16 bytes, in `crates/correspondence/src/mailbox.rs`.
pub fn mailbox_epoch_id(space: &SpaceId, actor: &ActorId, generation: u32) -> [u8; 16] {
    let digest = blake3::derive_key(
        "lait.correspondence.mailbox-epoch.v1",
        &framed(&[space.as_str().as_bytes(), actor.as_str().as_bytes(),
                  &generation.to_be_bytes()]),
    );
    let mut raw = [0u8; 16];
    raw.copy_from_slice(&digest[..16]);
    raw
}
```

Collision with a Space epoch id is a 2^-128 event, and the two id spaces are
resolved by the same `opening_key` lookup, so no namespace flag is needed.
`generation` increments on device removal — lazy re-key, matching
`docs/ARCHITECTURE.md:462-464`'s existing "lazy revocation cannot erase plaintext
already copied" posture.

**Edit 4 — the DEK-slot fan-out.** This is `crates/mechanics/src/custody.rs:17-20`
with *device-of-the-actor* substituted for *slot*, and it is close to a
line-for-line copy of `SlotSpec::RecoveryKey` at `custody.rs:301-309`:

```rust
// crates/correspondence/src/mailbox_custody.rs
pub struct MailboxKeySlot {
    pub recipient: DeviceId,
    /// Rides the RECORD, never the blob. See §1.4.
    pub seal_version: u8,
    /// The mailbox DEK, sealed to `recipient`.
    pub wrapped_dek: Vec<u8>,
}

pub struct MailboxKeyEnvelope {
    pub version: u16,
    pub space: SpaceId,
    pub actor: ActorId,
    pub generation: u32,
    pub epoch: [u8; 16],
    pub slots: Vec<MailboxKeySlot>,
}
```

Construction fans `crypto::seal_to` (`crates/mechanics/src/crypto.rs:208`) over
`actor::Directory::devices_of` (`crates/mechanics/src/actor.rs:389`):

```rust
for device in directory.devices_of(actor) {
    let wrapped_dek = crypto::seal_to(&device, dek.as_slice())?
        .ok_or(Failure::UnsealableRecipient)?;
    slots.push(MailboxKeySlot { recipient: device, seal_version: 1, wrapped_dek });
}
```

Unwrap is `crypto::open_sealed(seed, me, wrapped_dek)`
(`crates/mechanics/src/crypto.rs:233`) against the slot whose `recipient == me`,
exactly as `UnlockKey::unwrap` does at `custody.rs:347-356`. Adding a device
pushes one slot and re-encrypts nothing — the property `custody.rs:18-20` states.

### 1.3 Where the key envelope lives — and the bootstrap trap

The obvious home is a `keys` path inside the `mailbox` Body. **That does not
work**: the `mailbox` Body is sealed under the mailbox epoch, so opening the
slots would require the DEK the slots exist to deliver.

It also must not live in Mechanics. A mailbox is not Space authority and must
never enter an `AuthorityFrontier` (`docs/ARCHITECTURE.md:243-250`).

So it is a **fourth Body**, `mailbox-keys`, sealed under the ordinary Space
epoch and holding only `MailboxKeyEnvelope`. Every member can read it; it is
wrapped DEKs and nothing else, so what a non-owner learns is *which devices can
open this mailbox* — which the actor plane already tells them
(`crates/mechanics/src/actor.rs:389` is a public directory). The privacy claim
is unchanged and the bootstrap resolves.

Consequence: `sealing_key_for` returns the mailbox capability for `message`,
`thread`, and `mailbox` Bodies, and falls through to the Space capability for
`mailbox-keys`. A World-schema check, not a Body-id check.

### 1.4 Non-conflict with the HPKE workstream

`docs/plans/hpke-sealing.md:96-101` is emphatic: the version **must ride the
record, not the blob**, because a v1 envelope opens with `eph_x_pub[0]`, a
uniformly random byte, so any in-band tag collides with ~1/256 of v1 blobs and
the lengths overlap. `MailboxKeySlot.seal_version` is that discriminant, present
from the first commit, so the mailbox contributes **no v1 migration debt** and
needs no compatibility window of its own.

`hpke-sealing.md:43` already reserves the row: *"(planned) mailbox DEK | mail to
an actor's device | actor, mailbox"*. This design supplies exactly that binding:

- `info = b"lait/seal/mailbox-dek/1"`
- `aad = framed(space_id, actor_id, generation.to_be_bytes(), epoch, recipient_device)`

Either landing order works and neither blocks:

- **HPKE first** — `MailboxKeySlot` is written with `seal_version: 2` and v1 is
  never emitted for mail.
- **Mailbox first** — slots are written `seal_version: 1`; the HPKE workstream
  adds a `2` arm to the mailbox unwrap dispatch, the same edit it already owes
  `KeySlot::RecoveryKey` (`hpke-sealing.md:98-107`, step 1: "Add an explicit
  version discriminant to each durable holder").

The one hard constraint on the other workstream: **do not put the version inside
`wrapped_dek`.** That is the trap `hpke-sealing.md:88-95` documents.

### 1.5 What a non-owner sees

Nothing is added. `crates/replica/src/replica.rs:2264-2268` reads the epoch,
asks `opening_key`, gets `None`, and falls into the opaque branch; on reopen,
`replica.rs:892-928` degrades the record to `interpreted: false` and files the
envelope in `raw_material`. `docs/DATA-CONTRACT.md:181-192` is the contract this
satisfies.

**One quota consequence to size.** Opaque Bodies are counted per World —
`opaque_usage(world)` at `crates/replica/src/replica.rs:781` against
`max_unknown_world_bytes` (1 GiB) and `max_unknown_world_bodies` (25,000) at
`replica.rs:2486-2488`. Those bounds were written for "a World this build does
not implement", and a mailbox is a World the build *does* implement whose *key*
is absent. As written, every foreign mailbox in the Space shares one 25,000-Body
budget under `com.lait.mailbox`, and adoption starts failing there. Either the
budget is scoped per `(world, epoch)` rather than per world, or the mailbox
plane accepts a hard cap of 25,000 foreign mailbox Bodies per replica. **Flag
for the crate-layout workstream — this is the interaction they will hit first.**

---

## 2. Body schemas

### 2.1 The declarations

In `crates/correspondence/src/contract.rs`, consumed by the World at
registration in the shape `products/issues/src/implementation.rs:208-237` uses:

```rust
pub const MAILBOX_WORLD: &str = "com.lait.mailbox";

vec![
    // Received mail never mutates. One canonical value, no collaborative
    // overhead, no merge semantics to get wrong.
    Schema {
        id: SchemaId::parse("message").expect("message schema id"),
        version: 1,
        encoding: EncodingId::parse("lait.mailbox.message.v1").expect("message encoding"),
        mutation: MutationModel::Atomic,
        readable_predecessors: vec![],
    },
    // Order, labels, triage, assignment. Two devices triaging offline converge.
    Schema {
        id: SchemaId::parse("thread").expect("thread schema id"),
        version: 1,
        encoding: EncodingId::parse("lait.mailbox.thread.v1").expect("thread encoding"),
        mutation: MutationModel::Collaborative(CollaborativeSchema {
            max_encoded_bytes: 4 * 1024 * 1024,
        }),
        readable_predecessors: vec![],
    },
    // Thread refs plus per-device sync cursors. See §5.
    Schema {
        id: SchemaId::parse("mailbox").expect("mailbox schema id"),
        version: 1,
        encoding: EncodingId::parse("lait.mailbox.mailbox.v1").expect("mailbox encoding"),
        mutation: MutationModel::Collaborative(CollaborativeSchema {
            max_encoded_bytes: 32 * 1024 * 1024,
        }),
        readable_predecessors: vec![],
    },
    // Space-epoch-sealed. Wrapped DEKs only. §1.3.
    Schema {
        id: SchemaId::parse("mailbox-keys").expect("mailbox keys schema id"),
        version: 1,
        encoding: EncodingId::parse("lait.mailbox.keys.v1").expect("mailbox keys encoding"),
        mutation: MutationModel::Atomic,
        readable_predecessors: vec![],
    },
]
```

Grammar checked against `crates/replica/src/ids.rs:79` / `:7`: schema and
encoding ids are 1–63 lowercase ASCII `[a-z0-9][a-z0-9._-]*`, so the hyphen in
`mailbox-keys` and the dots in the encodings are legal. `com.lait.mailbox` is a
legal `WorldId` under `ids.rs:36-61` (two-plus labels, 3–63 bytes, no
underscores).

`readable_predecessors` is empty on all four: v1 has no predecessor, and
`crates/replica/src/algebra.rs:52-56` requires entries to be strictly older and
distinct.

**On `message` and `MAX_BODY_BYTES`.** `Op::ReplaceAtomic` is the one op
`translate` does **not** bound by `MAX_VALUE_BYTES`
(`crates/replica/src/replica.rs:3611-3620` — the `value_ok` closure is applied
to `RegisterSet`, `MapSet`, `ListInsert`, `SetAdd`, and not to `ReplaceAtomic`).
So an atomic message Body is bounded only by `MAX_PROTECTED_PLAINTEXT` = 64 MiB
− 44 bytes (`crates/replica/src/protected.rs:22-29`). A fat HTML message fits
with room. This is the property that makes `Atomic` the right choice and it is
worth an executable test, because it is an *absence* in `translate` and an
absence is easy to un-write.

**On `Op::Create` and Atomic Bodies.** `crates/runtime/src/session.rs:702-707`:

```rust
Op::ReplaceAtomic { .. } => !collaborative,
Op::Create => collaborative,
```

`Op::Create` on an Atomic schema is a `ContractViolation`. A `message` Body is
created by a `BodyDeclaration` plus its first `ReplaceAtomic` — never by
`Create`. The Op sequences below reflect that; getting it backwards is a
compile-clean runtime rejection.

### 2.2 Deterministic Body ids

`BodyId` is normally CSPRNG-minted and World code "cannot choose the randomness"
(`crates/replica/src/ids.rs:141-144`), but `BodyId::from_bytes` admits
deterministic derivation and `products/issues/src/contract.rs:380-426` is the
established precedent (`catalog_body_id`, `issue_body_id`, `named_body_id`).
Same shape:

```rust
pub fn mailbox_body_id(space: &SpaceId, actor: &ActorId) -> BodyId;       // "lait.mailbox.mailbox.v1"
pub fn mailbox_keys_body_id(space: &SpaceId, actor: &ActorId) -> BodyId;  // "lait.mailbox.keys.v1"
pub fn thread_body_id(space: &SpaceId, actor: &ActorId, thread: &str) -> BodyId;
pub fn message_body_id(space: &SpaceId, actor: &ActorId, ingest: &str) -> BodyId;
```

**`message_body_id` must not be derived from the RFC5322 `Message-ID`.** A Body
id is unsealed Manifest-visible state; deriving it from a plaintext header would
make it an equality oracle over message identity across mailboxes and across
Spaces — precisely what `crates/replica/src/content.rs:16` and
`crates/replica/src/body.rs:39-41` refuse to build ("commits to the ciphertext,
never the plaintext, so it is not an equality oracle"). Derive from a per-ingest
ULID instead, and keep `Message-ID` *inside* the sealed record.

Import idempotency (`correspondence-mailbox.md:234`, "idempotent on re-run, keyed
on `Message-ID`") is then a lookup inside the mailbox Body, which is itself
mailbox-epoch-sealed, so a plain digest is sufficient there:

```rust
Op::MapSet { path: "seen", key: hex(blake3(b"lait/mail-seen/1" || message_id)), value: vec![] }
```

If a `thread` Body is ever shared across mailboxes, that digest must become
keyed under the mailbox DEK — note it now, because the change is invisible later.

### 2.3 Op sequences

All sequences use the LAIT algebra at `crates/replica/src/body.rs:102-164` and
respect `MAX_OPS_PER_TRANSACTION = 4096`
(`crates/replica/src/algebra.rs:69`).

**A. Deliver one inbound message into mailbox `M`, thread `T`.** One transaction,
eight ops:

```rust
// declarations
BodyDeclaration { key: message_key, schema: "message", schema_version: 1 }
BodyDeclaration { key: thread_key,  schema: "thread",  schema_version: 1 }   // first time only

// the message — Atomic: no Create, ReplaceAtomic creates and fills it
(message_key, Op::ReplaceAtomic { value: postcard(MessageRecord) })

// the thread
(thread_key, Op::Create)                                                     // first time only
(thread_key, Op::ListInsert  { path: "order", index: n, value: message_body_id.as_bytes().to_vec() })
(thread_key, Op::MapSet      { path: "triage", key: "state".into(), value: b"unread".to_vec() })
(thread_key, Op::SetAdd      { path: "participants", value: from_actor_or_address_digest })
(thread_key, Op::RegisterSet { path: "last_at", value: received_millis.to_be_bytes().to_vec() })

// the mailbox
(mailbox_key, Op::SetAdd     { path: "threads", value: thread_body_id.as_bytes().to_vec() })
(mailbox_key, Op::MapSet     { path: "seen", key: hex_digest, value: vec![] })
(mailbox_key, Op::CounterAdd { path: "unread", delta: 1 })
```

Plus, on the `Effect`, `content_refs: { message_key => parts.iter().map(|p| p.content).collect() }`
— mandatory, see §3.2.

**B. Triage — archive a thread.**

```rust
(thread_key,  Op::MapSet      { path: "triage", key: "state".into(), value: b"archived".to_vec() })
(thread_key,  Op::SetRemove   { path: "labels", value: b"inbox".to_vec() })
(mailbox_key, Op::CounterAdd  { path: "unread", delta: -1 })
```

**Conformance warning on labels.** `crates/replica/src/algebra.rs:30` — "Sets are
add-wins; a concurrent add and remove of the same value keeps it." So a
`SetRemove "inbox"` on the laptop concurrent with any `SetAdd "inbox"` on the
phone (a re-delivery, a rule firing, a second reply landing) loses, and the
thread silently reappears in the inbox. `docs/THREAT-MODEL.md:240-243` is exactly
this class: "CRDT convergence is not authorization and is not automatically a
correct product conflict policy."

The settled mapping says `SetAdd`/`SetRemove` for labels, so this blueprint
declares it that way. The amendment to consider: move labels to
`MapSet { path: "labels", key: <label>, value: b"1"|b"0" }` — a per-label LWW
cell, where removal is expressible and the loser is a whole label rather than the
user's intent. Keep `SetAdd`/`SetRemove` for `participants`, where add-wins is
the right answer. **Decision for the design owner, not taken here.**

**C. Assignment and snooze.**

```rust
(thread_key, Op::RegisterSet { path: "assignee",     value: actor_id.as_str().as_bytes().to_vec() })
(thread_key, Op::RegisterSet { path: "snooze_until", value: millis.to_be_bytes().to_vec() })
(thread_key, Op::RegisterClear { path: "snooze_until" })   // unsnooze
```

Registers are LWW by committed transaction order (`algebra.rs:25-27`), which is
the deliberate single-winner acceptance `docs/ARCHITECTURE.md:286-292` describes.
Correct here: two people snoozing the same thread do not both need to win.

**D. Reorder / thread splice.** `ListRemove` and `ListMove` name an element by
its **stable id, never by index** (`algebra.rs:15-21`), and those ids are
assigned by Engine at insert and echoed to the World only through a Projection
(`crates/runtime/src/world.rs:605-612`). So a World cannot remove a message it
inserted in the same transaction — it must read the collaborative view first:

```rust
let view = ctx.read_collaborative_body(&thread_key)?;      // world.rs:609
let element = view.list("order").find(|e| e.value == target)?.id;
(thread_key, Op::ListMove { path: "order", element, index: 0 })
```

Worth stating because "insert then reorder in one transaction" reads natural and
is not expressible.

**E. Retire a message.**

```rust
(message_key, Op::Tombstone)                                       // permitted on both models, session.rs:705
(thread_key,  Op::ListRemove { path: "order", element: stable_id })
```

### 2.4 Path grammar check

Every path used above — `order`, `triage`, `labels`, `participants`, `last_at`,
`assignee`, `snooze_until`, `threads`, `seen`, `unread`, `cursors` — is one
segment of `[a-z0-9_]`, ≤ 64 bytes, and passes `algebra::valid_path`
(`crates/replica/src/algebra.rs:73-88`). Underscores are legal; hyphens are
**not** — `snooze-until` would be `Failure::PathInvalid` at `replica.rs:3609`.

---

## 3. Attachments

### 3.1 Attachments are not Bodies

They are content, per `crates/replica/src/content.rs:1-11`. The completeness
contract is the reason: a Replica is descriptor-complete but not byte-complete
(`docs/DATA-CONTRACT.md:195-199`), so a phone carries the ~128-byte descriptor
for a 40 MB deck and fetches 256 KiB chunks
(`content.rs:47`) only on open.

### 3.2 How a message references content

Two references, and both are required.

**Inside the Body** — the product metadata, exactly where `content.rs:9-11` says
it goes ("Filename, MIME type, caption, and disposition are product metadata and
live in a World Body; two names may reference one `ContentRef`"):

```rust
// crates/correspondence/src/record.rs — the postcard value of a `message` Body
pub struct MessageRecord {
    pub version: u8,
    pub ingest_id: String,          // ULID; the message_body_id input
    pub message_id: String,         // RFC5322 Message-ID, sealed, never a Body id input
    pub in_reply_to: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub body_text: String,
    pub body_html_sanitized: Option<String>,
    pub provenance: Provenance,     // ingress workstream owns the shape
    pub parts: Vec<AttachmentPart>,
}

pub struct AttachmentPart {
    pub content: ContentRef,        // replica::content::ContentRef — content.rs:88
    pub filename: String,           // as sent, unrepaired. See §3.3.
    pub mime: String,               // a stranger's claim. Never honoured. See §3.3.
    pub disposition: Disposition,   // Inline | Attachment
    pub content_id: Option<String>, // the `cid:` for an inline part
    pub mime_part_path: String,     // "1.2.3" — where it sat in the MIME tree
}
```

**On the Effect** — the declaration the substrate needs, because it may not
decode product bytes to find a `ContentRef`
(`crates/runtime/src/world.rs:450-459`). Without it, content reachability can
only grow and every attachment ever received becomes permanent disk.

The trap is recorded at `products/issues/src/implementation.rs:263-271`:
`content_refs` **replaces** what a Body declared, so an entry for a Body that
meant to say nothing erases its set. The map must be sparse — only a Body that
explicitly declares appears. A mailbox that re-commits a message Body for any
reason must re-declare its *complete* part set.

Two names to one `ContentRef` is the forwarded-attachment case and is supported
by construction: forwarding copies the `AttachmentPart` (possibly with a new
`filename`) into a new `MessageRecord` and declares the same ref.

### 3.3 What must not be relaxed for mail

- **Filename is repaired at both ends, refused at neither.**
  `docs/THREAT-MODEL.md:250-268` splits the rule by proposer: Issues *refuses* at
  intake because the proposer is a local actor with write authority, and
  *repairs* at save because the proposer is remote. Mail has **no local intake
  proposer** — the proposer at intake is a stranger — so refusal is not available
  at either end and `world_interface::destination`'s repair runs on both. This is
  an inversion of the Issues rule, not an application of it, and it is the single
  most likely place to copy the wrong half.
- **MIME is never honoured.** `docs/THREAT-MODEL.md` "Files on the local web
  surface": every content route serves `application/octet-stream` + `nosniff` +
  `Content-Security-Policy: sandbox; default-src 'none'` +
  `Content-Disposition: attachment`, regardless of stored MIME, because one
  origin holds the session credential. Mail must add **no** "known sender"
  exception. `AttachmentPart.mime` is display metadata only.
- **No `?token=` on a content route**, same section. A mail client that builds
  `<img src>` for an inline part inherits this unchanged.

### 3.4 Closing the attachment-key hole

`ContentPolicy.keys` is an `Arc<dyn ContentKeys>` supplied **per call**
(`crates/runtime/src/content_host.rs:145-152`), and `ingest` takes its capability
straight off it (`content_host.rs:232-235`). So the fix needs no change to
`content_host.rs` at all: the correspondence host constructs the policy with a
`MailboxContentKeys(mailbox_epoch_capability)` instead of
`StationContentKeys` (`content_host.rs:176-191`).

The descriptor then records the mailbox epoch in `ContentDescriptor.epoch`
(`content.rs:81`), and `opening_key(&descriptor.epoch)` at
`content_host.rs:343` gives a non-owner `None` — which the trait already
documents as "the content stays sealed — lazy revocation, not an error"
(`content_host.rs:162-165`). The chunk AAD binds `(space, content_nonce,
chunk_index)` and does not need the mailbox: the key already carries it.

Executable test for the enforcing boundary (`THREAT-MODEL.md:402`): ingest an
attachment under mailbox A's epoch, and prove a Station holding only the Space
epoch key gets `Invalid::Unopenable` on the chunk and `NotResident`-shaped
behaviour end to end.

### 3.5 What no-dedup costs — the numbers

`content.rs:16` and `content.rs:75-77`: `content_nonce` is random per ingest, so
two ingests of identical bytes produce different `ContentId`s and there is no
dedup. `crypto.rs:376-378` says the same for chunk nonces. Three axes, and only
two of them cost anything:

**Axis 1 — Body plane: multiplier 1, no cost.** Per-mailbox sealing already
forces one copy of a message per recipient mailbox. Plaintext-hash identity could
never have shared a Body across mailboxes without also sharing the DEK. The
no-dedup rule costs *nothing extra* here. This is worth stating plainly because
the scoping note's D3 implies otherwise.

**Axis 2 — content plane: multiplier N, but almost all of it is evictable.** One
5 MB deck to a 12-person team is 12 ingests → 12 `ContentId`s → 12 descriptors.

- Durable, replicated, non-evictable: a descriptor postcard-encodes to roughly
  **128 bytes** (`format_version` 1 + `space` ~30 + `nonce` 16 + `plaintext_len`
  8 + `chunk_plaintext_len` 4 + `chunk_count` 4 + `merkle_root` 32 + `epoch` 16,
  plus framing). 12 × 128 B = **1.5 KB**, against 128 B with dedup.
- Evictable, lease-held cache: 12 × 5 MB = **60 MB** if everyone opens it, against
  5 MB with dedup. But `docs/DATA-CONTRACT.md:238-241` — "Evicting an entry
  changes no root: the descriptor stays, the Replica is still
  descriptor-complete, and what was lost is refetchable."

So the honest statement of the cost: **the no-dedup multiplier is N on cache and
N on ~128 bytes of catalog, and 1 on everything durable that matters.** At team
scale that is not a reason to weaken §3.4 of the scoping note.

Sustained: a 12-person team, 50 attachments/day averaging 2 MB, five years —
2.19 TB of chunk-fetches versus 182 GB with dedup (12×, all evictable), and
**1.25 GB of redundant descriptors** (50 × 128 B × 11 × 365 × 5). That last
number is durable and, note, **unquotaed**: `Replica::usage`
(`crates/replica/src/replica.rs:760-778`) counts transaction bytes, receipts, and
Bodies — not committed content descriptors. Redundant descriptors grow durable
replicated state against no ceiling. **Flag for the crate-layout workstream.**

**Axis 3 — the wall that actually stops this.** `QuotaConfig`
(`crates/replica/src/replica.rs:244-267`) and, decisively, `clamped`
(`replica.rs:269-283`) — "lowering is allowed, raising is not":

| Bound | Protocol max |
|---|---|
| `max_body_bytes` | 64 MiB |
| `max_space_bytes` | 4 GiB |
| `max_space_bodies` | 100,000 |
| `max_unknown_world_bytes` | 1 GiB per World |
| `max_unknown_world_bodies` | 25,000 per World |

A 15-year Gmail archive is on the order of 200,000 messages / 15 GB. That is
**2× the protocol Body ceiling and 3.75× the protocol byte ceiling for the whole
Space** — shared with every issue, spec, baseline, thread, and every *other*
mailbox in it.

Moving attachments to the content plane fixes the byte axis and not the count
axis. At a realistic 6 KB header+text Body, 4 GiB / 6 KB ≈ 715,000 — so
`max_space_bytes` stops binding and **`max_space_bodies` = 100,000 becomes the
wall.** Without that move, a 75 KB average message hits the 4 GiB ceiling at
about **57,000 messages**.

The number to carry to the design owner: **a Space supports on the order of
100,000 archival messages, total, across all mailboxes, and only if attachments
go to content.** One decade-scale human mailbox does not fit. This is not a
tuning question — `clamped` makes it a protocol change under
`docs/COMPATIBILITY.md` §2/§4. It gates the `archival` class in §4, and the
`conversational` class exists partly because it does not have this problem.

---

## 4. The sync-cursor trap

Settled fix: `MapSet { path: "cursors", key: <DeviceId> }`. Verified against the
frozen algebra, four checks:

1. **Path.** `"cursors"` — one segment, 7 bytes, `[a-z0-9_]`. Passes
   `algebra::valid_path` (`crates/replica/src/algebra.rs:73-88`), reached from
   `translate` at `crates/replica/src/replica.rs:3644`.
2. **Key.** A `DeviceId` is exactly 64 lowercase hex characters — enforced at
   `crates/mechanics/src/crypto.rs:178-186` (`s.len() != 64` → reject). The
   ceiling is `MAX_MAP_KEY_BYTES = 256` (`algebra.rs:63`), checked at
   `replica.rs:3646`. 64 ≤ 256. ✓
3. **Value.** `SyncCursor { uid_validity: u32, uid_next: u32, jmap_state: String,
   at_millis: u64 }`. Bounded by `MAX_VALUE_BYTES = 64 KiB` (`algebra.rs:65`,
   applied at `replica.rs:3612` via `value_ok`). A JMAP state string is tens of
   bytes. ✓ — but the World must cap `jmap_state` at ~1 KiB in its own validator,
   because the string is **attacker-supplied by a foreign mail server** and 64 KiB
   of it would otherwise be replicated to every peer. This is an ingress bound
   inside the mailbox slice, not the ingress workstream's.
4. **Convergence.** `algebra.rs:25-27` — "map entries are last-writer-wins by the
   semantic transaction order Engine commits." LWW is scoped **per entry**, so two
   devices writing distinct keys never contend; each device reads only its own
   key. The union is the converged value, which is exactly right: every device is
   correct about itself and about nothing else. ✓

The trap is real and the fix holds. One addition the scoping note does not have:
**stale entries need reaping.** A removed device leaves its cursor forever. Reap
with `Op::MapRemove { path: "cursors", key: device }` for any key not in
`actor::Directory::devices_of(actor)` (`crates/mechanics/src/actor.rs:389`) —
driven by the same directory read that drives the DEK re-key in §1.2, so it is
one hook and not two.

Contrast with the wrong answer, for the record: `RegisterSet { path: "cursor" }`
is a single LWW cell (`algebra.rs:25`), so the phone's `UIDVALIDITY` overwrites
the laptop's and the laptop re-syncs from a cursor that was never its own. Both
writers correct, one durable answer, silent duplicate delivery.

---

## 5. Delivery / retention class

`docs/THREAT-MODEL.md:270` and following is decisive. Two constraints fall out of
it before any option is considered:

- **The class cannot be a Body field.** The transient class has no Body, so a
  Body field could never express it. And a per-Body retention value is
  peer-authored data — the same category as a display name, which
  `docs/DATA-CONTRACT.md` §12.1 says "is stored exactly as sent" and is a claim,
  not a property. A retention claim a sender chooses is one an attacker chooses.
- **The class cannot be a per-message runtime parameter**, for the same reason: on
  the ingress path the caller is a stranger.

### 5.1 The answer: a schema property, with transient not being a schema at all

**`transient`** is a `runtime::world::SignalSchema`
(`crates/runtime/src/world.rs:388-398`), declared in
`Descriptor::signal_schemas` (`world.rs:410-411`):

```rust
SignalSchema {
    name: SchemaId::parse("mail-arrived").expect("signal name"),
    max_payload_bytes: 256,                       // tighten MAX_SIGNAL_BYTES = 16 KiB
    demand: demand_mailbox_read(),
}
```

It rides `crates/runtime/src/signal.rs`, whose module doc states the contract —
"no journal entry, nothing replayed after a restart, and nothing that becomes
activity" (`signal.rs:6-12`) — and whose enforcement is structural: the module
may not name the Replica writer or the Observation ring, and
`tests/signal_is_not_durable.rs` parses the file and fails if it does
(`signal.rs:14-25`). `THREAT-MODEL.md:270`'s "A signed signal would be a durable
artefact by another name" is satisfied because **nothing new is built** — a
transient message is a signal that already exists. Nothing about a transient
message is signed, retained, forwardable, or evidence.

Follow the `IssueNudge` precedent (`products/issues/src/contract.rs:200-245`): the
signal **names durable material and never carries it**, or carries nothing at
all. A transient chat line carries its own payload, under 256 bytes, and is gone.

**`conversational`** and **`archival`** are both `body::Schema` entries in
`Descriptor::schemas`, differing by a declared retention:

```rust
// crates/replica/src/body.rs — added beside MutationModel
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Retention {
    #[default]
    Indefinite,
    Bounded { max_age_days: u32, max_bodies: u32 },
}

pub struct Schema {
    pub id: SchemaId,
    pub version: u32,
    pub encoding: EncodingId,
    pub mutation: MutationModel,
    pub readable_predecessors: Vec<u32>,
    /// NEW. Defaults to Indefinite, so every existing schema keeps its meaning.
    pub retention: Retention,
}
```

### 5.2 Why the schema and not somewhere else

The reviewed implementation id commits the descriptor — "The id commits its
descriptor, schemas, policy table, and artifact identity"
(`docs/ARCHITECTURE.md:299-301`), built section by section at
`crates/runtime/src/implementation.rs:404`. Putting retention in the `Schema`
makes it **reviewed, versioned, and unforgeable**: a build that quietly promoted
`conversational` to `archival` would move its implementation id, and every peer
would see a different reviewed build. That is the same enforcement `Descriptor`'s
signal and scope sections already get — `docs/DATA-CONTRACT.md` §12.1: "A World's
declared scopes and signals are enforced, not merely reviewed."

Declared on `Schema`, retention is also visible to a replica that cannot open the
Body, because the binding is in the Manifest record
(`BodyRecord.binding`, `crates/replica/src/replica.rs:2401-2407`) while the
payload is not. A retention rule inside a sealed payload could not be applied by
the peers who store the bytes — which is most of them.

### 5.3 The honest limit

**`Retention::Bounded` has no enforcer today.** Nothing in Replica expires a
Body; `Op::Tombstone` (`crates/replica/src/body.rs:163`) is World-authored, and
the sweep in the content plane is content-only. So the truthful specification is:

- **Substrate change:** carry the declaration on `Schema`, commit it into the
  implementation id, expose it on `BodyBinding` so a non-owner replica can read
  it. That is all.
- **Enforcement:** a World-side sweep that reads its own `Retention` and stages
  `Op::Tombstone` on a schedule, authorized like any other mutation.

Claiming substrate enforcement would be claiming an enforcing boundary with no
test behind it, which `docs/THREAT-MODEL.md:402` forbids. The tests this slice
does owe: the declaration survives a descriptor round-trip and moves the
implementation id when changed (`implementation.rs:732-775` is the existing
pattern); a `Bounded` schema's sweep tombstones past the age and stops at the
count; and the negative control — a `transient` message leaves the frontier,
every byte under the store directory, and the Observation sequence unchanged,
which is the shape `DATA-CONTRACT.md` §12.1 already describes for signals.

---

## 6. Where this registers

### 6.1 The case for a World package

- **There is no Body without a World.** `BodyKey { world, body }`
  (`crates/replica/src/ids.rs:190-196`). A substrate-owned mailbox Body has no
  expressible key.
- Runtime rejects "writes outside the Session's World" and "cross-World or
  cross-Space Body references" (`docs/DATA-CONTRACT.md:283-292`), and a Session
  "can never be reused across Worlds" (`docs/ARCHITECTURE.md:307-309`).
- Schemas that are not in a `Descriptor` are not committed to a reviewed
  implementation id and are not enforced at delivery
  (`crates/runtime/src/implementation.rs:404`; `DATA-CONTRACT.md` §12.1).
- `WorldPackages` is the supported extension point: injected once at composition
  (`src/composition.rs:153-155`), carried unchanged into every StationHost
  (`src/orbital/worlds.rs:175-183`), with duplicate ids and mismatches rejected
  at freeze (`worlds.rs:190-198`).
- The opaque-retention budget is **per World**
  (`crates/replica/src/replica.rs:781-791`, `:2486-2488`). A `WorldId` is what
  makes another member's unreadable mailbox a bounded liability.

### 6.2 The case for a substrate crate

- **A World structurally cannot do the sealing work.** A World "cannot access
  storage, Loro, transport, custody secrets, or authority mutation"
  (`docs/ARCHITECTURE.md:302-304`). `BodyKeySource` lives in `replica` and is
  implemented at the composition root. The §1 change is *necessarily* below the
  World seam.
- The plane serves issue notifications, invite delivery, and agent-to-agent
  messages. Building it inside one product means every later caller reaches
  across a product boundary.
- `crates/world-interface/src/destination.rs` is the recorded precedent —
  "Kept beside the sanitizer rather than in the product, because the product is
  one caller and the property is about the function."

### 6.3 Recommendation: both, split at the seam that already exists

**`crates/correspondence`** — substrate, depends on `mechanics` + `replica`, no
`runtime`, no Loro, no mail protocol:

- `MailboxKeyEnvelope`, `MailboxKeySlot`, the `seal_to` × `devices_of` fan-out
- `MailboxKeys: BodyKeySource + ContentKeys` (§1.2, §3.4)
- `mailbox_epoch_id` and the deterministic body-id derivations (§2.2)
- `MessageRecord` / `AttachmentPart` / `SyncCursor` record shapes
- the `Retention` type, if §5 puts it here rather than in `replica::body`

**`products/mailbox`** — a `WorldPackage` under `com.lait.mailbox`:

- the four `Schema` declarations and the `SignalSchema` (§2.1, §5.1)
- the `World` impl staging the §2.3 Op sequences
- the reviewed implementation id and the authorization demands
- depends on `correspondence` for ids and records; is **not** the mail client,
  which is a later separate product talking to this World

**The tell that this is the right cut.** `src/composition.rs` is the only file in
`src/**` permitted to name a product — its own doc comment says so and
`tests/it/product_independence.rs` allowlists exactly it
(`src/composition.rs:17-20`). The mailbox `BodyKeySource` must be constructed
there, because that is the only place a key source can be bound. If the whole
plane were one World package, the shell would have to name `products/mailbox` to
build the key source, and the product-independence test would have to be widened
— which is the invariant that file exists to protect. Split as above, the shell
depends on `crates/correspondence` (a substrate crate, like `replica` or
`mechanics`) and names no product it did not already name.

Second tell: the §1.5 opaque-quota interaction and the §3.5 descriptor-quota gap
are both **Replica** concerns. Neither is expressible from inside a World, and
neither should be discovered by a product.

---

## 7. What this slice owes as executable tests

Per `docs/THREAT-MODEL.md:402`, one at each enforcing boundary:

1. **Mailbox confidentiality within a shared Space.** Two members, one mailbox.
   The non-owner's Replica retains the `message` Body with `interpreted: false`,
   forwards it byte-identically, and `read_body` answers absent. Enforcing
   boundary: `crates/replica/src/replica.rs:2264-2268`.
2. **Device rotation.** Add a device to the actor; one new `MailboxKeySlot`
   appears; the mailbox DEK and every sealed Body are byte-identical before and
   after. That is the `custody.rs:18-20` property, restated for mail.
3. **Device removal re-keys.** `generation` increments, `mailbox_epoch_id`
   changes, new mail is unreadable to the removed device, and old mail is not
   retro-actively protected (state the limit; `ARCHITECTURE.md:462-464`).
4. **Attachment sealing follows the mailbox, not the Space.** §3.4.
5. **A 64 MiB `message` seals and opens**, and 64 MiB + 1 fails
   `Invalid::BodyTooLarge` — the `ReplaceAtomic`-is-unbounded-by-`MAX_VALUE_BYTES`
   property at `replica.rs:3611-3620`, which is an absence and needs a guard.
6. **Two devices write distinct cursor keys concurrently**; both survive; each
   reads its own; a `RegisterSet` variant of the same test loses one. §4.
7. **A transient message moves nothing durable** — frontier, store bytes, and
   Observation sequence identical over ten thousand deliveries, with a positive
   control that one ordinary commit moves all three. Mirrors the existing signal
   test described at `DATA-CONTRACT.md` §12.1.
8. **HPKE forward-compatibility.** A `MailboxKeySlot` with `seal_version: 2` and
   an unimplemented arm fails closed, never guesses. `hpke-sealing.md:88-95`.

---

## 8. Open items handed back

- **Labels: add-wins set vs. per-label LWW map** (§2.3 B). The settled mapping
  says `SetAdd`/`SetRemove`; the algebra says removal loses. Design owner's call.
- **The 25,000-opaque-Body-per-World budget** (§1.5) is shared by every foreign
  mailbox in the Space. Scope it per `(world, epoch)`, or accept the cap.
- **Content descriptors are unquotaed** (§3.5) — `Replica::usage` does not count
  them, and no-dedup makes them grow with fan-out.
- **`max_space_bodies = 100,000` is a hard clamp** and it gates the `archival`
  class (§3.5, axis 3). Raising it is a protocol change under
  `docs/COMPATIBILITY.md` §2/§4, not configuration.
