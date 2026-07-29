import { useEffect, useRef } from "react";
import {
  Editor,
  defaultValueCtx,
  editorViewCtx,
  editorViewOptionsCtx,
  remarkStringifyOptionsCtx,
  rootCtx,
} from "@milkdown/kit/core";
import { history } from "@milkdown/kit/plugin/history";
import { listener, listenerCtx } from "@milkdown/kit/plugin/listener";
import { trailing } from "@milkdown/kit/plugin/trailing";
import { clipboard } from "@milkdown/kit/plugin/clipboard";
import { cursor } from "@milkdown/kit/plugin/cursor";
import { commonmark } from "@milkdown/kit/preset/commonmark";
import { gfm } from "@milkdown/kit/preset/gfm";
import { replaceAll } from "@milkdown/kit/utils";
import { Milkdown, MilkdownProvider, useEditor } from "@milkdown/react";

import { cn } from "./primitives";

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
 * The commit model is unchanged: keystrokes update a draft, and the write goes
 * out on blur. A doorbell mid-keystroke would otherwise fight the cursor, and
 * `issue_edit` carries the whole description — one commit per session of typing
 * is the honest unit, not one per character.
 */
export default function MilkdownEditor({
  value,
  readOnly = false,
  placeholder,
  className,
  onChange,
  onCommit,
}: {
  /** The stored markdown. Read at mount; see `Bridge` for later changes. */
  value: string;
  readOnly?: boolean;
  placeholder?: string;
  className?: string;
  /** Every keystroke, as markdown. */
  onChange: (markdown: string) => void;
  /** Focus left the editor — the moment to write. */
  onCommit: () => void;
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
}: {
  value: string;
  readOnly: boolean;
  placeholder?: string;
  className?: string;
  onChange: (markdown: string) => void;
  onCommit: () => void;
}) {
  // The editor is built once. Its callbacks are read through refs so a parent
  // re-render never rebuilds the document — rebuilding would drop the caret
  // mid-sentence, which is the one thing a live editor must never do.
  const emit = useRef(onChange);
  emit.current = onChange;
  const latest = useRef(value);

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

  return (
    <div
      className="milkdown-host"
      onBlur={(event) => {
        // React's `onBlur` is `focusout`, so it fires when focus moves *within*
        // the editor too — a link tooltip, a table cell. Only a departure from
        // the whole subtree is a commit.
        if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
        onCommit();
      }}
    >
      <Milkdown />
    </div>
  );
}
