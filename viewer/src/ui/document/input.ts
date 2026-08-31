import { Fragment, type Node as ProseMirrorNode } from "prosemirror-model";
import { TextSelection, type EditorState, type Transaction } from "prosemirror-state";

import { safeDocumentHref } from "./schema";

export type DocumentBlockCommand =
  | "paragraph"
  | "heading-1"
  | "heading-2"
  | "heading-3"
  | "heading-4"
  | "bullet-list"
  | "ordered-list"
  | "task-list"
  | "quote"
  | "code"
  | "divider"
  | "callout";

export interface SlashCommand {
  readonly id: DocumentBlockCommand;
  readonly label: string;
  readonly hint: string;
  readonly keywords: readonly string[];
}

export const DOCUMENT_SLASH_COMMANDS: readonly SlashCommand[] = [
  { id: "paragraph", label: "Text", hint: "Plain paragraph", keywords: ["text", "paragraph"] },
  { id: "heading-1", label: "Heading 1", hint: "Large section heading", keywords: ["heading", "h1", "title"] },
  { id: "heading-2", label: "Heading 2", hint: "Medium section heading", keywords: ["heading", "h2", "subtitle"] },
  { id: "heading-3", label: "Heading 3", hint: "Small section heading", keywords: ["heading", "h3"] },
  { id: "heading-4", label: "Heading 4", hint: "Compact section heading", keywords: ["heading", "h4"] },
  { id: "bullet-list", label: "Bullet list", hint: "Unordered list", keywords: ["bullet", "unordered", "list"] },
  { id: "ordered-list", label: "Numbered list", hint: "Ordered list", keywords: ["number", "ordered", "list"] },
  { id: "task-list", label: "Task list", hint: "List with checkboxes", keywords: ["task", "todo", "check", "list"] },
  { id: "quote", label: "Quote", hint: "Quoted passage", keywords: ["quote", "blockquote"] },
  { id: "code", label: "Code block", hint: "Fenced source code", keywords: ["code", "pre", "fence"] },
  { id: "divider", label: "Divider", hint: "Horizontal rule", keywords: ["divider", "rule", "separator"] },
  { id: "callout", label: "Callout", hint: "Highlighted note", keywords: ["callout", "note", "aside"] },
];

function emptyBlock(state: EditorState, command: DocumentBlockCommand): {
  node: ProseMirrorNode | Fragment;
  cursor: number;
} {
  const { schema } = state;
  const paragraph = schema.nodes.paragraph!.create();
  switch (command) {
    case "paragraph":
      return { node: paragraph, cursor: 1 };
    case "heading-1":
    case "heading-2":
    case "heading-3":
    case "heading-4":
      return {
        node: schema.nodes.heading!.create({ level: Number(command.at(-1)) }),
        cursor: 1,
      };
    case "quote":
      return { node: schema.nodes.blockquote!.create(), cursor: 1 };
    case "code":
      return { node: schema.nodes.code_block!.create({ language: null }), cursor: 1 };
    case "callout":
      return { node: schema.nodes.callout!.create({ tone: "note" }), cursor: 1 };
    case "bullet-list":
    case "ordered-list":
    case "task-list": {
      const item = schema.nodes.list_item!.create(
        { checked: command === "task-list" ? false : null },
        paragraph,
      );
      return {
        node: schema.nodes[command === "ordered-list" ? "ordered_list" : "bullet_list"]!
          .create(null, item),
        // list + item + paragraph opening tokens
        cursor: 3,
      };
    }
    case "divider":
      return {
        node: Fragment.fromArray([schema.nodes.horizontal_rule!.create(), paragraph]),
        // The rule is an atom of size one; put the caret in the following paragraph.
        cursor: 2,
      };
  }
}

/** Replace the current top-level paragraph with one semantic document block. */
export function applyDocumentBlock(
  state: EditorState,
  command: DocumentBlockCommand,
): Transaction | null {
  const selection = state.selection;
  if (!selection.empty || selection.$from.depth !== 1) return null;
  if (selection.$from.parent.type !== state.schema.nodes.paragraph) return null;
  const from = selection.$from.before();
  const to = selection.$from.after();
  const replacement = emptyBlock(state, command);
  const transaction = state.tr.replaceWith(from, to, replacement.node);
  return transaction.setSelection(TextSelection.create(transaction.doc, from + replacement.cursor));
}

/**
 * Markdown block gestures for the rich editor.
 *
 * The punctuation is never persisted. Once a complete marker is typed at the
 * beginning of an empty paragraph, it becomes a semantic ProseMirror node; the
 * projection then serializes that node to canonical Typst.
 */
