# Qualification run — <platform> — <device model>

## Environment

| | |
|---|---|
| Device model | |
| OS / firmware | |
| Receiver build (git sha / package version) | |
| Coordinator build (git sha) | |
| Network (LAN topology, WAN path, router build if used) | |
| Date | |
| Operator | |

## Trust and enrollment — zero-tolerance

| Row | Result | Notes |
|---|---|---|
| Clean install shows six words + full coordinator fingerprint, never a bearer token | | |
| Enrollment waits for OK on the television AND authenticated Astrolabe approval | | |
| Wrong words / rejection / expiry / Back / power loss before approval / replayed completion → nothing enrolled | | |
| Power loss after credential, before completion → same idempotent completion resumes | | |
| Reinstall / factory reset → a new receiver | | |
| Hostname mismatch / untrusted cert / HTTP origin / redirect / origin change → fails closed in receiver-owned UI | | |
| Proof keys absent from logs, URLs, screenshots, storage dumps, crash reports, backups | | |

## Assignment and integrity — zero-tolerance

| Row | Result | Notes |
|---|---|---|
| Enrolled unassigned device shows only receiver ID + native unassigned state; cannot browse or guess | | |
| Assign / reassign / rotate revision / unassign / revoke / re-pair: each retires ineligible staged content | | |
| Unknown types, forged revision, wrong HMAC, consumed challenge, length/digest/dimension mismatch, over-limit, external URL → refused before display | | |
| Current + next + upcoming stage without a partial revision showing; interrupts at every asset boundary keep the old revision atomic | | |

## Playback and delivery

| Row | Result | Notes |
|---|---|---|
| Live H.264/AAC plays (MSE on webOS/Tizen/Android TV; native HLS on Roku/tvOS) | | |
| A/V sync, late join at keyframe, discontinuity, dropped Group, ticket expiry before use, reassignment mid-play, coordinator/source restart | | |
| Stored film plays start to end; the stream ends rather than stalling; receiver returns to program flow | | |
| No origin other than the coordinator is learned or requested | | |
| Retained window rolls; memory and queues bounded; obsolete segments refused closed; decoder recovery via new keyframe | | |
| hold_last / loop / blank-at-end / poll-at-end with 250 ms and long items | | |
| Clock changes never select content; suspend/resume and long poll keep monotonic cursors | | |
| Disconnect DNS / Wi-Fi / gateway / coordinator independently: eligible content continues, transport goes offline, stale policy fires exactly at its deadline | | |
| Restore: valid no-change clears offline/stale without upgrading source truth | | |
| Process death, OS restart, low memory, storage exhaustion, decoder failure, lost response: recovery uses a new challenge, never replays | | |
| Health carries no content, coordinates, credentials, or user identity | | |

## Remote, accessibility, and layout

| Row | Result | Notes |
|---|---|---|
| Enrollment and recovery with the shipping remote only | | |
| Back cancels only local pairing; at root follows platform convention | | |
| Overscan/safe areas at every supported resolution; trust chrome readable; aspect preserved; focus visible; no pointer needed | | |
| Screen reader announces pairing words, fingerprint purpose, states, actions, refusals, spoken summaries — without leaking secrets | | |

## Platform release gate

| Row | Result | Notes |
|---|---|---|
| Platform-specific rows from QUALIFICATION.md for this platform | | |

## Verdict

- Zero-tolerance failures: <count — must be 0 to release>
- Open defects filed: <issue refs>
- Released on this run: yes / no
