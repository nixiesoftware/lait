/**
 * A value you change by choosing — or by asking for it.
 *
 * `ChoiceMenu` is the two-gesture pick from a short, known list. This is the
 * same contract stretched over a list worth querying: a query, the matches,
 * and — when the value has parts — the fine adjustments folded beneath a
 * hairline. The pick is still the commit; there is no field to fill and
 * nothing to submit.
 *
 * Two components share the machinery. `ComboSurface` is the panel itself,
 * for a host that already owns a place to draw it — a chin that expands
 * over the screen, a sheet. `Combo` is the surface behind a popover
 * trigger, for a control standing alone. Either way the surface keeps its
 * state while it stays mounted: the query you typed, the row you reached,
 * the fold you opened, the drafts you left — closing is putting a thing
 * down, not clearing a form.
 *
 * Two shapes of ownership: leave `query` uncontrolled and the list is
 * filtered here by label; control `query`/`onQueryChange` and the parent
 * owns what the matches are (a city search, a directory ask), reporting
 * progress through `status`.
 */

import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { Popover } from "@base-ui/react/popover";
import { ChevronRight, Search } from "lucide-react";
import type { ChoiceItem } from "./menu";

export type ComboSurfaceProps = {
  label: string;
  placeholder?: string;
  /** The matches for the current query — pre-resolved when controlled. */
  items: ChoiceItem[];
  onPick: (id: string) => void;
  /** Controlled query: the parent owns what the matches are. */
  query?: string;
  onQueryChange?: (query: string) => void;
  /** One line under the query: "Searching…", or what went wrong. */
  status?: string | null;
  statusTone?: "quiet" | "danger";
  /** `null` draws no empty row at all — for when the placeholder already
      says everything an empty list would. */
  empty?: string | null;
  /** The fine adjustments, beneath a hairline, folded behind `moreLabel`.
      Hidden rather than unmounted, so drafts persist. */
  children?: ReactNode;
  moreLabel?: string;
  /** While true, the query owns the hands: the input is focused whenever
      this turns on. A popover host turns it on with its own openness; an
      inline host, with whichever panel it is showing. */
  active?: boolean;
  /** Bottom-anchored: the query sits at the foot and the matches stack
      upward, so a host that grows from its bottom edge keeps the hands
      still while the height changes. */
  inverted?: boolean;
  /** A foundation slot: when given, the query pill is rendered into this
      element (a chin's anchor bar) instead of the surface's own flow. The
      pill stays this component's — steering, focus and state unmoved —
      only its ground changes. */
  findSlot?: HTMLElement | null;
};

export function ComboSurface({
  label,
  placeholder = "Type to filter…",
  items,
  onPick,
  query,
  onQueryChange,
  status,
  statusTone = "quiet",
  empty = "Nothing matches.",
  children,
  moreLabel = "More options",
  active = false,
  inverted = false,
  findSlot = null,
}: ComboSurfaceProps) {
  const listId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [ownQuery, setOwnQuery] = useState("");
  const [activeRow, setActiveRow] = useState(0);
  const [moreOpen, setMoreOpen] = useState(false);

  const controlled = onQueryChange != null;
  const asked = controlled ? (query ?? "") : ownQuery;
  const needle = asked.trim().toLowerCase();
  const shown =
    controlled || needle === ""
      ? items
      : items.filter((item) => item.label.toLowerCase().includes(needle));

  // The active row survives closing but never dangles past the list.
  useEffect(() => {
    if (activeRow >= shown.length) setActiveRow(Math.max(0, shown.length - 1));
  }, [shown.length, activeRow]);

  // The query is where the hands land; a panel shown is a question asked.
  // `findSlot` is a dependency because the pill remounts when it lands in
  // (or leaves) the foundation slot, and focus dies with the old node.
  useEffect(() => {
    if (!active) return;
    const frame = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, [active, findSlot]);

  const ask = (next: string) => {
    if (controlled) onQueryChange?.(next);
    else setOwnQuery(next);
    setActiveRow(0);
  };

  const pick = (item: ChoiceItem) => {
    if (item.disabled) return;
    onPick(item.id);
  };

  const steer = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActiveRow((at) => Math.min(at + 1, Math.max(0, shown.length - 1)));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveRow((at) => Math.max(at - 1, 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      const item = shown[activeRow];
      if (item) pick(item);
    } else if (event.key === "Home" && shown.length > 0) {
      event.preventDefault();
      setActiveRow(0);
    } else if (event.key === "End" && shown.length > 0) {
      event.preventDefault();
      setActiveRow(shown.length - 1);
    }
  };

  const find = (
    <div className="ds-combo-find">
      <Search size={14} aria-hidden />
      <input
        ref={inputRef}
        className="ds-combo-input"
        role="combobox"
        aria-expanded={active}
        aria-controls={listId}
        aria-activedescendant={
          shown[activeRow] ? `${listId}-${shown[activeRow].id}` : undefined
        }
        autoComplete="off"
        spellCheck={false}
        placeholder={placeholder}
        value={asked}
        onChange={(event) => ask(event.target.value)}
        onKeyDown={steer}
      />
    </div>
  );

  return (
    <div className={`ds-combo-surface${inverted ? " is-inverted" : ""}`}>
      {findSlot ? createPortal(find, findSlot) : find}
      {status && (
        <p className={`ds-combo-status${statusTone === "danger" ? " is-danger" : ""}`}>
          {status}
        </p>
      )}
      <div role="listbox" id={listId} className="ds-combo-list ds-menu-choice" aria-label={label}>
        {shown.length === 0 && empty != null && (
          <span className="ds-menu-empty">{empty}</span>
        )}
        {shown.map((item, at) => (
          <button
            type="button"
            key={item.id}
            id={`${listId}-${item.id}`}
            role="option"
            aria-selected={item.on || undefined}
            className={`ds-menu-item${item.on ? " is-on" : ""}${item.danger ? " is-danger" : ""}`}
            data-highlighted={at === activeRow || undefined}
            disabled={item.disabled}
            onMouseEnter={() => setActiveRow(at)}
            onClick={() => pick(item)}
          >
            <span className="ds-menu-check" aria-hidden />
            <span className="ds-menu-copy">
              {item.label}
              {item.hint && <small>{item.hint}</small>}
            </span>
          </button>
        ))}
      </div>
      {children && (
        <div className="ds-combo-more">
          <button
            type="button"
            className="ds-combo-toggle"
            aria-expanded={moreOpen}
            onClick={() => setMoreOpen((was) => !was)}
          >
            <ChevronRight size={12} aria-hidden />
            {moreLabel}
          </button>
          <div className="ds-combo-fields" hidden={!moreOpen}>
            {children}
          </div>
        </div>
      )}
    </div>
  );
}

/** The surface behind a popover trigger, for a control standing alone. */
export function Combo({
  className,
  align = "start",
  trigger,
  onPick,
  ...surface
}: Omit<ComboSurfaceProps, "active"> & {
  className?: string;
  align?: "start" | "end";
  /** What the control shows: the chip content. */
  trigger: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger className={className} aria-label={surface.label}>
        {trigger}
      </Popover.Trigger>
      <Popover.Portal keepMounted>
        <Popover.Positioner className="ds-overlay" sideOffset={6} align={align}>
          <Popover.Popup className="ds-pop ds-combo" aria-label={surface.label}>
            <ComboSurface
              {...surface}
              active={open}
              onPick={(id) => {
                onPick(id);
                setOpen(false);
              }}
            />
          </Popover.Popup>
        </Popover.Positioner>
      </Popover.Portal>
    </Popover.Root>
  );
}
