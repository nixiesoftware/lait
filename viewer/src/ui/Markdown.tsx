import { useEffect, useMemo, useState } from "react";
import {
  Check,
  Copy,
  Info,
  Lightbulb,
  MessageSquareWarning,
  OctagonAlert,
  TriangleAlert,
} from "lucide-react";

import { highlight, isHighlightable, type Token } from "../core/highlight";
import {
  looksLikeMarkdown,
  parseMarkdown,
  type Block,
  type CalloutTone,
  type Inline,
} from "../core/markdown";
import { cn } from "./primitives";

/**
 * Issue prose, rendered.
 *
 * The parse lives in `core/markdown.ts` (typed AST, unit-tested); this file only
 * turns that AST into elements. No string ever reaches `innerHTML`, so the
 * safety property is structural, not an escaping discipline.
 *
 * Plain text short-circuits: a description with no Markdown in it renders as the
 * same `pre-wrap` paragraph it always was, byte for byte. The formatting layer
 * must be invisible until asked for.
 *
 * **Typeset as a document.** This used to be tuned for the split pane — headings
 * a step off body size, one flat 8px between every block — and that pane is
 * gone. An issue body is a document now, and the ones agents write are shaped
 * like documentation: headings, fenced code, ordered steps. The vertical rhythm
 * and the type scale live in the `.prose` layer in `styles.css`, because
 * spacing here depends on what a block *follows*, which a per-element class
 * cannot say and the cascade can.
 */
export function Markdown({
  text,
  className,
  /** `tight` is the comment rhythm: same type, less air between blocks. */
  density = "document",
}: {
  text: string;
  className?: string;
  density?: "document" | "tight";
}) {
  const blocks = useMemo(
    () => (looksLikeMarkdown(text) ? parseMarkdown(text) : null),
    [text],
  );
  const prose = density === "tight" ? "prose prose-tight" : "prose";

  if (!blocks) {
    return <p className={cn(prose, "whitespace-pre-wrap", className)}>{text}</p>;
  }
  return (
    <div className={cn(prose, className)}>
      {blocks.map((b, i) => (
        <BlockView key={i} block={b} />
      ))}
    </div>
  );
}

