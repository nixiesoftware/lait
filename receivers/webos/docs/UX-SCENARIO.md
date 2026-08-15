# webOS UX scenario — Astrolabe Display

## First launch and enrollment

The app opens protected receiver state. With no credential, it contacts only the
fixed Web-PKI coordinator at `nixiesoftware.com`, starts a bounded pairing, and
shows six confirmation words plus the full coordinator certificate fingerprint.
The user opens **Displays** in authenticated Astrolabe. They approve only if the
same words and fingerprint appear there, then press **OK** on the television.

The receiver polls with its independently generated poll key. On approval it
durably encrypts the pending device/proof credential before returning proof of
possession. A lost response may repeat the same completion but cannot create a
second receiver. Pairing grants no content; the screen then shows **Ready for an
assignment** until Astrolabe assigns an exact display surface.

## Assigned operation

The receiver submits bounded platform capability, retrieves a complete signed-
semantics snapshot, verifies the revision, and fetches only opaque assets named
by that assignment/revision. Every operation consumes a single-use challenge and
uses HMAC-SHA-256. Each image is bounded during transfer, checked for media type,
length, SHA-256, and decoded dimensions, then becomes eligible in one atomic
staging swap. Relative item timing uses the monotonic clock.

## Native recovery states

Receiver-owned chrome distinguishes transport offline, source partial or
unavailable, delivery stale, unsupported output, revocation, and coordinator
trust change. An authenticated no-change clears offline state but does not
upgrade source truth. `keep_with_native_banner` retains verified pixels with a
stale badge; `blank` removes them. Online revocation clears staged assets. A
re-pair result clears the old credential and starts a new confirmation ceremony.

**Info** toggles receiver status. **Back** follows webOS navigation; during an
uncommitted pairing it abandons only the local attempt, which expires server-
side and grants nothing.
