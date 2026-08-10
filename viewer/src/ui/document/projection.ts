import type { Mark, Node as ProseMirrorNode } from "prosemirror-model";

import {
  DOCUMENT_PREFIX,
  parseDocument,
  type DocumentSplice,
} from "../../core/document";
import type { Align, Block, CalloutTone, Inline } from "../../core/markdown";
import { laitDocumentSchema } from "./schema";

const INTRODUCERS = new Set(["\\", "#", "[", "]", "*", "_", "`", "$", "<", ">", "@"]);

function marked(nodes: readonly ProseMirrorNode[], mark: Mark): ProseMirrorNode[] {
  return nodes.map((node) => node.mark([...node.marks, mark]));
}

function textNodes(text: string): ProseMirrorNode[] {
  const out: ProseMirrorNode[] = [];
  const pieces = text.split("\n");
  pieces.forEach((piece, index) => {
    if (piece) out.push(laitDocumentSchema.text(piece));
    if (index < pieces.length - 1) out.push(laitDocumentSchema.nodes.hard_break!.create());
  });
  return out;
}

function inlineNodes(inline: Inline): ProseMirrorNode[] {
  switch (inline.kind) {
    case "text":
      return textNodes(inline.text);
    case "ref":
      return [laitDocumentSchema.nodes.issue_ref!.create({ ref: inline.ref })];
    case "code":
      return marked(textNodes(inline.text), laitDocumentSchema.marks.code!.create());
    case "strong":
      return marked(inline.children.flatMap(inlineNodes), laitDocumentSchema.marks.strong!.create());
    case "em":
      return marked(inline.children.flatMap(inlineNodes), laitDocumentSchema.marks.em!.create());
    case "strike":
      return marked(inline.children.flatMap(inlineNodes), laitDocumentSchema.marks.strike!.create());
    case "underline":
      return marked(inline.children.flatMap(inlineNodes), laitDocumentSchema.marks.underline!.create());
    case "link":
      return marked(
        inline.children.flatMap(inlineNodes),
        laitDocumentSchema.marks.link!.create({ href: inline.href }),
      );
  }
}

function blockNode(block: Block): ProseMirrorNode {
  const inline = "children" in block ? block.children.flatMap(inlineNodes) : [];
  switch (block.kind) {
    case "paragraph":
      return laitDocumentSchema.nodes.paragraph!.create(null, inline);
    case "heading":
      return laitDocumentSchema.nodes.heading!.create({ level: block.level }, inline);
    case "quote":
      return laitDocumentSchema.nodes.blockquote!.create(null, inline);
    case "callout":
      return laitDocumentSchema.nodes.callout!.create({ tone: block.tone }, inline);
    case "code":
      return laitDocumentSchema.nodes.code_block!.create(
        { language: block.lang },
        block.text ? laitDocumentSchema.text(block.text) : null,
      );
    case "hr":
      return laitDocumentSchema.nodes.horizontal_rule!.create();
    case "list": {
      const items = block.items.map((item) => {
        const paragraph = laitDocumentSchema.nodes.paragraph!.create(
          null,
          item.children.flatMap(inlineNodes),
        );
        return laitDocumentSchema.nodes.list_item!.create({ checked: item.checked }, paragraph);
      });
      return laitDocumentSchema.nodes[block.ordered ? "ordered_list" : "bullet_list"]!
        .create(null, items);
    }
    case "table": {
      const row = (cells: readonly Inline[][], header: boolean) =>
        laitDocumentSchema.nodes.table_row!.create(
          null,
          cells.map((cell, column) =>
            laitDocumentSchema.nodes[header ? "table_header" : "table_cell"]!.create(
              { align: block.align[column] ?? "left" },
              cell.flatMap(inlineNodes),
            ),
          ),
        );
      return laitDocumentSchema.nodes.table!.create(
        null,
        [row(block.head, true), ...block.rows.map((cells) => row(cells, false))],
      );
    }
  }
}

export function documentNodeFromSource(source: string): ProseMirrorNode {
  const blocks = parseDocument(source);
  return laitDocumentSchema.nodes.doc!.create(
    null,
    blocks.length > 0
      ? blocks.map(blockNode)
      : [laitDocumentSchema.nodes.paragraph!.create()],
  );
}

function sameWrapper(a: Inline, b: Inline): boolean {
  if (a.kind !== b.kind) return false;
  return a.kind === "strong" || a.kind === "em" || a.kind === "strike" || a.kind === "underline"
    || (a.kind === "link" && b.kind === "link" && a.href === b.href);
}

