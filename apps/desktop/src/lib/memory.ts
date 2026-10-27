// Turns Claude Code memory files into nanace pages.
//
// A memory file is markdown with YAML-ish frontmatter (name, description,
// metadata.type, metadata.modified) and `[[name]]` links to other memories.
// Pages get the index title from MEMORY.md, links rewritten to `[[Title]]`,
// and a `Source:` footer that identifies the file on later imports.

import { invoke } from "@tauri-apps/api/core";

export interface MemoryFile {
  name: string;
  content: string;
}

export interface MemoryProject {
  id: string;
  label: string;
  index: string | null;
  files: MemoryFile[];
}

export interface MemoryNote {
  /** `<project label>/<file name>`; identifies the note across imports. */
  source: string;
  name: string;
  title: string;
  content: string;
  tags: string[];
}

export const MEMORY_TAG = "claude-memory";
const MAX_TITLE = 200;

export const scanMemory = () => invoke<MemoryProject[]>("scan_claude_memory");

interface Parsed {
  name: string;
  description: string;
  type: string;
  modified: string;
  body: string;
}

export function parseMemoryFile(text: string): Parsed {
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(text);
  const meta = m ? m[1] : "";
  const body = (m ? text.slice(m[0].length) : text).trim();
  const field = (key: string) => {
    const f = new RegExp(`^\\s*${key}:[ \\t]*(.*)$`, "m").exec(meta);
    return f ? f[1].trim().replace(/^["']|["']$/g, "") : "";
  };
  return {
    name: field("name"),
    description: field("description"),
    type: field("type"),
    modified: field("modified").slice(0, 10),
    body,
  };
}

/** File name -> link text from `- [Title](file.md) — hook` index lines. */
export function indexTitles(index: string | null): Map<string, string> {
  const titles = new Map<string, string>();
  for (const m of (index ?? "").matchAll(/\[([^\]\n]+)\]\(([^)\s]+\.md)\)/g)) {
    titles.set(m[2], m[1].trim());
  }
  return titles;
}

const key = (name: string) => name.trim().toLowerCase().replace(/_/g, "-");

function humanize(name: string): string {
  const words = name
    .replace(/\.md$/, "")
    .replace(/^(feedback|project|user|reference)[-_]/, "")
    .replace(/[-_]+/g, " ")
    .trim();
  return words ? words[0].toUpperCase() + words.slice(1) : "Untitled memory";
}

/** First clause of the description, if it reads like a title. */
function titleFromDescription(description: string): string | null {
  const clause = description.split(/ \(| — | - |: |; |, |\. /)[0].trim();
  return clause.length >= 3 && clause.length <= 80 ? clause : null;
}

export function tagFor(label: string): string {
  return label.toLowerCase().replace(/[^\p{L}\p{N}._ -]+/gu, "-").slice(0, 50) || "memory";
}

/** Builds pages for the given projects; links resolve across all of them. */
export function buildNotes(projects: MemoryProject[]): MemoryNote[] {
  const parsed = projects.flatMap((project) => {
    const titles = indexTitles(project.index);
    return project.files.map((file) => {
      const p = parseMemoryFile(file.content);
      const title = (
        titles.get(file.name) ??
        titleFromDescription(p.description) ??
        humanize(p.name || file.name)
      ).slice(0, MAX_TITLE);
      return { project, file, p, title };
    });
  });
  const byName = new Map<string, string>();
  for (const { file, p, title } of parsed) {
    byName.set(key(p.name || file.name.replace(/\.md$/, "")), title);
    byName.set(key(file.name.replace(/\.md$/, "")), title);
  }
  return parsed.map(({ project, file, p, title }) => {
    const body = p.body.replace(/\[\[([^\]\n]+)\]\]/g, (raw, inner: string) => {
      const target = byName.get(key(inner));
      return target ? `[[${target}]]` : raw;
    });
    const source = `${project.label}/${file.name}`;
    const parts = [p.description ? `> ${p.description}` : "", body, "---", sourceLine(source, p.modified)];
    return {
      source,
      name: p.name,
      title,
      content: parts.filter(Boolean).join("\n\n") + "\n",
      tags: [...new Set([MEMORY_TAG, tagFor(project.label), tagFor(p.type || "memory")])].sort(),
    };
  });
}

function sourceLine(source: string, modified: string): string {
  return `Source: Claude memory \`${source}\`${modified ? `, updated ${modified}` : ""}`;
}

/** The `<label>/<file>` source recorded in a page's footer, if any. */
export function sourceOf(content: string): string | null {
  const m = /^Source: Claude memory `([^`\n]+)`/m.exec(content);
  return m ? m[1] : null;
}
