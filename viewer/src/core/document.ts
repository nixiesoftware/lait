import {
  CALLOUT_TONES,
  parseMarkdown,
  parseRefs,
  type Align,
  type Block,
  type CalloutTone,
  type Inline,
} from "./markdown";

/** Current version of Lait's user-invisible issue document model. */
export const DOCUMENT_SCHEMA = 1;
/** Storage discriminator. It is collapsed by the editor and ignored by the renderer. */
export const DOCUMENT_PREFIX = "// lait-document:1\n";

export interface DocumentSplice {
  /** Unicode-scalar offset in the source as it exists before this splice. */
  index: number;
  delete: number;
  insert: string;
}

export interface DocumentUpgrade {
  source: string;
  /** Applied in order, from the end of the legacy source to its beginning. */
  splices: DocumentSplice[];
}

/**
 * Convert the legacy Markdown grammar into canonical Typst source.
 *
 * Typst remains an implementation detail: callers render the returned source
 * through {@link parseDocument}, and editor decorations hide every structural
 * token. The `lait-*` functions belong to the restricted compiler World rather
 * than to user source, which keeps generated constructs explicit and editable.
 */
export function upgradeMarkdown(markdown: string): DocumentUpgrade {
  const source = serializeDocument(parseMarkdown(markdown));
  return { source, splices: sourceSplices(markdown, source) };
}

/** Plain text entered in the new-issue composer, escaped as Typst markup. */
export function plainDocument(text: string): string {
  return DOCUMENT_PREFIX + escapeText(text.replace(/\r\n?/g, "\n"));
}

export function serializeDocument(blocks: readonly Block[]): string {
  return DOCUMENT_PREFIX + blocks.map(serializeBlock).join("\n\n");
}

function serializeBlock(block: Block): string {
  switch (block.kind) {
    case "heading":
      return `${"=".repeat(block.level)} ${serializeInlines(block.children)}`;
    case "paragraph":
      return serializeInlines(block.children);
    case "quote":
      return `#quote(block: true)[${serializeInlines(block.children)}]`;
    case "callout":
      return `#lait-callout(${JSON.stringify(block.tone)})[${serializeInlines(block.children)}]`;
    case "code": {
      if (!block.text.split("\n").some((line) => line.startsWith("```"))) {
        return `\`\`\`${block.lang ?? ""}\n${block.text}\n\`\`\``;
      }
      const language = block.lang ? `, lang: ${JSON.stringify(block.lang)}` : "";
      return `#raw(block: true${language}, ${JSON.stringify(block.text)})`;
    }
    case "hr":
      return "#line(length: 100%)";
    case "list":
      return block.items
        .map((item) => {
          const body = serializeInlines(item.children);
          if (item.checked !== null) return `#lait-task(${item.checked})[${body}]`;
          return `${block.ordered ? "+" : "-"} ${body}`;
        })
        .join("\n");
    case "table": {
      const align = block.align.map((value) => JSON.stringify(value ?? "left")).join(", ");
      const header = block.head.map((cell) => `[${serializeInlines(cell)}]`).join(", ");
      const rows = block.rows
        .map((row) => `    (${row.map((cell) => `[${serializeInlines(cell)}]`).join(", ")}),`)
        .join("\n");
      return `#lait-table(\n  align: (${align}),\n  header: (${header}),\n  rows: (\n${rows}\n  ),\n)`;
    }
  }
}

function serializeInlines(inlines: readonly Inline[]): string {
  return inlines.map(serializeInline).join("");
}

function serializeInline(inline: Inline): string {
  switch (inline.kind) {
    case "text":
      return escapeText(inline.text);
    case "ref":
      return escapeText(inline.ref);
    case "code":
      return `#raw(${JSON.stringify(inline.text)})`;
    case "strong":
      return `*${serializeInlines(inline.children)}*`;
    case "em":
      return `_${serializeInlines(inline.children)}_`;
    case "strike":
      return `#strike[${serializeInlines(inline.children)}]`;
    case "underline":
      return `#underline[${serializeInlines(inline.children)}]`;
    case "link":
      return `#link(${JSON.stringify(inline.href)})[${serializeInlines(inline.children)}]`;
  }
}

