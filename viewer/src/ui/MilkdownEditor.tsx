import { useEffect, useRef } from "react";
import {
  Editor,
  defaultValueCtx,
  editorViewCtx,
  editorViewOptionsCtx,
  remarkStringifyOptionsCtx,
  rootCtx,
  serializerCtx,
} from "@milkdown/kit/core";
import { Plugin, PluginKey } from "@milkdown/kit/prose/state";
import { Decoration, DecorationSet, type EditorView } from "@milkdown/kit/prose/view";
import { history } from "@milkdown/kit/plugin/history";
import { listener, listenerCtx } from "@milkdown/kit/plugin/listener";
import { trailing } from "@milkdown/kit/plugin/trailing";
import { clipboard } from "@milkdown/kit/plugin/clipboard";
import { cursor } from "@milkdown/kit/plugin/cursor";
import { commonmark } from "@milkdown/kit/preset/commonmark";
import { gfm } from "@milkdown/kit/preset/gfm";
import { $prose, replaceAll } from "@milkdown/kit/utils";
import { Milkdown, MilkdownProvider, useEditor } from "@milkdown/react";

import { cn } from "./primitives";

export interface RemoteCursor {
  actor: string;
  name: string;
  color: string;
  /** Unicode-scalar offsets in the stored Markdown. */
  anchor: number;
  focus?: number;
  uncertain?: boolean;
}

interface DrawnCursor extends RemoteCursor {
  anchorPos: number;
  focusPos?: number;
}

const remoteCursorKey = new PluginKey<DecorationSet>("lait-remote-cursors");
const remoteCursorPlugin = $prose(
  () =>
    new Plugin<DecorationSet>({
      key: remoteCursorKey,
      state: {
        init: () => DecorationSet.empty,
        apply(transaction, held) {
          const cursors = transaction.getMeta(remoteCursorKey) as DrawnCursor[] | undefined;
          if (!cursors) return held.map(transaction.mapping, transaction.doc);
          const decorations: Decoration[] = [];
          for (const cursor of cursors) {
            const focus = cursor.focusPos;
            if (focus !== undefined && focus !== cursor.anchorPos) {
              decorations.push(
                Decoration.inline(
                  Math.min(cursor.anchorPos, focus),
                  Math.max(cursor.anchorPos, focus),
                  {
                    class: "remote-selection",
                    style: `--remote-color: ${cursor.color}`,
                  },
                  { key: `${cursor.actor}:selection` },
                ),
              );
            }
            decorations.push(
              Decoration.widget(
                focus ?? cursor.anchorPos,
                () => remoteCaret(cursor),
                { key: `${cursor.actor}:caret`, side: -1 },
              ),
            );
          }
          return DecorationSet.create(transaction.doc, decorations);
        },
      },
      props: {
        decorations: (state) => remoteCursorKey.getState(state) ?? DecorationSet.empty,
      },
    }),
);

function remoteCaret(cursor: RemoteCursor): HTMLElement {
  const caret = document.createElement("span");
  caret.className = `remote-caret${cursor.uncertain ? " remote-caret-uncertain" : ""}`;
  caret.style.setProperty("--remote-color", cursor.color);
  caret.setAttribute("aria-hidden", "true");
  const label = document.createElement("span");
  label.className = "remote-caret-label";
  label.textContent = cursor.name;
  caret.append(label);
  return caret;
}

/**
 * How the document is written back.
 *
 * remark's defaults are pretty-printer defaults — `*` bullets, `_` emphasis,
 * and tables padded so the pipes line up. Each one is a whole-document rewrite
 * the first time anyone edits a body that came from the CLI or an agent, and
 * `lait issues show` prints the result, so they are pinned to what people actually
 * type. Exported so the round-trip tests exercise the real configuration
 * rather than remark's.
 */
export const SERIALIZE = {
  bullet: "-",
  emphasis: "*",
  strong: "*",
  fence: "`",
  fences: true,
  rule: "-",
  resourceLink: false,
  // These two belong to `remark-gfm`'s table extension rather than to
  // `mdast-util-to-markdown`'s own options, and the context is typed against
  // the latter — they are read at runtime by the extension `gfm` installs.
  // Without `tablePipeAlign: false`, a three-row table comes back with
  // different bytes on every line even when nothing in it changed.
  ...({ tablePipeAlign: false, tableCellPadding: true } as Record<string, boolean>),
} as const;

