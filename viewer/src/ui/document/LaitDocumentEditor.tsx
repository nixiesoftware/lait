import { useLayoutEffect, useRef, useState } from "react";
import {
  baseKeymap,
  createParagraphNear,
  exitCode,
  liftEmptyBlock,
  setBlockType,
  splitBlock,
  toggleMark,
} from "prosemirror-commands";
import { history, redo, undo } from "prosemirror-history";
import { keymap } from "prosemirror-keymap";
import { EditorState, Plugin, TextSelection } from "prosemirror-state";
import { splitListItem, wrapInList } from "prosemirror-schema-list";
import { tableEditing } from "prosemirror-tables";
import {
  Decoration,
  DecorationSet,
  EditorView,
  type NodeView,
} from "prosemirror-view";

import { documentPlainText } from "../../core/document";
import { ISSUE_REF } from "../../core/markdown";
import { applyTextSplice, textRevision } from "../../core/textPreview";
import type {
  RemoteContext,
  RemoteCursor,
  RemoteTextPreview,
  TextChange,
  TextSplice,
} from "../CodeMirrorEditor";
import { refChipElement, useRefs, type Refs } from "../RefChip";
import {
  CodeBlockView,
  type CodePresenceCursor,
} from "./CodeBlockView";
import {
  editorPosition,
  projectDocument,
  projectSource,
  projectionSplice,
  sourcePosition,
  type DocumentProjection,
} from "./projection";
import { laitDocumentSchema, safeDocumentHref } from "./schema";

const EXTERNAL = "lait:external-document";
const TYPING_IDLE_MS = 1_200;

export const issueReferencePlugin = new Plugin({
  appendTransaction(transactions, _before, state) {
    if (!transactions.some((transaction) => transaction.docChanged)) return null;
    const replacements: Array<{ from: number; to: number; ref: string; marks: readonly import("prosemirror-model").Mark[] }> = [];
    state.doc.descendants((node, position, parent) => {
      if (!node.isText || parent?.type.name === "code_block") return;
      if (node.marks.some((mark) => mark.type.name === "code")) return;
      const pattern = new RegExp(ISSUE_REF.source, "g");
      for (let match = pattern.exec(node.text ?? ""); match; match = pattern.exec(node.text ?? "")) {
        replacements.push({
          from: position + match.index,
          to: position + match.index + match[0].length,
          ref: match[1]!,
          marks: node.marks,
        });
      }
    });
    if (replacements.length === 0) return null;
    const transaction = state.tr;
    for (const replacement of replacements.reverse()) {
      transaction.replaceWith(
        replacement.from,
        replacement.to,
        laitDocumentSchema.nodes.issue_ref!.create(
          { ref: replacement.ref },
          null,
          replacement.marks,
        ),
      );
    }
    return transaction;
  },
});

