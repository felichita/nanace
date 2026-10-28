// @vitest-environment jsdom
import { BlockNoteEditor } from "@blocknote/core";
import { BlockNoteView } from "@blocknote/mantine";
import { useCreateBlockNote } from "@blocknote/react";
import { act, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vitest";

import { blocksToMarkdown, markdownToBlocks, schema, type Editor } from "./schema";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// Mantine reads matchMedia, which jsdom lacks.
window.matchMedia ??= () =>
  ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) as never;

const MD = [
  "**Topic:** [[Topic: Release process]] and [[Auth|the auth page]].",
  "",
  "| Page | Status |",
  "| --- | --- |",
  "| [[PROJECT-2345: Deploy guide]] | done |",
].join("\n");
const LINKS = ["[[Topic: Release process]]", "[[Auth|the auth page]]", "[[PROJECT-2345: Deploy guide]]"];

function Mounted({ onEditor, onUnmount }: { onEditor: (e: Editor) => void; onUnmount: (md: string) => void }) {
  const [initialContent] = useState(() => markdownToBlocks(BlockNoteEditor.create({ schema }) as unknown as Editor, MD));
  const editor = useCreateBlockNote({ schema, initialContent }) as unknown as Editor;
  useEffect(() => onEditor(editor), [editor, onEditor]);
  // PageView saves pending edits from an effect cleanup when the page is left.
  useEffect(() => () => onUnmount(blocksToMarkdown(editor)), [editor, onUnmount]);
  return <BlockNoteView editor={editor as unknown as BlockNoteEditor} />;
}

describe("an editor mounted in a view", () => {
  it("keeps page links when serialized while mounted, on unmount and after", async () => {
    let editor: Editor | undefined;
    let onUnmount = "";
    const root = createRoot(document.body.appendChild(document.createElement("div")));
    await act(async () =>
      root.render(
        <Mounted
          onEditor={(e) => {
            editor = e;
          }}
          onUnmount={(md) => {
            onUnmount = md;
          }}
        />,
      ),
    );

    const mounted = blocksToMarkdown(editor!);
    await act(async () => root.unmount());
    for (const md of [mounted, onUnmount, blocksToMarkdown(editor!)]) {
      for (const link of LINKS) expect(md).toContain(link);
    }
  });
});
