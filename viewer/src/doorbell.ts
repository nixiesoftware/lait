import { useEffect, useRef, useState } from "react";

import { engineLink } from "./link";
import type { SpaceDoorbell } from "./types";

export type { Liveness } from "./link";
import type { Liveness } from "./link";

/**
 * The doorbell stream — one `EventSource` over every attached space.
 *
 * A frame is a **dirty flag, never state**: the client re-reads the authoritative
 * projection and never patches from the frame (UI.md §5). That is what keeps the
 * browser honest about a CRDT it does not hold.
 *
 * Note what this client does *not* yet do. §5's contract is per-*scope* re-reads —
 * intersect each World-tagged invalidation with what is on screen and fetch only
 * that. We re-read everything a ring could plausibly have touched: correct, and
 * wasteful. Invalidations are read only to decide which optimistic guesses to
 * retire, and `activity_advanced` / `presence_advanced` are not read at all. Say so
 * here rather than describe the design we mean to have.
 *
 * `lagged` means the server's broadcast dropped frames under load; its contract is
 * the same as `reset` or an `epoch` change — rebaseline rather than trust the view.
 * We surface it as a bare "something changed, trust nothing" signal, because the
 * recovery is identical and pretending otherwise invites a subtle bug.
 *
 * The stream itself belongs to the engine link: reconnection and its liveness
 * statements are one backend's mechanics, and every backend owes this hook the
 * same contract — rings are dirty flags, `null` means rebaseline.
 */
export function useDoorbell(onRing: (d: SpaceDoorbell | null) => void): Liveness {
  const [liveness, setLiveness] = useState<Liveness>("connecting");
  // Keep the newest callback without re-opening the stream on every render.
  const cb = useRef(onRing);
  cb.current = onRing;

  useEffect(
    () => engineLink().events((ring) => cb.current(ring), setLiveness),
    [],
  );

  return liveness;
}
