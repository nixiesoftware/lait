import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

import { rpc } from "../api";
import {
  PRIORITY_LABEL,
  type IssueView,
  type Priority,
  type ProjectDto,
  type Row,
  type StatusCategory,
  type WorkflowState,
} from "../types";
import { catalogColor } from "./colors";
import { PriorityIcon, StatusIcon, statusIconElement } from "./icons";
import { cn } from "./primitives";

/**
 * `ENG-142`, written in prose, drawn as the issue it names.
 *
 * The parser cannot do this on its own — it has no catalog, so it reports the
 * *shape* of a ref and this file decides whether the shape is an issue. That
 * split is what makes the feature free of false positives: `UTF-8` and
 * `COVID-19` parse as candidate refs, resolve to nothing, and render as the
 * text they always were. Nothing is eaten, and no syntax had to be invented for
 * agents to learn — a bare alias is what they already write.
 */

export interface RefTarget {
  /** The handle every command takes. */
  reff: string;
  /** The display alias, canonicalised — `eng-142` in prose shows as `ENG-142`. */
  alias: string;
  title: string;
  /** Workflow state id, resolved to a ring through `states`. */
  status: string;
  priority: Priority;
  /** Owning project id, resolved to a name and a swatch through `projects`. */
  project: string;
}

/** `undefined` = not looked up yet; `null` = looked at, not an issue. */
type Entry = RefTarget | null | undefined;

interface RefResolution {
  lookup: (ref: string) => Entry;
  /** Ask for a ref this render needed and did not have. Idempotent. */
  request: (ref: string) => void;
  states: WorkflowState[];
  projects: ProjectDto[];
  open: (reff: string) => void;
}

const RefContext = createContext<RefResolution | null>(null);

/**
 * The resolver, mounted once around every surface that draws prose.
 *
 * Three layers, cheapest first:
 *
 * 1. **The rows already on screen.** A description almost always references its
 *    own neighbours, and those are in hand — that lookup is synchronous and the
 *    chip is drawn on the first paint with no flash.
 * 2. **The known project KEYs.** A candidate whose KEY names no project is
 *    rejected without touching the network. This is the whole false-positive
 *    defence: `SHA-256` never becomes a request, because `SHA` is not a
 *    project. Without it, every acronym in every description would cost a
 *    round trip to learn it is an acronym.
 * 3. **One `issue_view` per surviving miss**, deduplicated across chips and
 *    negative-cached, so a ref to another project resolves once per session
 *    whether it appears once or thirty times.
 */