/** Escape every Typst markup introducer that may occur in ordinary prose. */
export function escapeText(text: string): string {
  return text.replace(/[\\#\[\]*_`$<>@]/g, "\\$&");
}

/** Parse only the canonical, restricted surface Lait writes. */
export function parseDocument(source: string): Block[] {
  const canonical = source.startsWith(DOCUMENT_PREFIX)
    ? source.slice(DOCUMENT_PREFIX.length)
    : source;
  const lines = canonical.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  const anchors = new Map<string, number>();
  let i = 0;

  while (i < lines.length) {
    const line = lines[i]!;
    if (!line.trim()) {
      i += 1;
      continue;
    }

    const tableCall = readStandaloneCall(lines, i, "lait-table");
    const table = tableCall ? parseTable(tableCall.call) : null;
    if (tableCall && table) {
      blocks.push(table);
      i = tableCall.next;
      continue;
    }

    const fence = /^```(\S*)\s*$/.exec(line);
    if (fence) {
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !/^```\s*$/.test(lines[i]!)) body.push(lines[i++]!);
      if (i < lines.length) i += 1;
      blocks.push({ kind: "code", lang: fence[1] || null, text: body.join("\n") });
      continue;
    }

    const heading = /^(={1,4})\s+(.*)$/.exec(line);
    if (heading) {
      const children = parseDocumentInline(heading[2]!);
      const base = slug(inlineText(children));
      const seen = anchors.get(base) ?? 0;
      anchors.set(base, seen + 1);
      blocks.push({
        kind: "heading",
        level: heading[1]!.length as 1 | 2 | 3 | 4,
        id: seen === 0 ? base : `${base}-${seen + 1}`,
        children,
      });
      i += 1;
      continue;
    }

    const raw = parseCall(line, "raw");
    if (raw?.args.includes("block: true")) {
      const strings = stringArguments(raw.args);
      const lang = /lang:\s*("(?:\\.|[^"])*")/.exec(raw.args);
      blocks.push({
        kind: "code",
        lang: lang ? (JSON.parse(lang[1]!) as string) : null,
        text: strings.at(-1) ?? "",
      });
      i += 1;
      continue;
    }

    if (line === "#line(length: 100%)") {
      blocks.push({ kind: "hr" });
      i += 1;
      continue;
    }

    const quote = parseCall(line, "quote");
    if (quote && quote.content !== null) {
      blocks.push({ kind: "quote", children: parseDocumentInline(quote.content) });
      i += 1;
      continue;
    }

    const calloutCall = readStandaloneCall(lines, i, "lait-callout");
    const callout = calloutCall ? parseCallout(calloutCall.call) : null;
    if (calloutCall && callout) {
      blocks.push({
        kind: "callout",
        tone: callout.tone,
        children: parseDocumentInline(callout.content),
      });
      i = calloutCall.next;
      continue;
    }

    const list: Extract<Block, { kind: "list" }>["items"] = [];
    let ordered: boolean | null = null;
    while (i < lines.length) {
      const current = lines[i]!;
      const item = /^([+-])\s+(.*)$/.exec(current);
      const task = parseCall(current, "lait-task");
      if (item) {
        const isOrdered = item[1] === "+";
        if (ordered !== null && ordered !== isOrdered) break;
        ordered = isOrdered;
        list.push({ checked: null, children: parseDocumentInline(item[2]!) });
        i += 1;
        continue;
      }
      if (task && task.content !== null) {
        if (ordered !== null && ordered) break;
        ordered = false;
        list.push({
          checked: task.args.trim() === "true",
          children: parseDocumentInline(task.content),
        });
        i += 1;
        continue;
      }
      break;
    }
    if (ordered !== null) {
      blocks.push({ kind: "list", ordered, items: list });
      continue;
    }

    const paragraph: string[] = [];
    while (i < lines.length && lines[i]!.trim()) {
      paragraph.push(lines[i]!);
      i += 1;
    }
    blocks.push({ kind: "paragraph", children: parseDocumentInline(paragraph.join("\n")) });
  }
  return blocks;
}

