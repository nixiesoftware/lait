/**
 * Reading the running app without taking its picture.
 *
 * A screenshot costs ~20k tokens and answers "does this look right". Almost
 * nothing we actually ask is that question. The walk that produced this module
 * found four defect classes, and **every one of them is a number**:
 *
 * - width stated on a wrapper instead of the field it meant to size,
 * - padding stated on the dialog sheet instead of the region, so a divider
 *   stopped 16px short at both ends,
 * - `justify-content` named by nobody, so a label centred itself,
 * - an elevation tuned for a card sitting under a 24px chip.
 *
 * Each is a `getComputedStyle` read away, and a number costs fifty tokens. So
 * this is the cheap half of the loop: measure by default, and spend a
 * screenshot only on the genuinely visual call — *is that shadow a halo?* — where
 * a number cannot answer.
 *
 * Three commitments make it cheap rather than merely different:
 *
 * 1. **It returns text, not objects.** JSON of a computed style is mostly
 *    punctuation and defaults. These return dense lines meant to be read.
 * 2. **Identical elements report once.** Eight project tabs are one shape with
 *    `8×` next to it. On a board or a list this is the whole saving.
 * 3. **It speaks the ladder.** 36px is reported as `bar-md`, because the
 *    question is never "how many pixels" — it is "which rung", and a raw pixel
 *    count makes the reader do the lookup this app already wrote down.
 *
 * **Dev-only, and read-only.** `main.tsx` imports it behind `import.meta.env.DEV`,
 * which Vite replaces with a literal `false` in a build, so the branch and this
 * module are eliminated before the bundle is embedded in the binary — the
 * shipped product never carries a debug surface. Nothing here writes to the DOM
 * except `go`, which dispatches the `lait:nav` event that `App.tsx` already
 * listens for. No synthetic clicks: see CLAUDE.md for why a click fired inside
 * an `eval` detaches the automation context and this event does not.
 */

/** How many distinct shapes `look` prints before it starts counting instead.
 *  A capped report says so on its last line — a silent truncation reads as
 *  "that was all of them" when it was not. */
const SHAPE_CAP = 8;

/** Visible text is worth having and a whole article is not. */
const TEXT_CAP = 400;

// ── The ladder ───────────────────────────────────────────────────────────────

/**
 * The axes worth resolving a measurement against, in the order a hit is worth
 * reporting. `ctl` before `bar` because a control is the thing you usually
 * measured; both can legitimately land on 32px and naming the wrong one is a
 * worse answer than naming the likelier one.
 */
const AXES = ["ctl", "bar", "icon", "mark"] as const;
const RUNGS = ["2xs", "xs", "sm", "md", "lg", "xl"] as const;

/**
 * Resolved by *measuring*, not by parsing.
 *
 * `getComputedStyle().getPropertyValue("--spacing-ctl-sm")` hands back the
 * substituted token stream — `calc(24px * 1)` — because an unregistered custom
 * property is substituted rather than computed. Rather than evaluate `calc` by
 * hand, set the value as a height on a hidden element and read the box back:
 * the engine does the arithmetic, `--scale` is honoured for free, and the
 * answer cannot drift from what the app actually renders.
 */
let ladder: Array<[string, number]> | null = null;

function theLadder(): Array<[string, number]> {
  if (ladder) return ladder;
  const probe = document.createElement("div");
  probe.style.cssText = "position:absolute;visibility:hidden;pointer-events:none;top:-9999px";
  document.body.appendChild(probe);

  const measured: Array<[string, number]> = [];
  for (const axis of AXES) {
    for (const rung of RUNGS) {
      probe.style.height = `var(--spacing-${axis}-${rung}, 0px)`;
      const px = probe.getBoundingClientRect().height;
      if (px > 0) measured.push([`${axis}-${rung}`, px]);
    }
  }
  probe.remove();
  ladder = measured;
  return measured;
}

/** `36` → `" bar-md"`, `37` → `""`. Exact hits only: a near miss is the
 *  interesting case and dressing it as a rung would hide it. */
function rungOf(px: number): string {
  const hit = theLadder().find(([, value]) => Math.abs(value - px) < 0.5);
  return hit ? ` ${hit[0]}` : "";
}

// ── Formatting ───────────────────────────────────────────────────────────────

