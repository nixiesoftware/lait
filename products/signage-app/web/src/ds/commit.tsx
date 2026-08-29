/**
 * Change is commitment.
 *
 * There is no Save button in this product. Typing in a field *is* the edit, and
 * the interface's only remaining job is to show that it took — on the field,
 * not in a toast across the room.
 *
 * The reference set nearly all gets this half-right: autosave, then a toast
 * somewhere else, and in two cases a greyed-out Save button still sitting
 * beside it. That is feedback detached from the thing you touched, which is the
 * opposite of direct. Frame and Substack are the exceptions — a persistent
 * state marker next to the subject — and this follows them.
 *
 * The shape is the one `docs/ARCHITECTURE.md` calls Constant-Time Feedback
 * Continuity: a draft renders instantly and lives *beside* the committed value
 * rather than replacing it, and the two reconcile when the write lands. That
 * is also why a refusal does not roll your typing back — losing what somebody
 * wrote is a worse failure than briefly showing something the World has not
 * accepted, provided the interface is honest about which it is.
 *
 * Engine-side this is affordable because `Session::submit` durably commits
 * before it returns: a write is a local journal commit, not a network round
 * trip. Debouncing exists to avoid one commit per keystroke, not to hide
 * latency.
 */

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";

export type CommitState = "settled" | "pending" | "committing" | "refused";

export type Commit<T> = {
  /** What to render. The draft while one is in flight, the truth otherwise. */
  value: T;
  /** Instant, local, and schedules the write. */
  set: (next: T) => void;
  /** Write now — for controls where waiting makes no sense (a select, a
   *  toggle, a chip). */
  setNow: (next: T) => void;
  state: CommitState;
  error: string | null;
  /** After a refusal, try the same value again. */
  retry: () => void;
  /** Throw the draft away and take the committed value. The only "cancel"
   *  in the product, and it is per-field rather than per-form. */
  revert: () => void;
};

export function useCommit<T>({
  committed,
  write,
  debounceMs = 450,
  equal = Object.is,
}: {
  /** The value as the World has it. Changes when a doorbell lands. */
  committed: T;
  write: (next: T) => Promise<unknown>;
  debounceMs?: number;
  equal?: (a: T, b: T) => boolean;
}): Commit<T> {
  const [draft, setDraft] = useState<T | null>(null);
  const [state, setState] = useState<CommitState>("settled");
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inflight = useRef(0);
  const latest = useRef(write);
  latest.current = write;

  /**
   * Reconcile. An arriving value is adopted only when nothing local is in
   * play — otherwise somebody else's write would yank the field out from
   * under the person typing in it.
   */
  useEffect(() => {
    if (state === "settled") setDraft(null);
  }, [committed, state]);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const send = useCallback(
    async (next: T) => {
      const ticket = ++inflight.current;
      setState("committing");
      setError(null);
      try {
        await latest.current(next);
        // A later edit already superseded this one; that write owns the state.
        if (ticket !== inflight.current) return;
        setState("settled");
        setDraft(null);
      } catch (err) {
        if (ticket !== inflight.current) return;
        setState("refused");
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [],
  );

  const set = useCallback(
    (next: T) => {
      setDraft(next);
      setState("pending");
      setError(null);
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => void send(next), debounceMs);
    },
    [debounceMs, send],
  );

  const setNow = useCallback(
    (next: T) => {
      setDraft(next);
      if (timer.current) clearTimeout(timer.current);
      void send(next);
    },
    [send],
  );

  const value = draft !== null && !equal(draft, committed) ? draft : committed;

  return {
    value,
    set,
    setNow,
    state,
    error,
    retry: () => void send(value),
    revert: () => {
      if (timer.current) clearTimeout(timer.current);
      inflight.current += 1;
      setDraft(null);
      setState("settled");
      setError(null);
    },
  };
}

/**
 * The state of a change, beside the thing changed.
 *
 * Quiet when settled — a product that announces every success teaches people
 * to ignore it, and then it cannot tell them about the one that failed.
 */
export function CommitMark({
  state,
  error,
  onRetry,
}: {
  state: CommitState;
  error?: string | null;
  onRetry?: () => void;
}) {
  if (state === "settled") return null;
  if (state === "refused") {
    return (
      <span className="ds-commit is-refused" role="status">
        {error ?? "refused"}
        {onRetry && (
          <button type="button" onClick={onRetry}>
            retry
          </button>
        )}
      </span>
    );
  }
  return (
    <span className={`ds-commit is-${state}`} role="status">
      <span className="ds-commit-sweep" aria-hidden />
      {state === "committing" ? "saving" : "…"}
    </span>
  );
}

/**
 * A field that commits itself.
 *
 * The label, the control, its commit state and its refusal live in one box, so
 * there is never a question of which field a message is about.
 */
export function Field({
  label,
  hint,
  commit,
  children,
}: {
  label: string;
  hint?: ReactNode;
  commit?: Pick<Commit<unknown>, "state" | "error" | "retry">;
  children: ReactNode;
}) {
  const state = commit?.state ?? "settled";
  return (
    <label className={`ds-field is-${state}`}>
      <span className="ds-field-head">
        <span className="ds-field-label">{label}</span>
        {commit && (
          <CommitMark state={state} error={commit.error} onRetry={commit.retry} />
        )}
      </span>
      {children}
      {hint && <span className="ds-field-hint">{hint}</span>}
    </label>
  );
}

/**
 * Text that writes itself.
 *
 * Debounced while typing, flushed on blur, so leaving a field never leaves a
 * change hanging — the one moment where a person's model of "I am done with
 * this" and the debounce timer could disagree.
 */
export function CommitText({
  label,
  hint,
  value,
  onWrite,
  placeholder,
  list,
  inputMode,
}: {
  label: string;
  hint?: ReactNode;
  value: string;
  onWrite: (next: string) => Promise<unknown>;
  placeholder?: string;
  list?: string;
  inputMode?: "text" | "decimal" | "numeric";
}) {
  const commit = useCommit<string>({ committed: value, write: onWrite });
  return (
    <Field label={label} hint={hint} commit={commit}>
      <input
        className="ds-input"
        value={commit.value}
        placeholder={placeholder}
        list={list}
        inputMode={inputMode}
        onChange={(event) => commit.set(event.target.value)}
        onBlur={() => {
          if (commit.state === "pending") commit.setNow(commit.value);
        }}
      />
    </Field>
  );
}

export function CommitSelect({
  label,
  hint,
  value,
  options,
  onWrite,
}: {
  label: string;
  hint?: ReactNode;
  value: string;
  options: { value: string; label: string }[];
  onWrite: (next: string) => Promise<unknown>;
}) {
  // A select has no typing to debounce: the choice is the whole gesture.
  const commit = useCommit<string>({ committed: value, write: onWrite });
  return (
    <Field label={label} hint={hint} commit={commit}>
      <select
        className="ds-input"
        value={commit.value}
        onChange={(event) => commit.setNow(event.target.value)}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    </Field>
  );
}
