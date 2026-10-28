---
name: nanace
description: Save a work log of the current task to the nanace knowledge base, e.g. `/nanace project-2345`. Reads the team's `Wiki: Schema` first (initializes the wiki if it has none), finds the task's page by its key and appends a dated section or creates the page, then updates the related Topic and `Wiki: Index`. Use when the user asks to record, log or save what was done for a task in nanace.
argument-hint: <task-key> [what to focus on]
---

# Save a task work log to nanace

Arguments: `$ARGUMENTS`

The first word is the **task key** (for example `project-2345`). Normalize it to
upper case (`PROJECT-2345`). Any remaining words are an optional note on what to
focus on or leave out. Without a key, take it from the current git branch name
(`project-2345`, `feature/PROJECT-2345-...`); if there is none, ask the user.

## Requirements

This needs the nanace MCP tools `search_contexts`, `get_context`,
`create_context`, `update_context`, `list_contexts` and `list_teams`, named
`mcp__nanace__<tool>` in Claude Code. MCP tools are often loaded on demand: if
they are not in your tool list, load them first (tool search for `nanace`, or
`select:mcp__nanace__search_contexts,mcp__nanace__get_context,mcp__nanace__create_context,mcp__nanace__update_context,mcp__nanace__list_contexts,mcp__nanace__list_teams`).
Only conclude they are missing after that.

- **No nanace tools at all**: the nanace server is not connected or the nanace
  API is not running; say so.
- **Only read tools** (no `create_context` / `update_context`): the connection
  is read-only. Tell the user to use **Connect to Claude** in the nanace app
  with **Allow Claude to create and edit pages** once; it applies to all
  future sessions, not only this one.

In both cases still draft the entry and show it, so nothing is lost.

## 1. Gather the facts

Use only what actually happened in this conversation, plus read-only git
commands in the current repository (`git branch --show-current`,
`git log --oneline`, `git status --short`, `git diff --stat`). Never invent
results; mark anything unverified as such. Collect:

- **Goal**: what the task is about, in one sentence.
- **Done**: what changed in this session: repositories, branches, commits
  (short hashes), PRs and their state, files or resources touched,
  infrastructure changed.
- **Decisions**: what was chosen and why, including approaches rejected.
- **Verification**: commands, tests or checks run and their outcome.
- **Open items**: what is left, blocked, or waiting on someone, as a checklist.

Never include secrets: tokens, passwords, private keys, `.env` values,
credentials embedded in URLs. Write "[redacted]" instead.

## 2. Pick the team and read its wiki schema

The team's wiki is organised by a schema page titled exactly `Wiki: Schema`,
next to `Wiki: Index` (catalog). The schema is the authority on page types,
title prefixes, templates, tags and links; follow it over the defaults in this
skill wherever they differ.

1. Pick the team: the only team the user can write to; otherwise the team that
   clearly matches the work (for example `platform`) from `list_teams`; ask the
   user when it is not clear.
2. Find `Wiki: Schema` and `Wiki: Index` with `list_contexts` for that team
   (page through with `offset` if needed; match the titles exactly). Do not
   rely on `search_contexts` for this.
3. `get_context` the `Wiki: Schema` page and read it before writing anything.
4. **No schema, or its body is empty** (blank or only whitespace): initialize
   the wiki.
   - Create or fill `Wiki: Schema` from [schema-template.md](schema-template.md)
     in this skill's directory, with `{{DATE}}` replaced by today's date. The
     template keeps its note that it was initialized automatically. Tags:
     `wiki`, `schema`.
   - If missing, create `Wiki: Index` (`> Catalog of every page in this team's
     wiki, by type. Rules: [[Wiki: Schema]].` plus one heading per page type).
     Tags: `wiki`, `index`.
   - Tell the user in the report that the wiki was initialized and the schema
     needs a review.
   - Do not rename or restructure existing pages during initialization; only
     list them in the Index under their current titles.

## 3. Find the task's page

1. `search_contexts` with the task key (for example `PROJECT-2345`).
2. A page belongs to this task if its **title starts with the key**. Search is
   semantic, so do not accept a page just because it ranked first.
3. If several match, prefer the most recently updated one and mention the
   others to the user.

## 4. Write

Write in English everywhere: page titles, headings, body, tags and log entries
(nanace's semantic search works best with English text), even when the
conversation is in another language. Keep it factual and scannable.

The dated section, with today's date:

```markdown
## YYYY-MM-DD: <what this session achieved, a few words>

**Done**
- ...

**Decisions**
- ... (why)

**Verification**
- ...

**Open items**
- [ ] ...
```

Leave out empty headings. Link related nanace pages found while searching with
`[[Exact Page Title]]`.

- **Page exists**: `get_context` it first, skip anything already recorded, then
  `update_context` with `append` set to the new section. If the update reports a
  conflict, read the page again and retry once.
- **No page yet**: `create_context` in the team from step 2 with
  - `title`: `<KEY>: <short task summary>` (for example
    `PROJECT-2345: Move staging ingress to a new gateway`),
  - `content`: the task template from the schema — `> <goal>`, a
    `**Topic:** [[Topic: …]] · **Status:** …` line when a matching Topic exists,
    then the dated section,
  - `tags`: as the schema says (default: `task`, the lowercase key, and the
    repository name if any).

## 5. Keep the wiki consistent

After writing the task page, do what the schema's ingest operation asks.
Prefer `append`; when a page must be rewritten (`content`), `get_context` it
first, pass its `updated` value as `base_updated_at`, keep everything you are
not changing, and on a conflict read it again and retry once. At minimum:

1. **Topic**: if a `Topic:` page covers this task, update it — add the task link
   if missing, adjust "Current state" / "Open items" when this session changed
   them. Do not create a new Topic just for one task.
2. **Index**: if the task page is new (or any other page was created), add it to
   `Wiki: Index` under the right section with a one-line summary; for task rows
   also update the status when it changed.

## 6. Report

Tell the user whether the task page was created or appended to, its title, team
and `context_id`, which Topic/Index pages were updated, whether the wiki was
initialized in step 2, and give a 2–3 line summary of what was recorded.