/** Plain text for clipboard/export surfaces that must not expose serialization. */
export function documentPlainText(source: string): string {
  const inlineText = (inlines: readonly Inline[]): string => inlines
    .map((inline) => inline.kind === "text" || inline.kind === "code"
      ? inline.text
      : inline.kind === "ref"
        ? inline.ref
        : inlineText(inline.children))
    .join("");
  return parseDocument(source).map((block) => {
    switch (block.kind) {
      case "heading":
      case "paragraph":
      case "quote":
      case "callout":
        return inlineText(block.children);
      case "code":
        return block.text;
      case "hr":
        return "";
      case "list":
        return block.items.map((item) => inlineText(item.children)).join("\n");
      case "table":
        return [block.head, ...block.rows]
          .map((row) => row.map(inlineText).join("\t"))
          .join("\n");
    }
  }).join("\n\n");
}

function parseTable(call: ParsedCall): Extract<Block, { kind: "table" }> | null {
  const alignSource = namedArgument(call.args, "align");
  const headerSource = namedArgument(call.args, "header");
  const rowsSource = namedArgument(call.args, "rows");
  const alignBody = alignSource === null ? "" : tupleBody(alignSource);
  const headerBody = headerSource === null ? null : tupleBody(headerSource);
  const rowsBody = rowsSource === null ? null : tupleBody(rowsSource);
  if (alignBody === null || headerBody === null || rowsBody === null) return null;

  const align = stringArguments(alignBody).map((value) =>
    value === "center" || value === "right" ? value : "left",
  ) as Align[];
  const head = parseContentTuple(headerBody);
  if (head === null) return null;

  const rows: Inline[][][] = [];
  for (const rowSource of topLevelItems(rowsBody)) {
    const body = tupleBody(rowSource);
    if (body === null) return null;
    const row = parseContentTuple(body);
    if (row === null) return null;
    rows.push(row);
  }
  return { kind: "table", align, head, rows };
}

function parseContentTuple(source: string): Inline[][] | null {
  const cells: Inline[][] = [];
  for (const item of topLevelItems(source)) {
    const body = contentBody(item);
    if (body === null) return null;
    cells.push(parseDocumentInline(body));
  }
  return cells;
}

function parseCallout(call: ParsedCall): { tone: CalloutTone; content: string } | null {
  const args = topLevelItems(call.args);
  const value = stringArguments(args[0] ?? "")[0];
  const tone = typeof value === "string" && (CALLOUT_TONES as readonly string[]).includes(value)
    ? value as CalloutTone
    : "note";
  const content = call.content ?? contentBody(args[1] ?? "");
  return content === null ? null : { tone, content };
}

/** Parse canonical inline Typst without evaluating code or accepting HTML. */
export function parseDocumentInline(source: string): Inline[] {
  const out: Inline[] = [];
  let text = "";
  const flush = () => {
    if (!text) return;
    out.push(...parseRefs(text));
    text = "";
  };

  for (let i = 0; i < source.length;) {
    const char = source[i]!;
    if (char === "\\" && i + 1 < source.length) {
      text += source[i + 1]!;
      i += 2;
      continue;
    }
    if (char === "*" || char === "_") {
      const close = findUnescaped(source, char, i + 1);
      if (close >= 0) {
        flush();
        const children = parseDocumentInline(source.slice(i + 1, close));
        out.push(char === "*" ? { kind: "strong", children } : { kind: "em", children });
        i = close + 1;
        continue;
      }
    }
    if (source.startsWith("#raw(", i)) {
      const end = findClosing(source, i + 4, "(", ")");
      if (end >= 0) {
        const value = stringArguments(source.slice(i + 5, end))[0];
        if (value !== undefined) {
          flush();
          out.push({ kind: "code", text: value });
          i = end + 1;
          continue;
        }
      }
    }
    const named = ["strike", "underline", "link"] as const;
    let consumed = false;
    for (const name of named) {
      if (!source.startsWith(`#${name}`, i)) continue;
      const call = parseCall(source.slice(i), name);
      if (!call || call.content === null) continue;
      flush();
      const children = parseDocumentInline(call.content);
      if (name === "link") {
        out.push({ kind: "link", href: stringArguments(call.args)[0] ?? "", children });
      } else {
        out.push({ kind: name, children });
      }
      i += call.length;
      consumed = true;
      break;
    }
    if (consumed) continue;
    text += char;
    i += 1;
  }
  flush();
  return out;
}

interface ParsedCall {
  args: string;
  content: string | null;
  length: number;
}

/**
 * Read one block-level function call without prescribing its line breaks.
 *
 * Typst treats parenthesized arguments and trailing content blocks as one call,
 * even when either spans lines. The compiler accepts that grammar, so the safe
 * browser projection must not make visualization depend on the serializer's
 * preferred whitespace.
 */
