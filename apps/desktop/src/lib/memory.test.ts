import { describe, expect, it } from "vitest";

import { buildNotes, indexTitles, MEMORY_TAG, parseMemoryFile, sourceOf, tagFor, type MemoryProject } from "./memory";

const note = (name: string, type: string, body: string) =>
  `---\nname: ${name}\ndescription: "About ${name}"\nmetadata:\n  type: ${type}\n  modified: 2026-10-22T16:10:55.405Z\n---\n\n${body}\n`;

describe("memory import", () => {
  it("parses frontmatter", () => {
    const p = parseMemoryFile(note("no-auto-push", "feedback", "Never push."));
    expect(p).toEqual({
      name: "no-auto-push",
      description: "About no-auto-push",
      type: "feedback",
      modified: "2026-10-22",
      body: "Never push.",
    });
    expect(parseMemoryFile("plain text").body).toBe("plain text");
  });

  it("builds titled pages with cross-project links and a source footer", () => {
    const projects: MemoryProject[] = [
      {
        id: "-Users-me-Develop-team-apps",
        label: "team-apps",
        index: "- [Clone repos into apps/](clone.md) — hook\n",
        files: [{ name: "clone.md", content: note("clone-repos", "feedback", "See [[pulumi_aliases]] and [[missing]].") }],
      },
      {
        id: "-Users-me-Develop-team-devops",
        label: "team-devops",
        index: null,
        files: [{ name: "user_pulumi_aliases.md", content: note("pulumi_aliases", "user", "plmstg") }],
      },
    ];
    const [clone, aliases] = buildNotes(projects);
    expect(clone.title).toBe("Clone repos into apps/");
    expect(aliases.title).toBe("About pulumi_aliases");
    const bare = buildNotes([{ ...projects[1], files: [{ name: "x_y.md", content: "no frontmatter" }] }]);
    expect(bare[0].title).toBe("X y");
    const described = buildNotes([
      { ...projects[1], files: [{ name: "v.md", content: note("v", "project", "b").replace("About v", "VPC refactoring for staging (in progress)") }] },
    ]);
    expect(described[0].title).toBe("VPC refactoring for staging");
    expect(clone.content).toContain("See [[About pulumi_aliases]] and [[missing]].");
    expect(clone.content.startsWith("> About clone-repos")).toBe(true);
    expect(clone.tags).toEqual(["team-apps", "feedback", MEMORY_TAG].sort());
    expect(sourceOf(clone.content)).toBe("team-apps/clone.md");
    expect(sourceOf(aliases.content)).toBe("team-devops/user_pulumi_aliases.md");
  });

  it("reads index titles and makes valid tags", () => {
    expect(indexTitles("- [A](a.md) — x\n- [B c](b.md)").get("b.md")).toBe("B c");
    expect(tagFor("Proj:One/x")).toBe("proj-one-x");
  });
});
