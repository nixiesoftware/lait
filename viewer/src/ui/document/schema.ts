import { Schema, type MarkSpec, type NodeSpec } from "prosemirror-model";
import { tableNodes } from "prosemirror-tables";

import { CALLOUT_TONES, type CalloutTone } from "../../core/markdown";

const tone = (value: unknown): CalloutTone =>
  typeof value === "string" && (CALLOUT_TONES as readonly string[]).includes(value)
    ? value as CalloutTone
    : "note";

export function safeDocumentHref(value: unknown): string | null {
  if (typeof value !== "string") return null;
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

const nodes: Record<string, NodeSpec> = {
  doc: { content: "block+" },
  paragraph: {
    content: "inline*",
    group: "block",
    parseDOM: [{ tag: "p" }],
    toDOM: () => ["p", 0],
  },
  heading: {
    attrs: { level: { default: 1 } },
    content: "inline*",
    group: "block",
    defining: true,
    parseDOM: [1, 2, 3, 4].map((level) => ({ tag: `h${level}`, attrs: { level } })),
    toDOM: (node) => [`h${Math.max(1, Math.min(4, Number(node.attrs.level)))}`, 0],
  },
  blockquote: {
    content: "inline*",
    group: "block",
    defining: true,
    parseDOM: [{ tag: "blockquote" }],
    toDOM: () => ["blockquote", 0],
  },
  callout: {
    attrs: { tone: { default: "note" } },
    content: "inline*",
    group: "block",
    defining: true,
    parseDOM: [{
      tag: "aside.lait-doc-callout",
      getAttrs: (dom) => ({ tone: tone((dom as HTMLElement).dataset.tone) }),
    }],
    toDOM: (node) => [
      "aside",
      {
        class: `lait-doc-callout lait-doc-callout-${tone(node.attrs.tone)}`,
        "data-tone": tone(node.attrs.tone),
      },
      ["span", { class: "lait-doc-callout-label", contenteditable: "false" }, tone(node.attrs.tone)],
      ["div", 0],
    ],
  },
  code_block: {
    attrs: { language: { default: null } },
    content: "text*",
    marks: "",
    group: "block",
    code: true,
    defining: true,
    isolating: true,
    parseDOM: [{
      tag: "pre",
      preserveWhitespace: "full",
      getAttrs: (dom) => ({ language: (dom as HTMLElement).dataset.language || null }),
    }],
    toDOM: (node) => [
      "pre",
      { class: "lait-doc-code", "data-language": node.attrs.language ?? "" },
      ["code", 0],
    ],
  },
  horizontal_rule: {
    group: "block",
    atom: true,
    selectable: true,
    parseDOM: [{ tag: "hr" }],
    toDOM: () => ["hr"],
  },
  bullet_list: {
    content: "list_item+",
    group: "block",
    parseDOM: [{ tag: "ul" }],
    toDOM: () => ["ul", 0],
  },
  ordered_list: {
    content: "list_item+",
    group: "block",
    parseDOM: [{ tag: "ol" }],
    toDOM: () => ["ol", 0],
  },
  list_item: {
    attrs: { checked: { default: null } },
    content: "paragraph",
    defining: true,
    parseDOM: [{
      tag: "li",
      getAttrs: (dom) => {
        const checked = (dom as HTMLElement).dataset.checked;
        return { checked: checked === "true" ? true : checked === "false" ? false : null };
      },
    }],
    toDOM: (node) => {
      const checked = node.attrs.checked;
      if (typeof checked !== "boolean") return ["li", 0];
      return [
        "li",
        { class: "lait-doc-task", "data-checked": String(checked) },
        [
          "span",
          {
            class: "lait-doc-task-box",
            contenteditable: "false",
            "aria-hidden": "true",
          },
          checked ? "☑" : "☐",
        ],
        ["div", 0],
      ];
    },
  },
  text: { group: "inline" },
  hard_break: {
    inline: true,
    group: "inline",
    selectable: false,
    parseDOM: [{ tag: "br" }],
    toDOM: () => ["br"],
  },
  issue_ref: {
    attrs: { ref: {} },
    inline: true,
    group: "inline",
    atom: true,
    selectable: true,
    leafText: (node) => String(node.attrs.ref),
    parseDOM: [{
      tag: "span[data-ref]",
      getAttrs: (dom) => ({ ref: (dom as HTMLElement).dataset.ref ?? "" }),
    }],
    toDOM: (node) => ["span", { "data-ref": String(node.attrs.ref) }, String(node.attrs.ref)],
  },
  ...tableNodes({
    tableGroup: "block",
    cellContent: "inline*",
    cellAttributes: {
      align: {
        default: "left",
        getFromDOM: (dom) => dom.getAttribute("data-align") ?? "left",
        setDOMAttr: (value, attrs) => {
          attrs["data-align"] = value;
          attrs.style = `text-align: ${String(value)}`;
        },
      },
    },
  }),
};

const marks: Record<string, MarkSpec> = {
  strong: { parseDOM: [{ tag: "strong" }, { tag: "b" }], toDOM: () => ["strong", 0] },
  em: { parseDOM: [{ tag: "em" }, { tag: "i" }], toDOM: () => ["em", 0] },
  strike: { parseDOM: [{ tag: "s" }, { tag: "del" }], toDOM: () => ["s", 0] },
  underline: { parseDOM: [{ tag: "u" }], toDOM: () => ["u", 0] },
  code: {
    excludes: "_",
    code: true,
    parseDOM: [{ tag: "code" }],
    toDOM: () => ["code", { class: "lait-doc-inline-code" }, 0],
  },
  link: {
    attrs: { href: {} },
    inclusive: false,
    parseDOM: [{
      tag: "a[href]",
      getAttrs: (dom) => {
        const href = safeDocumentHref((dom as HTMLAnchorElement).href);
        return href ? { href } : false;
      },
    }],
    toDOM: (mark) => [
      "a",
      {
        href: safeDocumentHref(mark.attrs.href) ?? "#",
        target: "_blank",
        rel: "noreferrer noopener",
      },
      0,
    ],
  },
};

/**
 * The browser-side shape of Lait's controlled document vocabulary.
 *
 * This is deliberately a projection schema, not a persistence schema. JSON
 * produced by ProseMirror is never stored; canonical controlled Typst remains
 * the only durable and collaborative representation.
 */
export const laitDocumentSchema = new Schema({ nodes, marks });