const round = (n: number) => Math.round(n * 10) / 10;

/** Collapse a four-sided value the way CSS shorthand would: `8px`, `0 8px`,
 *  or all four. Reading `0px 8px 0px 8px` four times is reading noise. */
function sides(cs: CSSStyleDeclaration, prop: string, suffix = ""): string {
  const [t = "0px", r = "0px", b = "0px", l = "0px"] = ["top", "right", "bottom", "left"].map(
    (side) => cs.getPropertyValue(`${prop}-${side}${suffix}`) || "0px",
  );
  if (t === r && r === b && b === l) return t === "0px" ? "" : t;
  if (t === b && r === l) return `${t} ${r}`;
  return `${t} ${r} ${b} ${l}`;
}

/** Values that mean "nobody said anything". Printing them is printing the
 *  absence of a decision, which is what makes a raw style dump unreadable. */
const SILENT = new Set(["none", "normal", "auto", "0px", "visible", "rgba(0, 0, 0, 0)", "static"]);

/**
 * Is this element on screen, and does it have a box?
 *
 * Both callers need it and both got it wrong independently, so it lives in one
 * place. Pruning has to distinguish *unrendered* from *boxless*, and the two
 * obvious tests each fail one case: a zero box prunes `display: contents` — the
 * app shell's own root is one, so a tree vanished at node one — and
 * `checkVisibility()` prunes it too, because CSSOM defines that as false for an
 * element with no associated box, which is exactly what `contents` means.
 *
 * So ask `display` directly. `none` is not rendered and takes its subtree with
 * it. `contents` renders its children through a box it does not have, and is
 * reported as such rather than measured.
 */
function rendered(el: Element): { box: DOMRect; contents: boolean } | null {
  const cs = getComputedStyle(el);
  if (cs.display === "none" || cs.visibility === "hidden") return null;
  const contents = cs.display === "contents";
  const box = el.getBoundingClientRect();
  if (box.width === 0 && box.height === 0 && !contents) return null;
  return { box, contents };
}

/**
 * One element's shape, as lines.
 *
 * Deliberately opinionated about what earns a line. The set is the union of
 * what the walk found broken and what the ladder governs; anything quiet — a
 * default, a transparent background, an unset max-width — is dropped unless
 * `all` is set, because the reason this is cheaper than a screenshot is that it
 * declines to say most of what it could.
 */
function shape(el: Element, all: boolean): string[] {
  const cs = getComputedStyle(el);
  const out: string[] = [];
  const say = (label: string, value: string) => {
    if (value && (all || !SILENT.has(value))) out.push(`  ${label.padEnd(7)}${value}`);
  };

  const box = el.getBoundingClientRect();
  out.push(`  ${"size".padEnd(7)}${round(box.width)}×${round(box.height)}${rungOf(box.height)}`);

  // The width-on-the-wrong-box class. A `max-width` on a wrapper caps a field
  // that already sized itself to its content, and nothing in a centred flex row
  // stretches it back — so an explicit constraint is always worth naming.
  const constraints = (["width", "min-width", "max-width", "flex"] as const)
    .map((p) => [p, cs.getPropertyValue(p)] as const)
    .filter(([, v]) => v && !SILENT.has(v) && v !== "0 1 auto")
    .map(([p, v]) => `${p}:${v}`)
    .join("  ");
  say("sizing", constraints);

  say("pad", sides(cs, "padding"));
  say("margin", sides(cs, "margin"));
  const [colGap, rowGap] = [cs.columnGap, cs.rowGap];
  say("gap", colGap === rowGap ? colGap : `${rowGap} / ${colGap}`);

  // The `justify-content` class. `.astryx-button` centres its label, and
  // `flex items-center` on the call site never argues with it because neither
  // utility names the property. So print the axis properties whenever the box
  // is a flex or grid container, even when they hold their initial value.
  const display = cs.display;
  if (display.includes("flex") || display.includes("grid")) {
    out.push(
      `  ${"layout".padEnd(7)}${display} ${cs.flexDirection}  align:${cs.alignItems}  justify:${cs.justifyContent}` +
        (cs.flexWrap === "nowrap" ? "" : `  wrap:${cs.flexWrap}`),
    );
  }

  say("radius", sides(cs, "border", "-radius") || cs.borderRadius);
  const borderWidth = sides(cs, "border", "-width");
  say("border", borderWidth ? `${borderWidth} ${cs.borderTopColor}` : "");
  say("shadow", cs.boxShadow);
  say("bg", cs.backgroundColor);

  // Type, but only where there is type. Reporting a font on a spacer is how a
  // dump gets long enough that nobody reads it.
  const ownText = Array.from(el.childNodes).some(
    (n) => n.nodeType === Node.TEXT_NODE && (n.textContent ?? "").trim() !== "",
  );
  if (ownText || all) {
    say(
      "font",
      `${cs.fontSize}/${cs.lineHeight} ${cs.fontWeight}` +
        (SILENT.has(cs.letterSpacing) ? "" : ` ${cs.letterSpacing}`) +
        (cs.textTransform === "none" ? "" : ` ${cs.textTransform}`),
    );
    say("color", cs.color);
  }

  say("overflow", cs.overflow);
  say("z", cs.position === "static" ? "" : `${cs.position} z:${cs.zIndex}`);
  return out;
}

