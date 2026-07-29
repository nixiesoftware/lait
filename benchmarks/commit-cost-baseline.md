# Commit-cost baseline (plan 13 F0)

Recorded by `tests/commit_cost_baseline.rs` at `925c947`, release profile,
Windows 11 (26200), NVMe. Regenerate with:

```sh
LAIT_COMMIT_BASELINE_FULL=1 cargo test --release --test commit_cost_baseline -- --nocapture
```

One single-Body edit against a store already holding N Bodies:

| Bodies | p50 | p95 | p99 | objects/edit | object bytes/edit | manifest |
|---:|---:|---:|---:|---:|---:|---:|
| 1,000 | 138 ms | 177 ms | 184 ms | 5.0 | 100 KB | 289 KB |
| 10,000 | 597 ms | 630 ms | 649 ms | 5.1 | 432 KB | 2.9 MB |
| 50,000 | 2.81 s | 4.06 s | 4.38 s | 5.6 | 628 KB | 14.4 MB |
| 100,000 | 5.81 s | 5.99 s | 6.31 s | 6.2 | 874 KB | 28.8 MB |

Peak RSS 395 MiB. 100,000 is `max_space_bodies`'s protocol maximum.

## What the numbers say, which is not quite what §2.1 said

**Editing one Body at the Body-count ceiling takes 5.8 seconds and fsyncs
28.8 MB.** That is the docket's headline problem, measured.

The dominant cost is **not** the signed manifest pages. Objects written per
edit stays at 5–6 across a hundredfold change in store size, because
`ManifestPage` entries are BodyKey-sorted: editing one Body rewrites the one
page holding it, and the other 24 pages keep their content addresses and are
carried by reference. Page rewriting is already close to O(changed).

The cost is `journal::StoreManifest`, re-encoded and atomically replaced in
full on every commit — the complete `objects: Vec<ObjectRef>` plus the complete
`StoreMeta.bodies: Vec<(BodyKey, BodyRecord)>` carried in its opaque `meta`.
At 100k Bodies that is 28.8 MB written and fsynced to change one Body's head,
and it scales exactly linearly: 289 KB → 2.9 MB → 14.4 MB → 28.8 MB against
1k → 10k → 50k → 100k.

Two consequences for F1:

1. The order of work is settled by measurement. Replacing
   `StoreMeta.bodies: Vec` and the journal's complete object vector with index
   roots is where the whole win is; converting the signed pages to a radix
   index matters for the *shape* of the format, not for this number.
2. Object-count growth is mild but real (5.0 → 6.2). Superseded objects are
   collected only at `JournaledStore::open`, never during a session, so a
   long-lived daemon accumulates every superseded manifest page and meta blob
   until it restarts. F1's streaming GC from roots should not inherit that.

## Collaborative export sizes

From `crates/fabric/tests/history_growth.rs`, incompressible content:

| edits | width | state chars | snapshot | all updates | checkpoint | ckpt/snap |
|---:|---:|---:|---:|---:|---:|---:|
| 100 | 10 | 1,000 | 2,410 | 1,096 | 1,299 | 54% |
| 1,000 | 10 | 10,000 | 21,828 | 10,105 | 10,323 | 47% |
| 10,000 | 10 | 100,000 | 216,261 | 100,115 | 100,341 | 46% |
| 1,000 | 100 | 100,000 | 205,361 | 100,115 | 100,339 | 49% |
| 10,000 | 100 | 1,000,000 | 2,051,791 | 1,000,115 | 1,000,358 | 49% |
| 10,000 | 1,000 | 10,000,000 | 20,311,230 | 10,000,125 | 10,000,382 | 49% |

A snapshot runs about twice its current state: the state, plus the history that
produced it. A checkpoint gives the state back and drops the rest, so it is
consistently ~49% of the snapshot it replaces and tracks state size alone
afterwards.

One ordinary edit's delta is **105 bytes**, and 256 of them encode to 33,792 B.
So §5.3's count threshold is what bounds an ordinary Body; the 8 MiB byte
threshold only fires for a Body that pastes megabytes at a time, which is the
case it should exist for.

Snapshot growth is ~21.6 B per edit, putting the 64 MiB envelope cap at roughly
3.1M edits or ~32M characters of state. §2.2's unwritable Body is real but
distant; the near-term reason to split checkpoint from delta is the per-commit
write, not the ceiling.

## Content geometry

From `crates/replica/tests/content_fixtures.rs`:

| content | chunk | chunks | proof depth | sidecar | overhead |
|---:|---:|---:|---:|---:|---:|
| 1 MiB | 256 KiB | 4 | 2 | 106 B | 0.057% |
| 1 MiB | 1 MiB | 1 | 0 | 40 B | 0.008% |
| 16 MiB | 256 KiB | 64 | 6 | 238 B | 0.108% |
| 16 MiB | 1 MiB | 16 | 4 | 172 B | 0.021% |
| 256 MiB | 256 KiB | 1,024 | 10 | 370 B | 0.158% |
| 256 MiB | 1 MiB | 256 | 8 | 304 B | 0.033% |

1 MiB chunks cost ~5x less metadata overhead, but both are under 0.2% and the
difference is not what the choice turns on. **256 KiB is frozen** because a
sealed chunk must fit inside Contact's 1 MiB frame with room for framing, and
because a failed transfer should waste a quarter megabyte rather than a whole
one. Max proof depth 22 covers the 1 TiB protocol maximum at this geometry.