export function RefResolutionProvider({
  spaceId,
  rows,
  projects,
  states,
  onOpen,
  children,
}: {
  /** The rpc space id. Empty disables fetching (nothing to ask). */
  spaceId: string;
  rows: Row[];
  projects: ProjectDto[];
  states: WorkflowState[];
  onOpen: (reff: string) => void;
  children: React.ReactNode;
}) {
  const seed = useMemo(() => {
    const map = new Map<string, RefTarget>();
    for (const row of rows) {
      if (!row.key_alias || row.tombstone) continue;
      map.set(row.key_alias.toUpperCase(), {
        reff: row.reff,
        alias: row.key_alias,
        title: row.title,
        status: row.status,
        priority: row.priority,
        project: row.project_id,
      });
    }
    return map;
  }, [rows]);

  const keys = useMemo(
    () => new Set(projects.map((project) => project.key.toUpperCase())),
    [projects],
  );

  const [fetched, setFetched] = useState<ReadonlyMap<string, RefTarget | null>>(new Map());
  /** In-flight and settled, so a re-render cannot re-ask. Held in a ref because
   *  two chips for the same ref mount in the same commit and neither has seen
   *  the other's state update yet. */
  const asked = useRef(new Set<string>());

  const resolution = useMemo<RefResolution>(() => {
    const keyOf = (ref: string) => ref.slice(0, ref.lastIndexOf("-")).toUpperCase();
    return {
      states,
      projects,
      open: onOpen,
      lookup: (ref) => {
        const key = ref.toUpperCase();
        const local = seed.get(key);
        if (local) return local;
        if (fetched.has(key)) return fetched.get(key) ?? null;
        // A candidate no project could own is settled without asking anyone.
        if (!keys.has(keyOf(ref))) return null;
        return undefined;
      },
      request: (ref) => {
        const key = ref.toUpperCase();
        if (!spaceId || seed.has(key) || asked.current.has(key)) return;
        if (!keys.has(keyOf(ref))) return;
        asked.current.add(key);
        void rpc(spaceId, { cmd: "issue_view", reff: ref })
          .then((reply) => {
            const issue = reply as { kind: string } & Partial<IssueView>;
            const target: RefTarget | null =
              issue.kind === "issue" && issue.reff
                ? {
                    reff: issue.reff,
                    alias: issue.key_alias ?? ref.toUpperCase(),
                    title: issue.title ?? "",
                    status: issue.status ?? "",
                    priority: issue.priority ?? "none",
                    project: issue.project_id ?? "",
                  }
                : null;
            setFetched((held) => new Map(held).set(key, target));
          })
          .catch(() => {
            // A ref that does not resolve is prose. This is the ordinary
            // outcome for an acronym that got past the KEY gate (a project
            // really called `SHA` would make `SHA-256` a fair question), and
            // it is also what a denied or not-yet-converged issue looks like —
            // all three render as the words the author typed.
            setFetched((held) => new Map(held).set(key, null));
          });
      },
    };
  }, [seed, keys, fetched, spaceId, states, projects, onOpen]);

  return (
    <RefContext.Provider value={resolution}>
      {children}
      <RefHoverCard />
    </RefContext.Provider>
  );
}

/** A ref that resolved, with its ring, swatch and labels worked out. */
export interface ResolvedRef extends RefTarget {
  category: StatusCategory | null;
  color: string;
  statusName: string;
  projectName: string;
  projectColor: string;
}

export interface Refs {
  /** `null` for anything that is not (yet) an issue. */
  resolve: (ref: string) => ResolvedRef | null;
  request: (ref: string) => void;
  open: (reff: string) => void;
}

/**
 * The resolver, for renderers that are not React.
 *
 * The CodeMirror live preview draws the same chip through a decoration widget,
 * and a widget is a `Node` — there is no component underneath it to read a
 * context. So the editor takes the resolver as a value and hands it to the DOM
 * builder below. Returns `null` outside a provider, which is how a bare test
 * render degrades to plain text.
 */
export function useRefs(): Refs | null {
  const ctx = useContext(RefContext);
  return useMemo(() => (ctx ? refsOf(ctx) : null), [ctx]);
}

function refsOf(ctx: RefResolution): Refs {
  return {
    resolve: (ref) => {
      const target = ctx.lookup(ref);
      if (!target) return null;
      const state = ctx.states.find((s) => s.id === target.status);
      const project = ctx.projects.find((p) => p.id === target.project);
      return {
        ...target,
        category: state?.category ?? null,
        color: state ? catalogColor(state.color) : "currentColor",
        statusName: state?.name ?? "",
        projectName: project?.name ?? "",
        projectColor: project ? catalogColor(project.color) : "transparent",
      };
    },
    request: ctx.request,
    open: ctx.open,
  };
}

/**
 * The chip.
 *
 * `display: inline`, not `inline-flex`, and that is the whole reason it wraps
 * instead of truncating. A flex box is one unbreakable rectangle: a long title
 * inside it can only be cut off, which is what `truncate` was doing — it hid
 * exactly the words that make the chip worth drawing. An inline box breaks at
 * the same places a sentence does, and `box-decoration-clone` gives each
 * fragment its own border so the pieces still read as one chip rather than as
 * a rectangle someone sliced.
 *
 * The cost is that the glyph can no longer be centred by the flex box, so it
 * takes an explicit `vertical-align` instead. `-0.18em` is the offset that puts
 * a 14px ring's centre on the x-height of 15px text; `middle` aligns to the
 * *font's* middle, which sits lower and left the ring visibly high.
 */