function BlockView({ block }: { block: Block }) {
  switch (block.kind) {
    case "heading": {
      const Tag = `h${block.level}` as "h1";
      return (
        <Tag id={block.id} className="group/h scroll-mt-4">
          {inlines(block.children)}
          {/* The docs convention: a link to the section, revealed on hover so
              it costs the heading nothing while you are reading it. */}
          <a
            href={`#${block.id}`}
            aria-label="Link to this section"
            className="text-mute hover:text-fg ml-2 opacity-0 transition-opacity group-hover/h:opacity-100 focus-visible:opacity-100"
          >
            #
          </a>
        </Tag>
      );
    }
    case "paragraph":
      return <p className="whitespace-pre-wrap">{inlines(block.children)}</p>;
    case "quote":
      return (
        <blockquote className="border-line-strong text-dim border-l-2 pl-4 whitespace-pre-wrap italic">
          {inlines(block.children)}
        </blockquote>
      );
    case "callout":
      return <Callout tone={block.tone}>{inlines(block.children)}</Callout>;
    case "table":
      return (
        // The scroll container is the wrapper, not the table: a wide table has
        // to be reachable without the whole page scrolling sideways, and the
        // prose measure is narrow on purpose.
        <div className="prose-figure border-line overflow-x-auto rounded-lg border">
          <table className="w-full border-collapse text-[0.875em]">
            <thead>
              <tr className="border-line/70 bg-active/30 border-b">
                {block.head.map((cell, i) => (
                  <th
                    key={i}
                    scope="col"
                    className={cn(
                      "px-3 py-2 font-semibold",
                      ALIGN[block.align[i] ?? "left"],
                    )}
                  >
                    {inlines(cell)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, r) => (
                <tr key={r} className="border-line/40 last:border-0 border-b">
                  {row.map((cell, c) => (
                    <td
                      key={c}
                      className={cn("px-3 py-2 align-top", ALIGN[block.align[c] ?? "left"])}
                    >
                      {inlines(cell)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case "code":
      return <CodeBlock lang={block.lang} text={block.text} />;
    case "hr":
      return <hr className="border-line" />;
    case "list": {
      const Tag = block.ordered ? "ol" : "ul";
      return (
        <Tag className={block.ordered ? "list-decimal pl-6" : "list-disc pl-6"}>
          {block.items.map((item, i) => (
            <li key={i} className={item.checked !== null ? "-ml-6 list-none" : "pl-1"}>
              {item.checked !== null && (
                <input
                  type="checkbox"
                  checked={item.checked}
                  // Read-only on purpose: the checkbox is prose, not state — there
                  // is no per-character write path back into the CRDT from here.
                  readOnly
                  tabIndex={-1}
                  className="mr-2 align-middle"
                  aria-label={item.checked ? "Done" : "Not done"}
                />
              )}
              <span className={item.checked === true ? "text-mute line-through" : ""}>
                {inlines(item.children)}
              </span>
            </li>
          ))}
        </Tag>
      );
    }
  }
}

const ALIGN = {
  left: "text-left",
  center: "text-center",
  right: "text-right",
} as const;

/**
 * A callout, after Mintlify's prerequisites panel.
 *
 * Tinted from the tone's own colour rather than a neutral grey, because the
 * whole point of `> [!WARNING]` is that it should not read the same as
 * `> [!NOTE]` when you are skimming. The label is printed rather than left to
 * the icon: an issue body gets read in a terminal and a diff too, and "Warning"
 * survives both.
 */
const CALLOUT: Record<
  CalloutTone,
  { label: string; icon: typeof Info; tone: string; edge: string }
> = {
  note: { label: "Note", icon: Info, tone: "text-accent", edge: "border-accent/40 bg-accent/5" },
  tip: { label: "Tip", icon: Lightbulb, tone: "text-ok", edge: "border-ok/40 bg-ok/5" },
  important: {
    label: "Important",
    icon: MessageSquareWarning,
    tone: "text-accent",
    edge: "border-accent/40 bg-accent/5",
  },
  warning: {
    label: "Warning",
    icon: TriangleAlert,
    tone: "text-warn",
    edge: "border-warn/40 bg-warn/5",
  },
  caution: {
    label: "Caution",
    icon: OctagonAlert,
    tone: "text-danger",
    edge: "border-danger/40 bg-danger/5",
  },
};

function Callout({ tone, children }: { tone: CalloutTone; children: React.ReactNode }) {
  const { label, icon: Glyph, tone: colour, edge } = CALLOUT[tone];
  return (
    <div className={cn("prose-figure rounded-lg border px-4 py-3", edge)}>
      <div className={cn("mb-1 flex items-center gap-1.5 text-[0.875em] font-semibold", colour)}>
        <Glyph className="size-icon-md shrink-0" aria-hidden />
        {label}
      </div>
      <div className="whitespace-pre-wrap">{children}</div>
    </div>
  );
}

/**
 * A fenced block, after Mintlify's and Hashnode's: its own darker panel, the
 * language named in a strip along the top, and a copy button that is the whole
 * point of putting code on a page someone else will read.
 *
 * The panel is `bg-bg` — a step *below* the surface the issue sits on rather
 * than above it — so a long block reads as a well in the page instead of a card
 * floating over it. The type goes up to 13px too: the old `text-xs` made the
 * most quotable thing on the page the smallest, which is backwards for a
 * tracker where an agent's repro *is* the report.
 */
function CodeBlock({ lang, text }: { lang: string | null; text: string }) {
  const [copied, setCopied] = useState(false);
  const tokens = useHighlighted(text, lang);

  const copy = () => {
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    });
  };

  return (
    <div className="prose-figure border-line bg-bg group/code relative overflow-hidden rounded-lg border">
      {lang && (
        <div className="border-line/70 text-mute flex h-ctl-md items-center border-b px-3 font-mono text-2xs">
          {lang}
        </div>
      )}
      {/* Reveals on hover, but never on touch/keyboard-only — hence
          `focus-visible` and the always-on state once it has been used. */}
      <button
        type="button"
        onClick={copy}
        aria-label={copied ? "Copied" : "Copy code"}
        className={cn(
          "border-line bg-raised text-mute hover:text-fg hover:border-line-strong absolute right-2 flex size-ctl-sm items-center justify-center rounded border opacity-0 transition-opacity group-hover/code:opacity-100 focus-visible:opacity-100",
          lang ? "top-9" : "top-2",
          copied && "opacity-100",
        )}
      >
        {copied ? <Check className="text-ok size-icon-xs" /> : <Copy className="size-icon-xs" />}
      </button>
      <pre className="shiki-block overflow-x-auto p-3 font-mono text-[0.8125rem] leading-relaxed">
        <code>
          {tokens
            ? tokens.map((line, i) => (
                // The newline is emitted rather than relying on a block element
                // per line: `pre` already preserves it, and a `div` per line
                // would break selecting across the block.
                <span key={i}>
                  {line.map((token, j) => (
                    <span key={j} style={token.style as React.CSSProperties}>
                      {token.content}
                    </span>
                  ))}
                  {i < tokens.length - 1 ? "\n" : ""}
                </span>
              ))
            : text}
        </code>
      </pre>
    </div>
  );
}

/**
 * Colour for a block, once the highlighter has loaded.
 *
 * Returns `null` until then — and forever, for a language we do not carry — so
 * the block always renders its text immediately and gains colour a tick later.
 * Shiki is a dynamic import; making the code wait for it would mean an issue
 * body with a blank rectangle in it on first paint.
 */
function useHighlighted(text: string, lang: string | null): Token[][] | null {
  const [tokens, setTokens] = useState<Token[][] | null>(null);

  useEffect(() => {
    if (!isHighlightable(lang)) {
      setTokens(null);
      return;
    }
    let alive = true;
    void highlight(text, lang).then((next) => {
      if (alive) setTokens(next);
    });
    return () => {
      alive = false;
    };
  }, [text, lang]);

  return tokens;
}

function inlines(parts: Inline[]): React.ReactNode {
  return parts.map((p, i) => <InlineView key={i} inline={p} />);
}

function InlineView({ inline }: { inline: Inline }) {
  switch (inline.kind) {
    case "text":
      return inline.text;
    case "code":
      // Tinted rather than outlined: a border around every `foo` in a paragraph
      // draws more boxes than words. `0.875em` keeps the monospace optically
      // level with the prose around it instead of towering over it.
      //
      // Horizontal padding only. Padding the block *vertically* grows the line
      // box, so a paragraph carrying inline code was visibly more leaded than
      // the plain one under it — the chip has to sit in the line rather than
      // push it open.
      return (
        <code className="bg-active/60 text-fg rounded px-1 font-mono text-[0.875em]">
          {inline.text}
        </code>
      );
    case "strong":
      return <strong className="font-semibold">{inlines(inline.children)}</strong>;
    case "em":
      return <em>{inlines(inline.children)}</em>;
    case "strike":
      return <s className="text-mute">{inlines(inline.children)}</s>;
    case "link":
      // The parser admits only http(s) hrefs; `noreferrer` because an issue
      // tracker's prose links to the whole internet.
      return (
        <a
          href={inline.href}
          target="_blank"
          rel="noreferrer noopener"
          className="text-accent decoration-accent/40 underline underline-offset-2 hover:decoration-current"
        >
          {inlines(inline.children)}
        </a>
      );
  }
}
