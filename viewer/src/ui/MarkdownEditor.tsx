import { lazy, Suspense } from "react";

import { Markdown } from "./Markdown";

/**
 * The live editor, off the critical path.
 *
 * Milkdown brings ProseMirror, remark and unified with it — a little over
 * 460 kB raw, which is two thirds of everything else the viewer ships. Loading
 * that to draw a list of issues would be paying for an editor on every screen
 * that has no editor on it, so it arrives when a description does, exactly like
 * the Shiki grammars.
 *
 * The fallback is the read-only renderer, not a spinner: the words are already
 * known, and a blank rectangle where the body should be is a worse answer than
 * the body.
 */
const Editor = lazy(() => import("./MilkdownEditor"));

export function MarkdownEditor(props: {
  value: string;
  readOnly?: boolean;
  placeholder?: string;
  className?: string;
  onChange: (markdown: string) => void;
  onCommit: () => void;
}) {
  return (
    <Suspense
      fallback={
        <div className={props.className}>
          {props.value ? (
            <Markdown text={props.value} />
          ) : (
            <span className="text-mute">{props.placeholder}</span>
          )}
        </div>
      }
    >
      <Editor {...props} />
    </Suspense>
  );
}
