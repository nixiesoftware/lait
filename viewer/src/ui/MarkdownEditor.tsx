import { lazy, Suspense } from "react";

import { Markdown } from "./Markdown";
import { Document } from "./Document";
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
 * Both editing kernels stay off list and board routes that never draw an
 * editor. Legacy Markdown loads CodeMirror; controlled Typst loads Lait's
 * semantic document editor, whose ProseMirror state is only a projection.
 *
 * The fallback is the read-only renderer, not a spinner: the words are already
 * known, and a blank rectangle where the body should be is a worse answer than
 * the body.
 */
const SourceEditor = lazy(() => import("./CodeMirrorEditor"));
const DocumentEditor = lazy(() => import("./document/LaitDocumentEditor"));

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
  documentSchema?: number;
  onAwareness?: (
    anchor: number | null,
    focus: number | null,
    typing: boolean,
    markdown: string,
  ) => void;
}) {
  const { documentSchema = 0, ...editorProps } = props;
  return (
    <Suspense
      fallback={
        <div className={props.className}>
          {props.value ? (
            documentSchema ? <Document source={props.value} /> : <Markdown text={props.value} />
          ) : (
            <span className="text-mute">{props.placeholder}</span>
          )}
        </div>
      }
    >
      {documentSchema > 0
        ? <DocumentEditor {...editorProps} />
        : <SourceEditor {...editorProps} documentSchema={documentSchema} />}
    </Suspense>
  );
}
