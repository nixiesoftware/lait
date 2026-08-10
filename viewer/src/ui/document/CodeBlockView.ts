import { defaultKeymap } from "@codemirror/commands";
import { defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorSelection, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  drawSelection,
  EditorView as CodeMirror,
  keymap as codeMirrorKeymap,
  WidgetType,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { exitCode } from "prosemirror-commands";
import { redo, undo } from "prosemirror-history";
import type { Node as ProseMirrorNode } from "prosemirror-model";
import { Selection, TextSelection } from "prosemirror-state";
import type { EditorView as ProseMirrorView, NodeView } from "prosemirror-view";

type GetPos = () => number | undefined;

export interface CodePresenceCursor {
  actor: string;
  name: string;
  color: string;
  anchor: number;
  focus: number;
  uncertain?: boolean;
}

const setPresence = StateEffect.define<DecorationSet>();
const presenceField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(held, transaction) {
    let next = held.map(transaction.changes);
    for (const effect of transaction.effects) if (effect.is(setPresence)) next = effect.value;
    return next;
  },
  provide: (field) => CodeMirror.decorations.from(field),
});

class PresenceCaret extends WidgetType {
  constructor(private readonly cursor: CodePresenceCursor) {
    super();
  }

  eq(other: PresenceCaret): boolean {
    return this.cursor.actor === other.cursor.actor
      && this.cursor.name === other.cursor.name
      && this.cursor.color === other.cursor.color
      && this.cursor.uncertain === other.cursor.uncertain;
  }

  toDOM(): HTMLElement {
    const caret = document.createElement("span");
    caret.className = `remote-caret${this.cursor.uncertain ? " remote-caret-uncertain" : ""}`;
    caret.style.setProperty("--remote-color", this.cursor.color);
    caret.dataset.remoteActor = this.cursor.actor;
    caret.setAttribute("aria-hidden", "true");
    const label = document.createElement("span");
    label.className = "remote-caret-label";
    label.textContent = this.cursor.name;
    caret.append(label);
    return caret;
  }

  ignoreEvent(): boolean {
    return true;
  }
}

/**
 * CodeMirror is a component inside Lait's document editor, not its document
 * model. The outer ProseMirror node owns selection/history and the canonical
 * Typst projection; this view contributes code-specific input behavior only.
 */
export class CodeBlockView implements NodeView {
  readonly dom: HTMLElement;
  private node: ProseMirrorNode;
  private readonly outer: ProseMirrorView;
  private readonly getPos: GetPos;
  private readonly onBlur: () => void;
  private readonly presence: () => readonly CodePresenceCursor[];
  private readonly onDestroy: () => void;
  private readonly language: HTMLInputElement;
  private readonly code: CodeMirror;
  private forwarding = false;

  constructor(
    node: ProseMirrorNode,
    outer: ProseMirrorView,
    getPos: GetPos,
    onBlur: () => void = () => undefined,
    presence: () => readonly CodePresenceCursor[] = () => [],
    onDestroy: () => void = () => undefined,
  ) {
    this.node = node;
    this.outer = outer;
    this.getPos = getPos;
    this.onBlur = onBlur;
    this.presence = presence;
    this.onDestroy = onDestroy;

    this.dom = document.createElement("div");
    this.dom.className = "lait-doc-code-shell";

    const header = document.createElement("div");
    header.className = "lait-doc-code-header";
    header.contentEditable = "false";
    const label = document.createElement("span");
    label.textContent = "Code";
    this.language = document.createElement("input");
    this.language.className = "lait-doc-code-language";
    this.language.setAttribute("aria-label", "Code language");
    this.language.placeholder = "plain text";
    this.language.value = typeof node.attrs.language === "string" ? node.attrs.language : "";
    this.language.addEventListener("change", () => this.setLanguage());
    header.append(label, this.language);

    this.code = new CodeMirror({
      doc: node.textContent,
      extensions: [
        drawSelection(),
        syntaxHighlighting(defaultHighlightStyle),
        presenceField,
        codeMirrorKeymap.of([
          { key: "ArrowUp", run: () => this.maybeEscape("line", -1) },
          { key: "ArrowLeft", run: () => this.maybeEscape("char", -1) },
          { key: "ArrowDown", run: () => this.maybeEscape("line", 1) },
          { key: "ArrowRight", run: () => this.maybeEscape("char", 1) },
          {
            key: "Ctrl-Enter",
            run: () => {
              if (!exitCode(this.outer.state, this.outer.dispatch)) return false;
              this.outer.focus();
              return true;
            },
          },
          {
            key: "Mod-z",
            run: () => undo(this.outer.state, this.outer.dispatch),
          },
          {
            key: "Shift-Mod-z",
            run: () => redo(this.outer.state, this.outer.dispatch),
          },
          ...defaultKeymap,
        ]),
        CodeMirror.lineWrapping,
        CodeMirror.theme({
          "&": { background: "transparent", color: "var(--color-fg)" },
          ".cm-scroller": { fontFamily: "var(--font-mono)" },
          ".cm-content": { padding: "12px 14px", caretColor: "var(--color-accent)" },
          ".cm-line": { padding: "0" },
          ".cm-gutters": { display: "none" },
          "&.cm-focused": { outline: "none" },
        }),
        CodeMirror.domEventHandlers({
          blur: () => {
            this.onBlur();
            return false;
          },
        }),
        CodeMirror.updateListener.of((update) => this.forward(update)),
      ],
    });

    this.dom.append(header, this.code.dom);
    this.refreshPresence();
  }

