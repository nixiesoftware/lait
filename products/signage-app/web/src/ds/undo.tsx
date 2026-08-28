/**
 * Undo, instead of a warning.
 *
 * A confirmation dialog asks a question people learn to answer without
 * reading, and then it cannot stop the one time it should. So nothing here
 * asks. A removal happens on the press, and for a few seconds afterwards a
 * quiet bar says what happened and offers the one act that matters — putting
 * it back. The record is held in memory by whoever removed it; undo is a
 * fresh put of the same document.
 *
 * One bar at a time: a second removal replaces the first offer, because two
 * undos stacked is a dialog by another name.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Undo2 } from "lucide-react";

type Offer = {
  id: number;
  label: string;
  undo: () => void | Promise<unknown>;
};

type Undo = {
  /** Say what happened, and how to reverse it. */
  offer: (label: string, undo: () => void | Promise<unknown>) => void;
};

const UndoContext = createContext<Undo>({ offer: () => undefined });

const LINGER_MS = 8000;

export function useUndo(): Undo {
  return useContext(UndoContext);
}

export function UndoProvider({ children }: { children: ReactNode }) {
  const [current, setCurrent] = useState<Offer | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const serial = useRef(0);

  const clear = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    setCurrent(null);
  }, []);

  const offer = useCallback(
    (label: string, undo: () => void | Promise<unknown>) => {
      if (timer.current) clearTimeout(timer.current);
      const id = ++serial.current;
      setCurrent({ id, label, undo });
      timer.current = setTimeout(() => {
        setCurrent((held) => (held?.id === id ? null : held));
        timer.current = null;
      }, LINGER_MS);
    },
    [],
  );

  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);

  const value = useMemo(() => ({ offer }), [offer]);

  return (
    <UndoContext.Provider value={value}>
      {children}
      {current && (
        <div className="ds-undo" role="status" key={current.id}>
          <span className="ds-undo-label">{current.label}</span>
          <button
            type="button"
            className="ds-undo-act"
            onClick={() => {
              const held = current;
              clear();
              void held.undo();
            }}
          >
            <Undo2 size={14} />
            Undo
          </button>
          <span className="ds-undo-fuse" aria-hidden style={{ animationDuration: `${LINGER_MS}ms` }} />
        </div>
      )}
    </UndoContext.Provider>
  );
}
