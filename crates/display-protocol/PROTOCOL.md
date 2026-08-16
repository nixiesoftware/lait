# Astrolabe Display protocol major 1

This is the production receiver contract. It is product-neutral: receiver
requests cannot name a Space, World, display surface, controller identity,
package input, filesystem path, or external URL.

## Transport

All operations use HTTPS. A hosted coordinator uses Web PKI. A self-hosted
coordinator uses a stable certificate whose SHA-256 fingerprint is confirmed on
both the receiver and an authenticated Astrolabe controller before enrollment.
Discovery, DIAL launch parameters, deep links, QR data, and manual entry are
untrusted doorbells and carry at most an HTTPS origin plus an expiring
rendezvous ID.

JSON bodies are UTF-8 and reject unknown fields. HTTP adapters apply the body
bounds in `src/bounds.rs` before parsing. Fixed byte strings are lowercase hex
without prefixes: random IDs are 32 characters; SHA-256 values, challenges,
proof keys, request tags, derived item IDs, and derived asset IDs are 64.
Every list described as sorted and unique is ordered by its serialized
snake-case wire string, never by a language's enum declaration order.

The receiver-facing routes are closed:

| Route | Authentication | Purpose |
| --- | --- | --- |
| `GET /head/v1/instance` | trusted TLS | Coordinator identity and trust profile |
| `POST /head/v1/pairings` | trusted TLS | Start enrollment |
| `POST /head/v1/pairings/status` | pairing poll HMAC | Await controller approval |
| `POST /head/v1/pairings/complete` | new receiver-key HMAC | Prove possession and commit enrollment |
| `POST /head/v1/challenges` | bounded device ID, trusted TLS | Recover a request challenge |
| `POST /head/v1/capabilities` | request HMAC | Negotiate only narrower receiver limits |
| `GET /head/v1/program` | request HMAC | Fetch a complete current snapshot |
| `GET /head/v1/program/changes` | request HMAC | Complete with snapshot, no-change, reset, unassigned, revoked, or re-pair |
| `GET /head/v1/assets/{opaque_asset}` | request HMAC | Fetch one assignment/revision-bound asset |
| `POST /head/v1/live/tickets` | request HMAC | Mint one assignment/item/manifest-bound MSE or HLS grant |
| `GET /head/v1/live/{ticket}/socket` | single-use opaque ticket | Upgrade to the bounded CMAF WebSocket stream |
| `GET /head/v1/live/{ticket}/master.m3u8` | opaque ticket | Fetch the current native-HLS master playlist |
| `GET /head/v1/live/{ticket}/renditions/{rendition}.m3u8` | opaque ticket | Fetch one bounded HLS live window |
| `GET /head/v1/live/{ticket}/segments/{sequence}.ts` | opaque ticket | Fetch one retained MPEG-TS segment |
| `POST /head/v1/health` | request HMAC | Submit bounded operational facts |

There is no catalog, generic RPC, command, upload, browser route, arbitrary URL
proxy, cookie session, acting-identity selector, or receiver-chosen media origin
in major 1. Live tickets are random 32-byte bearer values scoped to one enrolled
device, assignment, program revision, current item, opaque manifest, Orbit, and
transport. The coordinator revalidates assignment eligibility and revision on
the MSE session and every HLS request.

## Pairing

The receiver generates a 32-byte receiver nonce and a separate 32-byte poll
key, then starts pairing over trusted TLS with its bounded capabilities. The
coordinator returns a six-word confirmation phrase derived from the coordinator
fingerprint, pairing ID, and receiver nonce. The television and authenticated
Astrolabe controller must show the same phrase and full fingerprint detail.

Approval returns a coordinator-minted device ID, random 32-byte proof key, and
single-use enrollment challenge only to a status request authenticated by the
poll key. The receiver durably stores the pending credential before proving the
pairing-complete transcript. The coordinator commits enrollment only after that
proof. Repeating the identical completion is idempotent; it cannot mint a
second device. Expired, rejected, mismatched, replayed, or interrupted attempts
grant no assignment.

Pairing authorizes one receiver installation to authenticate to one coordinator.
Assignment is a later coordinator-local policy operation and pins the exact
source contract outside this receiver protocol.

## Request authentication

Every operation after enrollment consumes a random server challenge and uses
HMAC-SHA-256 with the receiver proof key. `request_transcript` is the normative
binary encoding: every field is a four-byte unsigned big-endian length followed
by the exact bytes. The first field is
`astrolabe-display/request/v1`. Remaining fields are, in order:

