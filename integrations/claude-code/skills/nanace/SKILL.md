---
name: nanace
description: Save a work log of the current task to the nanace knowledge base, e.g. `/nanace project-2345`. Finds the task's page by its key and appends a dated section, or creates the page. Use when the user asks to record, log or save what was done for a task in nanace.
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
`create_context` and `update_context`, named `mcp__nanace__<tool>` in Claude
Code. MCP tools are often loaded on demand: if they are not in your tool list,
load them first (tool search for `nanace`, or `select:mcp__nanace__search_contexts,mcp__nanace__get_context,mcp__nanace__create_context,mcp__nanace__update_context`).
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

## 2. Find the task's page

1. `search_contexts` with the task key (for example `PROJECT-2345`).
2. A page belongs to this task if its **title starts with the key**. Search is
   semantic, so do not accept a page just because it ranked first.
3. If several match, prefer the most recently updated one and mention the
   others to the user.

## 3. Write

Write in English (nanace's semantic search works best with English text) unless
the user asks otherwise. Keep it factual and scannable.

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
- **No page yet**: `create_context` with
  - `title`: `<KEY>: <short task summary>` (for example
    `PROJECT-2345: Move staging ingress to a new gateway`),
  - `content`: `> <goal>` followed by the dated section,
  - `tags`: the lowercase key, `work-log`, and the repository name if any,
  - `team`: the only team the user can write to; otherwise pick the team that
    clearly matches the work (for example `platform`) from `list_teams`, and ask
    the user when it is not clear.

## 4. Report

Tell the user whether the page was created or appended to, its title, team and
`context_id`, and give a 2–3 line summary of what was recorded.