// ── The surface ──────────────────────────────────────────────────────────────

/**
 * Measure everything matching `sel`, then say it once per distinct shape.
 *
 * The dedup is the point. A board column is forty rows that are the same row;
 * asking about "the row" and getting forty near-identical blocks back is how a
 * cheap read becomes an expensive one. Position is excluded from the
 * fingerprint — two tabs differing only in `x` are one shape — and reported
 * separately as the first match's origin, so the answer still tells you where
 * to look.
 *
 *   lait.look(".astryx-button")
 *   lait.look("[data-view] header", { all: true })
 */
function look(sel: string, opts: { all?: boolean; within?: string } = {}): string {
  const root = opts.within ? document.querySelector(opts.within) : document;
  if (!root) return `look(${sel}) — no element matches within:${opts.within}`;
  const all = opts.all ?? false;
  const els = Array.from(root.querySelectorAll(sel));
  if (els.length === 0) return `look(${sel}) — 0 matches`;

  // Unrendered matches are dropped, and this is not a nicety. A board answers
  // `.astryx-button` with 407 elements of which 382 measure 0×0 — collapsed
  // popovers and menus that exist in the tree and render nothing — and a report
  // dominated by boxes nobody can see is the same failure as a screenshot: too
  // much output to read. They are counted in the header rather than vanishing,
  // because "382 not rendered" is itself sometimes the answer.
  const live = all ? els : els.filter((el) => rendered(el) !== null);
  const hidden = els.length - live.length;
  if (live.length === 0) return `look(${sel}) — ${els.length} matches, none rendered`;

  const shapes = new Map<string, { n: number; at: string }>();
  for (const el of live) {
    const lines = shape(el, all).join("\n");
    const box = el.getBoundingClientRect();
    const seen = shapes.get(lines);
    if (seen) seen.n += 1;
    else shapes.set(lines, { n: 1, at: `(${round(box.x)},${round(box.y)})` });
  }

  const head =
    `look(${sel}) — ${live.length} rendered match${live.length === 1 ? "" : "es"}` +
    (hidden ? ` (+${hidden} not rendered)` : "") +
    `, ${shapes.size} shape${shapes.size === 1 ? "" : "s"}`;
  const shown = Array.from(shapes).slice(0, SHAPE_CAP);
  const body = shown.map(
    ([lines, { n, at }]) => `${n > 1 ? `${n}× ` : ""}first at ${at}\n${lines}`,
  );
  const dropped = shapes.size - shown.length;
  return [head, ...body, dropped > 0 ? `… ${dropped} further shapes not shown` : ""]
    .filter(Boolean)
    .join("\n");
}

/**
 * The structure, without the styling.
 *
 * What a screenshot is genuinely good at is "what is on this surface and how is
 * it nested" — so this is the text answer to that question. Identity is the
 * Astryx component class, a `data-*` hook, a role or an aria-label: the
 * app's utility classes are enormous and say nothing about which box this is.
 * Invisible nodes are skipped, because a tree full of unrendered branches is
 * the same problem as a style dump full of defaults.
 */
