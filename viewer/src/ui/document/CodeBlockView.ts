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

import { highlight, type Token } from "../../core/highlight";

type GetPos = () => number | undefined;

/** How long the code has to hold still before it is worth tokenising. Shiki
 *  colours the whole block at once, so this is a rate limit on a burst of
 *  keystrokes rather than a delay anybody waits out: the text is on screen the
 *  moment it is typed and only the colour arrives late. */
const HIGHLIGHT_IDLE_MS = 120;

/**
 * Where the outer selection should go when an arrow key runs off the edge of a
 * code block — or `null` when there is nowhere outside the block to go.
 *
 * `Selection.near` promises a *valid* position, not one outside a given node.
 * A code block that opens or closes the document has no block on its far side,
 * so the nearest valid position is back inside the block's own content, and
 * moving the outer selection there puts a caret where ProseMirror cannot draw
 * one — the node view owns that DOM — and then scrolls the page to it. From the
 * first line of a leading code block, that position is 1: the top of the
 * document. Answering `null` leaves the key to CodeMirror's own motion instead.
 */
export function escapeSelection(
  doc: ProseMirrorNode,
  position: number,
  nodeSize: number,
  direction: -1 | 1,
): Selection | null {
  const target = position + (direction < 0 ? 0 : nodeSize);
  const next = Selection.near(doc.resolve(target), direction);
  const inside = next.from > position && next.to < position + nodeSize;
  return inside ? null : next;
}

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

/**
 * Syntax colour, from the same tokeniser the reading view uses.
 *
 * Not a CodeMirror language: the app carries one highlighter, and giving the
 * editor a second one — a lezer grammar per language, with its own theme
 * mapping — would let an open code block and a closed one disagree about what
 * Rust looks like. The cost is that Shiki tokenises whole and asynchronously,
 * so the colour arrives a frame after the character does. Mapping through
 * `transaction.changes` keeps the existing colours roughly in place until it
 * does, rather than dropping to plain text on every keystroke.
 */
const setHighlight = StateEffect.define<DecorationSet>();
const highlightField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(held, transaction) {
    let next = held.map(transaction.changes);
    for (const effect of transaction.effects) if (effect.is(setHighlight)) next = effect.value;
    return next;
  },
  provide: (field) => CodeMirror.decorations.from(field),
});

/** Shiki's per-line tokens laid back onto CodeMirror's flat offsets. Tokens
 *  carry no newline, so one is added between lines — the same accounting the
 *  reading view does when it emits them into a `pre`. */
function highlightDecorations(lines: Token[][], length: number): DecorationSet {
  const ranges: Array<ReturnType<Decoration["range"]>> = [];
  let at = 0;
  for (const line of lines) {
    for (const token of line) {
      const to = Math.min(at + token.content.length, length);
      if (to > at) {
        const style = Object.entries(token.style)
          .map(([property, value]) => `${property}:${value}`)
          .join(";");
        if (style) {
          ranges.push(
            Decoration.mark({ class: "cm-shiki", attributes: { style } }).range(at, to),
          );
        }
      }
      at = to;
    }
    at = Math.min(at + 1, length);
  }
  return Decoration.set(ranges, true);
}

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
  private highlightTimer: ReturnType<typeof setTimeout> | null = null;
  /** Bumped on every request so a slow grammar's answer to an older document
   *  cannot land on a newer one. */
  private highlightGeneration = 0;
  /** The `text language` pair the current colours were computed from. */
  private highlighted: string | null = null;

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
        highlightField,
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
        CodeMirror.updateListener.of((update) => {
          this.forward(update);
          if (update.docChanged) this.refreshHighlight();
        }),
      ],
    });

    this.dom.append(header, this.code.dom);
    this.refreshPresence();
    this.refreshHighlight();
  }

  /**
   * Re-colour the block, unless it already carries the right colours.
   *
   * Guarded at both ends. Before the request, on the `text language` pair, so
   * that a re-render — a remote caret arriving, the outer document reprojecting
   * — costs nothing. After it, on the same pair again, because the answer is
   * asynchronous and the person kept typing while it was in flight.
   */
  private refreshHighlight(immediate = false): void {
    const language = typeof this.node.attrs.language === "string" ? this.node.attrs.language : "";
    const text = this.code.state.doc.toString();
    const key = `${language} ${text}`;
    if (key === this.highlighted) return;

    if (this.highlightTimer !== null) clearTimeout(this.highlightTimer);
    const generation = (this.highlightGeneration += 1);
    const run = () => {
      this.highlightTimer = null;
      void highlight(text, language || null).then((lines) => {
        if (generation !== this.highlightGeneration) return;
        if (this.code.state.doc.toString() !== text) return;
        this.highlighted = key;
        this.code.dispatch({
          effects: setHighlight.of(
            lines === null
              ? Decoration.none
              : highlightDecorations(lines, this.code.state.doc.length),
          ),
        });
      }).catch(() => {
        // An uncoloured block is a small loss; a throw here would take the
        // surrounding document editor with it. `highlight` already declines
        // rather than throws for a language we do not carry.
      });
    };
    if (immediate) run();
    else this.highlightTimer = setTimeout(run, HIGHLIGHT_IDLE_MS);
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
    const next = escapeSelection(
      this.outer.state.doc,
      position,
      this.node.nodeSize,
      direction,
    );
    // Nowhere outside this block to go: leave the key to CodeMirror.
    if (!next) return false;
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
    // Still worth asking even when the text is settled: the language can change
    // on its own, and `refreshHighlight` costs nothing when neither has.
    if (next === current || this.forwarding) {
      this.refreshHighlight();
      return true;
    }
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
    this.refreshHighlight();
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
    if (this.highlightTimer !== null) clearTimeout(this.highlightTimer);
    // Nothing in flight may land on a destroyed view.
    this.highlightGeneration += 1;
    this.code.destroy();
    this.onDestroy();
  }
}
