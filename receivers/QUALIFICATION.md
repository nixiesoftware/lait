# Astrolabe receiver production qualification

Record device model, OS/firmware, receiver build, coordinator build, network,
date, and operator for every run. A platform is not production-qualified from
an emulator, compiler, static test, or successful store upload alone.

## Trust and enrollment

- A clean install displays six confirmation words and the full coordinator
  SHA-256 fingerprint; it never shows an activation bearer token.
- Enrollment remains pending until OK/select is pressed on the television and
  an authenticated Astrolabe controller approves the identical ceremony.
- Wrong words, rejection, expiry, Back/cancel, loss of power before approval,
  and a replayed completion create no enrolled or assigned receiver.
- Power loss after receipt of a credential but before completion resumes the
  same idempotent completion. Reinstall/factory reset creates a new receiver.
- A hostname mismatch, untrusted/expired certificate, HTTP origin, redirect,
  or coordinator-origin change fails closed in receiver-owned UI.
- Inspect platform storage: proof keys never appear in logs, URLs, screenshots,
  localStorage, preferences/plain registry values, crash reports, or backups.

## Assignment and integrity

- An enrolled unassigned device shows only its receiver ID and native
  unassigned state. It cannot browse or guess programs/assets.
- Assign, reassign, rotate a revision, unassign, revoke, and require re-pair;
  each transition retires ineligible staged content and has the expected
  receiver-owned state.
- Unknown major/field/scene/media type, forged revision, wrong request HMAC,
  consumed/expired challenge, asset length/digest/type/dimension mismatch,
  over-limit JSON/asset/program/staging horizon, and external asset URL are
  refused before display.
- Confirm current, next, then upcoming assets stage without showing a partial
  new revision. Interrupt every asset boundary and verify the old eligible
  revision remains atomic.

## Playback and delivery

- Publish a live H.264/AAC resource from the assigned Orbit. Verify MSE on
  webOS/Tizen/Android TV and native HLS on Roku/tvOS, including audio/video
  sync, late join at a keyframe, a discontinuity, a dropped Group, ticket
  expiry before first use, reassignment while playing, and coordinator/source
  restart. No receiver may learn or request an origin other than its coordinator.
- On Android TV, repeat capability negotiation with a dynamically pinned
  bootstrap and verify it advertises Frame rather than attempting a WebView
  WebSocket outside the Java trust manager.
- Hold a live stream long enough to roll the retained six-segment window. Memory
  and append queues remain bounded, obsolete MPEG-TS segments return a closed
  refusal, and decoder recovery requests a new keyframe without replaying stale
  media.
- Exercise hold-last, loop, blank-at-end, and poll-at-end with 250 ms and long
  items. Wall-clock/time-zone/manual-clock changes never select content;
  suspend/resume and long poll preserve monotonic cursor behavior.
- Disconnect DNS, Wi-Fi/Ethernet, gateway, and coordinator independently.
  Eligible content continues; transport becomes offline; source truth does not
  silently change; the stale policy adds native chrome or blanks exactly when
  its monotonic deadline expires.
- Restore delivery and verify a valid no-change response clears offline/stale
  delivery without upgrading partial or unavailable source state.
- Force process death, OS restart, low-memory pressure, storage exhaustion,
  decoder failure, and coordinator restart/lost response. Recovery uses a new
  challenge and never replays an authenticated operation.
- Observe health: exact revision/item/elapsed, displayed asset, connection,
  playback, staged bounds, errors, latency visibility, drift, and correction
  counts contain no content, World coordinates, credentials, or user identity.

## Remote, accessibility, and layout

- Complete enrollment and recovery with the shipping remote only: DPAD/select,
  Back, and Info/options where available. Back cancels only local pairing; at a
  normal root it exits according to platform convention.
- Verify overscan/safe areas at every supported resolution and display mode.
  Trust chrome remains readable; frame pixels preserve aspect ratio; focus is
  visible; no touch/pointer is required.
- Run VoiceOver/TalkBack/Audio Guide/vendor screen reader. Pairing words,
  fingerprint purpose, source/transport/stale state, actions, refusal details,
  and authored spoken frame summaries are announced without leaking secrets.

## Platform release gates

- **webOS:** physical current and oldest-supported LG TVs; hosted-origin/CSP
  update; launcher offline behavior; `ares-package --check`; Seller Lounge
  self-check and signed IPK.
- **Tizen:** oldest Tizen 5.5 and current Samsung TV/signage; KeyManager power
  interruption; Return key; WARP/CSP; Seller certificate and signed WGT.
- **Android TV:** API 26 and current Google TV; TV quality checklist; DPAD-only;
  WebView version floor; backup/transfer exclusion; release AAB signing and Play
  pre-launch/physical review.
- **Roku:** low-memory and current Roku TV; protocol startup self-check;
  chunked-range behavior; Audio Guide; certification tests; publisher-key
  package on the physical device.
- **tvOS:** oldest tvOS 17 and current Apple TV; Siri Remote and VoiceOver;
  Keychain persistence/removal; XCTest conformance; archive validation and
  TestFlight physical-device pass.

Release only after every applicable row has an attached result and all
security/integrity failures are zero-tolerance passes.