function mergeInlines(inlines: Inline[]): Inline[] {
  const out: Inline[] = [];
  for (const inline of inlines) {
    const previous = out.at(-1);
    if (previous?.kind === "text" && inline.kind === "text") {
      previous.text += inline.text;
    } else if (previous && sameWrapper(previous, inline) && "children" in previous && "children" in inline) {
      previous.children.push(...inline.children);
    } else {
      out.push(inline);
    }
  }
  return out;
}

function inlineFromNode(node: ProseMirrorNode): Inline {
  let inline: Inline;
  if (node.type.name === "issue_ref") {
    inline = { kind: "ref", ref: String(node.attrs.ref) };
  } else if (node.type.name === "hard_break") {
    inline = { kind: "text", text: "\n" };
  } else {
    inline = { kind: "text", text: node.text ?? "" };
  }

  for (let index = node.marks.length - 1; index >= 0; index -= 1) {
    const mark = node.marks[index]!;
    switch (mark.type.name) {
      case "strong":
      case "em":
      case "strike":
      case "underline":
        inline = { kind: mark.type.name, children: [inline] };
        break;
      case "code":
        inline = { kind: "code", text: node.textContent };
        break;
      case "link":
        inline = { kind: "link", href: String(mark.attrs.href), children: [inline] };
        break;
    }
  }
  return inline;
}

function inlinesFromNode(node: ProseMirrorNode): Inline[] {
  const children: Inline[] = [];
  node.forEach((child) => children.push(inlineFromNode(child)));
  return mergeInlines(children);
}

function listItemText(item: ProseMirrorNode): Inline[] {
  const paragraph = item.firstChild;
  return paragraph ? inlinesFromNode(paragraph) : [];
}

export function blocksFromDocumentNode(doc: ProseMirrorNode): Block[] {
  const blocks: Block[] = [];
  doc.forEach((node) => {
    switch (node.type.name) {
      case "paragraph":
        blocks.push({ kind: "paragraph", children: inlinesFromNode(node) });
        break;
      case "heading":
        blocks.push({
          kind: "heading",
          level: Math.max(1, Math.min(4, Number(node.attrs.level))) as 1 | 2 | 3 | 4,
          id: "",
          children: inlinesFromNode(node),
        });
        break;
      case "blockquote":
        blocks.push({ kind: "quote", children: inlinesFromNode(node) });
        break;
      case "callout":
        blocks.push({
          kind: "callout",
          tone: String(node.attrs.tone) as CalloutTone,
          children: inlinesFromNode(node),
        });
        break;
      case "code_block":
        blocks.push({
          kind: "code",
          lang: typeof node.attrs.language === "string" ? node.attrs.language : null,
          text: node.textContent,
        });
        break;
      case "horizontal_rule":
        blocks.push({ kind: "hr" });
        break;
      case "bullet_list":
      case "ordered_list": {
        const items: Extract<Block, { kind: "list" }>["items"] = [];
        node.forEach((item) => {
          items.push({
            checked: typeof item.attrs.checked === "boolean" ? item.attrs.checked : null,
            children: listItemText(item),
          });
        });
        blocks.push({ kind: "list", ordered: node.type.name === "ordered_list", items });
        break;
      }
      case "table": {
        const rows: Inline[][][] = [];
        let head: Inline[][] = [];
        const align: Align[] = [];
        node.forEach((row, _rowOffset, rowIndex) => {
          const cells: Inline[][] = [];
          row.forEach((cell, _cellOffset, column) => {
            cells.push(inlinesFromNode(cell));
            if (rowIndex === 0) {
              const value = cell.attrs.align;
              align[column] = value === "center" || value === "right" ? value : "left";
            }
          });
          if (rowIndex === 0) head = cells;
          else rows.push(cells);
        });
        blocks.push({ kind: "table", align, head, rows });
        break;
      }
    }
  });
  return blocks;
}

class ProjectionWriter {
  readonly scalars: string[] = [];
  readonly sourceToEditor: number[] = [0];
  readonly editorToSource: Array<number | undefined>;

  constructor(editorSize: number) {
    this.editorToSource = new Array(editorSize + 1);
  }

  get length(): number {
    return this.scalars.length;
  }

  boundary(editor: number): void {
    this.editorToSource[Math.max(0, Math.min(this.editorToSource.length - 1, editor))] = this.length;
  }

  syntax(value: string, editor: number): void {
    for (const scalar of value) {
      this.scalars.push(scalar);
      this.sourceToEditor.push(editor);
    }
  }

  text(value: string, editorStart: number, escape = true): void {
    let editor = editorStart;
    this.boundary(editor);
    for (const scalar of value) {
      if (escape && INTRODUCERS.has(scalar)) this.syntax("\\", editor);
      this.scalars.push(scalar);
      editor += scalar.length;
      this.sourceToEditor.push(editor);
      this.boundary(editor);
    }
  }