export function markdownBlockInput(
  state: EditorState,
  from: number,
  to: number,
  text: string,
): Transaction | null {
  if (from !== to || text !== " ") return null;
  const $from = state.doc.resolve(from);
  if ($from.depth !== 1 || $from.parent.type !== state.schema.nodes.paragraph) return null;
  if ($from.parentOffset !== $from.parent.content.size) return null;
  const marker = $from.parent.textBetween(0, $from.parentOffset, "", "\ufffc");
  const command: DocumentBlockCommand | null = /^#{1,4}$/.test(marker)
    ? `heading-${marker.length}` as DocumentBlockCommand
    : marker === "-" || marker === "*"
      ? "bullet-list"
      : /^\d+[.)]$/.test(marker)
        ? "ordered-list"
        : marker === "- [ ]" || marker === "* [ ]"
          ? "task-list"
          : marker === ">"
            ? "quote"
            : null;
  if (!command) return null;
  return applyDocumentBlock(state, command);
}

type InlineMatch = {
  readonly all: string;
  readonly content: string;
  readonly mark: "strong" | "em" | "strike" | "code" | "link";
  readonly attrs?: { href: string };
};

function inlineMatch(value: string): InlineMatch | null {
  const link = /\[([^\]\n]+)\]\(([^)\s]+)\)$/.exec(value);
  if (link) {
    const href = safeDocumentHref(link[2]);
    if (href) return { all: link[0], content: link[1]!, mark: "link", attrs: { href } };
  }
  const patterns: ReadonlyArray<readonly [RegExp, InlineMatch["mark"]]> = [
    [/\*\*([^*\n]+)\*\*$/, "strong"],
    [/__([^_\n]+)__$/, "strong"],
    [/~~([^~\n]+)~~$/, "strike"],
    [/`([^`\n]+)`$/, "code"],
    [/(?<!\*)\*([^*\n]+)\*$/, "em"],
    [/(?<!_)_([^_\n]+)_$/, "em"],
  ];
  for (const [pattern, mark] of patterns) {
    const match = pattern.exec(value);
    if (match) return { all: match[0], content: match[1]!, mark };
  }
  return null;
}

/** Convert a just-completed Markdown inline span into a semantic mark. */
export function markdownInlineInput(
  state: EditorState,
  from: number,
  to: number,
  text: string,
): Transaction | null {
  if (from !== to || !/[)*_~`]$/.test(text)) return null;
  const $from = state.doc.resolve(from);
  if (!$from.parent.isTextblock || $from.parent.type === state.schema.nodes.code_block) return null;
  const before = $from.parent.textBetween(0, $from.parentOffset, "", "\ufffc") + text;
  const match = inlineMatch(before);
  if (!match) return null;
  const start = from - (match.all.length - text.length);
  const transaction = state.tr.insertText(match.content, start, to);
  const mark = state.schema.marks[match.mark];
  if (!mark) return null;
  transaction.addMark(start, start + match.content.length, mark.create(match.attrs));
  return transaction.setSelection(TextSelection.create(transaction.doc, start + match.content.length));
}

/** Enter-time Markdown blocks whose marker has no trailing space. */
export function markdownBlockEnter(state: EditorState): Transaction | null {
  const selection = state.selection;
  if (!selection.empty || selection.$from.depth !== 1) return null;
  const parent = selection.$from.parent;
  if (parent.type !== state.schema.nodes.paragraph || selection.$from.parentOffset !== parent.content.size) {
    return null;
  }
  const value = parent.textContent;
  const fence = /^```([^\s`]*)$/.exec(value);
  if (fence) {
    const transaction = applyDocumentBlock(state, "code");
    if (!transaction) return null;
    const at = selection.$from.before();
    return transaction.setNodeMarkup(at, undefined, { language: fence[1] || null });
  }
  if (/^(?:-{3,}|\*{3,}|_{3,})$/.test(value)) return applyDocumentBlock(state, "divider");
  return null;
}

export function slashQuery(state: EditorState): { from: number; to: number; query: string } | null {
  const selection = state.selection;
  if (!selection.empty || selection.$from.depth !== 1) return null;
  if (selection.$from.parent.type !== state.schema.nodes.paragraph) return null;
  const value = selection.$from.parent.textBetween(0, selection.$from.parentOffset, "", "\ufffc");
  const match = /^\/([a-z0-9 -]*)$/i.exec(value);
  if (!match) return null;
  return {
    from: selection.$from.start(),
    to: selection.from,
    query: match[1]!.trim().toLowerCase(),
  };
}

export function matchingSlashCommands(query: string): readonly SlashCommand[] {
  if (!query) return DOCUMENT_SLASH_COMMANDS;
  return DOCUMENT_SLASH_COMMANDS.filter((command) =>
    [command.label, ...command.keywords].some((word) => word.toLowerCase().includes(query)));
}

/** Remove `/query`, then replace its paragraph with the selected block. */
export function runSlashCommand(
  state: EditorState,
  command: DocumentBlockCommand,
): Transaction | null {
  const slash = slashQuery(state);
  if (!slash) return null;
  const from = state.selection.$from.before();
  const to = state.selection.$from.after();
  const replacement = emptyBlock(state, command);
  const transaction = state.tr.replaceWith(from, to, replacement.node);
  return transaction.setSelection(TextSelection.create(transaction.doc, from + replacement.cursor));
}
