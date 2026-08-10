import { EditorSelection, type EditorState, type TransactionSpec } from "@codemirror/state";

import { toggleFence, toggleWrap, wrapped } from "./markdownCommands";

const BLOCK_PREFIX = /^\s*(={1,4} |[-+] )/;

export { toggleFence, toggleWrap, wrapped };

export function toggleDocumentBlock(
  state: EditorState,
  marker: (index: number) => string,
): TransactionSpec {
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from).number;
  const end = state.doc.lineAt(range.to);
  const last = end.from === range.to && end.number > first ? end.number - 1 : end.number;
  let held = true;
  for (let number = first; number <= last; number += 1) {
    const line = state.doc.line(number);
    if ((BLOCK_PREFIX.exec(line.text)?.[0] ?? "") !== marker(number - first)) held = false;
  }
  const changes = [];
  for (let number = first; number <= last; number += 1) {
    const line = state.doc.line(number);
    const existing = BLOCK_PREFIX.exec(line.text)?.[0] ?? "";
    changes.push({
      from: line.from,
      to: line.from + existing.length,
      insert: held ? "" : marker(number - first),
    });
  }
  return { changes };
}

export function documentBlocked(state: EditorState, marker: (index: number) => string): boolean {
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from).number;
  const end = state.doc.lineAt(range.to);
  const last = end.from === range.to && end.number > first ? end.number - 1 : end.number;
  for (let number = first; number <= last; number += 1) {
    const line = state.doc.line(number);
    if ((BLOCK_PREFIX.exec(line.text)?.[0] ?? "") !== marker(number - first)) return false;
  }
  return true;
}

export const DOCUMENT_HEADING = (level: number) => (): string => `${"=".repeat(level)} `;
export const DOCUMENT_BULLET = (): string => "- ";
export const DOCUMENT_ORDERED = (): string => "+ ";

export function insertDocumentLink(state: EditorState, placeholder = "url"): TransactionSpec {
  const { from, to } = state.selection.main;
  const text = state.sliceDoc(from, to);
  const insert = `#link(${JSON.stringify(placeholder)})[${text}]`;
  const target = from + 7;
  return {
    changes: { from, to, insert },
    selection: EditorSelection.range(target, target + placeholder.length),
  };
}

export function toggleDocumentQuote(state: EditorState): TransactionSpec {
  return toggleWrap(state, "#quote(block: true)[", "]");
}

export function toggleDocumentTask(state: EditorState): TransactionSpec {
  return toggleWrap(state, "#lait-task(false)[", "]");
}
