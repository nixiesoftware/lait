/**
 * A small Markdown parser for issue prose.
 *
 * Descriptions and comments are plain CRDT text in the engine — Markdown is a
 * *reading* convention, not a storage format, so this parses on render and the
 * stored bytes stay exactly what the author typed (and what the CLI prints).
 *
 * Hand-rolled rather than a dependency, deliberately. This bundle is committed
 * into the `lait` binary (`src/serve/assets`), so a full CommonMark engine is
 * dead weight every install carries; and the safety argument wants to be short:
 * the parser emits a typed AST, the renderer builds React elements from it, and
 * no string is ever handed to `innerHTML` — XSS is unrepresentable rather than
 * escaped. The grammar is the subset people actually type into a tracker:
 * headings, emphasis, code, quotes, lists + checklists, fences, rules, links,
 * tables, and GitHub's alert callouts.
 *
 * The last two arrived with the document treatment. Tables were the gap that
 * mattered most in practice — an agent writing up a comparison reaches for one
 * immediately, and an unparsed table is not degraded prose, it is a wall of
 * pipes. Callouts (`> [!WARNING]`) are the same syntax GitHub renders, so the
 * text an agent already writes for a PR body lands here formatted.
 *
 * What it still deliberately does not do: nested lists, images, HTML
 * passthrough. Lines that don't parse stay visible as text — prose must never
 * be eaten by its formatting.
 *
 * `<u>` is the one exception to the no-HTML rule, and it is an exception on
 * purpose rather than the first crack in the wall. Markdown has no underline —
 * CommonMark, GFM and every extension of them spell emphasis with `*` and `_`
 * and leave underline to HTML, because in a document an underlined run that is
 * not a link is a word-processor habit. The toolbar has the button anyway, so
 * the choice is between a button that writes a tag this parser then renders as
 * literal text, and one tag admitted by name. It is admitted by name: the
 * pattern below matches `<u>…</u>` exactly and nothing else, so this is a
 * second spelling of emphasis, not an HTML subset with a boundary to defend.
 * Everything the safety argument rests on is unchanged — the tag becomes an
 * AST node, and no string reaches `innerHTML`.
 */

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; children: Inline[] }
  | { kind: "em"; children: Inline[] }
  | { kind: "strike"; children: Inline[] }
  | { kind: "underline"; children: Inline[] }
  /** A bare `KEY-7` that *might* name an issue. The parser cannot know — it has
   *  no catalog — so it reports the shape and the renderer resolves it. An
   *  unresolvable ref renders as the text it always was, which is what makes
   *  matching `UTF-8` and `COVID-19` free rather than wrong. */
  | { kind: "ref"; ref: string }
  | { kind: "link"; href: string; children: Inline[] };

export interface ListItem {
  /** `null` = plain bullet; boolean = checklist state. */
  checked: boolean | null;
  children: Inline[];
}

/** Column alignment from the delimiter row; `null` is the default (left). */
export type Align = "left" | "center" | "right" | null;

/** GitHub's alert kinds, which is the set agents already write. */
export const CALLOUT_TONES = ["note", "tip", "important", "warning", "caution"] as const;
export type CalloutTone = (typeof CALLOUT_TONES)[number];