const CHIP_CLASS = cn(
  "border-line bg-active/40 hover:bg-active rounded-control box-decoration-clone",
  "cursor-pointer border px-1.5 py-0.5",
);
const RING_CLASS = "mr-1 inline-block align-[-0.18em]";
const ALIAS_CLASS = "text-mute mr-1 tabular-nums";

export function RefChip({ reff }: { reff: string }) {
  const ctx = useContext(RefContext);
  const target = useMemo(() => (ctx ? refsOf(ctx).resolve(reff) : null), [ctx, reff]);

  useEffect(() => {
    ctx?.request(reff);
  }, [ctx, reff]);

  if (!ctx || !target) return <>{reff}</>;

  return (
    <span
      role="link"
      tabIndex={0}
      data-ref={reff}
      onClick={() => ctx.open(target.reff)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          ctx.open(target.reff);
        }
      }}
      className={cn(
        CHIP_CLASS,
        "focus-visible:ring-accent/50 focus-visible:outline-none focus-visible:ring-2",
      )}
    >
      {target.category && (
        <span className={RING_CLASS}>
          <StatusIcon category={target.category} color={target.color} />
        </span>
      )}
      <span className={ALIAS_CLASS}>{target.alias}</span>
      <span className={target.category === "done" ? "text-mute line-through" : ""}>
        {target.title}
      </span>
    </span>
  );
}

/** The chip as DOM, spelling out the same classes the React one does. */
export function refChipElement(target: ResolvedRef, open: (reff: string) => void): HTMLElement {
  const root = document.createElement("span");
  root.className = CHIP_CLASS;
  root.setAttribute("role", "link");
  root.dataset.ref = target.alias;
  // The editor owns its own selection and caret; a chip that took focus would
  // pull both out of the document the moment one scrolled into view.
  root.addEventListener("mousedown", (event) => {
    event.preventDefault();
    event.stopPropagation();
    open(target.reff);
  });
  if (target.category) {
    const ring = document.createElement("span");
    ring.className = RING_CLASS;
    ring.append(statusIconElement(target.category, target.color));
    root.append(ring);
  }
  const alias = document.createElement("span");
  alias.className = ALIAS_CLASS;
  alias.textContent = target.alias;
  const title = document.createElement("span");
  if (target.category === "done") title.className = "text-mute line-through";
  title.textContent = target.title;
  root.append(alias, title);
  return root;
}

const DWELL_MS = 320;
const CARD_GAP = 8;
const CARD_WIDTH = 340;

/** Low bound wins when the two cross — a window narrower than the card should
 *  clip its right edge, not push its left edge off screen. */
function clamp(value: number, low: number, high: number): number {
  return Math.max(low, Math.min(value, Math.max(low, high)));
}

/**
 * One card for every chip on the page.
 *
 * Mounted once by the provider and driven by delegated pointer events rather
 * than wrapped around each chip, because the chips do not all have a component
 * to wrap: half of them are CodeMirror decoration widgets built as raw DOM. A
 * `data-ref` attribute is the one thing both renderers can carry, so the
 * listener finds its anchor by attribute and there is exactly one
 * implementation instead of one per renderer.
 *
 * Not the app's `Tooltip`. That surface is inverted — dark fill, light text —
 * which is right for a one-line hint and wrong for a card of structured facts;
 * this is a small raised sheet, the same species as a popover.
 *
 * `pointer-events: none`, so the card can never be hovered, entered, or
 * clicked. It costs nothing (there is nothing in it to interact with — the chip
 * itself is the link) and it removes the entire class of bugs where a card
 * chases the pointer that is trying to reach it.
 */
