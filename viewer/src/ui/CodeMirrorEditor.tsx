import { useEffect, useLayoutEffect, useRef } from "react";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { markdown } from "@codemirror/lang-markdown";
import { defaultHighlightStyle, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import {
  Annotation,
  EditorSelection,
  EditorState,
  StateEffect,
  StateField,
  type Extension,
} from "@codemirror/state";
import {
  Decoration,
  EditorView,
  keymap,
  placeholder as placeholderExtension,
  WidgetType,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";

import { codeUnitOffset } from "../core/anchor";
import {
  applyTextSplice,
  textRevision,
  textSplice,
  type TextSplice,
} from "../core/textPreview";

export { applyTextSplice, extendTextSplice, textRevision, textSplice } from "../core/textPreview";
export type { TextSplice } from "../core/textPreview";

export interface TextChange {
  previousRevision: string;
  resultRevision: string;
}

export interface RemoteCursor {
  actor: string;
  name: string;
  color: string;
  /** Unicode-scalar offsets in the canonical Markdown string. */
  anchor: number;
  focus?: number;
  uncertain?: boolean;
}

export interface RemoteContext {
  actor: string;
  name: string;
  color: string;
  uncertain?: boolean;
}

export interface RemoteTextPreview extends TextSplice {
  actor: string;
  name: string;
  color: string;
  base: string;
  result: string;
  anchor?: number;
  focus?: number;
  uncertain?: boolean;
}

export type PreviewPhase = "optimistic" | "settled";

interface PreviewMemory {
  baseRevision: string;
  baseText: string;
}

/** The revision-bound optimistic-to-durable handoff. */
export function previewPhase(
  current: string,
  currentRevision: string,
  preview: RemoteTextPreview,
): PreviewPhase | null {
  if (preview.uncertain) return null;
  if (preview.result === currentRevision) return "settled";
  if (preview.base !== currentRevision) return null;
  const applied = applyTextSplice(current, preview);
  return applied !== null && textRevision(applied) === preview.result ? "optimistic" : null;
}

function projectedPreview(
  current: string,
  currentRevision: string,
  preview: RemoteTextPreview,
  memory?: Map<string, PreviewMemory>,
): { phase: PreviewPhase; preview: RemoteTextPreview } | null {
  const direct = previewPhase(current, currentRevision, preview);
  if (direct !== null) {
    if (direct === "optimistic") {
      memory?.set(preview.actor, { baseRevision: preview.base, baseText: current });
    }
    return { phase: direct, preview };
  }

  const remembered = memory?.get(preview.actor);
  if (!remembered || remembered.baseRevision !== preview.base) return null;
  const result = applyTextSplice(remembered.baseText, preview);
  if (result === null || textRevision(result) !== preview.result) return null;

  // A cumulative preview can be several keystrokes ahead of durability. When
  // the acknowledged document still has the unchanged material on both sides
  // of that edit, replace only its intermediate middle with the final intent.
  const base = Array.from(remembered.baseText);
  const now = Array.from(current);
  const prefix = base.slice(0, preview.index);
  const suffix = base.slice(preview.index + preview.delete);
  if (now.length < prefix.length + suffix.length) return null;
  if (!prefix.every((scalar, index) => now[index] === scalar)) return null;
  const suffixAt = now.length - suffix.length;
  if (!suffix.every((scalar, index) => now[suffixAt + index] === scalar)) return null;

  return {
    phase: "optimistic",
    preview: {
      ...preview,
      base: currentRevision,
      index: prefix.length,
      delete: suffixAt - prefix.length,
    },
  };
}

/** Convert a CodeMirror UTF-16 position to Lait's Unicode-scalar coordinate. */
export function scalarOffset(text: string, codeUnits: number): number {
  return Array.from(text.slice(0, Math.max(0, Math.min(codeUnits, text.length)))).length;
}

export function mapOffsetThroughSplice(offset: number, splice: TextSplice): number {
  if (offset <= splice.index) return offset;
  const inserted = Array.from(splice.insert).length;
  const end = splice.index + splice.delete;
  if (offset <= end) return splice.index + inserted;
  return offset - splice.delete + inserted;
}

/** Translate a Lait scalar-coordinate splice into CodeMirror's UTF-16 change. */
export function codeMirrorChange(text: string, splice: TextSplice) {
  return {
    from: codeUnitOffset(text, splice.index),
    to: codeUnitOffset(text, splice.index + splice.delete),
    insert: splice.insert,
  };
}

const externalUpdate = Annotation.define<boolean>();

class CaretWidget extends WidgetType {
  constructor(private readonly cursor: Pick<RemoteCursor, "actor" | "name" | "color" | "uncertain">) {
    super();
  }

  eq(other: CaretWidget): boolean {
    return this.cursor.actor === other.cursor.actor
      && this.cursor.name === other.cursor.name
      && this.cursor.color === other.cursor.color
      && this.cursor.uncertain === other.cursor.uncertain;
  }

  toDOM(): HTMLElement {
    return remoteCaret(this.cursor);
  }

  ignoreEvent(): boolean {
    return true;
  }
}

class PreviewWidget extends WidgetType {
  constructor(private readonly preview: RemoteTextPreview) {
    super();
  }

  eq(other: PreviewWidget): boolean {
    return this.preview.actor === other.preview.actor
      && this.preview.insert === other.preview.insert
      && this.preview.anchor === other.preview.anchor
      && this.preview.index === other.preview.index;
  }

  toDOM(): HTMLElement {
    const inserted = Array.from(this.preview.insert);
    const relativeCaret = this.preview.anchor === undefined
      ? null
      : this.preview.anchor - this.preview.index;
    const caretInside = relativeCaret !== null
      && relativeCaret >= 0
      && relativeCaret <= inserted.length;
    const root = document.createElement("span");
    root.className = "remote-preview-insert";
    root.style.setProperty("--remote-color", this.preview.color);
    root.dataset.remoteActor = this.preview.actor;
    root.setAttribute("aria-hidden", "true");
    if (!caretInside) {
      root.textContent = this.preview.insert;
      return root;
    }
    root.append(document.createTextNode(inserted.slice(0, relativeCaret).join("")));
    root.append(remoteCaret(this.preview));
    root.append(document.createTextNode(inserted.slice(relativeCaret).join("")));
    return root;
  }

  ignoreEvent(): boolean {
    return true;
  }
}

function remoteCaret(
  cursor: Pick<RemoteCursor, "actor" | "name" | "color" | "uncertain">,
): HTMLElement {
  const caret = document.createElement("span");
  caret.className = `remote-caret${cursor.uncertain ? " remote-caret-uncertain" : ""}`;
  caret.style.setProperty("--remote-color", cursor.color);
  caret.dataset.remoteActor = cursor.actor;
  caret.setAttribute("aria-hidden", "true");
  const label = document.createElement("span");
  label.className = "remote-caret-label";
  label.textContent = cursor.name;
  caret.append(label);
  return caret;
}

const setRemoteDecorations = StateEffect.define<DecorationSet>();
const remoteDecorations = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(held, transaction) {
    let next = held.map(transaction.changes);
    for (const effect of transaction.effects) {
      if (effect.is(setRemoteDecorations)) next = effect.value;
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

export function collaborationDecorations(
  state: EditorState,
  cursors: RemoteCursor[],
  previews: RemoteTextPreview[],
  memory?: Map<string, PreviewMemory>,
): DecorationSet {
  const text = state.doc.toString();
  const revision = textRevision(text);
  const decorations: Array<ReturnType<Decoration["range"]>> = [];
  const previewActors = new Set<string>();
  if (memory) {
    const present = new Set(previews.map((preview) => preview.actor));
    for (const actor of memory.keys()) if (!present.has(actor)) memory.delete(actor);
  }
  for (const preview of previews) {
    // A preview is revision-bound whereas the accompanying CRDT caret is not.
    // Never fall back to that bare offset while the preview is crossing a
    // document handoff: it may have been resolved against a newer replica.
    previewActors.add(preview.actor);
    const projected = projectedPreview(text, revision, preview, memory);
    if (projected === null) continue;
    const display = projected.preview;
    if (projected.phase === "settled") {
      if (display.anchor === undefined) continue;
      decorations.push(
        Decoration.widget({ widget: new CaretWidget(display), side: -1 })
          .range(codeUnitOffset(text, display.anchor)),
      );
      continue;
    }
    const from = codeUnitOffset(text, display.index);
    const to = codeUnitOffset(text, display.index + display.delete);
    if (to > from) decorations.push(Decoration.replace({}).range(from, to));
    decorations.push(
      Decoration.widget({ widget: new PreviewWidget(display), side: -2 }).range(from),
    );
    const caretBase = previewCaretBaseOffset(display);
    if (caretBase !== null) {
      decorations.push(
        Decoration.widget({ widget: new CaretWidget(display), side: -1 })
          .range(codeUnitOffset(text, caretBase)),
      );
    }
  }
  for (const cursor of cursors) {
    if (previewActors.has(cursor.actor)) continue;
    const anchor = codeUnitOffset(text, cursor.anchor);
    const focus = cursor.focus === undefined ? anchor : codeUnitOffset(text, cursor.focus);
    if (focus !== anchor) {
      decorations.push(
        Decoration.mark({
          class: "remote-selection",
          attributes: { style: `--remote-color: ${cursor.color}` },
        }).range(Math.min(anchor, focus), Math.max(anchor, focus)),
      );
    }
    decorations.push(
      Decoration.widget({ widget: new CaretWidget(cursor), side: -1 }).range(focus),
    );
  }
  return Decoration.set(decorations, true);
}

function previewCaretBaseOffset(preview: RemoteTextPreview): number | null {
  if (preview.anchor === undefined) return null;
  const inserted = Array.from(preview.insert).length;
  if (preview.anchor < preview.index) return preview.anchor;
  if (preview.anchor > preview.index + inserted) {
    return preview.anchor - inserted + preview.delete;
  }
  return null;
}

/** Source-native live preview. Markdown punctuation is hidden only away from
 * the active line, so every byte is directly editable whenever the caret
 * approaches it. */
function livePreview(state: EditorState): DecorationSet {
  const active = new Set(state.selection.ranges.map((range) => state.doc.lineAt(range.head).number));
  const decorations: Array<ReturnType<Decoration["range"]>> = [];
  syntaxTree(state).iterate({
    enter(node) {
      const line = state.doc.lineAt(node.from);
      const inactive = !active.has(line.number);
      if (/^ATXHeading[1-6]$/.test(node.name)) {
        const level = node.name.slice(-1);
        decorations.push(Decoration.line({ class: `cm-md-heading cm-md-h${level}` }).range(line.from));
      } else if (node.name === "StrongEmphasis") {
        decorations.push(Decoration.mark({ class: "cm-md-strong" }).range(node.from, node.to));
      } else if (node.name === "Emphasis") {
        decorations.push(Decoration.mark({ class: "cm-md-emphasis" }).range(node.from, node.to));
      } else if (node.name === "InlineCode") {
        decorations.push(Decoration.mark({ class: "cm-md-inline-code" }).range(node.from, node.to));
      } else if (node.name === "CodeText") {
        decorations.push(Decoration.mark({ class: "cm-md-code-block" }).range(node.from, node.to));
      } else if (node.name === "Blockquote") {
        decorations.push(Decoration.line({ class: "cm-md-quote" }).range(line.from));
      }
      if (!inactive) return;
      if (node.name === "HeaderMark") {
        const after = state.sliceDoc(node.to, Math.min(node.to + 1, state.doc.length));
        decorations.push(Decoration.replace({}).range(node.from, node.to + (after === " " ? 1 : 0)));
      } else if (node.name === "EmphasisMark" || node.name === "CodeMark") {
        decorations.push(Decoration.replace({}).range(node.from, node.to));
      } else if (node.name === "LinkMark" || node.name === "URL") {
        decorations.push(Decoration.replace({}).range(node.from, node.to));
      }
    },
  });
  return Decoration.set(decorations, true);
}

const livePreviewField = StateField.define<DecorationSet>({
  create: livePreview,
  update(held, transaction) {
    if (!transaction.docChanged && !transaction.selection) return held;
    return livePreview(transaction.state);
  },
  provide: (field) => EditorView.decorations.from(field),
});

const editorTheme = EditorView.theme({
  "&": { color: "var(--color-fg)", backgroundColor: "transparent" },
  ".cm-scroller": { fontFamily: "var(--font-sans)", overflow: "visible" },
  ".cm-content": {
    padding: "8px 0",
    caretColor: "var(--color-accent)",
    lineHeight: "1.65",
    overflowWrap: "anywhere",
  },
  ".cm-line": { padding: "0" },
  // CodeMirror's base theme puts a dotted outline on the focused editor root.
  // `&` is required here—without it this selector looks for a focused child.
  "&.cm-focused": { outline: "none" },
  ".cm-cursor": { borderLeftColor: "var(--color-accent)" },
  ".cm-selectionBackground": {
    backgroundColor: "color-mix(in oklab, var(--color-accent) 25%, transparent) !important",
  },
  ".cm-gutters": { display: "none" },
  ".cm-placeholder": { color: "var(--color-mute)" },
});

export const cursorRefreshMs = 10_000;

export default function CodeMirrorEditor({
  value,
  readOnly = false,
  placeholder,
  className,
  onChange,
  onCommit,
  remoteCursors = [],
  remoteContexts = [],
  remotePreviews = [],
  acceptRemote = true,
  onAwareness,
}: {
  value: string;
  readOnly?: boolean;
  placeholder?: string;
  className?: string;
  onChange: (markdown: string, splice: TextSplice, change: TextChange) => void;
  onCommit: () => void;
  remoteCursors?: RemoteCursor[];
  remoteContexts?: RemoteContext[];
  remotePreviews?: RemoteTextPreview[];
  acceptRemote?: boolean;
  onAwareness?: (
    anchor: number | null,
    focus: number | null,
    typing: boolean,
    markdown: string,
  ) => void;
}) {
  const mount = useRef<HTMLDivElement | null>(null);
  const view = useRef<EditorView | null>(null);
  const emit = useRef(onChange);
  const commit = useRef(onCommit);
  const awareness = useRef(onAwareness);
  const typing = useRef(false);
  const typingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const previewMemory = useRef(new Map<string, PreviewMemory>());
  emit.current = onChange;
  commit.current = onCommit;
  awareness.current = onAwareness;

  useLayoutEffect(() => {
    if (!mount.current) return;
    const publish = (editor: EditorView, active = typing.current) => {
      if (!awareness.current || !editor.hasFocus) return;
      const text = editor.state.doc.toString();
      const selection = editor.state.selection.main;
      awareness.current(
        scalarOffset(text, selection.anchor),
        scalarOffset(text, selection.head),
        active,
        text,
      );
    };
    const extensions: Extension[] = [
      markdown(),
      syntaxHighlighting(defaultHighlightStyle),
      history(),
      keymap.of([...defaultKeymap, ...historyKeymap]),
      EditorView.lineWrapping,
      EditorView.editable.of(!readOnly),
      EditorState.readOnly.of(readOnly),
      editorTheme,
      livePreviewField,
      remoteDecorations,
      EditorView.updateListener.of((update: ViewUpdate) => {
        const editor = update.view;
        if (update.docChanged) {
          const external = update.transactions.some((transaction) =>
            transaction.annotation(externalUpdate) === true
          );
          if (!external) {
            const before = update.startState.doc.toString();
            const markdown = update.state.doc.toString();
            const splice = textSplice(before, markdown);
            if (splice) emit.current(markdown, splice, {
              previousRevision: textRevision(before),
              resultRevision: textRevision(markdown),
            });
            typing.current = true;
            if (typingTimer.current !== null) clearTimeout(typingTimer.current);
            typingTimer.current = setTimeout(() => {
              typing.current = false;
              publish(editor, false);
            }, 1_200);
          }
        }
        if (update.focusChanged && !editor.hasFocus) {
          typing.current = false;
          awareness.current?.(null, null, false, editor.state.doc.toString());
          commit.current();
        } else if (editor.hasFocus && (update.docChanged || update.selectionSet || update.focusChanged)) {
          publish(editor);
        }
      }),
      ...(placeholder ? [placeholderExtension(placeholder)] : []),
    ];
    const editor = new EditorView({
      state: EditorState.create({ doc: value, extensions }),
      parent: mount.current,
    });
    view.current = editor;
    const refresh = setInterval(() => publish(editor), cursorRefreshMs);
    return () => {
      clearInterval(refresh);
      if (typingTimer.current !== null) clearTimeout(typingTimer.current);
      awareness.current?.(null, null, false, editor.state.doc.toString());
      editor.destroy();
      view.current = null;
    };
  }, [readOnly]);

  useLayoutEffect(() => {
    const editor = view.current;
    if (!editor || !acceptRemote) return;
    const current = editor.state.doc.toString();
    if (current === value) return;
    const incoming = textSplice(current, value);
    const selection = editor.state.selection.main;
    const anchor = scalarOffset(current, selection.anchor);
    const head = scalarOffset(current, selection.head);
    const mappedAnchor = incoming ? mapOffsetThroughSplice(anchor, incoming) : anchor;
    const mappedHead = incoming ? mapOffsetThroughSplice(head, incoming) : head;
    const changes = incoming
      ? codeMirrorChange(current, incoming)
      : { from: 0, to: current.length, insert: value };
    editor.dispatch({
      // Preserve the real edit shape. A whole-document replacement collapses
      // every mapped decoration to a boundary during CRDT reconciliation.
      changes,
      selection: EditorSelection.single(
        codeUnitOffset(value, mappedAnchor),
        codeUnitOffset(value, mappedHead),
      ),
      annotations: externalUpdate.of(true),
    });
  }, [value, acceptRemote]);

  useLayoutEffect(() => {
    const editor = view.current;
    if (!editor) return;
    editor.dispatch({
      effects: setRemoteDecorations.of(
        collaborationDecorations(
          editor.state,
          remoteCursors,
          remotePreviews,
          previewMemory.current,
        ),
      ),
    });
  }, [remoteCursors, remotePreviews, value, acceptRemote]);

  useEffect(() => () => {
    if (typingTimer.current !== null) clearTimeout(typingTimer.current);
  }, []);

  return (
    <div className="markdown-editor-host">
      {remoteContexts.length > 0 && (
        <div className="remote-contexts" aria-label="Collaborators editing this description">
          {remoteContexts.map((remote) => (
            <span
              key={remote.actor}
              className={`remote-context${remote.uncertain ? " remote-context-uncertain" : ""}`}
              style={{ "--remote-color": remote.color } as React.CSSProperties}
              data-remote-actor={remote.actor}
              title={`${remote.name} is editing here; exact position unavailable`}
            >
              {remote.name}
            </span>
          ))}
        </div>
      )}
      <div ref={mount} className={`codemirror-markdown ${className ?? ""}`} />
    </div>
  );
}