  private position(): number | null {
    const position = this.getPos();
    return typeof position === "number" ? position : null;
  }

  private setLanguage(): void {
    const position = this.position();
    if (position === null) return;
    const value = this.language.value.trim();
    this.outer.dispatch(this.outer.state.tr.setNodeMarkup(position, undefined, {
      ...this.node.attrs,
      language: value || null,
    }));
  }

  private forward(update: ViewUpdate): void {
    if (this.forwarding || !this.code.hasFocus) return;
    const position = this.position();
    if (position === null) return;
    let offset = position + 1;
    const selection = update.state.selection.main;
    const from = offset + selection.from;
    const to = offset + selection.to;
    const outerSelection = this.outer.state.selection;
    if (!update.docChanged && outerSelection.from === from && outerSelection.to === to) return;

    let transaction = this.outer.state.tr;
    update.changes.iterChanges((fromA, toA, fromB, toB, inserted) => {
      const text = inserted.toString();
      if (text) {
        transaction = transaction.replaceWith(
          offset + fromA,
          offset + toA,
          this.outer.state.schema.text(text),
        );
      } else {
        transaction = transaction.delete(offset + fromA, offset + toA);
      }
      offset += (toB - fromB) - (toA - fromA);
    });
    transaction = transaction.setSelection(TextSelection.create(transaction.doc, from, to));
    this.outer.dispatch(transaction);
  }

  refreshPresence(): void {
    const position = this.position();
    if (position === null) return;
    const content = position + 1;
    const end = content + this.node.content.size;
    const decorations: Array<ReturnType<Decoration["range"]>> = [];
    for (const cursor of this.presence()) {
      if (cursor.anchor < content || cursor.anchor > end || cursor.focus < content || cursor.focus > end) {
        continue;
      }
      const anchor = cursor.anchor - content;
      const focus = cursor.focus - content;
      if (anchor !== focus) {
        decorations.push(Decoration.mark({
          class: "remote-selection",
          attributes: { style: `--remote-color: ${cursor.color}` },
        }).range(Math.min(anchor, focus), Math.max(anchor, focus)));
      }
      decorations.push(Decoration.widget({
        widget: new PresenceCaret(cursor),
        side: -1,
      }).range(focus));
    }
    this.code.dispatch({ effects: setPresence.of(Decoration.set(decorations, true)) });
  }

  setSelection(anchor: number, head: number): void {
    this.code.focus();
    this.forwarding = true;
    this.code.dispatch({ selection: EditorSelection.single(anchor, head) });
    this.forwarding = false;
  }

  private maybeEscape(unit: "line" | "char", direction: -1 | 1): boolean {
    const position = this.position();
    if (position === null) return false;
    const selection = this.code.state.selection.main;
    if (!selection.empty) return false;
    const boundary = unit === "line" ? this.code.state.doc.lineAt(selection.head) : selection;
    if (direction < 0 ? boundary.from > 0 : boundary.to < this.code.state.doc.length) return false;
    const target = position + (direction < 0 ? 0 : this.node.nodeSize);
    const next = Selection.near(this.outer.state.doc.resolve(target), direction);
    this.outer.dispatch(this.outer.state.tr.setSelection(next).scrollIntoView());
    this.outer.focus();
    return true;
  }

  update(node: ProseMirrorNode): boolean {
    if (node.type !== this.node.type) return false;
    this.node = node;
    const nextLanguage = typeof node.attrs.language === "string" ? node.attrs.language : "";
    if (this.language.value !== nextLanguage) this.language.value = nextLanguage;

    const next = node.textContent;
    const current = this.code.state.doc.toString();
    if (next === current || this.forwarding) return true;
    let start = 0;
    let currentEnd = current.length;
    let nextEnd = next.length;
    while (start < currentEnd && current.charCodeAt(start) === next.charCodeAt(start)) start += 1;
    while (
      currentEnd > start
      && nextEnd > start
      && current.charCodeAt(currentEnd - 1) === next.charCodeAt(nextEnd - 1)
    ) {
      currentEnd -= 1;
      nextEnd -= 1;
    }
    this.forwarding = true;
    this.code.dispatch({ changes: { from: start, to: currentEnd, insert: next.slice(start, nextEnd) } });
    this.forwarding = false;
    this.refreshPresence();
    return true;
  }

  selectNode(): void {
    this.code.focus();
  }

  stopEvent(): boolean {
    return true;
  }

  ignoreMutation(): boolean {
    return true;
  }

  destroy(): void {
    this.code.destroy();
    this.onDestroy();
  }
}