1. protocol major as four big-endian bytes;
2. uppercase HTTP method;
3. closed route ID;
4. device ID;
5. assignment ID or empty;
6. program ID or empty;
7. revision or empty;
8. current item ID or empty;
9. elapsed milliseconds as four big-endian bytes or empty;
10. long-poll wait milliseconds as four big-endian bytes or empty;
11. opaque asset ID or empty;
12. range start as eight big-endian bytes or empty;
13. range length as four big-endian bytes or empty;
14. challenge; and
15. SHA-256 of the exact body bytes.

The coordinator checks body and identifier bounds, enrollment, challenge
lifetime, and single-use state before application work or file creation. It
consumes the challenge before executing the operation. Every authenticated
response supplies a replacement. A lost response or restart returns the client
to `/challenges`; no credential appears in a URL.

The authenticated HTTP adapter is also closed. `Authorization` is exactly
`Astrolabe-HMAC <lowercase-tag>`. The request carries
`X-Astrolabe-Protocol-Major`, `X-Astrolabe-Route`, `X-Astrolabe-Device`,
`X-Astrolabe-Challenge`, and `X-Astrolabe-Body-SHA256`; it carries each of
`X-Astrolabe-Assignment`, `X-Astrolabe-Program`, `X-Astrolabe-Revision`,
`X-Astrolabe-Current-Item`, `X-Astrolabe-Elapsed-Ms`, `X-Astrolabe-Wait-Ms`,
`X-Astrolabe-Asset`, `X-Astrolabe-Range-Start`, and
`X-Astrolabe-Range-Length` exactly when the matching transcript field is
present. The adapter rejects duplicated protocol headers, a header/route
mismatch, and an asset path whose final segment differs from the authenticated
asset ID. Every authenticated response, including an API error after challenge
consumption, returns the replacement in `X-Astrolabe-Next-Challenge`. Browser
origins must be allowed to send these headers and read that response header.

`/program/changes` encodes the bounded long-poll duration only in the
authenticated `X-Astrolabe-Wait-Ms` header; it has no unauthenticated query
parameter. A byte range, when supported, is described by the authenticated
range fields and by one syntactically matching HTTP `Range` header.

## Program and assets

`DisplayProgram` is always a complete bounded snapshot. Its revision is the
SHA-256 of `program_semantics_transcript`: assignment/program IDs, source state,
freshness policy, cycle, ordered item IDs, durations, scenes, asset semantic
metadata, spoken summaries, and the optional sync group plus effective sync
mode. The revision excludes playback position, sync sample time, asset handles,
and asset bytes. Cursor-only correction therefore does not manufacture a
content revision.

An optional playback `sync` target has exactly `group`, `mode`, and
`sampled_at_unix_ms`. Group names are 1–64 bytes of lowercase ASCII letters,
digits, `_`, or `-`. The only modes are `stay_in_sync`, which aligns item
boundaries, and `positional`, which also aligns the position within an item.
The coordinator derives all members' cursors from one persisted group epoch and
degrades a positional group to boundary alignment when any active member lacks
positional capability. Static per-receiver delay belongs to coordinator-local
assignment policy and is never exposed to receivers.

The receiver validates the full snapshot before replacing eligible state. It
fetches current, next, then later frame assets and stages grants for eligible
live scenes. Each frame transfer is authenticated and committed to assignment,
program, revision, opaque handle, expected media type, encoded length, SHA-256,
dimensions, and optional range. Bytes are written to a new app-owned temporary
file, bounded while streaming, verified, decoded within declared dimensions,
and only then made displayable.

`mse_live` receivers obtain a single-use WebSocket grant. The coordinator sends
one closed JSON track catalog followed by binary init/media envelopes, and each
selected SourceBuffer has a bounded append queue and retained live window.
`native_hls` receivers receive only a coordinator URL. Astrolabe transmuxes the
same H.264/AAC access units into real HLS-v3 MPEG-TS segments with a six-segment
window; Roku Video and AVPlayer perform the decode. Catalog resource names are
opaque identifiers, never URLs. Revision replacement is atomic; reassignment,
revocation, normal exit, and startup sweep retire ineligible staged material and
grants.

Playback uses monotonic relative time. A sync value is a sampled target, never a
command: receivers adopt it according to their negotiated tier, continue on a
monotonic clock, and report bounded residual drift and correction counts. TV
wall-clock time never chooses authored content. Transport liveness, source
state, and delivery staleness remain separate native states. A valid no-change
response clears offline delivery state without upgrading partial or unavailable
source truth. Unknown majors, fields, scenes, media kinds, or broken integrity
produce native refusal chrome—never HTML or a best-effort fallback.

## Conformance

`fixtures/v1/conformance.json` contains fixed inputs, program JSON, canonical
transcript bytes, expected HMACs, confirmation words, and refusal names. The
keys are public test material only. Each platform adapter must reproduce these
values without calling Rust code and must exercise the negative cases before it
can claim protocol-major-1 compatibility.
