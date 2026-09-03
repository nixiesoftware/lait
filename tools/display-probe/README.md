# display-probe

A headless signage receiver. It enrols with a running `lait daemon`'s display
coordinator over the real receiver protocol, plays the whole-program HLS
stream the way a strict live player (Roku, AVPlayer) does, and writes a JSON
report of what it measured. It replaces "ask a person to look at the TV".

Node 20+, ESM, no dependencies.

```sh
node tools/display-probe/probe.mjs --seconds 60 --verbose --revoke
node --test tools/display-probe/probe.test.mjs        # the pure parts
```

## What it does

1. Opens the daemon's unauthenticated control socket, reads `display_status`
   (origin, pinned certificate, assignments) and copies the pin of one active
   assignment (`--assignment`, default the first active one: orbit, World,
   surface, input, theme, sync). Freshness is `--stale-after-ms` (120 s).
2. Mints a rendezvous code pinned to that copy (`display_rendezvous_mint`),
   builds a `pinned_certificate` bootstrap and pairs through
   `receivers/shared/web/client.mjs` — the same client the browser and tvOS
   receivers run. The credential lands in `--state` (`.state/credential.json`)
   so re-runs skip pairing; `--fresh` pairs again.
3. Declares `native_hls` capabilities (1280×720), takes the program snapshot,
   mints an `hls` live ticket, then plays: fetch the master, pick the highest
   bandwidth rendition, reload the media playlist every `EXT-X-TARGETDURATION`,
   start three target durations behind the edge, consume segments on a play
   clock, fetch each segment one target duration before it is needed
   (`--prefetch-ms` to change), check it is MPEG-TS.
4. Concurrently long-polls `program/changes` and reports health on the
   protocol's cadence, exactly as `client.mjs` does for every receiver.
5. On `--revoke`, revokes its own device over the socket on exit.

## The numbers

| Field | Meaning |
|---|---|
| `startup.pairing_ms` | rendezvous start → enrolled. `null` when the credential was reused. |
| `startup.pair_to_first_segment_ms` | enrolled → first segment bytes in hand. The number an installer feels. |
| `startup.first_playlist` | the first window: sequence range, where playback started, target duration. |
| `runway_ms` | listed media ahead of the play clock, sampled each second. A strict player with runway under one target duration is about to stall. |
| `stalls` | play clock stopped because the next segment was not listed, not fetched yet, refused, or fell behind the window. Count and total ms; each entry names the sequence and reason. |
| `segments.latency_ms` | p50/p95/max per-segment fetch time; `bytes` total. |
| `violations.by_kind` | count and first example per invariant broken (below). |
| `recoveries` | each time the stream was lost (`reset` from the poll, a 403 on the playlist or a segment, a poll network failure): cause, time to the first segment of the replacement stream, and how many requests were refused meanwhile. |
| `health` | accepted / refused health reports. |
| `poll` | how many `snapshot`, `no_change`, `reset`, … answers the long-poll gave, and how many polls errored. |
| `coordinator` | instance, label, identity, certificate digest, the daemon build from the socket handshake, and any comment lines the master playlist carried. |
| `samples` | one row per second: play sequence, listed end sequence, runway, stalled. |

Invariants, checked on every playlist reload and segment:

- `media_sequence_decreased` — `EXT-X-MEDIA-SEQUENCE` went backwards.
- `segment_duration_changed` / `discontinuity_flag_changed` — a sequence once listed changed.
- `discontinuity_sequence_mismatch` — `EXT-X-DISCONTINUITY-SEQUENCE` did not grow by exactly the number of discontinuous segments that left the window.
- `window_gap` — the window jumped past the previous one; a player loses its place.
- `target_duration_changed`, `segment_exceeds_target`, `playlist_malformed` — what RFC 8216 forbids and Roku refuses.
- `listed_segment_refused` — 403/404 on a segment the playlist lists.
- `playlist_refused` — a non-200, non-403 answer to a playlist reload.
- `segment_not_ts` / `ts_packet_unsynced` — the bytes are not whole 0x47-synced 188-byte packets.

Exit code 0 when there were no violations and no stalls, 1 otherwise, 2 when
the run could not be set up (no daemon, no active assignment, pairing refused,
revoked).

## How it reaches the coordinator

The coordinator's certificate is self-signed and its SAN names the address it
had when minted, which need not be the one it announces now. `NODE_EXTRA_CA_CERTS`
would trust the issuer and then fail hostname verification, so the probe does
what a native receiver does: `node:https` with `checkServerIdentity` pinned to
the certificate's SHA-256 (`lib/transport.mjs`). The shared client's
`transport.mjs` prefers a native bridge (`globalThis.AstrolabeNativeTransport`,
the tvOS seam) over `XMLHttpRequest`; the probe fills that seam, so
`client.mjs` is imported unmodified. IndexedDB is replaced by a JSON file vault
and `requestAnimationFrame` by a timer.

`--origin` must be the origin the coordinator announces (`display_status`,
`/head/v1/instance`): a pinned receiver refuses a coordinator whose instance
names a different origin, and so does this one.

## Files

- `probe.mjs` — CLI, socket setup, enrolment, the run, the report.
- `lib/hls.mjs` — playlist parsing, the play-clock model, the invariants. Pure; tested.
- `lib/receiver.mjs` — `DisplayReceiverClient` subclass for native HLS, the HLS session, the stats ledger.
- `lib/transport.mjs` — pinned HTTPS and the native-bridge shim.
- `lib/control.mjs` — the control socket client.