function tree(sel = "#root", depth = 4): string {
  const root = document.querySelector(sel);
  if (!root) return `tree(${sel}) — no match`;

  const lines: string[] = [];
  const walk = (el: Element, level: number) => {
    const vis = rendered(el);
    if (!vis) return;
    const { box, contents } = vis;

    const astryx = Array.from(el.classList).filter((c) => c.startsWith("astryx-"));
    const data = Array.from(el.attributes)
      .filter((a) => a.name.startsWith("data-") || a.name === "role" || a.name === "aria-label")
      .map((a) => (a.value ? `${a.name}=${a.value}` : a.name));
    const own = Array.from(el.childNodes)
      .filter((n) => n.nodeType === Node.TEXT_NODE)
      .map((n) => (n.textContent ?? "").trim())
      .join(" ")
      .slice(0, 40);

    lines.push(
      `${"  ".repeat(level)}${el.tagName.toLowerCase()}` +
        (astryx.length ? `.${astryx.join(".")}` : "") +
        (data.length ? ` [${data.join(" ")}]` : "") +
        (contents ? "  contents" : `  ${round(box.width)}×${round(box.height)}`) +
        (own ? `  "${own}"` : ""),
    );
    if (level + 1 < depth) for (const child of el.children) walk(child, level + 1);
  };
  walk(root, 0);
  return lines.join("\n");
}

/** Visible text, whitespace collapsed. `innerText` rather than `textContent`
 *  so a hidden branch does not turn up in the answer as if it rendered. */
function text(sel = "#root"): string {
  const el = document.querySelector(sel);
  if (!(el instanceof HTMLElement)) return `text(${sel}) — no match`;
  const collapsed = el.innerText.replace(/\n{2,}/g, "\n").replace(/[ \t]{2,}/g, " ").trim();
  return collapsed.length > TEXT_CAP ? `${collapsed.slice(0, TEXT_CAP)}\n… +${collapsed.length - TEXT_CAP} chars` : collapsed;
}

/**
 * Where the app currently is — the cheap orientation read.
 *
 * The route is canonical now (`/spaces/:space/projects/:project/...`), so the
 * URL is the honest answer to "which view" and needs no coupling to React
 * state. The open overlay is worth a line because it is the thing most likely
 * to explain why a `look` found nothing: a dialog owns the surface.
 */
function where(): string {
  const dialog = document.querySelector('[role="dialog"], .astryx-dialog');
  const heading = dialog?.querySelector("h1, h2, [class*=title]");
  return [
    `url    ${location.pathname}${location.search}${location.hash}`,
    `size   ${window.innerWidth}×${window.innerHeight} @${window.devicePixelRatio}x`,
    `theme  ${document.documentElement.dataset.theme ?? getComputedStyle(document.documentElement).colorScheme}`,
    dialog ? `dialog open — "${(heading?.textContent ?? "").trim().slice(0, 60)}"` : "",
  ]
    .filter(Boolean)
    .join("\n");
}

/**
 * Dispatch a navigation. **Does not wait for it, on purpose.**
 *
 * The obvious version of this function awaits a couple of frames and returns
 * the new `where()`, saving a round trip. It deadlocks the tab. CLAUDE.md
 * explains why a synthetic click is unreliable from automation — the re-render
 * it triggers detaches the calling eval's execution context — and awaiting here
 * walks into the same wall from the other side: `App.tsx` defers the actual
 * navigation to a fresh task precisely so the dispatcher's stack has unwound
 * before React re-renders, and an `await` holds that stack open across exactly
 * the render it was meant to miss. Observed cost: a 45s CDP timeout on a
 * navigation that had, in fact, already succeeded.
 *
 * So: dispatch, return, and let the caller ask `where()` in a second call. Two
 * cheap round trips beat one that hangs. `detail`'s grammar is documented in
 * CLAUDE.md and owned there, so it passes straight through rather than
 * restating a union that would drift.
 */
function go(detail: Record<string, unknown>): string {
  window.dispatchEvent(new CustomEvent("lait:nav", { detail }));
  return `dispatched ${JSON.stringify(detail)} — ask lait.where() in a separate call`;
}

/** The ladder itself, for when the question is "what rungs exist". */
function rungs(): string {
  return theLadder()
    .map(([name, px]) => `${name.padEnd(10)}${px}px`)
    .join("\n");
}

export const inspect = { look, tree, text, where, go, rungs };
export type Inspect = typeof inspect;
