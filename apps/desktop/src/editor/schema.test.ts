// @vitest-environment jsdom
import { BlockNoteEditor } from "@blocknote/core";
import { describe, expect, it } from "vitest";

import { blocksToMarkdown, markdownToBlocks, schema, type Editor } from "./schema";

function editorWith(markdown: string): Editor {
  const editor = BlockNoteEditor.create({ schema }) as unknown as Editor;
  const blocks = markdownToBlocks(editor, markdown);
  if (blocks.length) editor.replaceBlocks(editor.document, blocks);
  return editor;
}

describe("wiki links", () => {
  it("parses [[links]] into pageLink nodes, except in code", () => {
    const editor = editorWith("See [[Deploy Guide]] and [[Auth|the auth page]], not `[[code]]`.");
    const inline = editor.document[0].content as { type: string; props?: Record<string, string> }[];
    const links = inline.filter((c) => c.type === "pageLink").map((c) => c.props);
    expect(links).toEqual([
      { title: "Deploy Guide", label: "" },
      { title: "Auth", label: "the auth page" },
    ]);
  });

  it("round-trips links and formatting through markdown", () => {
    const md = [
      "# Release notes",
      "",
      "Linked: [[Deploy_Guide *v2*]] and [[Auth|the auth page]].",
      "",
      "* item with [[Nested]]",
      "",
      "```",
      "[[not a link]]",
      "```",
    ].join("\n");
    const out = blocksToMarkdown(editorWith(md));
    expect(out).toContain("[[Deploy_Guide *v2*]]");
    expect(out).toContain("[[Auth|the auth page]]");
    expect(out).toContain("[[Nested]]");
    expect(out).toContain("[[not a link]]");
    expect(out).not.toMatch(/\\\[/);
    // A second pass is stable.
    expect(blocksToMarkdown(editorWith(out))).toBe(out);
  });

  it("survives pages crafted to make the link regex backtrack", () => {
    const started = Date.now();
    const out = blocksToMarkdown(editorWith(`[[${"\\".repeat(200)}`));
    expect(out.length).toBeGreaterThan(0);
    expect(Date.now() - started).toBeLessThan(1000);
  });

  it("serializes an empty page as empty content", () => {
    expect(blocksToMarkdown(editorWith(""))).toBe("");
  });
});