/**
 * The issue body, compiled as you type.
 *
 * This replaces a mode switch. The description used to be rendered prose that
 * became a raw textarea on click and back again on blur, so writing an issue
 * meant reading `## ` and `**bold**` while the version everyone else saw lived
 * somewhere you could not see it. Now there is one view: type `## ` and it
 * becomes a heading under the caret.
 *
 * **Milkdown, and specifically Milkdown.** The editor is ProseMirror, but the
 * document is parsed and serialized by *remark* — the same pipeline the rest of
 * the Markdown world runs on — rather than by a serializer written against a
 * ProseMirror schema. That distinction is the whole reason for the choice.
 * lait stores descriptions as plain CRDT text and `lait issues show` prints them
 * verbatim, so an editor that normalises on save would rewrite an agent's issue
 * body the moment a human touched one word, and every such rewrite is a
 * document-wide CRDT op and a noisy diff. remark round-trips what it was given.
 *
 * Keystrokes update a recoverable draft and stream a durable edit after a short
 * quiet window; blur flushes whatever remains. Cursor and selection awareness
 * uses the faster transient lane, so it can move for peers without turning each
 * arrow key into a journal entry.
 */
export default function MilkdownEditor({
  value,
  readOnly = false,
  placeholder,
  className,
  onChange,
  onCommit,
  remoteCursors = [],
  onAwareness,
}: {
  /** The stored markdown. Read at mount; see `Socket` for later changes. */
  value: string;
  readOnly?: boolean;
  placeholder?: string;
  className?: string;
  /** Every keystroke, as markdown. */
  onChange: (markdown: string) => void;
  /** Focus left the editor — flush any write still inside its quiet window. */
  onCommit: () => void;
  remoteCursors?: RemoteCursor[];
  onAwareness?: (anchor: number | null, focus: number | null, typing: boolean) => void;
}) {
  return (
    <MilkdownProvider>
      <Inner
        value={value}
        readOnly={readOnly}
        {...(placeholder !== undefined ? { placeholder } : {})}
        {...(className !== undefined ? { className } : {})}
        onChange={onChange}
        onCommit={onCommit}
        remoteCursors={remoteCursors}
        {...(onAwareness ? { onAwareness } : {})}
      />
    </MilkdownProvider>
  );
}