function readStandaloneCall(
  lines: readonly string[],
  start: number,
  name: string,
): { call: ParsedCall; next: number } | null {
  const remaining = lines.slice(start).join("\n");
  const leading = /^[ \t]*/.exec(remaining)?.[0].length ?? 0;
  const call = parseCall(remaining.slice(leading), name);
  if (!call) return null;

  const end = leading + call.length;
  const lineEnd = remaining.indexOf("\n", end);
  const tail = remaining.slice(end, lineEnd < 0 ? remaining.length : lineEnd);
  if (tail.trim()) return null;
  return {
    call,
    next: start + remaining.slice(0, end).split("\n").length,
  };
}

function parseCall(source: string, name: string): ParsedCall | null {
  const prefix = `#${name}(`;
  if (!source.startsWith(prefix)) return null;
  const close = findClosing(source, prefix.length - 1, "(", ")");
  if (close < 0) return null;
  const args = source.slice(prefix.length, close);
  if (source[close + 1] !== "[") return { args, content: null, length: close + 1 };
  const contentClose = findClosing(source, close + 1, "[", "]");
  if (contentClose < 0) return null;
  return {
    args,
    content: source.slice(close + 2, contentClose),
    length: contentClose + 1,
  };
}

/** Split a Typst argument/tuple body on commas outside nested values. */
function topLevelItems(source: string): string[] {
  const out: string[] = [];
  const stack: string[] = [];
  let start = 0;
  let string = false;
  const closeFor: Record<string, string> = { "(": ")", "[": "]", "{": "}" };

  for (let i = 0; i < source.length; i += 1) {
    const char = source[i]!;
    if (char === "\\") {
      i += 1;
      continue;
    }
    if (char === '"') {
      string = !string;
      continue;
    }
    if (string) continue;
    const close = closeFor[char];
    if (close) {
      stack.push(close);
      continue;
    }
    if (stack.at(-1) === char) {
      stack.pop();
      continue;
    }
    if (char === "," && stack.length === 0) {
      const item = source.slice(start, i).trim();
      if (item) out.push(item);
      start = i + 1;
    }
  }

  const item = source.slice(start).trim();
  if (item) out.push(item);
  return out;
}

function namedArgument(source: string, name: string): string | null {
  for (const item of topLevelItems(source)) {
    const match = /^([\w-]+)\s*:\s*([\s\S]*)$/.exec(item);
    if (match?.[1] === name) return match[2]!.trim();
  }
  return null;
}

function delimitedBody(source: string, open: "(" | "[", close: ")" | "]"): string | null {
  const value = source.trim();
  if (!value.startsWith(open)) return null;
  const end = findClosing(value, 0, open, close);
  return end === value.length - 1 ? value.slice(1, -1) : null;
}

function tupleBody(source: string): string | null {
  return delimitedBody(source, "(", ")");
}

function contentBody(source: string): string | null {
  return delimitedBody(source, "[", "]");
}

function findClosing(source: string, openAt: number, open: string, close: string): number {
  let depth = 0;
  let string = false;
  for (let i = openAt; i < source.length; i += 1) {
    const char = source[i]!;
    if (char === "\\") {
      i += 1;
      continue;
    }
    if (char === '"') string = !string;
    if (string) continue;
    if (char === open) depth += 1;
    if (char === close && --depth === 0) return i;
  }
  return -1;
}

function findUnescaped(source: string, needle: string, start: number): number {
  for (let i = start; i < source.length; i += 1) {
    if (source[i] === "\\") i += 1;
    else if (source[i] === needle) return i;
  }
  return -1;
}

function stringArguments(source: string): string[] {
  const out: string[] = [];
  const pattern = /"(?:\\.|[^"\\])*"/g;
  for (let match = pattern.exec(source); match; match = pattern.exec(source)) {
    try {
      out.push(JSON.parse(match[0]) as string);
    } catch {
      // Invalid canonical source stays visible through the plain-text fallback.
    }
  }
  return out;
}

function inlineText(inlines: readonly Inline[]): string {
  return inlines
    .map((inline) =>
      inline.kind === "text" || inline.kind === "code"
        ? inline.text
        : inline.kind === "ref"
          ? inline.ref
          : inlineText(inline.children),
    )
    .join("");
}

