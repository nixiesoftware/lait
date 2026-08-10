import { useMemo } from "react";

import { parseDocument } from "../core/document";
import { RichDocument } from "./Markdown";

/** The issue document renderer. Its Typst source is never inserted into DOM. */
export function Document({
  source,
  className,
  density = "document",
}: {
  source: string;
  className?: string;
  density?: "document" | "tight";
}) {
  const blocks = useMemo(() => parseDocument(source), [source]);
  return <RichDocument blocks={blocks} {...(className ? { className } : {})} density={density} />;
}