function remoteCaret(cursor: Pick<RemoteCursor, "actor" | "name" | "color" | "uncertain">): HTMLElement {
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

function remoteSelection(color: string, from: number, to: number) {
  return Decoration.inline(Math.min(from, to), Math.max(from, to), {
    class: "remote-selection",
    style: `--remote-color: ${color}`,
  });
}

function previewText(insert: string): string {
  if (!insert) return "";
  try {
    return documentPlainText(`// lait-document:1\n${insert}`);
  } catch {
    return "";
  }
}

function presenceDecorations(
  projection: DocumentProjection,
  cursors: readonly RemoteCursor[],
  previews: readonly RemoteTextPreview[],
): DecorationSet {
  const decorations: Decoration[] = [];
  const previewActors = new Set(previews.map((preview) => preview.actor));
  const revision = textRevision(projection.source);

  for (const preview of previews) {
    if (preview.uncertain) continue;
    if (preview.result === revision && preview.anchor !== undefined) {
      const anchor = editorPosition(projection, preview.anchor);
      const focus = editorPosition(projection, preview.focus ?? preview.anchor);
      if (anchor !== focus) decorations.push(remoteSelection(preview.color, anchor, focus));
      decorations.push(Decoration.widget(focus, () => remoteCaret(preview), { side: -1 }));
      continue;
    }
    if (preview.base !== revision) continue;
    const result = applyTextSplice(projection.source, preview);
    if (result === null || textRevision(result) !== preview.result) continue;
    const at = editorPosition(projection, preview.index);
    const visible = previewText(preview.insert);
    if (!visible && preview.anchor === undefined) continue;
    decorations.push(Decoration.widget(at, () => {
      const root = document.createElement("span");
      root.className = "remote-preview-insert";
      root.style.setProperty("--remote-color", preview.color);
      if (visible) root.append(document.createTextNode(visible));
      if (preview.anchor !== undefined) root.append(remoteCaret(preview));
      return root;
    }, { side: -1 }));
  }

  for (const cursor of cursors) {
    if (previewActors.has(cursor.actor)) continue;
    const anchor = editorPosition(projection, cursor.anchor);
    const focus = editorPosition(projection, cursor.focus ?? cursor.anchor);
    if (anchor !== focus) decorations.push(remoteSelection(cursor.color, anchor, focus));
    decorations.push(Decoration.widget(focus, () => remoteCaret(cursor), { side: -1 }));
  }
  return DecorationSet.create(projection.doc, decorations);
}

function codePresence(
  projection: DocumentProjection,
  cursors: readonly RemoteCursor[],
  previews: readonly RemoteTextPreview[],
): CodePresenceCursor[] {
  const present: CodePresenceCursor[] = [];
  const previewActors = new Set(previews.map((preview) => preview.actor));
  const revision = textRevision(projection.source);
  for (const preview of previews) {
    if (preview.result === revision && preview.anchor !== undefined) {
      present.push({
        ...preview,
        anchor: editorPosition(projection, preview.anchor),
        focus: editorPosition(projection, preview.focus ?? preview.anchor),
      });
    } else if (preview.base === revision) {
      const result = applyTextSplice(projection.source, preview);
      if (result !== null && textRevision(result) === preview.result) {
        const at = editorPosition(projection, preview.index);
        present.push({ ...preview, anchor: at, focus: at });
      }
    }
  }
  for (const cursor of cursors) {
    if (previewActors.has(cursor.actor)) continue;
    present.push({
      ...cursor,
      anchor: editorPosition(projection, cursor.anchor),
      focus: editorPosition(projection, cursor.focus ?? cursor.anchor),
    });
  }
  return present;
}

class IssueRefView implements NodeView {
  readonly dom = document.createElement("span");
  private ref: string;
  private refs: Refs | null;

  constructor(ref: string, refs: Refs | null) {
    this.ref = ref;
    this.refs = refs;
    this.draw();
  }

  update(node: Parameters<NonNullable<NodeView["update"]>>[0]): boolean {
    if (node.type.name !== "issue_ref") return false;
    this.ref = String(node.attrs.ref);
    this.draw();
    return true;
  }

  setRefs(refs: Refs | null): void {
    this.refs = refs;
    this.draw();
  }

  private draw(): void {
    this.dom.replaceChildren();
    this.dom.className = "lait-doc-ref";
    this.dom.dataset.ref = this.ref;
    const target = this.refs?.resolve(this.ref);
    if (target && this.refs) {
      this.dom.append(refChipElement(target, this.refs.open));
    } else {
      this.dom.textContent = this.ref;
    }
  }

  stopEvent(event: Event): boolean {
    return event.type === "mousedown" && Boolean(this.refs?.resolve(this.ref));
  }
}

type ToolbarState = {
  top: number;
  left: number;
  strong: boolean;
  em: boolean;
  underline: boolean;
  strike: boolean;
  code: boolean;
  link: boolean;
};

function markHeld(view: EditorView, name: string): boolean {
  const mark = view.state.schema.marks[name];
  if (!mark) return false;
  const { from, to, empty, $from } = view.state.selection;
  return empty
    ? Boolean(mark.isInSet(view.state.storedMarks ?? $from.marks()))
    : view.state.doc.rangeHasMark(from, to, mark);
}

function toolbarState(view: EditorView, host: HTMLElement): ToolbarState | null {
  const selection = view.state.selection;
  if (selection.empty || !view.hasFocus()) return null;
  const start = view.coordsAtPos(selection.from);
  const end = view.coordsAtPos(selection.to);
  const box = host.getBoundingClientRect();
  return {
    top: Math.min(start.top, end.top) - box.top,
    left: (Math.min(start.left, end.left) + Math.max(start.right, end.right)) / 2 - box.left,
    strong: markHeld(view, "strong"),
    em: markHeld(view, "em"),
    underline: markHeld(view, "underline"),
    strike: markHeld(view, "strike"),
    code: markHeld(view, "code"),
    link: markHeld(view, "link"),
  };
}

function mapOffset(offset: number, splice: TextSplice): number {
  if (offset <= splice.index) return offset;
  const inserted = Array.from(splice.insert).length;
  const end = splice.index + splice.delete;
  if (offset <= end) return splice.index + inserted;
  return offset - splice.delete + inserted;
}

export default function LaitDocumentEditor({
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
  onChange: (source: string, splice: TextSplice, change: TextChange) => void;
  onCommit: () => void;
  remoteCursors?: RemoteCursor[];
  remoteContexts?: RemoteContext[];
  remotePreviews?: RemoteTextPreview[];
  acceptRemote?: boolean;
  onAwareness?: (
    anchor: number | null,
    focus: number | null,
    typing: boolean,
    source: string,
  ) => void;
}) {
  const refs = useRefs();
  const [initialProjection] = useState(() => projectSource(value));
  const host = useRef<HTMLDivElement | null>(null);
  const mount = useRef<HTMLDivElement | null>(null);
  const editor = useRef<EditorView | null>(null);
  const projection = useRef(initialProjection);
  const emit = useRef(onChange);
  const commit = useRef(onCommit);
  const awareness = useRef(onAwareness);
  const cursors = useRef(remoteCursors);
  const previews = useRef(remotePreviews);
  const currentRefs = useRef(refs);
  const issueViews = useRef(new Set<IssueRefView>());
  const codeViews = useRef(new Set<CodeBlockView>());
  const typing = useRef(false);
  const typingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [toolbar, setToolbar] = useState<ToolbarState | null>(null);
  const [focused, setFocused] = useState(false);
  emit.current = onChange;
  commit.current = onCommit;
  awareness.current = onAwareness;
  cursors.current = remoteCursors;
  previews.current = remotePreviews;
  currentRefs.current = refs;

  useLayoutEffect(() => {
    if (!mount.current || !host.current) return;
    const initial = initialProjection;
    projection.current = initial;

    const publish = (view: EditorView, active = typing.current) => {
      const editing = view.hasFocus()
        || Boolean(host.current?.contains(document.activeElement));
      if (!awareness.current || !editing) return;
      const selection = view.state.selection;
      awareness.current(
        sourcePosition(projection.current, selection.anchor),
        sourcePosition(projection.current, selection.head),
        active,
        projection.current.source,
      );
    };

    const updateToolbar = (view: EditorView) => {
      if (!host.current) return;
      setToolbar(toolbarState(view, host.current));
    };

    const state = EditorState.create({
      schema: laitDocumentSchema,
      doc: initial.doc,
      plugins: [
        history(),
        keymap({
          "Mod-b": toggleMark(laitDocumentSchema.marks.strong!),
          "Mod-i": toggleMark(laitDocumentSchema.marks.em!),
          "Mod-u": toggleMark(laitDocumentSchema.marks.underline!),
          "Mod-`": toggleMark(laitDocumentSchema.marks.code!),
          "Mod-z": undo,
          "Shift-Mod-z": redo,
          Enter: splitListItem(laitDocumentSchema.nodes.list_item!),
          "Ctrl-Enter": exitCode,
        }),
        keymap({
          Enter: splitBlock,
          "Mod-Enter": createParagraphNear,
          Backspace: liftEmptyBlock,
        }),
        keymap(baseKeymap),
        tableEditing(),
        issueReferencePlugin,
      ],
    });

    const view = new EditorView(mount.current, {
      state,
      editable: () => !readOnly,
      attributes: {
        class: "lait-document-editor prose",
        role: "textbox",
        "aria-multiline": "true",
        ...(placeholder ? { "data-placeholder": placeholder } : {}),
      },
      nodeViews: {
        code_block: (node, outer, getPos) => {
          let held: CodeBlockView;
          held = new CodeBlockView(
            node,
            outer,
            getPos,
            () => {
              typing.current = false;
              awareness.current?.(null, null, false, projection.current.source);
              commit.current();
            },
            () => codePresence(projection.current, cursors.current, previews.current),
            () => codeViews.current.delete(held),
          );
          codeViews.current.add(held);
          return held;
        },
        issue_ref: (node) => {
          const held = new IssueRefView(String(node.attrs.ref), currentRefs.current);
          issueViews.current.add(held);
          return {
            dom: held.dom,
            update: (next) => held.update(next),
            stopEvent: (event) => held.stopEvent(event),
            destroy: () => issueViews.current.delete(held),
          };
        },
      },
      decorations: () => presenceDecorations(
        projection.current,
        cursors.current,
        previews.current,
      ),
      dispatchTransaction(transaction) {
        const previous = projection.current;
        const applied = view.state.applyTransaction(transaction);
        const nextState = applied.state;
        projection.current = projectDocument(nextState.doc);
        view.updateState(nextState);
        codeViews.current.forEach((held) => held.refreshPresence());
        const localDocumentChange = applied.transactions.some(
          (held) => held.docChanged && held.getMeta(EXTERNAL) !== true,
        );
        if (localDocumentChange) {
          const splice = projectionSplice(previous.source, projection.current.source);
          if (splice) {
            emit.current(projection.current.source, splice, {
              previousRevision: textRevision(previous.source),
              resultRevision: textRevision(projection.current.source),
            });
          }
          typing.current = true;
          if (typingTimer.current !== null) clearTimeout(typingTimer.current);
          typingTimer.current = setTimeout(() => {
            typing.current = false;
            publish(view, false);
          }, TYPING_IDLE_MS);
          nextState.doc.descendants((node) => {
            if (node.type.name === "issue_ref") currentRefs.current?.request(String(node.attrs.ref));
          });
        }
        publish(view);
        updateToolbar(view);
      },
      handleDOMEvents: {
        blur(_view) {
          typing.current = false;
          awareness.current?.(null, null, false, projection.current.source);
          setToolbar(null);
          setFocused(false);
          commit.current();
          return false;
        },
        focus(view) {
          setFocused(true);
          publish(view);
          updateToolbar(view);
          return false;
        },
        copy(view, event) {
          if (!event.clipboardData || view.state.selection.empty) return false;
          const slice = view.state.selection.content().content;
          event.clipboardData.setData("text/plain", slice.textBetween(0, slice.size, "\n\n"));
          event.preventDefault();
          return true;
        },
      },
      handleClickOn(view, _position, node, nodePosition, event) {
        const target = event.target instanceof Element ? event.target : null;
        if (
          node.type.name !== "list_item"
          || typeof node.attrs.checked !== "boolean"
          || !target?.closest(".lait-doc-task-box")
        ) return false;
        view.dispatch(view.state.tr.setNodeMarkup(nodePosition, undefined, {
          ...node.attrs,
          checked: !node.attrs.checked,
        }));
        event.preventDefault();
        return true;
      },
    });
    editor.current = view;
    const refresh = window.setInterval(() => publish(view), 10_000);
    return () => {
      window.clearInterval(refresh);
      if (typingTimer.current !== null) clearTimeout(typingTimer.current);
      awareness.current?.(null, null, false, projection.current.source);
      view.destroy();
      issueViews.current.clear();
      codeViews.current.clear();
      editor.current = null;
    };
  }, []);

  useLayoutEffect(() => {
    const view = editor.current;
    if (!view || !acceptRemote || value === projection.current.source) return;
    const previous = projection.current;
    const incoming = projectSource(value);
    const scalarSplice = projectionSplice(previous.source, value);
    const anchor = sourcePosition(previous, view.state.selection.anchor);
    const head = sourcePosition(previous, view.state.selection.head);
    const nextAnchor = scalarSplice ? mapOffset(anchor, scalarSplice) : anchor;
    const nextHead = scalarSplice ? mapOffset(head, scalarSplice) : head;
    let transaction = view.state.tr.replaceWith(0, view.state.doc.content.size, incoming.doc.content);
    const $anchor = transaction.doc.resolve(editorPosition(incoming, nextAnchor));
    const $head = transaction.doc.resolve(editorPosition(incoming, nextHead));
    transaction = transaction
      .setSelection(TextSelection.between($anchor, $head))
      .setMeta(EXTERNAL, true);
    projection.current = incoming;
    view.dispatch(transaction);
  }, [value, acceptRemote]);

  useLayoutEffect(() => {
    const view = editor.current;
    if (!view) return;
    view.setProps({ editable: () => !readOnly });
  }, [readOnly]);

  useLayoutEffect(() => {
    issueViews.current.forEach((held) => held.setRefs(refs));
    const view = editor.current;
    if (!view) return;
    view.state.doc.descendants((node) => {
      if (node.type.name === "issue_ref") refs?.request(String(node.attrs.ref));
    });
    view.updateState(view.state);
  }, [refs]);

  useLayoutEffect(() => {
    editor.current?.updateState(editor.current.state);
    codeViews.current.forEach((held) => held.refreshPresence());
  }, [remoteCursors, remotePreviews]);

  const runMark = (name: "strong" | "em" | "underline" | "strike" | "code") => {
    const view = editor.current;
    const mark = view?.state.schema.marks[name];
    if (!view || !mark) return;
    toggleMark(mark)(view.state, view.dispatch);
    view.focus();
  };

  const runLink = () => {
    const view = editor.current;
    const link = view?.state.schema.marks.link;
    if (!view || !link) return;
    if (markHeld(view, "link")) {
      toggleMark(link)(view.state, view.dispatch);
    } else {
      const href = window.prompt("Link URL", "https://");
      const safe = safeDocumentHref(href?.trim());
      if (!safe) return;
      toggleMark(link, { href: safe })(view.state, view.dispatch);
    }
    view.focus();
  };

  const runBlock = (kind: DocumentBlockKind) => {
    const view = editor.current;
    if (!view) return;
    insertDocumentBlock(view, kind);
    view.focus();
  };

  return (
    <div ref={host} className={`lait-document-editor-host ${className ?? ""}`}>
      {toolbar && (
        <div
          className="lait-document-toolbar"
          style={{ top: toolbar.top, left: toolbar.left }}
          role="toolbar"
          aria-label="Text formatting"
          onMouseDown={(event) => event.preventDefault()}
        >
          {([
            ["strong", "B"],
            ["em", "I"],
            ["underline", "U"],
            ["strike", "S"],
            ["code", "<>"],
          ] as const).map(([name, label]) => (
            <button
              key={name}
              type="button"
              aria-label={name}
              aria-pressed={toolbar[name]}
              onClick={() => runMark(name)}
            >
              {label}
            </button>
          ))}
          <button
            type="button"
            aria-label="link"
            aria-pressed={toolbar.link}
            onClick={runLink}
          >
            ↗
          </button>
        </div>
      )}
      {focused && !readOnly && (
        <div
          className="lait-document-blockbar"
          role="toolbar"
          aria-label="Document blocks"
          onMouseDown={(event) => event.preventDefault()}
        >
          {([
            ["paragraph", "¶", "Paragraph"],
            ["heading", "H", "Heading"],
            ["bullet", "•", "Bulleted list"],
            ["ordered", "1.", "Numbered list"],
            ["callout", "!", "Callout"],
            ["code", "{}", "Code block"],
            ["table", "▦", "Table"],
            ["rule", "—", "Divider"],
          ] as const).map(([kind, label, title]) => (
            <button key={kind} type="button" aria-label={title} onClick={() => runBlock(kind)}>
              {label}
            </button>
          ))}
        </div>
      )}
      {remoteContexts.length > 0 && (
        <div className="remote-contexts" aria-label="Collaborators editing this description">
          {remoteContexts.map((remote) => (
            <span
              key={remote.actor}
              className={`remote-context${remote.uncertain ? " remote-context-uncertain" : ""}`}
              style={{ "--remote-color": remote.color } as React.CSSProperties}
              data-remote-actor={remote.actor}
            >
              {remote.name}
            </span>
          ))}
        </div>
      )}
      <div ref={mount} />
    </div>
  );
}

export type DocumentBlockKind =
  | "paragraph"
  | "heading"
  | "code"
  | "bullet"
  | "ordered"
  | "callout"
  | "table"
  | "rule";

export function insertDocumentBlock(
  view: EditorView,
  kind: DocumentBlockKind,
): boolean {
  switch (kind) {
    case "paragraph":
      return setBlockType(laitDocumentSchema.nodes.paragraph!)(view.state, view.dispatch);
    case "heading":
      return setBlockType(laitDocumentSchema.nodes.heading!, { level: 2 })(view.state, view.dispatch);
    case "code":
      return setBlockType(laitDocumentSchema.nodes.code_block!)(view.state, view.dispatch);
    case "bullet":
      return wrapInList(laitDocumentSchema.nodes.bullet_list!)(view.state, view.dispatch);
    case "ordered":
      return wrapInList(laitDocumentSchema.nodes.ordered_list!)(view.state, view.dispatch);
    case "callout":
      return setBlockType(laitDocumentSchema.nodes.callout!, { tone: "note" })(
        view.state,
        view.dispatch,
      );
    case "table": {
      const cell = (header: boolean) => laitDocumentSchema.nodes[
        header ? "table_header" : "table_cell"
      ]!.create({ align: "left" });
      const row = (header: boolean) => laitDocumentSchema.nodes.table_row!.create(
        null,
        [cell(header), cell(header)],
      );
      const table = laitDocumentSchema.nodes.table!.create(null, [row(true), row(false)]);
      view.dispatch(view.state.tr.replaceSelectionWith(table).scrollIntoView());
      return true;
    }
    case "rule":
      view.dispatch(
        view.state.tr.replaceSelectionWith(laitDocumentSchema.nodes.horizontal_rule!.create())
          .scrollIntoView(),
      );
      return true;
  }
}