function slug(text: string): string {
  return text
    .toLowerCase()
    .trim()
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-|-$/g, "") || "section";
}

type Diff = { kind: "equal" | "delete" | "insert"; value: string };

/**
 * Myers' shortest-edit script over Unicode scalars. Keeping equal runs as
 * equal is the migration's anchor-preservation guarantee.
 */
export function sourceSplices(before: string, after: string): DocumentSplice[] {
  const a = Array.from(before);
  const b = Array.from(after);
  if (before === after) return [];
  const trace: Map<number, number>[] = [];
  let frontier = new Map<number, number>([[1, 0]]);
  const limit = a.length + b.length;

  for (let distance = 0; distance <= limit; distance += 1) {
    trace.push(new Map(frontier));
    const next = new Map<number, number>();
    for (let diagonal = -distance; diagonal <= distance; diagonal += 2) {
      const down = frontier.get(diagonal + 1) ?? Number.NEGATIVE_INFINITY;
      const right = frontier.get(diagonal - 1) ?? Number.NEGATIVE_INFINITY;
      let x = diagonal === -distance || (diagonal !== distance && right < down)
        ? down
        : right + 1;
      if (!Number.isFinite(x)) x = 0;
      let y = x - diagonal;
      while (x < a.length && y < b.length && a[x] === b[y]) {
        x += 1;
        y += 1;
      }
      next.set(diagonal, x);
      if (x >= a.length && y >= b.length) {
        const diffs = backtrack(trace, next, a, b, distance);
        return compressDiffs(diffs).reverse();
      }
    }
    frontier = next;
  }
  return [{ index: 0, delete: a.length, insert: after }];
}

function backtrack(
  trace: readonly Map<number, number>[],
  final: Map<number, number>,
  a: readonly string[],
  b: readonly string[],
  distance: number,
): Diff[] {
  const diffs: Diff[] = [];
  let x = a.length;
  let y = b.length;
  let current = final;

  for (let d = distance; d > 0; d -= 1) {
    const diagonal = x - y;
    const previous = trace[d]!;
    const down = previous.get(diagonal + 1) ?? Number.NEGATIVE_INFINITY;
    const right = previous.get(diagonal - 1) ?? Number.NEGATIVE_INFINITY;
    const previousDiagonal = diagonal === -d || (diagonal !== d && right < down)
      ? diagonal + 1
      : diagonal - 1;
    const previousX = previous.get(previousDiagonal) ?? 0;
    const previousY = previousX - previousDiagonal;

    while (x > previousX && y > previousY) {
      diffs.push({ kind: "equal", value: a[x - 1]! });
      x -= 1;
      y -= 1;
    }
    if (x === previousX) {
      diffs.push({ kind: "insert", value: b[y - 1]! });
      y -= 1;
    } else {
      diffs.push({ kind: "delete", value: a[x - 1]! });
      x -= 1;
    }
    current = previous;
  }
  void current;
  while (x > 0 && y > 0) {
    diffs.push({ kind: "equal", value: a[x - 1]! });
    x -= 1;
    y -= 1;
  }
  while (x-- > 0) diffs.push({ kind: "delete", value: a[x]! });
  while (y-- > 0) diffs.push({ kind: "insert", value: b[y]! });
  return diffs.reverse();
}

function compressDiffs(diffs: readonly Diff[]): DocumentSplice[] {
  const splices: DocumentSplice[] = [];
  let index = 0;
  let start: number | null = null;
  let remove = 0;
  let insert = "";
  const flush = () => {
    if (start !== null && (remove > 0 || insert)) {
      splices.push({ index: start, delete: remove, insert });
    }
    start = null;
    remove = 0;
    insert = "";
  };
  for (const diff of diffs) {
    if (diff.kind === "equal") {
      flush();
      index += 1;
    } else if (diff.kind === "delete") {
      start ??= index;
      remove += 1;
      index += 1;
    } else {
      start ??= index;
      insert += diff.value;
    }
  }
  flush();
  return splices;
}

/** Test/helper mirror of the server's ordered-splice validation. */
export function applySourceSplices(source: string, splices: readonly DocumentSplice[]): string {
  const chars = Array.from(source);
  for (const splice of splices) {
    chars.splice(splice.index, splice.delete, ...Array.from(splice.insert));
  }
  return chars.join("");
}
