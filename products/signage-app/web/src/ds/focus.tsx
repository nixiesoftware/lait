/**
 * One held thing, and everything related to it lights.
 *
 * A caption that says "used in 3 programs" is a label standing in for a
 * relation. The relation itself is drawn by the other things reacting: hold a
 * channel and the screens tuned to it light; hold a file and the programs
 * holding it lift. One focus for the whole app, so two surfaces on one page
 * cannot disagree about what is held.
 *
 * Hover holds on a pointer that can hover; a tap toggles on one that cannot.
 * Pages decide what "related" means, because pages hold the graph.
 */

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

export type HeldKind =
  | "screen"
  | "channel"
  | "program"
  | "file"
  | "broadcast"
  | "audience";

export type Held = { kind: HeldKind; id: string };

type Focus = {
  held: Held | null;
  hold: (next: Held) => void;
  release: (was?: Held) => void;
  /** Toggle for coarse pointers, where there is no hover to release. */
  toggle: (next: Held) => void;
};

const FocusContext = createContext<Focus>({
  held: null,
  hold: () => undefined,
  release: () => undefined,
  toggle: () => undefined,
});

export function FocusProvider({ children }: { children: ReactNode }) {
  const [held, setHeld] = useState<Held | null>(null);
  const hold = useCallback((next: Held) => setHeld(next), []);
  const release = useCallback(
    (was?: Held) =>
      setHeld((current) =>
        !was || (current && current.kind === was.kind && current.id === was.id)
          ? null
          : current,
      ),
    [],
  );
  const toggle = useCallback(
    (next: Held) =>
      setHeld((current) =>
        current && current.kind === next.kind && current.id === next.id ? null : next,
      ),
    [],
  );
  const value = useMemo(() => ({ held, hold, release, toggle }), [held, hold, release, toggle]);
  return <FocusContext.Provider value={value}>{children}</FocusContext.Provider>;
}

export function useFocus(): Focus {
  return useContext(FocusContext);
}

export function isHeld(held: Held | null, kind: HeldKind, id: string): boolean {
  return held != null && held.kind === kind && held.id === id;
}

/**
 * The handlers that make an element hold something. Spread onto the element;
 * the page reads `held` and decides what lights.
 */
export function useHoldable(kind: HeldKind, id: string) {
  const { held, hold, release, toggle } = useFocus();
  const me = useMemo(() => ({ kind, id }), [kind, id]);
  return {
    held: isHeld(held, kind, id),
    bind: {
      onPointerEnter: (event: React.PointerEvent) => {
        if (event.pointerType === "mouse") hold(me);
      },
      onPointerLeave: (event: React.PointerEvent) => {
        if (event.pointerType === "mouse") release(me);
      },
      onFocus: () => hold(me),
      onBlur: () => release(me),
    },
    toggle: () => toggle(me),
  };
}

/**
 * The attributes a related thing wears. `lit` when it relates to what is held,
 * `dim` when something else is held and it does not. Nothing is dimmed when
 * nothing is held: the resting state is the honest one.
 */
export function litProps(held: Held | null, related: boolean) {
  if (!held) return {};
  return related ? { "data-lit": "true" } : { "data-dim": "true" };
}
