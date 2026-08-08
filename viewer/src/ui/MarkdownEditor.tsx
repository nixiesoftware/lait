import { lazy, Suspense } from "react";

import { Markdown } from "./Markdown";
import type {
  RemoteContext,
  RemoteCursor,
  RemoteTextPreview,
  TextChange,
  TextSplice,
} from "./CodeMirrorEditor";

/**
 * The live editor, off the critical path.
 *
 * CodeMirror and the Markdown grammar stay off list and board routes that never
 * draw an editor, so source editing arrives only when a document does.
 *
 * The fallback is the read-only renderer, not a spinner: the words are already
 * known, and a blank rectangle where the body should be is a worse answer than
 * the body.
 */
const Editor = lazy(() => import("./CodeMirrorEditor"));

export function MarkdownEditor(props: {
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
  /** Caret offset to open at, when the editor is being entered from the
   *  rendered view. See `CodeMirrorEditor`. */
  openAt?: number;
  onAwareness?: (
    anchor: number | null,
    focus: number | null,
    typing: boolean,
    markdown: string,
  ) => void;
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