  jsonText(value: string, editorStart: number): void {
    let editor = editorStart;
    this.boundary(editor);
    for (const scalar of value) {
      const encoded = JSON.stringify(scalar).slice(1, -1);
      const encodedScalars = Array.from(encoded);
      encodedScalars.forEach((part, index) => {
        this.scalars.push(part);
        this.sourceToEditor.push(index === encodedScalars.length - 1 ? editor + scalar.length : editor);
      });
      editor += scalar.length;
      this.boundary(editor);
    }
  }

  finish(): { source: string; sourceToEditor: number[]; editorToSource: number[] } {
    let held = 0;
    const editorToSource = this.editorToSource.map((value) => {
      if (value !== undefined) held = value;
      return held;
    });
    return { source: this.scalars.join(""), sourceToEditor: this.sourceToEditor, editorToSource };
  }
}

function markOpen(mark: Mark): string {
  switch (mark.type.name) {
    case "strong": return "*";
    case "em": return "_";
    case "strike": return "#strike[";
    case "underline": return "#underline[";
    case "code": return "#raw(\"";
    case "link": return `#link(${JSON.stringify(String(mark.attrs.href))})[`;
    default: return "";
  }
}

function markClose(mark: Mark): string {
  switch (mark.type.name) {
    case "strong": return "*";
    case "em": return "_";
    case "strike":
    case "underline":
    case "link": return "]";
    case "code": return "\")";
    default: return "";
  }
}

function writeInline(node: ProseMirrorNode, contentStart: number, out: ProjectionWriter): void {
  let open: readonly Mark[] = [];
  let previousEnd = contentStart;
  node.forEach((child, offset) => {
    const at = contentStart + offset;
    let shared = 0;
    while (shared < open.length && shared < child.marks.length && open[shared]!.eq(child.marks[shared]!)) {
      shared += 1;
    }
    for (let index = open.length - 1; index >= shared; index -= 1) {
      out.syntax(markClose(open[index]!), previousEnd);
    }
    for (let index = shared; index < child.marks.length; index += 1) {
      out.syntax(markOpen(child.marks[index]!), at);
    }
    out.boundary(at);
    if (child.type.name === "issue_ref") {
      out.text(String(child.attrs.ref), at);
    } else if (child.type.name === "hard_break") {
      out.syntax("\n", at + child.nodeSize);
      out.boundary(at + child.nodeSize);
    } else if (child.marks.some((mark) => mark.type.name === "code")) {
      out.jsonText(child.text ?? "", at);
    } else {
      out.text(child.text ?? "", at);
    }
    previousEnd = at + child.nodeSize;
    open = child.marks;
  });
  for (let index = open.length - 1; index >= 0; index -= 1) {
    out.syntax(markClose(open[index]!), previousEnd);
  }
  out.boundary(contentStart + node.content.size);
}

