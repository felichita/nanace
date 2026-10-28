// BlockNote schema with a `pageLink` inline node that is stored in markdown
// as `[[Title]]` / `[[Title|label]]`, the syntax the server indexes.

import { BlockNoteSchema, defaultInlineContentSpecs } from "@blocknote/core";
import { createReactInlineContentSpec } from "@blocknote/react";
import { FileText } from "lucide-react";
import { createContext, useContext } from "react";

export interface PageLinkResolver {
  /** Page id for a title, if a readable page has it. */
  resolve(title: string): string | undefined;
  open(title: string): void;
}

export const PageLinkContext = createContext<PageLinkResolver>({
  resolve: () => undefined,
  open: () => {},
});

function PageLinkChip({ title, label }: { title: string; label: string }) {
  const links = useContext(PageLinkContext);
  const exists = links.resolve(title) !== undefined;
  return (
    <span
      className={exists ? "page-link" : "page-link missing"}
      title={exists ? title : `${title} (no such page yet)`}
      onMouseDown={(e) => e.preventDefault()}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        links.open(title);
      }}
    >
      <FileText size={14} />
      {label || title}
    </span>
  );
}

export const PageLink = createReactInlineContentSpec(
  {
    type: "pageLink",
    propSchema: { title: { default: "" }, label: { default: "" } },
    content: "none",
  },
  {
    render: ({ inlineContent }) => (
      <PageLinkChip title={inlineContent.props.title} label={inlineContent.props.label} />
    ),
    toExternalHTML: ({ inlineContent }) => {
      const { title, label } = inlineContent.props;
      return <span>{label && label !== title ? `[[${title}|${label}]]` : `[[${title}]]`}</span>;
    },
  },
);

export const schema = BlockNoteSchema.create({
  inlineContentSpecs: { ...defaultInlineContentSpecs, pageLink: PageLink },
});

export type Editor = typeof schema.BlockNoteEditor;
type Block = typeof schema.Block;

const WIKI_LINK = /\[\[([^[\]\n]+?)\]\]/g;

type Inline = { type: string; text?: string; styles?: Record<string, unknown>; [key: string]: unknown };

/** Splits plain text runs containing `[[...]]` into text + pageLink nodes. */
function linkify(content: Inline[]): Inline[] {
  const out: Inline[] = [];
  for (const item of content) {
    if (item.type !== "text" || !item.text || item.styles?.code || !item.text.includes("[[")) {
      out.push(item);
      continue;
    }
    let last = 0;
    for (const match of item.text.matchAll(WIKI_LINK)) {
      const [raw, inner] = match;
      const [title, label = ""] = inner.split("|", 2).map((s) => s.trim());
      if (!title) continue;
      const start = match.index ?? 0;
      if (start > last) out.push({ ...item, text: item.text.slice(last, start) });
      out.push({ type: "pageLink", props: { title, label } });
      last = start + raw.length;
    }
    if (last < item.text.length) out.push({ ...item, text: item.text.slice(last) });
  }
  return out;
}

function linkifyBlocks(blocks: Block[]): Block[] {
  return blocks.map((block) => {
    const b = block as unknown as { type: string; content: unknown; children: Block[] };
    let content = b.content;
    if (b.type === "codeBlock") {
      // Code is literal text; `[[...]]` there is not a link.
    } else if (Array.isArray(content)) {
      content = linkify(content as Inline[]);
    } else if (content && typeof content === "object" && (content as { type?: string }).type === "tableContent") {
      const table = content as { rows: { cells: unknown[] }[] };
      content = {
        ...table,
        rows: table.rows.map((row) => ({
          ...row,
          cells: row.cells.map((cell) => {
            if (Array.isArray(cell)) return linkify(cell as Inline[]);
            const c = cell as { content?: Inline[] };
            return c && Array.isArray(c.content) ? { ...c, content: linkify(c.content) } : cell;
          }),
        })),
      };
    }
    return { ...block, content, children: linkifyBlocks(b.children ?? []) } as Block;
  });
}

export function markdownToBlocks(editor: Editor, markdown: string): Block[] {
  if (!markdown.trim()) return [];
  return linkifyBlocks(editor.tryParseMarkdownToBlocks(markdown));
}

/** Undoes markdown escaping inside `[[...]]`, which serializers apply to brackets. */
function unescapeWikiLinks(markdown: string): string {
  // The two alternatives must not overlap (a backslash is only ever `\\.`),
  // or a long run of backslashes backtracks exponentially and freezes the UI.
  return markdown.replace(/\\?\[\\?\[((?:\\.|[^\]\n\\])+?)\\?\]\\?\]/g, (_m, inner: string) => {
    return `[[${inner.replace(/\\([\\`*_{}[\]()#+\-.!|<>~])/g, "$1")}]]`;
  });
}

export function blocksToMarkdown(editor: Editor): string {
  const blocks = editor.document;
  const empty =
    blocks.length === 1 && Array.isArray(blocks[0].content) && blocks[0].content.length === 0 && !blocks[0].children.length;
  if (empty) return "";
  return unescapeWikiLinks(editor.blocksToMarkdownLossy(blocks)).trimEnd() + "\n";
}
