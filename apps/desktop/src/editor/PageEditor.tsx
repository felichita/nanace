import "@blocknote/mantine/style.css";

import { BlockNoteEditor } from "@blocknote/core";
import { BlockNoteView } from "@blocknote/mantine";
import { SuggestionMenuController, useCreateBlockNote, type DefaultReactSuggestionItem } from "@blocknote/react";
import { FileText } from "lucide-react";
import { useEffect, useState } from "react";

import { useColorScheme } from "../lib/layout";
import { useWorkspace } from "../lib/workspace";
import { markdownToBlocks, schema, type Editor } from "./schema";

let parser: Editor | null = null;

/** A detached editor used only to parse markdown into initial blocks. */
function parse(markdown: string) {
  parser ??= BlockNoteEditor.create({ schema }) as unknown as Editor;
  return markdownToBlocks(parser, markdown);
}

export function PageEditor({
  markdown,
  editable,
  onChange,
  onReady,
}: {
  /** Initial content; the editor must be remounted (via `key`) to replace it. */
  markdown: string;
  editable: boolean;
  onChange: () => void;
  onReady: (editor: Editor) => void;
}) {
  const scheme = useColorScheme();
  const ws = useWorkspace();
  // Parsed once per mount; the parent remounts the editor to reload.
  const [initialContent] = useState(() => {
    const blocks = parse(markdown);
    return blocks.length ? blocks : undefined;
  });
  const editor = useCreateBlockNote({ schema, initialContent }) as unknown as Editor;
  useEffect(() => onReady(editor), [editor, onReady]);

  const pageItems = async (query: string): Promise<DefaultReactSuggestionItem[]> => {
    const q = query.trim().toLowerCase();
    return [...ws.pages.values()]
      .filter((p) => p.title.toLowerCase().includes(q))
      .sort((a, b) => a.title.localeCompare(b.title))
      .slice(0, 12)
      .map((p) => ({
        title: p.title,
        subtext: ws.team(p.team_id)?.name,
        icon: <FileText size={16} />,
        group: "Link to page",
        onItemClick: () => {
          editor.insertInlineContent([{ type: "pageLink", props: { title: p.title, label: "" } }, " "]);
        },
      }));
  };

  return (
    <BlockNoteView
      editor={editor as unknown as BlockNoteEditor}
      editable={editable}
      theme={scheme}
      onChange={onChange}
    >
      <SuggestionMenuController triggerCharacter="@" getItems={pageItems} />
    </BlockNoteView>
  );
}