export type Block =
  | { kind: "heading"; level: 1 | 2 | 3 | 4; id: string; children: Inline[] }
  /** Text runs keep their soft line breaks — render with `pre-wrap`. */
  | { kind: "paragraph"; children: Inline[] }
  | { kind: "quote"; children: Inline[] }
  | { kind: "callout"; tone: CalloutTone; children: Inline[] }
  | { kind: "code"; lang: string | null; text: string }
  | { kind: "list"; ordered: boolean; items: ListItem[] }
  | { kind: "table"; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { kind: "hr" };

const BULLET = /^\s*([-*+]|\d+[.)])\s+(.*)$/;
const CHECKBOX = /^\[([ xX])\]\s+(.*)$/;
const HEADING = /^(#{1,4})\s+(.*)$/;
const FENCE = /^```(\S*)\s*$/;
const HR = /^(?:-{3,}|_{3,}|\*{3,})\s*$/;
const QUOTE = /^>\s?(.*)$/;
/** `> [!NOTE]` on the first line of a quote turns it into a callout. */
const ALERT = /^\[!(note|tip|important|warning|caution)\]\s*$/i;
/** The `|---|:--:|` line that makes the row above a header. */
const TABLE_RULE = /^\s*\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$/;

/**
 * An issue alias as the engine derives it: a project KEY of 1–8 ASCII letters,
 * a sequence number, and the lowercase collision suffix a same-second pair of
 * issues gets (`ENG-7`, `ENG-7b`). Bounded on both sides by a non-word so a
 * path segment or a hyphenated compound cannot half-match — `x/ENG-1` finds it,
 * `SUPERSENG-1` does not.
 *
 * UPPERCASE ONLY, though the engine resolves an alias case-insensitively. The
 * display form everywhere in the product is uppercase, so that is what people
 * and agents type; accepting `step-1` and `part-2` as candidate refs would put
 * this pattern through ordinary prose for nothing. The cost is that a lowercase
 * `eng-142` renders as text — which is what an unresolvable ref does anyway.
 *
 * Exported because the CodeMirror live preview matches the same shape against
 * its own document, and two regexes for one grammar drift.
 */
export const ISSUE_REF = /(?<![\w-])([A-Z]{1,8}-\d+[a-z]*)(?![\w-])/;

/** Whether this text uses any Markdown at all — a plain paragraph should render
 *  exactly as it always has, without even entering the block path.
 *
 *  Refs are deliberately NOT a trigger. `UTF-8`, `SHA-256` and `COVID-19` all
 *  have a ref's shape, and a plain paragraph that mentions one must not change
 *  how its line breaks render just because something in it *might* be an issue.
 *  The plain path resolves refs on its own — see `parseRefs`. */
export function looksLikeMarkdown(text: string): boolean {
  return /(^|\n)\s*(#{1,4}\s|[-*+]\s|\d+[.)]\s|>|```|\|)|(\*\*|__|~~|`|\[[^\]]*\]\(|<u>)/.test(text);
}

/**
 * A heading's anchor.
 *
 * Deterministic from the text so a link into a section survives an edit
 * elsewhere in the document, and de-duplicated by the caller because two
 * sections called "Notes" are common and an ambiguous anchor silently jumps to
 * the wrong one.
 */
export function slug(text: string): string {
  return (
    text
      .toLowerCase()
      .replace(/[^\p{L}\p{N}]+/gu, "-")
      .replace(/^-+|-+$/g, "") || "section"
  );
}

/**
 * Split a table row into cells, honouring `\|` as a literal pipe.
 *
 * The outer pipes are stripped by hand rather than by regex. A single pattern
 * that both proves a line is a row and captures its interior kept the trailing
 * delimiter in the capture — `| a | b |` came back as three cells and an empty
 * fourth, which lined the header up one short of its own alignment row.
 */
function cells(line: string): string[] {
  let inner = line.trim();
  if (inner.startsWith("|")) inner = inner.slice(1);
  // `\\|` at the end is an escaped pipe and part of the last cell, not the edge.
  if (inner.endsWith("|") && !inner.endsWith("\\|")) inner = inner.slice(0, -1);

  const out: string[] = [];
  let cur = "";
  for (let c = 0; c < inner.length; c++) {
    const ch = inner[c]!;
    if (ch === "\\" && inner[c + 1] === "|") {
      cur += "|";
      c++;
    } else if (ch === "|") {
      out.push(cur.trim());
      cur = "";
    } else {
      cur += ch;
    }
  }
  out.push(cur.trim());
  return out;
}

/**
 * Join a run of lines into one paragraph, CommonMark-style.
 *
 * A lone newline inside a paragraph is a *space*, not a break. This used to
 * keep every source newline and render with `pre-wrap`, which was defensible
 * when the body was a 320px pane echoing what the CLI prints — but the body is
 * a document at a 35rem measure now, and text hard-wrapped at 78 columns came
 * out ragged, breaking mid-sentence wherever the author's editor happened to
 * wrap. The rendered width is the browser's business.
 *
 * An intentional break survives: a line ending in two spaces or a backslash
 * keeps its newline, which is how every other Markdown tool spells one.
 */
const HARD_BREAK = /( {2,}|\\)$/;

function joinSoft(lines: string[]): string {
  return lines
    .map((line, index) => {
      if (index === lines.length - 1) return line.trimEnd();
      if (HARD_BREAK.test(line)) return line.replace(HARD_BREAK, "") + "\n";
      return line.trimEnd() + " ";
    })
    .join("");
}

function alignments(rule: string): Align[] {
  return cells(rule).map((c) => {
    const left = c.startsWith(":");
    const right = c.endsWith(":");
    if (left && right) return "center";
    if (right) return "right";
    if (left) return "left";
    return null;
  });
}

export function parseMarkdown(text: string): Block[] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  /** Anchors already handed out, so a second "Notes" becomes `notes-2`. */
  const anchors = new Map<string, number>();
  let i = 0;

  /** Accumulate consecutive lines matching `test`, mapped through `pick`. */
  const run = (test: (l: string) => boolean, pick: (l: string) => string): string[] => {
    const out: string[] = [];
    while (i < lines.length && test(lines[i]!)) {
      out.push(pick(lines[i]!));
      i++;
    }
    return out;
  };

  while (i < lines.length) {
    const line = lines[i]!;

    if (line.trim() === "") {
      i++;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence) {
      i++;
      const body: string[] = [];
      while (i < lines.length && !FENCE.test(lines[i]!)) {
        body.push(lines[i]!);
        i++;
      }
      i++; // the closing fence (or EOF, which closes it too — never eat prose)
      blocks.push({ kind: "code", lang: fence[1] || null, text: body.join("\n") });
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      i++;
      const base = slug(heading[2]!);
      const seen = anchors.get(base) ?? 0;
      anchors.set(base, seen + 1);
      blocks.push({
        kind: "heading",
        level: heading[1]!.length as 1 | 2 | 3 | 4,
        id: seen === 0 ? base : `${base}-${seen + 1}`,
        children: parseInline(heading[2]!),
      });
      continue;
    }

    // A table is a row, then a delimiter row, then rows until something else.
    // Both leading lines are required: a lone pipe-bearing sentence is prose,
    // and treating it as a one-column table would eat it.
    if (line.includes("|") && i + 1 < lines.length && TABLE_RULE.test(lines[i + 1]!)) {
      const head = cells(line).map((c) => parseInline(c));
      const align = alignments(lines[i + 1]!);
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && lines[i]!.includes("|") && lines[i]!.trim() !== "") {
        rows.push(cells(lines[i]!).map((c) => parseInline(c)));
        i++;
      }
      blocks.push({ kind: "table", align, head, rows });
      continue;
    }

    if (HR.test(line)) {
      i++;
      blocks.push({ kind: "hr" });
      continue;
    }

    if (QUOTE.test(line)) {
      const body = run(
        (l) => QUOTE.test(l),
        (l) => QUOTE.exec(l)![1]!,
      );
      // GitHub's alert syntax: the marker is the whole first line, and what
      // follows is the callout's body. A quote that merely *starts* with
      // something bracket-shaped stays a quote.
      const alert = ALERT.exec(body[0]?.trim() ?? "");
      if (alert) {
        blocks.push({
          kind: "callout",
          tone: alert[1]!.toLowerCase() as CalloutTone,
          children: parseInline(joinSoft(body.slice(1)).trim()),
        });
        continue;
      }
      blocks.push({ kind: "quote", children: parseInline(joinSoft(body)) });
      continue;
    }

    const bullet = BULLET.exec(line);
    if (bullet) {
      const ordered = /^\d/.test(bullet[1]!);
      const items: ListItem[] = [];
      while (i < lines.length) {
        const m = BULLET.exec(lines[i]!);
        if (!m) break;
        i++;
        const check = CHECKBOX.exec(m[2]!);
        items.push(
          check
            ? { checked: check[1] !== " ", children: parseInline(check[2]!) }
            : { checked: null, children: parseInline(m[2]!) },
        );
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }

    // Paragraph: everything until a blank line or a line another block claims.
    // Written out rather than using `run` because a table has to be recognised
    // by *two* lines — without the look-ahead a table that follows a sentence
    // with no blank line between them is swallowed into the sentence.
    const para: string[] = [];
    while (i < lines.length) {
      const l = lines[i]!;
      if (
        l.trim() === "" ||
        HEADING.test(l) ||
        FENCE.test(l) ||
        HR.test(l) ||
        QUOTE.test(l) ||
        BULLET.test(l) ||
        (l.includes("|") && i + 1 < lines.length && TABLE_RULE.test(lines[i + 1]!))
      ) {
        break;
      }
      para.push(l);
      i++;
    }
    blocks.push({ kind: "paragraph", children: parseInline(joinSoft(para)) });
  }

  return blocks;
}

/**
 * Inline grammar, longest-marker-first so `**` is never read as two `*`.
 *
 * Each pattern's inner text is parsed recursively except code, whose content is
 * literal by definition. Links only keep `http(s)` hrefs — any other scheme
 * renders as the text it was, which is the safe reading of `javascript:`.
 */
const INLINE: Array<{
  re: RegExp;
  make: (m: RegExpExecArray) => Inline;
}> = [
  { re: /`([^`\n]+)`/, make: (m) => ({ kind: "code", text: m[1]! }) },
  { re: /\*\*([^*\n]+)\*\*/, make: (m) => ({ kind: "strong", children: parseInline(m[1]!) }) },
  { re: /__([^_\n]+)__/, make: (m) => ({ kind: "strong", children: parseInline(m[1]!) }) },
  { re: /~~([^~\n]+)~~/, make: (m) => ({ kind: "strike", children: parseInline(m[1]!) }) },
  {
    re: /<u>([^<\n]+)<\/u>/,
    make: (m) => ({ kind: "underline", children: parseInline(m[1]!) }),
  },
  { re: /\*([^*\n]+)\*/, make: (m) => ({ kind: "em", children: parseInline(m[1]!) }) },
  // `_`-emphasis must not fire inside snake_case_names: both edges are guarded.
  {
    re: /(?<![\w_])_([^_\n]+)_(?![\w_])/,
    make: (m) => ({ kind: "em", children: parseInline(m[1]!) }),
  },
  {
    re: /\[([^\]\n]+)\]\((https?:\/\/[^)\s]+)\)/,
    make: (m) => ({ kind: "link", href: m[2]!, children: parseInline(m[1]!) }),
  },
  {
    // Bare URLs, with trailing punctuation left to the sentence it belongs to.
    re: /https?:\/\/[^\s<>()]*[^\s<>().,;:!?'"]/,
    make: (m) => ({ kind: "link", href: m[0], children: [{ kind: "text", text: m[0] }] }),
  },
  // Last, so that on a tie every explicit marker wins. It cannot fire inside a
  // URL or a link target either: both patterns start earlier than the ref they
  // contain, and only a link's *text* is parsed recursively.
  { re: ISSUE_REF, make: (m) => ({ kind: "ref", ref: m[1]! }) },
];