function RefHoverCard() {
  const ctx = useContext(RefContext);
  const [shown, setShown] = useState<{ target: ResolvedRef; rect: DOMRect } | null>(null);
  const [height, setHeight] = useState<number | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const card = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (!shown) {
      if (height !== null) setHeight(null);
      return;
    }
    const measured = card.current?.getBoundingClientRect().height ?? null;
    if (measured !== null && measured !== height) setHeight(measured);
  });

  useEffect(() => {
    if (!ctx) return;
    const refs = refsOf(ctx);
    const cancel = () => {
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = null;
    };
    const chipOf = (node: EventTarget | null) =>
      node instanceof Element ? node.closest<HTMLElement>("[data-ref]") : null;

    const over = (event: MouseEvent) => {
      const chip = chipOf(event.target);
      if (!chip?.dataset.ref) return;
      const ref = chip.dataset.ref;
      cancel();
      timer.current = setTimeout(() => {
        const target = refs.resolve(ref);
        // The chip's FIRST fragment, not its union box. A chip that wrapped
        // across two lines has a bounding box spanning the full measure, and a
        // card centred on that points at the gap between the pieces.
        const rect = chip.getClientRects()[0] ?? chip.getBoundingClientRect();
        if (target) setShown({ target, rect });
      }, DWELL_MS);
    };
    const out = (event: MouseEvent) => {
      if (!chipOf(event.target)) return;
      cancel();
      setShown(null);
    };
    // Any scroll invalidates the rect the card was placed from, and there is no
    // cheap way to know which ancestor moved — so the capture phase catches all
    // of them and the card simply goes.
    const scrolled = () => {
      cancel();
      setShown(null);
    };

    document.addEventListener("mouseover", over);
    document.addEventListener("mouseout", out);
    document.addEventListener("scroll", scrolled, true);
    window.addEventListener("blur", scrolled);
    return () => {
      cancel();
      document.removeEventListener("mouseover", over);
      document.removeEventListener("mouseout", out);
      document.removeEventListener("scroll", scrolled, true);
      window.removeEventListener("blur", scrolled);
    };
  }, [ctx]);

  if (!shown) return null;
  const { target, rect } = shown;
  // Measured, not guessed. The title wraps, so the card's height depends on the
  // issue — the same reason the format bar measures itself before deciding
  // which side of the selection it takes.
  const above = height !== null && rect.top - height - CARD_GAP >= CARD_GAP;
  return createPortal(
    <div
      ref={card}
      role="tooltip"
      className={cn(
        "border-line bg-raised shadow-overlay rounded-surface pointer-events-none fixed z-50",
        "flex flex-col gap-1.5 border p-3",
        height === null && "invisible",
      )}
      style={{
        width: CARD_WIDTH,
        left: clamp(rect.left, CARD_GAP, window.innerWidth - CARD_WIDTH - CARD_GAP),
        top: above ? undefined : rect.bottom + CARD_GAP,
        bottom: above ? window.innerHeight - rect.top + CARD_GAP : undefined,
      }}
    >
      <span className="text-mute text-xs tabular-nums">{target.alias}</span>
      <span className="text-fg text-sm leading-snug">{target.title}</span>
      <span className="bg-line my-1 h-px w-full" />
      <span className="text-dim flex flex-wrap items-center gap-x-4 gap-y-1 text-xs">
        {target.category && (
          <span className="inline-flex items-center gap-1.5">
            <StatusIcon category={target.category} color={target.color} />
            {target.statusName}
          </span>
        )}
        {target.projectName && (
          <span className="inline-flex items-center gap-1.5">
            <span
              className="size-mark-sm rounded-mark shrink-0"
              style={{ background: target.projectColor }}
            />
            {target.projectName}
          </span>
        )}
        <span className="inline-flex items-center gap-1.5">
          <PriorityIcon priority={target.priority} size="sm" />
          {PRIORITY_LABEL[target.priority]}
        </span>
      </span>
    </div>,
    document.body,
  );
}