function writeBlock(node: ProseMirrorNode, at: number, out: ProjectionWriter): void {
  out.boundary(at);
  const content = at + 1;
  switch (node.type.name) {
    case "paragraph":
      writeInline(node, content, out);
      break;
    case "heading":
      out.syntax(`${"=".repeat(Math.max(1, Math.min(4, Number(node.attrs.level))))} `, content);
      out.boundary(content);
      writeInline(node, content, out);
      break;
    case "blockquote":
      out.syntax("#quote(block: true)[", content);
      out.boundary(content);
      writeInline(node, content, out);
      out.syntax("]", content + node.content.size);
      break;
    case "callout":
      out.syntax(`#lait-callout(${JSON.stringify(String(node.attrs.tone))})[`, content);
      out.boundary(content);
      writeInline(node, content, out);
      out.syntax("]", content + node.content.size);
      break;
    case "code_block": {
      const language = typeof node.attrs.language === "string" ? node.attrs.language : "";
      const fenced = !node.textContent.split("\n").some((line) => line.startsWith("```"));
      if (fenced) {
        out.syntax(`\`\`\`${language}\n`, content);
        out.boundary(content);
        out.text(node.textContent, content, false);
        out.syntax("\n```", content + node.content.size);
      } else {
        const lang = language ? `, lang: ${JSON.stringify(language)}` : "";
        out.syntax(`#raw(block: true${lang}, \"`, content);
        out.boundary(content);
        out.jsonText(node.textContent, content);
        out.syntax("\")", content + node.content.size);
      }
      break;
    }
    case "horizontal_rule":
      out.syntax("#line(length: 100%)", at);
      break;
    case "bullet_list":
    case "ordered_list":
      node.forEach((item, itemOffset, itemIndex) => {
        if (itemIndex > 0) out.syntax("\n", at + 1 + itemOffset);
        const itemAt = at + 1 + itemOffset;
        const paragraph = item.firstChild;
        const inlineAt = itemAt + 2;
        const checked = item.attrs.checked;
        if (typeof checked === "boolean") {
          out.syntax(`#lait-task(${String(checked)})[`, inlineAt);
          out.boundary(inlineAt);
          if (paragraph) writeInline(paragraph, inlineAt, out);
          out.syntax("]", inlineAt + (paragraph?.content.size ?? 0));
        } else {
          out.syntax(node.type.name === "ordered_list" ? "+ " : "- ", inlineAt);
          out.boundary(inlineAt);
          if (paragraph) writeInline(paragraph, inlineAt, out);
        }
        out.boundary(itemAt + item.nodeSize);
      });
      break;
    case "table": {
      out.syntax("#lait-table(\n  align: (", content);
      const firstRow = node.firstChild;
      firstRow?.forEach((cell, _offset, index) => {
        if (index > 0) out.syntax(", ", content);
        out.syntax(JSON.stringify(String(cell.attrs.align ?? "left")), content);
      });
      out.syntax("),\n  header: (", content);
      node.forEach((row, rowOffset, rowIndex) => {
        const rowAt = at + 1 + rowOffset;
        if (rowIndex === 0) {
          row.forEach((cell, cellOffset, cellIndex) => {
            if (cellIndex > 0) out.syntax(", ", rowAt + 1 + cellOffset);
            const cellAt = rowAt + 1 + cellOffset;
            const inlineAt = cellAt + 1;
            out.syntax("[", inlineAt);
            out.boundary(inlineAt);
            writeInline(cell, inlineAt, out);
            out.syntax("]", inlineAt + cell.content.size);
          });
          out.syntax("),\n  rows: (", rowAt + row.nodeSize);
        } else {
          out.syntax("\n    (", rowAt);
          row.forEach((cell, cellOffset, cellIndex) => {
            if (cellIndex > 0) out.syntax(", ", rowAt + 1 + cellOffset);
            const cellAt = rowAt + 1 + cellOffset;
            const inlineAt = cellAt + 1;
            out.syntax("[", inlineAt);
            out.boundary(inlineAt);
            writeInline(cell, inlineAt, out);
            out.syntax("]", inlineAt + cell.content.size);
          });
          out.syntax("),", rowAt + row.nodeSize);
        }
      });
      out.syntax("\n  ),\n)", at + node.nodeSize);
      break;
    }
  }
  out.boundary(at + node.nodeSize);
}

export interface DocumentProjection {
  doc: ProseMirrorNode;
  source: string;
  /** ProseMirror UTF-16 document position to canonical Typst scalar offset. */
  editorToSource: readonly number[];
  /** Canonical Typst scalar offset to nearest ProseMirror document position. */
  sourceToEditor: readonly number[];
  canonical: boolean;
}

export function projectDocument(doc: ProseMirrorNode, original?: string): DocumentProjection {
  const out = new ProjectionWriter(doc.content.size);
  out.syntax(DOCUMENT_PREFIX, 0);
  out.boundary(0);
  doc.forEach((node, offset, index) => {
    if (index > 0) out.syntax("\n\n", offset);
    writeBlock(node, offset, out);
  });
  out.boundary(doc.content.size);
  const result = out.finish();
  return {
    doc,
    ...result,
    canonical: original === undefined || original === result.source,
  };
}

export function projectSource(source: string): DocumentProjection {
  return projectDocument(documentNodeFromSource(source), source);
}

export function editorPosition(projection: DocumentProjection, scalar: number): number {
  const index = Math.max(0, Math.min(projection.sourceToEditor.length - 1, scalar));
  return projection.sourceToEditor[index] ?? 0;
}

export function sourcePosition(projection: DocumentProjection, editor: number): number {
  const index = Math.max(0, Math.min(projection.editorToSource.length - 1, editor));
  return projection.editorToSource[index] ?? 0;
}

/** The one-splice transport used by live previews and the existing text API. */
export function projectionSplice(before: string, after: string): DocumentSplice | null {
  if (before === after) return null;
  const left = Array.from(before);
  const right = Array.from(after);
  let prefix = 0;
  while (prefix < left.length && prefix < right.length && left[prefix] === right[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < left.length - prefix
    && suffix < right.length - prefix
    && left[left.length - 1 - suffix] === right[right.length - 1 - suffix]
  ) suffix += 1;
  return {
    index: prefix,
    delete: left.length - prefix - suffix,
    insert: right.slice(prefix, right.length - suffix).join(""),
  };
}