/**
 * Refs and nothing else.
 *
 * The plain-text path in the renderer needs chips without paying for the block
 * grammar: a description with no Markdown in it renders inside one `pre-wrap`
 * paragraph, byte for byte, and running `parseInline` over it would collapse
 * its line breaks and start reading stray asterisks as emphasis. This walks the
 * same `ISSUE_REF` and leaves every other character exactly where it was.
 */
export function parseRefs(text: string): Inline[] {
  const out: Inline[] = [];
  let rest = text;
  while (rest.length > 0) {
    const m = ISSUE_REF.exec(rest);
    if (!m) {
      out.push({ kind: "text", text: rest });
      break;
    }
    if (m.index > 0) out.push({ kind: "text", text: rest.slice(0, m.index) });
    out.push({ kind: "ref", ref: m[1]! });
    rest = rest.slice(m.index + m[0].length);
  }
  return out;
}

export function parseInline(text: string): Inline[] {
  const out: Inline[] = [];
  let rest = text;
  while (rest.length > 0) {
    // The *earliest* match wins; ties go to the earlier pattern in the table,
    // which is what makes `**` beat `*` (it is listed first and matches at the
    // same index).
    let best: { at: number; m: RegExpExecArray; make: (m: RegExpExecArray) => Inline } | null =
      null;
    for (const { re, make } of INLINE) {
      const m = re.exec(rest);
      if (m && (best === null || m.index < best.at)) best = { at: m.index, m, make };
    }
    if (!best) {
      out.push({ kind: "text", text: rest });
      break;
    }
    if (best.at > 0) out.push({ kind: "text", text: rest.slice(0, best.at) });
    out.push(best.make(best.m));
    rest = rest.slice(best.at + best.m[0].length);
  }
  return out;
}