function Inner({
  value,
  readOnly,
  placeholder,
  className,
  onChange,
  onCommit,
  remoteCursors,
  onAwareness,
}: {
  value: string;
  readOnly: boolean;
  placeholder?: string;
  className?: string;
  onChange: (markdown: string) => void;
  onCommit: () => void;
  remoteCursors: RemoteCursor[];
  onAwareness?: (anchor: number | null, focus: number | null, typing: boolean) => void;
}) {
  // The editor is built once. Its callbacks are read through refs so a parent
  // re-render never rebuilds the document — rebuilding would drop the caret
  // mid-sentence, which is the one thing a live editor must never do.
  const emit = useRef(onChange);
  emit.current = onChange;
  const latest = useRef(value);
  const typingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const typing = useRef(false);

  const { get } = useEditor(
    (root) =>
      Editor.make()
        .config((ctx) => {
          ctx.set(rootCtx, root);
          ctx.set(defaultValueCtx, value);
          ctx.set(remarkStringifyOptionsCtx, SERIALIZE);
          ctx.update(editorViewOptionsCtx, (prev) => ({
            ...prev,
            editable: () => !readOnly,
            attributes: {
              // The same `.prose` layer the read-only renderer uses, so the
              // typeset document and the thing you type into are one design.
              class: cn("prose milkdown-body outline-none", className),
              ...(placeholder ? { "data-placeholder": placeholder } : {}),
            },
          }));
          ctx.get(listenerCtx).markdownUpdated((_ctx, markdown) => {
            // Milkdown fires this on mount too; ignoring the no-op keeps a
            // freshly-opened issue from looking dirty.
            if (markdown === latest.current) return;
            latest.current = markdown;
            emit.current(markdown);
          });
        })
        .use(commonmark)
        .use(gfm)
        .use(listener)
        .use(history)
        .use(clipboard)
        .use(cursor)
        .use(remoteCursorPlugin)
        .use(trailing),
    [readOnly],
  );

  // Adopt server truth while we are not the one holding the pen. The doorbell
  // reloads the issue on every remote write, so without this a teammate's edit
  // would be invisible until the page was rebuilt — and *with* it applied
  // unconditionally, your own in-flight typing would be clobbered by the echo
  // of your own commit.
  useEffect(() => {
    if (value === latest.current) return;
    const editor = get();
    if (!editor) return;
    editor.action((ctx) => {
      // Never while the caret is in here. The doorbell fires on our own commit
      // too, so an unconditional replace would yank the document out from under
      // someone who had already started the next sentence.
      if (ctx.get(editorViewCtx).hasFocus()) return;
      latest.current = value;
      replaceAll(value)(ctx);
    });
  }, [value, get]);

  useEffect(() => {
    const editor = get();
    if (!editor) return;
    editor.action((ctx) => {
      const view = ctx.get(editorViewCtx);
      const serialize = ctx.get(serializerCtx);
      const drawn = remoteCursors.flatMap<DrawnCursor>((remote) => {
        const anchorPos = proseMirrorPositionAt(view, serialize, remote.anchor);
        if (anchorPos === null) return [];
        const focusPos = remote.focus === undefined
          ? undefined
          : proseMirrorPositionAt(view, serialize, remote.focus) ?? undefined;
        return [{ ...remote, anchorPos, ...(focusPos === undefined ? {} : { focusPos }) }];
      });
      view.dispatch(view.state.tr.setMeta(remoteCursorKey, drawn));
    });
  }, [remoteCursors, get]);

  useEffect(
    () => () => {
      if (typingTimer.current !== null) clearTimeout(typingTimer.current);
      onAwareness?.(null, null, false);
    },
    [onAwareness],
  );

  const publishAwareness = (typing: boolean) => {
    if (!onAwareness) return;
    const editor = get();
    if (!editor) return;
    editor.action((ctx) => {
      const view = ctx.get(editorViewCtx);
      if (!view.hasFocus()) return;
      const serialize = ctx.get(serializerCtx);
      const anchor = markdownOffsetAt(view, serialize, view.state.selection.anchor);
      const focus = markdownOffsetAt(view, serialize, view.state.selection.head);
      if (anchor !== null && focus !== null) onAwareness(anchor, focus, typing);
    });
  };

  return (
    <div
      className="milkdown-host"
      onFocus={() => publishAwareness(typing.current)}
      // Typing itself moves the selection and can emit `select` after `input`.
      // Preserve the coarse typing state here instead of immediately erasing
      // the flag the input handler just raised.
      onSelect={() => publishAwareness(typing.current)}
      onInput={() => {
        typing.current = true;
        publishAwareness(true);
        if (typingTimer.current !== null) clearTimeout(typingTimer.current);
        typingTimer.current = setTimeout(() => {
          typing.current = false;
          publishAwareness(false);
        }, 1_200);
      }}
      onBlur={(event) => {
        // React's `onBlur` is `focusout`, so it fires when focus moves *within*
        // the editor too — a link tooltip, a table cell. Only a departure from
        // the whole subtree is a commit.
        if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
        if (typingTimer.current !== null) clearTimeout(typingTimer.current);
        typing.current = false;
        onAwareness?.(null, null, false);
        onCommit();
      }}
    >
      <Milkdown />
    </div>
  );
}

type Serialize = (doc: EditorView["state"]["doc"]) => string;
const CURSOR_MARKER = "LAITCURSORBOUNDARY";

/** Serialize a temporary marker into the ProseMirror document. This is the
 * coordinate bridge between a rich-tree selection and the Markdown CRDT text;
 * using `textContent` would be wrong for headings, links, marks, and tables. */
export function markdownOffsetAt(
  view: EditorView,
  serialize: Serialize,
  position: number,
): number | null {
  const doc = view.state.tr.insertText(CURSOR_MARKER, position).doc;
  const markdown = serialize(doc);
  const at = markdown.indexOf(CURSOR_MARKER);
  return at < 0 ? null : [...markdown.slice(0, at)].length;
}

/** Inverse of `markdownOffsetAt`, found monotonically over legal ProseMirror
 * positions. Remote offsets originated through the forward mapping, so an exact
 * answer normally exists; nearest is the honest fallback after a concurrent
 * structural edit. */
export function proseMirrorPositionAt(
  view: EditorView,
  serialize: Serialize,
  wanted: number,
): number | null {
  let low = 1;
  let high = Math.max(1, view.state.doc.content.size - 1);
  let best: { position: number; distance: number } | null = null;
  while (low <= high) {
    const position = Math.floor((low + high) / 2);
    const offset = markdownOffsetAt(view, serialize, position);
    if (offset === null) return best?.position ?? null;
    const distance = Math.abs(offset - wanted);
    if (!best || distance < best.distance) best = { position, distance };
    if (offset === wanted) return position;
    if (offset < wanted) low = position + 1;
    else high = position - 1;
  }
  return best?.position ?? null;
}
